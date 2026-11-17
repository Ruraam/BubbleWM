//! Dedicated, single-threaded BubbleWM PAM session worker.
//!
//! The worker receives the password only on inherited stdin. Descriptor 3 is
//! reserved for the private, non-secret supervisor control protocol.

use std::io::{self, Read, Write};
use std::ffi::{OsStr, OsString};
use std::time::Duration;

use bubblewm::auth::PamLoginSession;
use bubblewm::session_control::{take_worker_channel, write_session_ready};

const WORKER_HELLO: &[u8; 8] = b"BUBWORK1";
const MAX_PASSWORD_BYTES: usize = 1024;
const COMPOSITOR: &str = "/usr/bin/bubblewm";
const SESSION_CONFIG: &str = "/etc/bubblewm/session.toml";
const TERMINATION_GRACE: Duration = Duration::from_secs(5);

fn main() {
    if let Err(error) = run() {
        eprintln!("bubblewm-session-worker: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    // Own fd 3 before parsing arguments, reading the secret, or logging.
    let mut channel = take_worker_channel().map_err(|error| error.to_string())?;
    announce_worker(&mut channel).map_err(|error| error.to_string())?;

    let arguments: Vec<OsString> = std::env::args_os().collect();
    if arguments.len() != 3 {
        return Err("usage: bubblewm-session-worker UTILISATEUR /dev/ttyN".into());
    }
    let user = arguments[1]
        .to_str()
        .ok_or_else(|| "identifiant utilisateur invalide".to_owned())?;
    let tty = arguments[2]
        .to_str()
        .ok_or_else(|| "terminal invalide".to_owned())?;
    validate_vt(tty)?;

    if unsafe { libc::geteuid() } != 0 {
        return Err("le worker PAM doit être lancé avec les privilèges système".into());
    }
    if unsafe { libc::isatty(libc::STDIN_FILENO) } != 0 {
        return Err("le secret doit être transmis par un pipe stdin".into());
    }

    let mut password = read_password(io::stdin().lock())?;
    // PAM is now the only consumer of the secret; don't let the compositor
    // inherit the password pipe descriptor.
    unsafe { libc::close(libc::STDIN_FILENO) };
    let session = PamLoginSession::open(user, &password, tty);
    wipe_string(&mut password);
    let session = session?;

    let ready = session.session_ready()?;
    write_session_ready(&mut channel, &ready).map_err(|error| {
        format!("impossible d'annoncer les métadonnées de session: {error}")
    })?;

    let mut process = session.spawn_user_command(
        OsStr::new(COMPOSITOR),
        &[OsString::from(SESSION_CONFIG)],
    )?;
    let status = process
        .wait()
        .map_err(|error| format!("impossible d'attendre la session utilisateur: {error}"))?;
    let process_cleanup = process
        .terminate(TERMINATION_GRACE)
        .map_err(|error| format!("impossible de nettoyer le groupe utilisateur: {error}"));
    drop(process);
    let pam_cleanup = session.close();
    process_cleanup?;
    pam_cleanup?;
    Ok(())
}

fn announce_worker<W: Write>(channel: &mut W) -> io::Result<()> {
    channel.write_all(WORKER_HELLO)?;
    channel.flush()
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

fn read_password<R: Read>(reader: R) -> Result<String, String> {
    let mut bytes = Vec::new();
    if reader
        .take((MAX_PASSWORD_BYTES + 3) as u64)
        .read_to_end(&mut bytes)
        .is_err()
    {
        wipe_bytes(&mut bytes);
        return Err("impossible de lire le secret depuis stdin".into());
    }
    if bytes.len() > MAX_PASSWORD_BYTES + 2 {
        wipe_bytes(&mut bytes);
        return Err("mot de passe trop long".into());
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.is_empty()
        || bytes.len() > MAX_PASSWORD_BYTES
        || bytes.iter().any(|byte| matches!(byte, b'\n' | b'\r' | 0))
    {
        wipe_bytes(&mut bytes);
        return Err("format de mot de passe invalide".into());
    }
    match String::from_utf8(bytes) {
        Ok(password) => Ok(password),
        Err(error) => {
            let mut bytes = error.into_bytes();
            wipe_bytes(&mut bytes);
            Err("le mot de passe doit être encodé en UTF-8".into())
        }
    }
}

fn wipe_bytes(bytes: &mut [u8]) {
    for byte in bytes {
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

fn wipe_string(value: &mut String) {
    wipe_bytes(unsafe { value.as_mut_vec() });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::os::unix::net::UnixStream;

    #[test]
    fn worker_announces_versioned_bootstrap_identity() {
        let (mut supervisor, mut worker) = UnixStream::pair().unwrap();
        announce_worker(&mut worker).unwrap();
        let mut hello = [0_u8; WORKER_HELLO.len()];
        supervisor.read_exact(&mut hello).unwrap();
        assert_eq!(&hello, WORKER_HELLO);
    }

    #[test]
    fn password_reader_accepts_lf_and_crlf_framing() {
        assert_eq!(read_password(Cursor::new(b"secret\n")).unwrap(), "secret");
        assert_eq!(read_password(Cursor::new(b"secret\r\n")).unwrap(), "secret");
        assert_eq!(read_password(Cursor::new(b"secret")).unwrap(), "secret");
    }

    #[test]
    fn password_reader_rejects_empty_multiline_and_oversized_input() {
        assert!(read_password(Cursor::new(b"\n")).is_err());
        assert!(read_password(Cursor::new(b"first\nsecond\n")).is_err());
        assert!(read_password(Cursor::new(vec![b'x'; MAX_PASSWORD_BYTES + 1])).is_err());
    }

    #[test]
    fn only_virtual_terminal_paths_are_accepted() {
        assert!(validate_vt("/dev/tty1").is_ok());
        assert!(validate_vt("/dev/tty63").is_ok());
        assert!(validate_vt("/dev/tty0").is_err());
        assert!(validate_vt("/dev/pts/2").is_err());
    }
}
