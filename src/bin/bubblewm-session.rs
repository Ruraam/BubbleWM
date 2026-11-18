//! Root-only supervisor for one BubbleWM Wayland login session.
//!
//! The supervisor never reads the password. It inherits the greeter's private
//! stdin pipe into the dedicated worker and independently verifies the
//! worker's post-PAM session metadata against logind.

use std::{
    ffi::{OsStr, OsString},
    process,
    time::Duration,
};

use bubblewm::{
    logind::resolve_session_for_pid,
    session_control::{SessionWorker, validate_password_pipe_fd},
};

const WORKER: &str = "/usr/libexec/bubblewm-session-worker";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const METADATA_TIMEOUT: Duration = Duration::from_secs(10);
const SCOPE_STOP_TIMEOUT: Duration = Duration::from_secs(15);

fn main() {
    match run() {
        Ok(code) => process::exit(code),
        Err(error) => {
            eprintln!("bubblewm-session: {error}");
            process::exit(1);
        }
    }
}

fn run() -> Result<i32, String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("ce helper doit être lancé par le superviseur système".into());
    }

    let arguments: Vec<OsString> = std::env::args_os().collect();
    if arguments.len() != 4 || arguments[1] != OsStr::new("run") {
        return Err(
            "usage: bubblewm-session run UTILISATEUR /dev/ttyN (mot de passe sur stdin)".into(),
        );
    }
    let user = arguments[2]
        .to_str()
        .ok_or_else(|| "identifiant utilisateur invalide".to_owned())?;
    let tty = arguments[3]
        .to_str()
        .ok_or_else(|| "terminal invalide".to_owned())?;
    validate_vt(tty)?;

    validate_password_pipe_fd(libc::STDIN_FILENO).map_err(|error| {
        format!(
            "le mot de passe doit être transmis sur un pipe stdin, jamais saisi dans le helper: {error}"
        )
    })?;

    // posix_spawn preserves fd 0 while mapping only the private control
    // socket to fd 3. The worker consumes the secret directly from stdin.
    let mut worker = SessionWorker::start(
        OsStr::new(WORKER),
        &[OsString::from(user), OsString::from(tty)],
        STARTUP_TIMEOUT,
    )
    .map_err(|error| format!("impossible de démarrer le worker PAM: {error}"))?;

    // Keep the privileged orchestration in a separately tested state machine.
    // The worker's PID and metadata are independently checked against logind.
    bubblewm::session_lifecycle::supervise_session(
        &mut worker,
        METADATA_TIMEOUT,
        SCOPE_STOP_TIMEOUT,
        resolve_session_for_pid,
        bubblewm::logind::stop_session_scope,
    )
}

fn validate_vt(tty: &str) -> Result<(), String> {
    let Some(number) = tty.strip_prefix("/dev/tty") else {
        return Err("terminal invalide: un VT /dev/ttyN est requis".into());
    };
    if number.is_empty()
        || !number.bytes().all(|byte| byte.is_ascii_digit())
        || number
            .parse::<u32>()
            .map_or(true, |value| !(1..=63).contains(&value))
    {
        return Err("terminal invalide: un VT /dev/ttyN entre 1 et 63 est requis".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_virtual_terminal_paths_are_accepted() {
        assert!(validate_vt("/dev/tty1").is_ok());
        assert!(validate_vt("/dev/tty63").is_ok());
        for invalid in [
            "/dev/tty0",
            "/dev/tty64",
            "/dev/pts/2",
            "/dev/tty1/../null",
            "/dev/tty",
        ] {
            assert!(validate_vt(invalid).is_err(), "{invalid}");
        }
    }
}
