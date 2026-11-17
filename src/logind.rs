//! Trusted login-session identity lookup through systemd-logind.
//!
//! This module uses libsystemd's sd-login API directly. It deliberately avoids
//! D-Bus clients that may create background executor threads: PAM session
//! ownership requires the privileged supervisor to remain single-threaded.

use std::{
    ffi::{CStr, CString, c_char},
    io,
    os::unix::fs::MetadataExt,
    path::PathBuf,
};

use crate::session_control::SessionIdentity;

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
    if !metadata.file_type().is_dir() || metadata.uid() != uid {
        return Err(invalid_data(
            "logind runtime directory is not a real directory owned by the session UID",
        ));
    }

    // Re-read the PID-to-session association after collecting the properties.
    // This detects a process exit/reuse or session reassignment during lookup.
    let confirmed_session = sd_string_from_pid(pid, sd_pid_get_session)?;
    if confirmed_session != session_id {
        return Err(invalid_data(
            "process session changed during logind identity lookup",
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

type PidStringLookup = unsafe extern "C" fn(libc::pid_t, *mut *mut c_char) -> libc::c_int;

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
    fn rejects_zero_pid_before_calling_logind() {
        assert_eq!(
            resolve_session_for_pid(0).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
