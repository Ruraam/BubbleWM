//! Root-only entry point for one BubbleWM Wayland login session.
//!
//! The greeter/supervisor must start this helper as a dedicated, single-threaded
//! root process and write exactly one password to its stdin. The password is
//! never accepted in argv or environment variables.

use std::{
    ffi::{OsStr, OsString},
    io::{self, Read},
    process,
    time::Duration,
};

const COMPOSITOR: &str = "/usr/bin/bubblewm";
const SESSION_CONFIG: &str = "/etc/bubblewm/session.toml";
const MAX_PASSWORD_BYTES: usize = 1024;
const TERMINATION_GRACE: Duration = Duration::from_secs(5);

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
    if arguments.len() != 4
        || arguments[1] != OsStr::new("run")
    {
        return Err("usage: bubblewm-session run UTILISATEUR /dev/ttyN (mot de passe sur stdin)".into());
    }

    let user = arguments[2]
        .to_str()
        .ok_or_else(|| "identifiant utilisateur invalide".to_owned())?;
    let tty = arguments[3]
        .to_str()
        .ok_or_else(|| "terminal invalide".to_owned())?;
    validate_vt(tty)?;

    if unsafe { libc::isatty(libc::STDIN_FILENO) } != 0 {
        return Err("le mot de passe doit être transmis sur un pipe stdin, jamais saisi dans le helper".into());
    }

    let mut password = read_password(io::stdin().lock())?;
    let result = bubblewm::auth::run_login_session(
        user,
        &password,
        tty,
        OsStr::new(COMPOSITOR),
        &[OsString::from(SESSION_CONFIG)],
        TERMINATION_GRACE,
    );
    wipe_string(&mut password);

    result
        .map(|status| status.code().unwrap_or(1))
        .map_err(|error| format!("échec de la session: {error}"))
}

fn validate_vt(tty: &str) -> Result<(), String> {
    let Some(number) = tty.strip_prefix("/dev/tty") else {
        return Err("terminal invalide: un VT /dev/ttyN est requis".into());
    };
    if number.is_empty()
        || !number.bytes().all(|byte| byte.is_ascii_digit())
        || number.parse::<u32>().map_or(true, |value| !(1..=63).contains(&value))
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

    #[test]
    fn password_reader_accepts_lf_and_crlf_framing() {
        assert_eq!(read_password(Cursor::new(b"secret\n")).unwrap(), "secret");
        assert_eq!(read_password(Cursor::new(b"secret\r\n")).unwrap(), "secret");
        assert_eq!(read_password(Cursor::new(b"secret")).unwrap(), "secret");
    }

    #[test]
    fn password_reader_rejects_empty_and_multiline_input() {
        assert!(read_password(Cursor::new(b"\n")).is_err());
        assert!(read_password(Cursor::new(b"first\nsecond\n")).is_err());
        assert!(read_password(Cursor::new(b"secret\0")).is_err());
    }

    #[test]
    fn password_reader_enforces_the_pam_input_limit() {
        assert!(read_password(Cursor::new(vec![b'x'; MAX_PASSWORD_BYTES])).is_ok());
        assert!(read_password(Cursor::new(vec![b'x'; MAX_PASSWORD_BYTES + 1])).is_err());
    }

    #[test]
    fn password_reader_rejects_invalid_utf8() {
        assert!(read_password(Cursor::new([0xff, b'\n'])).is_err());
    }

    #[test]
    fn only_virtual_terminal_paths_are_accepted() {
        assert!(validate_vt("/dev/tty1").is_ok());
        assert!(validate_vt("/dev/tty63").is_ok());
        for invalid in ["/dev/tty0", "/dev/tty64", "/dev/pts/2", "/dev/tty1/../null", "/dev/tty"] {
            assert!(validate_vt(invalid).is_err(), "{invalid}");
        }
    }
}
