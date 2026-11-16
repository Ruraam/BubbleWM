//! Versioned, bounded private protocol for supervised session workers.
//!
//! The protocol carries non-secret session metadata only. Worker-reported
//! values remain untrusted until independently validated against logind.
//! All socket operations use one absolute deadline per message.

use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    path::{Component, Path},
    time::{Duration, Instant},
};

const MAGIC: &[u8; 8] = b"BUBSESS1";
const WORKER_HELLO: &[u8; 8] = b"BUBWORK1";
const HEADER_LEN: usize = 12;
const MIN_PAYLOAD: usize = 14;
const MAX_PAYLOAD: usize = 4_240;
const MAX_SESSION_ID: usize = 64;
const MAX_RUNTIME_DIR: usize = 4_096;
const MAX_SEAT: usize = 64;

/// Non-secret metadata reported by a worker after opening its login session.
///
/// This structure is not proof of identity. The supervisor must resolve the
/// session through logind and compare every relevant field before Ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionReady {
    pub(crate) uid: u32,
    pub(crate) session_id: String,
    pub(crate) runtime_dir: String,
    pub(crate) seat: Option<String>,
    pub(crate) vt_number: Option<u32>,
}

/// Writes the exact worker bootstrap marker before any metadata is sent.
pub(crate) fn write_worker_hello(stream: &mut UnixStream, timeout: Duration) -> io::Result<()> {
    let deadline = deadline_after(timeout)?;
    prepare_nonblocking(stream)?;
    write_all_until(stream, WORKER_HELLO, deadline)
}

/// Reads and verifies the exact worker bootstrap marker.
pub(crate) fn read_worker_hello(stream: &mut UnixStream, timeout: Duration) -> io::Result<()> {
    let deadline = deadline_after(timeout)?;
    prepare_nonblocking(stream)?;
    let mut hello = [0_u8; WORKER_HELLO.len()];
    read_exact_until(stream, &mut hello, deadline)?;
    if &hello != WORKER_HELLO {
        return Err(invalid_data("invalid BubbleWM session worker handshake"));
    }
    Ok(())
}

/// Writes one bounded, versioned SessionReady frame.
pub(crate) fn write_session_ready(
    stream: &mut UnixStream,
    ready: &SessionReady,
    timeout: Duration,
) -> io::Result<()> {
    validate(ready)?;
    let deadline = deadline_after(timeout)?;
    prepare_nonblocking(stream)?;

    let mut payload = Vec::with_capacity(128);
    payload.extend_from_slice(&ready.uid.to_be_bytes());
    put_string(&mut payload, &ready.session_id)?;
    put_string(&mut payload, &ready.runtime_dir)?;
    put_string(&mut payload, ready.seat.as_deref().unwrap_or(""))?;
    payload.extend_from_slice(&ready.vt_number.unwrap_or(0).to_be_bytes());
    if !(MIN_PAYLOAD..=MAX_PAYLOAD).contains(&payload.len()) {
        return Err(invalid_input("session metadata frame is too large"));
    }

    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
    frame.extend_from_slice(MAGIC);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    write_all_until(stream, &frame, deadline)
}

/// Reads exactly one bounded SessionReady frame.
///
/// A timeout, EOF, malformed header, or malformed payload is a worker
/// handshake failure. The caller must not retry with a replacement frame.
pub(crate) fn read_session_ready(
    stream: &mut UnixStream,
    timeout: Duration,
) -> io::Result<SessionReady> {
    let deadline = deadline_after(timeout)?;
    prepare_nonblocking(stream)?;

    let mut header = [0_u8; HEADER_LEN];
    read_exact_until(stream, &mut header, deadline)?;
    if &header[..8] != MAGIC {
        return Err(invalid_data("invalid session control protocol magic"));
    }

    let payload_len = u32::from_be_bytes(header[8..12].try_into().expect("fixed header")) as usize;
    if !(MIN_PAYLOAD..=MAX_PAYLOAD).contains(&payload_len) {
        return Err(invalid_data("invalid session control frame length"));
    }

    let mut payload = vec![0_u8; payload_len];
    read_exact_until(stream, &mut payload, deadline)?;
    decode_payload(&payload)
}

fn decode_payload(payload: &[u8]) -> io::Result<SessionReady> {
    let mut offset = 0;
    let uid = take_u32(payload, &mut offset)?;
    let session_id = take_string(payload, &mut offset, MAX_SESSION_ID)?;
    let runtime_dir = take_string(payload, &mut offset, MAX_RUNTIME_DIR)?;
    let seat = take_string(payload, &mut offset, MAX_SEAT)?;
    let vt_number = take_u32(payload, &mut offset)?;
    if offset != payload.len() {
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
    validate(&ready).map_err(|_| invalid_data("invalid session metadata"))?;
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

fn take_string(input: &[u8], offset: &mut usize, maximum: usize) -> io::Result<String> {
    let length_bytes = take_bytes::<2>(input, offset)?;
    let length = u16::from_be_bytes(length_bytes) as usize;
    if length > maximum {
        return Err(invalid_data("session metadata field exceeds its limit"));
    }
    let bytes = take_slice(input, offset, length)?;
    String::from_utf8(bytes.to_vec()).map_err(|_| invalid_data("session metadata is not UTF-8"))
}

fn take_u32(input: &[u8], offset: &mut usize) -> io::Result<u32> {
    Ok(u32::from_be_bytes(take_bytes::<4>(input, offset)?))
}

fn take_bytes<const N: usize>(input: &[u8], offset: &mut usize) -> io::Result<[u8; N]> {
    let bytes = take_slice(input, offset, N)?;
    bytes
        .try_into()
        .map_err(|_| invalid_data("truncated session metadata"))
}

fn take_slice<'a>(input: &'a [u8], offset: &mut usize, length: usize) -> io::Result<&'a [u8]> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| invalid_data("session metadata length overflow"))?;
    let value = input
        .get(*offset..end)
        .ok_or_else(|| invalid_data("truncated session metadata"))?;
    *offset = end;
    Ok(value)
}

fn prepare_nonblocking(stream: &UnixStream) -> io::Result<()> {
    stream.set_nonblocking(true)
}

fn deadline_after(timeout: Duration) -> io::Result<Instant> {
    Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| invalid_input("protocol timeout is too large"))
}

fn wait_ready(stream: &UnixStream, events: i16, deadline: Instant) -> io::Result<()> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "session worker protocol deadline expired",
            ));
        }
        let remaining = deadline - now;
        let millis = remaining
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: stream.as_raw_fd(),
            events,
            revents: 0,
        };
        // SAFETY: descriptor points to one initialized pollfd for this call.
        let result = unsafe { libc::poll(&mut descriptor, 1, millis) };
        if result > 0 {
            if descriptor.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid session control descriptor",
                ));
            }
            if descriptor.revents & (events | libc::POLLERR | libc::POLLHUP) != 0 {
                return Ok(());
            }
            continue;
        }
        if result == 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn read_exact_until(
    stream: &mut UnixStream,
    mut output: &mut [u8],
    deadline: Instant,
) -> io::Result<()> {
    while !output.is_empty() {
        wait_ready(stream, libc::POLLIN, deadline)?;
        match stream.read(output) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(read) => output = &mut output[read..],
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn write_all_until(stream: &mut UnixStream, mut input: &[u8], deadline: Instant) -> io::Result<()> {
    while !input.is_empty() {
        wait_ready(stream, libc::POLLOUT, deadline)?;
        match stream.write(input) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => input = &input[written..],
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

use std::os::fd::AsRawFd;

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::net::UnixStream, thread};

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
    fn worker_hello_round_trips() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        write_worker_hello(&mut sender, Duration::from_secs(1)).unwrap();
        read_worker_hello(&mut receiver, Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn worker_hello_rejects_wrong_marker() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        sender.write_all(b"BUBWORKX").unwrap();
        assert_eq!(
            read_worker_hello(&mut receiver, Duration::from_secs(1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn session_ready_round_trips() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        let expected = sample();
        write_session_ready(&mut sender, &expected, Duration::from_secs(1)).unwrap();
        let actual = read_session_ready(&mut receiver, Duration::from_secs(1)).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn optional_seat_and_vt_are_supported() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        let mut expected = sample();
        expected.seat = None;
        expected.vt_number = None;
        write_session_ready(&mut sender, &expected, Duration::from_secs(1)).unwrap();
        assert_eq!(
            read_session_ready(&mut receiver, Duration::from_secs(1)).unwrap(),
            expected
        );
    }

    #[test]
    fn reader_rejects_bad_magic_and_oversized_frame() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        sender.write_all(&[0_u8; HEADER_LEN]).unwrap();
        assert_eq!(
            read_session_ready(&mut receiver, Duration::from_secs(1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );

        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        sender.write_all(MAGIC).unwrap();
        sender
            .write_all(&((MAX_PAYLOAD as u32) + 1).to_be_bytes())
            .unwrap();
        assert_eq!(
            read_session_ready(&mut receiver, Duration::from_secs(1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn reader_rejects_truncated_payload() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        let expected = sample();
        let mut frame = Vec::new();
        let (mut temporary_reader, mut temporary_writer) = UnixStream::pair().unwrap();
        write_session_ready(&mut temporary_writer, &expected, Duration::from_secs(1)).unwrap();
        temporary_reader
            .set_nonblocking(true)
            .expect("set nonblocking");
        let mut buffer = [0_u8; 512];
        let count = temporary_reader.read(&mut buffer).unwrap();
        frame.extend_from_slice(&buffer[..count - 1]);
        sender.write_all(&frame).unwrap();
        drop(sender);
        assert_eq!(
            read_session_ready(&mut receiver, Duration::from_secs(1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn reader_enforces_one_absolute_deadline_for_partial_frames() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        thread::spawn(move || {
            sender.write_all(&MAGIC[..4]).unwrap();
            thread::sleep(Duration::from_millis(80));
            sender.write_all(&MAGIC[4..]).unwrap();
        });
        let error = read_session_ready(&mut receiver, Duration::from_millis(20)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn writer_rejects_invalid_metadata() {
        let (mut sender, _receiver) = UnixStream::pair().unwrap();
        let mut invalid = sample();
        invalid.uid = 0;
        assert_eq!(
            write_session_ready(&mut sender, &invalid, Duration::from_secs(1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );

        let mut invalid = sample();
        invalid.runtime_dir = "/run/user/1000/../../etc".into();
        assert!(write_session_ready(&mut sender, &invalid, Duration::from_secs(1)).is_err());
    }
}
