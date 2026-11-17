//! Trusted login-session identity lookup through systemd-logind.
//!
//! This module uses libsystemd's sd-login API directly. It deliberately avoids
//! D-Bus clients that may create background executor threads: PAM session
//! ownership requires the privileged supervisor to remain single-threaded.

use std::{
    ffi::{CStr, CString, c_char},
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::session_control::SessionIdentity;

const SYSTEMCTL: &str = "/usr/bin/systemctl";

#[link(name = "systemd")]
unsafe extern "C" {
    fn sd_pid_get_session(pid: libc::pid_t, session: *mut *mut c_char) -> libc::c_int;
    fn sd_pid_get_unit(pid: libc::pid_t, unit: *mut *mut c_char) -> libc::c_int;
    fn sd_session_get_uid(session: *const c_char, uid: *mut libc::uid_t) -> libc::c_int;
    fn sd_session_get_seat(session: *const c_char, seat: *mut *mut c_char) -> libc::c_int;
    fn sd_session_get_vt(session: *const c_char, vt: *mut libc::c_uint) -> libc::c_int;
}

/// Identity resolved independently from logind for one process.
///
/// The scope is supervisor-owned metadata. It is intentionally not accepted
/// from the worker protocol and must never be replaced with a worker claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLogindSession {
    pub identity: SessionIdentity,
    pub scope: String,
}

/// Resolves the logind session containing the supplied PID.
///
/// The PID must be the privileged PAM/session supervisor after PAM has opened
/// the session. All lookups fail closed. The runtime directory is accepted only
/// when /run/user/<uid> exists as a real directory owned by that UID.
pub fn resolve_session_for_pid(pid: u32) -> io::Result<ResolvedLogindSession> {
    let pid = libc::pid_t::try_from(pid)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| invalid_input("invalid process identifier"))?;

    let session_id = sd_string_from_pid(pid, sd_pid_get_session)?;
    validate_session_id(&session_id)?;
    let session = CString::new(session_id.as_bytes())
        .map_err(|_| invalid_data("logind returned an invalid session identifier"))?;

    let mut uid: libc::uid_t = 0;
    let result = unsafe { sd_session_get_uid(session.as_ptr(), &mut uid) };
    check_sd_result(result)?;
    if uid == 0 {
        return Err(invalid_data("root login sessions are not supported"));
    }

    let scope = sd_string_from_pid(pid, sd_pid_get_unit)?;
    validate_session_scope(&scope, &session_id)?;

    let seat = optional_sd_string(|value| unsafe { sd_session_get_seat(session.as_ptr(), value) })?;
    let vt_number = optional_sd_vt(session.as_ptr())?;

    let runtime_dir = PathBuf::from(format!("/run/user/{uid}"));
    let metadata = std::fs::symlink_metadata(&runtime_dir)?;
    validate_runtime_directory_metadata(&metadata, uid)?;

    // Re-read both PID associations after collecting all session properties.
    // This detects a process exit/reuse or session/scope reassignment during
    // lookup. The supervisor still owns and has not reaped this child PID.
    let confirmed_session = sd_string_from_pid(pid, sd_pid_get_session)?;
    let confirmed_scope = sd_string_from_pid(pid, sd_pid_get_unit)?;
    if confirmed_session != session_id || confirmed_scope != scope {
        return Err(invalid_data(
            "process session or scope changed during logind identity lookup",
        ));
    }

    Ok(ResolvedLogindSession {
        identity: SessionIdentity {
            uid,
            session_id,
            runtime_dir: runtime_dir.to_string_lossy().into_owned(),
            seat,
            vt_number,
        },
        scope,
    })
}

/// Stops the logind session scope from the privileged supervisor.
///
/// The scope name must come from `resolve_session_for_pid`, never from the
/// worker protocol or user input. The worker must already have exited so PAM
/// can close its session before systemd terminates remaining descendants.
///
/// systemctl is used as a short-lived D-Bus client because zbus 4.4 has no
/// method-call timeout API. Each client process is polled and killed on
/// timeout, so a stalled system bus cannot pin the privileged supervisor.
pub fn stop_session_scope(scope: &str, timeout: Duration) -> io::Result<()> {
    validate_scope_name(scope)?;
    if timeout.is_zero() {
        return Err(invalid_input("scope stop timeout must be non-zero"));
    }

    stop_scope_with(
        timeout,
        |remaining| read_unit_active_state(scope, remaining),
        |remaining| {
            run_systemctl(&["--no-block", "stop", scope], remaining)?;
            Ok(())
        },
    )
}

fn run_systemctl(arguments: &[&str], timeout: Duration) -> io::Result<String> {
    run_command_with_timeout(SYSTEMCTL, arguments, timeout)
}

/// Runs one systemctl client with a hard process deadline.
///
/// Output is collected only after the child exits. The only caller requesting
/// stdout asks systemd for one short ActiveState value; stop requests ignore
/// their empty output. No shell is involved, and the validated scope remains
/// a single argv element.
fn run_command_with_timeout(
    program: &str,
    arguments: &[&str],
    timeout: Duration,
) -> io::Result<String> {
    if timeout.is_zero() {
        return Err(timed_out("system command exceeded its time budget"));
    }

    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let started = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        format!("{program} {} exited with {status}", arguments.join(" ")),
                    ));
                }

                let mut output = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    stdout.read_to_string(&mut output)?;
                }
                return Ok(output.trim().to_owned());
            }
            Ok(None) => {
                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(timed_out("system command exceeded its time budget"));
                }
                thread::sleep(Duration::from_millis(10).min(timeout - elapsed));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
    }
}

fn timed_out(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, message)
}

fn stop_scope_with(
    timeout: Duration,
    mut read_active_state: impl FnMut(Duration) -> io::Result<String>,
    stop_unit: impl FnOnce(Duration) -> io::Result<()>,
) -> io::Result<()> {
    let started = Instant::now();
    let active_state = read_active_state(remaining_timeout(started, timeout)?)?;
    match active_state.as_str() {
        "inactive" => return Ok(()),
        "failed" => return Err(invalid_data("session scope is already in failed state")),
        _ => {}
    }

    stop_unit(remaining_timeout(started, timeout)?)?;
    let remaining = remaining_timeout(started, timeout)?;
    wait_for_scope_inactive(remaining, read_active_state)
}

fn remaining_timeout(started: Instant, timeout: Duration) -> io::Result<Duration> {
    let elapsed = started.elapsed();
    if elapsed >= timeout {
        return Err(timed_out("timed out while stopping the session scope"));
    }
    Ok(timeout - elapsed)
}

fn wait_for_scope_inactive(
    timeout: Duration,
    mut read_active_state: impl FnMut(Duration) -> io::Result<String>,
) -> io::Result<()> {
    let started = Instant::now();
    loop {
        let remaining = remaining_timeout(started, timeout)?;
        let active_state = read_active_state(remaining)?;
        let remaining = remaining_timeout(started, timeout)?;
        match active_state.as_str() {
            "inactive" => return Ok(()),
            "failed" => {
                return Err(invalid_data("systemd failed to stop the session scope"));
            }
            _ => thread::sleep(Duration::from_millis(50).min(remaining)),
        }
    }
}

fn read_unit_active_state(scope: &str, timeout: Duration) -> io::Result<String> {
    let state = run_systemctl(
        &[
            "show",
            "--no-pager",
            "--property=ActiveState",
            "--value",
            scope,
        ],
        timeout,
    )?;
    if state.is_empty() || state.lines().count() != 1 {
        return Err(invalid_data("systemd returned an invalid unit ActiveState"));
    }
    Ok(state)
}

fn validate_scope_name(scope: &str) -> io::Result<()> {
    let Some(session_id) = scope
        .strip_prefix("session-")
        .and_then(|value| value.strip_suffix(".scope"))
    else {
        return Err(invalid_input("invalid logind session scope name"));
    };
    validate_session_id(session_id).map_err(|_| invalid_input("invalid logind session scope name"))
}

type PidStringLookup = unsafe extern "C" fn(libc::pid_t, *mut *mut c_char) -> libc::c_int;

fn validate_runtime_directory_metadata(
    metadata: &std::fs::Metadata,
    uid: libc::uid_t,
) -> io::Result<()> {
    if !metadata.file_type().is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(invalid_data(
            "logind runtime directory must be a private real directory owned by the session UID",
        ));
    }
    Ok(())
}

fn sd_string_from_pid(pid: libc::pid_t, lookup: PidStringLookup) -> io::Result<String> {
    let mut value = std::ptr::null_mut();
    let result = unsafe { lookup(pid, &mut value) };
    check_sd_result(result)?;
    take_sd_string(value)
}

fn optional_sd_string(
    lookup: impl FnOnce(*mut *mut c_char) -> libc::c_int,
) -> io::Result<Option<String>> {
    let mut value = std::ptr::null_mut();
    let result = lookup(&mut value);
    if result == -libc::ENODATA {
        return Ok(None);
    }
    check_sd_result(result)?;
    take_sd_string(value).map(Some)
}

fn optional_sd_vt(session: *const c_char) -> io::Result<Option<u32>> {
    let mut vt = 0;
    let result = unsafe { sd_session_get_vt(session, &mut vt) };
    if result == -libc::ENODATA {
        return Ok(None);
    }
    check_sd_result(result)?;
    if !(1..=63).contains(&vt) {
        return Err(invalid_data("logind returned an invalid VT number"));
    }
    Ok(Some(vt))
}

fn take_sd_string(value: *mut c_char) -> io::Result<String> {
    if value.is_null() {
        return Err(invalid_data("libsystemd returned a null string"));
    }
    let owned = unsafe { CStr::from_ptr(value).to_owned() };
    unsafe { libc::free(value.cast()) };
    owned
        .into_string()
        .map_err(|_| invalid_data("libsystemd returned a non-UTF-8 string"))
}

fn check_sd_result(result: libc::c_int) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::from_raw_os_error(-result))
    } else {
        Ok(())
    }
}

fn validate_session_id(session_id: &str) -> io::Result<()> {
    if session_id.is_empty()
        || session_id.len() > 64
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(invalid_data(
            "logind returned an invalid session identifier",
        ));
    }
    Ok(())
}

fn validate_session_scope(scope: &str, session_id: &str) -> io::Result<()> {
    let expected = format!("session-{session_id}.scope");
    if scope != expected {
        return Err(invalid_data(
            "process does not belong to its expected logind session scope",
        ));
    }
    Ok(())
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_runner_returns_stdout_on_success() {
        let output =
            run_command_with_timeout("/bin/echo", &["active"], Duration::from_secs(1)).unwrap();
        assert_eq!(output.trim(), "active");
    }

    #[test]
    fn command_runner_kills_a_process_after_its_deadline() {
        let started = Instant::now();
        let error = run_command_with_timeout(
            "/bin/sleep",
            &["5"],
            Duration::from_millis(20),
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn accepts_logind_session_identifiers() {
        for value in ["c1", "seat0-42", "session_7.test"] {
            assert!(validate_session_id(value).is_ok(), "{value}");
        }
    }

    #[test]
    fn rejects_invalid_logind_session_identifiers() {
        let too_long = "a".repeat(65);
        for value in ["", "../c1", "c1/session", "session name", too_long.as_str()] {
            assert_eq!(
                validate_session_id(value).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn accepts_only_the_matching_session_scope() {
        assert!(validate_session_scope("session-c1.scope", "c1").is_ok());
        for scope in [
            "user@1000.service",
            "session-c2.scope",
            "session-c1.service",
            "session-c1.scope/evil",
        ] {
            assert_eq!(
                validate_session_scope(scope, "c1").unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn accepts_only_logind_session_scope_names_for_cleanup() {
        assert!(validate_scope_name("session-c1.scope").is_ok());
        for value in [
            "user@1000.service",
            "session-c1.service",
            "session-../evil.scope",
            "session-c1.scope/evil",
        ] {
            assert!(validate_scope_name(value).is_err(), "{value}");
        }
    }

    #[test]
    fn scope_cleanup_wait_accepts_inactive_state_immediately() {
        let mut reads = 0;
        let result = wait_for_scope_inactive(Duration::from_secs(1), |_| {
            reads += 1;
            Ok("inactive".to_owned())
        });
        assert!(result.is_ok());
        assert_eq!(reads, 1);
    }

    #[test]
    fn scope_cleanup_accepts_extremely_large_timeout() {
        let result = wait_for_scope_inactive(Duration::MAX, |_| Ok("inactive".to_owned()));
        assert!(result.is_ok());
    }

    #[test]
    fn scope_cleanup_waits_until_systemd_reports_inactive() {
        let mut states = ["activating", "deactivating", "inactive"].into_iter();
        let result = wait_for_scope_inactive(Duration::from_secs(1), |_| {
            Ok(states.next().unwrap_or("inactive").to_owned())
        });
        assert!(result.is_ok());
    }

    #[test]
    fn scope_cleanup_rejects_failed_state() {
        let error = wait_for_scope_inactive(Duration::from_secs(1), |_| Ok("failed".to_owned()))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn scope_cleanup_times_out_while_scope_remains_active() {
        let error = wait_for_scope_inactive(Duration::from_millis(1), |_| Ok("active".to_owned()))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn scope_cleanup_propagates_dbus_read_errors() {
        let error = wait_for_scope_inactive(Duration::from_secs(1), |_| {
            Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "bus disconnected",
            ))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
    }

    #[test]
    fn scope_cleanup_does_not_stop_an_already_inactive_unit() {
        let mut reads = 0;
        let mut stops = 0;
        let result = stop_scope_with(
            Duration::from_secs(1),
            |_| {
                reads += 1;
                Ok("inactive".to_owned())
            },
            |_| {
                stops += 1;
                Ok(())
            },
        );
        assert!(result.is_ok());
        assert_eq!(reads, 1);
        assert_eq!(stops, 0);
    }

    #[test]
    fn scope_cleanup_does_not_stop_a_failed_unit() {
        let mut stops = 0;
        let error = stop_scope_with(
            Duration::from_secs(1),
            |_| Ok("failed".to_owned()),
            |_| {
                stops += 1;
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(stops, 0);
    }

    #[test]
    fn scope_cleanup_requests_stop_once_then_waits_for_inactive() {
        let mut states = ["active", "deactivating", "inactive"].into_iter();
        let mut reads = 0;
        let mut stops = 0;
        let result = stop_scope_with(
            Duration::from_secs(1),
            |_| {
                reads += 1;
                Ok(states.next().unwrap().to_owned())
            },
            |_| {
                stops += 1;
                Ok(())
            },
        );
        assert!(result.is_ok());
        assert_eq!(reads, 3);
        assert_eq!(stops, 1);
    }

    #[test]
    fn scope_cleanup_includes_stop_unit_time_in_shared_timeout_budget() {
        let error = stop_scope_with(
            Duration::from_millis(5),
            |_| Ok("active".to_owned()),
            |_| {
                thread::sleep(Duration::from_millis(10));
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn scope_cleanup_propagates_stop_unit_errors_without_polling() {
        let mut reads = 0;
        let error = stop_scope_with(
            Duration::from_secs(1),
            |_| {
                reads += 1;
                Ok("active".to_owned())
            },
            |_| {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "StopUnit denied",
                ))
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(reads, 1);
    }

    #[test]
    fn scope_cleanup_propagates_initial_state_read_errors() {
        let mut stops = 0;
        let error = stop_scope_with(
            Duration::from_secs(1),
            |_| {
                Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "bus disconnected",
                ))
            },
            |_| {
                stops += 1;
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
        assert_eq!(stops, 0);
    }

    #[test]
    fn runtime_directory_must_be_private_and_owned_by_session_uid() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let metadata = std::fs::symlink_metadata(directory.path()).unwrap();
        let uid = unsafe { libc::getuid() };
        assert!(validate_runtime_directory_metadata(&metadata, uid).is_ok());
        assert!(validate_runtime_directory_metadata(&metadata, uid.saturating_add(1)).is_err());

        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let metadata = std::fs::symlink_metadata(directory.path()).unwrap();
        assert!(validate_runtime_directory_metadata(&metadata, uid).is_err());
    }

    #[test]
    fn runtime_directory_rejects_symlink_metadata() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let link = parent.path().join("runtime-link");
        symlink(&target, &link).unwrap();
        let metadata = std::fs::symlink_metadata(&link).unwrap();
        assert!(validate_runtime_directory_metadata(&metadata, unsafe { libc::getuid() }).is_err());
    }

    #[test]
    fn rejects_zero_pid_before_calling_logind() {
        assert_eq!(
            resolve_session_for_pid(0).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
