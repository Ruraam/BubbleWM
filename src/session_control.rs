//! Private supervisor/worker control-channel protocol.
//!
//! The protocol carries session metadata only. It has no password, credential,
//! arbitrary command, or systemd unit-name field.

use std::io::{self, Cursor, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Component, Path};

const MAGIC: &[u8; 8] = b"BUBSESS1";
const HEADER_LEN: usize = 12;
const MAX_PAYLOAD: usize = 4_240;
const MAX_SESSION_ID: usize = 64;
const MAX_RUNTIME_DIR: usize = 4_096;
const MAX_SEAT: usize = 64;

/// Non-secret metadata reported by a worker after PAM has opened the session.
///
/// The supervisor must independently resolve `session_id` through logind and
/// validate the returned session, UID, scope and runtime directory. None of
/// these fields is authority to stop a systemd unit by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionReady {
    pub uid: u32,
    pub session_id: String,
    pub runtime_dir: String,
    pub seat: Option<String>,
    pub vt_number: Option<u32>,
}

/// Creates a private, connected supervisor/worker channel.
///
/// Both ends are local Unix stream sockets with `FD_CLOEXEC` enforced. The
/// supervisor keeps one endpoint and passes the other to a dedicated worker
/// using an explicit file-descriptor inheritance step. The descriptor is not
/// advertised through argv, environment variables or a filesystem pathname.
pub fn private_channel_pair() -> io::Result<(UnixStream, UnixStream)> {
    let (supervisor, worker) = UnixStream::pair()?;
    set_close_on_exec(&supervisor)?;
    set_close_on_exec(&worker)?;
    Ok((supervisor, worker))
}

fn set_close_on_exec(stream: &UnixStream) -> io::Result<()> {
    let fd = stream.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Spawns a trusted absolute-path worker and maps its private channel to fd 3.
///
/// The worker endpoint is consumed by this function. The child receives only
/// that endpoint at fd 3; the original descriptor is duplicated with
/// close-on-exec semantics and closed in the child after the dup2 action.
/// The child starts with an empty environment. The caller must pass only
/// fixed, non-secret arguments and retain the supervisor endpoint separately.
///
/// This uses `posix_spawn` file actions rather than a post-fork Rust callback,
/// which avoids running Rust code in a potentially multithreaded child.
pub fn spawn_worker_with_channel(
    program: &std::ffi::OsStr,
    arguments: &[std::ffi::OsString],
    worker_channel: UnixStream,
) -> io::Result<libc::pid_t> {
    use std::os::unix::ffi::OsStrExt;

    if !Path::new(program).is_absolute() {
        return Err(invalid_input("worker path must be absolute"));
    }
    let program = std::ffi::CString::new(program.as_bytes())
        .map_err(|_| invalid_input("worker path contains a NUL byte"))?;

    let mut argv = Vec::with_capacity(arguments.len() + 1);
    argv.push(program.clone());
    for argument in arguments {
        argv.push(
            std::ffi::CString::new(argument.as_bytes())
                .map_err(|_| invalid_input("worker argument contains a NUL byte"))?,
        );
    }
    let mut argv_ptrs: Vec<*mut libc::c_char> =
        argv.iter().map(|value| value.as_ptr() as *mut libc::c_char).collect();
    argv_ptrs.push(std::ptr::null_mut());
    let mut envp = [std::ptr::null_mut()];

    let duplicate = unsafe { libc::fcntl(worker_channel.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 4) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    let duplicate = OwnedFdGuard(duplicate);

    let mut actions = std::mem::MaybeUninit::<libc::posix_spawn_file_actions_t>::uninit();
    let status = unsafe { libc::posix_spawn_file_actions_init(actions.as_mut_ptr()) };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status));
    }
    let mut actions = SpawnFileActions(unsafe { actions.assume_init() });

    let status = unsafe {
        libc::posix_spawn_file_actions_adddup2(&mut actions.0, duplicate.0, 3)
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status));
    }
    let status = unsafe { libc::posix_spawn_file_actions_addclose(&mut actions.0, duplicate.0) };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status));
    }

    let mut pid = 0;
    let status = unsafe {
        libc::posix_spawn(
            &mut pid,
            program.as_ptr(),
            actions.as_ptr(),
            std::ptr::null(),
            argv_ptrs.as_mut_ptr(),
            envp.as_mut_ptr(),
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status));
    }
    Ok(pid)
}

struct OwnedFdGuard(libc::c_int);

impl Drop for OwnedFdGuard {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

struct SpawnFileActions(libc::posix_spawn_file_actions_t);

impl SpawnFileActions {
    fn as_ptr(&self) -> *const libc::posix_spawn_file_actions_t {
        &self.0
    }
}

impl Drop for SpawnFileActions {
    fn drop(&mut self) {
        unsafe { libc::posix_spawn_file_actions_destroy(&mut self.0) };
    }
}

/// Writes one versioned, bounded SessionReady frame to a private stream.
pub fn write_session_ready<W: Write>(writer: &mut W, ready: &SessionReady) -> io::Result<()> {
    validate(ready)?;
    let mut payload = Vec::with_capacity(128);
    payload.extend_from_slice(&ready.uid.to_be_bytes());
    put_string(&mut payload, &ready.session_id)?;
    put_string(&mut payload, &ready.runtime_dir)?;
    put_string(&mut payload, ready.seat.as_deref().unwrap_or(""))?;
    payload.extend_from_slice(&ready.vt_number.unwrap_or(0).to_be_bytes());

    if payload.len() > MAX_PAYLOAD {
        return Err(invalid_data("session metadata frame is too large"));
    }

    writer.write_all(MAGIC)?;
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()
}

/// Reads exactly one versioned SessionReady frame from a private stream.
///
/// The caller should treat EOF before a complete frame as worker failure and
/// must not accept a second frame as a replacement for the first.
pub fn read_session_ready<R: Read>(reader: &mut R) -> io::Result<SessionReady> {
    let mut header = [0_u8; HEADER_LEN];
    reader.read_exact(&mut header)?;
    if &header[..8] != MAGIC {
        return Err(invalid_data("invalid session control protocol magic"));
    }

    let payload_len = u32::from_be_bytes(header[8..12].try_into().expect("fixed header")) as usize;
    if !(14..=MAX_PAYLOAD).contains(&payload_len) {
        return Err(invalid_data("invalid session control frame length"));
    }

    let mut payload = vec![0_u8; payload_len];
    reader.read_exact(&mut payload)?;
    let mut cursor = Cursor::new(payload.as_slice());

    let uid = read_u32(&mut cursor)?;
    let session_id = read_string(&mut cursor, MAX_SESSION_ID)?;
    let runtime_dir = read_string(&mut cursor, MAX_RUNTIME_DIR)?;
    let seat = read_string(&mut cursor, MAX_SEAT)?;
    let vt_number = read_u32(&mut cursor)?;

    if cursor.position() != payload.len() as u64 {
        return Err(invalid_data("trailing data in session control frame"));
    }

    let ready = SessionReady {
        uid,
        session_id,
        runtime_dir,
        seat: if seat.is_empty() { None } else { Some(seat) },
        vt_number: if vt_number == 0 {
            None
        } else {
            Some(vt_number)
        },
    };
    validate(&ready)?;
    Ok(ready)
}

fn validate(ready: &SessionReady) -> io::Result<()> {
    if ready.uid == 0 {
        return Err(invalid_input("root sessions are not supported"));
    }
    if ready.session_id.is_empty()
        || ready.session_id.len() > MAX_SESSION_ID
        || !ready
            .session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(invalid_input("invalid logind session identifier"));
    }

    let runtime = Path::new(&ready.runtime_dir);
    if ready.runtime_dir.len() > MAX_RUNTIME_DIR
        || !runtime.is_absolute()
        || runtime
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        || ready.runtime_dir.chars().any(char::is_control)
    {
        return Err(invalid_input("invalid XDG_RUNTIME_DIR metadata"));
    }

    if let Some(seat) = &ready.seat
        && (seat.is_empty()
            || seat.len() > MAX_SEAT
            || !seat
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
    {
        return Err(invalid_input("invalid logind seat metadata"));
    }

    if let Some(vt) = ready.vt_number
        && !(1..=63).contains(&vt)
    {
        return Err(invalid_input("invalid virtual terminal metadata"));
    }
    Ok(())
}

fn put_string(output: &mut Vec<u8>, value: &str) -> io::Result<()> {
    let length =
        u16::try_from(value.len()).map_err(|_| invalid_input("metadata field too long"))?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

fn read_string<R: Read>(reader: &mut R, maximum: usize) -> io::Result<String> {
    let mut length = [0_u8; 2];
    reader.read_exact(&mut length)?;
    let length = u16::from_be_bytes(length) as usize;
    if length > maximum {
        return Err(invalid_data("session metadata field exceeds its limit"));
    }
    let mut bytes = vec![0_u8; length];
    reader.read_exact(&mut bytes)?;
    String::from_utf8(bytes).map_err(|_| invalid_data("session metadata is not UTF-8"))
}

fn read_u32<R: Read>(reader: &mut R) -> io::Result<u32> {
    let mut bytes = [0_u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
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

    fn sample() -> SessionReady {
        SessionReady {
            uid: 1000,
            session_id: "c7".into(),
            runtime_dir: "/run/user/1000".into(),
            seat: Some("seat0".into()),
            vt_number: Some(2),
        }
    }

    #[test]
    fn private_channel_is_connected_and_close_on_exec() {
        let (mut supervisor, mut worker) = private_channel_pair().unwrap();
        worker.write_all(b"ok").unwrap();
        let mut bytes = [0_u8; 2];
        supervisor.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"ok");

        for stream in [&supervisor, &worker] {
            let flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFD) };
            assert!(flags >= 0);
            assert_ne!(flags & libc::FD_CLOEXEC, 0);
        }
    }

    #[test]
    fn worker_spawn_receives_channel_on_fd_three() {
        let (mut supervisor, worker) = private_channel_pair().unwrap();
        let pid = spawn_worker_with_channel(
            std::ffi::OsStr::new("/bin/sh"),
            &[std::ffi::OsString::from("-c"), std::ffi::OsString::from("printf ok >&3")],
            worker,
        )
        .unwrap();

        let mut bytes = [0_u8; 2];
        supervisor.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"ok");

        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 0);
    }

    #[test]
    fn session_ready_round_trips_over_a_stream() {
        let expected = sample();
        let mut frame = Vec::new();
        write_session_ready(&mut frame, &expected).unwrap();
        let actual = read_session_ready(&mut frame.as_slice()).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn optional_seat_and_vt_are_supported() {
        let mut expected = sample();
        expected.seat = None;
        expected.vt_number = None;
        let mut frame = Vec::new();
        write_session_ready(&mut frame, &expected).unwrap();
        assert_eq!(read_session_ready(&mut frame.as_slice()).unwrap(), expected);
    }

    #[test]
    fn writer_rejects_invalid_metadata() {
        let mut invalid = sample();
        invalid.uid = 0;
        assert_eq!(
            write_session_ready(&mut Vec::new(), &invalid)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );

        let mut invalid = sample();
        invalid.runtime_dir = "/run/user/1000/../../etc".into();
        assert!(write_session_ready(&mut Vec::new(), &invalid).is_err());

        let mut invalid = sample();
        invalid.vt_number = Some(64);
        assert!(write_session_ready(&mut Vec::new(), &invalid).is_err());
    }

    #[test]
    fn reader_rejects_bad_magic_and_oversized_frame() {
        let mut bad_magic = [0_u8; HEADER_LEN];
        assert_eq!(
            read_session_ready(&mut bad_magic.as_slice())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );

        let mut oversized = Vec::from(*MAGIC);
        oversized.extend_from_slice(&((MAX_PAYLOAD as u32) + 1).to_be_bytes());
        assert_eq!(
            read_session_ready(&mut oversized.as_slice())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn reader_rejects_truncated_frames() {
        let mut frame = Vec::new();
        write_session_ready(&mut frame, &sample()).unwrap();
        frame.pop();
        assert_eq!(
            read_session_ready(&mut frame.as_slice())
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}
