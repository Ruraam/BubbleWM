//! Private supervisor/worker control-channel protocol.
//!
//! The protocol carries session metadata only. It has no password, credential,
//! arbitrary command, or systemd unit-name field.

use std::io::{self, Read, Write, Cursor};
use std::path::{Component, Path};

const MAGIC: &[u8; 8] = b"BUBSESS1";
const HEADER_LEN: usize = 12;
const MAX_PAYLOAD: usize = 4_200;
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
    if !(10..=MAX_PAYLOAD).contains(&payload_len) {
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
        vt_number: if vt_number == 0 { None } else { Some(vt_number) },
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
        || runtime.components().any(|part| matches!(part, Component::ParentDir))
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
    let length = u16::try_from(value.len()).map_err(|_| invalid_input("metadata field too long"))?;
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
            write_session_ready(&mut Vec::new(), &invalid).unwrap_err().kind(),
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
            read_session_ready(&mut bad_magic.as_slice()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        let mut oversized = Vec::from(*MAGIC);
        oversized.extend_from_slice(&((MAX_PAYLOAD as u32) + 1).to_be_bytes());
        assert_eq!(
            read_session_ready(&mut oversized.as_slice()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn reader_rejects_truncated_frames() {
        let mut frame = Vec::new();
        write_session_ready(&mut frame, &sample()).unwrap();
        frame.pop();
        assert_eq!(
            read_session_ready(&mut frame.as_slice()).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}
