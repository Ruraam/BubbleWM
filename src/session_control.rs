//! Private supervisor/worker control-channel protocol.
//!
//! The protocol carries session metadata only. It has no password, credential,
//! arbitrary command, or systemd unit-name field.

use std::io::{self, Cursor, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Component, Path};
use std::time::{Duration, Instant};

const MAGIC: &[u8; 8] = b"BUBSESS1";
const HEADER_LEN: usize = 12;
const MAX_PAYLOAD: usize = 4_240;
const MAX_SESSION_ID: usize = 64;
const MAX_RUNTIME_DIR: usize = 4_096;
const MAX_SEAT: usize = 64;
const WORKER_HELLO: &[u8; 8] = b"BUBWORK1";
const SUPERVISOR_APPROVAL: &[u8; 8] = b"BUBOKAY1";

/// Waits for the fixed bootstrap marker sent by a freshly started worker.
///
/// This is intentionally a narrow, exact handshake. No session metadata or
/// credentials are accepted until a later reviewed protocol stage.
pub fn read_worker_hello<R: Read>(reader: &mut R) -> io::Result<()> {
    let mut hello = [0_u8; WORKER_HELLO.len()];
    reader.read_exact(&mut hello)?;
    if hello != *WORKER_HELLO {
        return Err(invalid_data("invalid BubbleWM session worker handshake"));
    }
    Ok(())
}

/// Waits for the supervisor's explicit approval before the worker starts the
/// compositor. EOF or any other marker means the session must be aborted.
pub fn read_supervisor_approval<R: Read>(reader: &mut R) -> io::Result<()> {
    let mut approval = [0_u8; SUPERVISOR_APPROVAL.len()];
    reader.read_exact(&mut approval)?;
    if approval != *SUPERVISOR_APPROVAL {
        return Err(invalid_data("invalid supervisor session approval"));
    }
    Ok(())
}
