//! Linux primitives and bounded protocol for the privileged session guardian.
//!
//! This module deliberately does not start a guardian or stop a logind scope
//! yet. It provides the reviewed, testable pidfd/SCM_RIGHTS boundary first.

use std::{
    io::{self, Read, Write},
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    os::unix::net::UnixStream,
    time::Duration,
};

const PIDFD_MARKER: &[u8; 8] = b"BUBPIDF1";
const ARM_MARKER: &[u8; 8] = b"BUBARM01";
const ARMED_MARKER: &[u8; 8] = b"BUBRDY01";
const COMPLETE_MARKER: &[u8; 8] = b"BUBDON01";
const MAX_SCOPE_LEN: usize = 128;
const MAX_UNIT_COMPONENT: usize = 64;

/// An owned Linux pidfd referring to one exact process instance.
pub struct PidFd(OwnedFd);

impl PidFd {
    /// Opens a pidfd for a positive PID. The returned descriptor is CLOEXEC.
    pub fn open(pid: libc::pid_t) -> io::Result<Self> {
        if pid <= 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "worker PID must be positive"));
        }
        #[cfg(target_os = "linux")]
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0_u32) as RawFd };
        #[cfg(not(target_os = "linux"))]
        let fd = -1;
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: pidfd_open returned a new owned descriptor.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        let flags = unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_GETFD) };
        if flags < 0 || unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(owned))
    }

    /// Waits for process exit without consuming or closing the pidfd.
    pub fn wait_exit(&self, timeout: Duration) -> io::Result<bool> {
        let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
        let mut pollfd = libc::pollfd { fd: self.0.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        loop {
            let result = unsafe { libc::poll(&mut pollfd, 1, millis) };
            if result > 0 {
                if pollfd.revents & (libc::POLLIN | libc::POLLHUP) != 0 { return Ok(true); }
                if pollfd.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                    return Err(io::Error::other("pidfd poll reported an invalid descriptor"));
                }
                return Ok(false);
            }
            if result == 0 { return Ok(false); }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted { return Err(error); }
        }
    }

    fn as_raw_fd(&self) -> RawFd { self.0.as_raw_fd() }
}

/// Transfers exactly one pidfd over a private Unix stream using SCM_RIGHTS.
pub fn send_pidfd(channel: &UnixStream, pidfd: &PidFd) -> io::Result<()> {
    let mut byte = *PIDFD_MARKER;
    let mut iov = libc::iovec { iov_base: byte.as_mut_ptr().cast(), iov_len: 1 };
    let mut control = [0_usize; 8];
    let space = unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as usize };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = space;
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        if header.is_null() { return Err(io::Error::other("cannot allocate SCM_RIGHTS header")); }
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as usize;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<RawFd>(), pidfd.as_raw_fd());
    }
    let sent = unsafe { libc::sendmsg(channel.as_raw_fd(), &message, libc::MSG_NOSIGNAL) };
    if sent < 0 { return Err(io::Error::last_os_error()); }
    if sent != PIDFD_MARKER.len() as isize { return Err(io::Error::new(io::ErrorKind::WriteZero, "short pidfd transfer")); }
    Ok(())
}

/// Receives one pidfd and rejects missing, truncated, or unexpected ancillary data.
pub fn receive_pidfd(channel: &UnixStream) -> io::Result<PidFd> {
    let mut byte = [0_u8; 8];
    let mut iov = libc::iovec { iov_base: byte.as_mut_ptr().cast(), iov_len: 1 };
    let mut control = [0_usize; 8];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control);
    let received = unsafe { libc::recvmsg(channel.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) };
    if received < 0 { return Err(io::Error::last_os_error()); }
    if received != PIDFD_MARKER.len() as isize || &byte != PIDFD_MARKER || message.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid pidfd transfer frame"));
    }
    let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
    if header.is_null()
        || unsafe { (*header).cmsg_level != libc::SOL_SOCKET || (*header).cmsg_type != libc::SCM_RIGHTS
            || (*header).cmsg_len != libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as usize }
        || !unsafe { libc::CMSG_NXTHDR(&message, header) }.is_null()
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "unexpected ancillary data"));
    }
    let fd = unsafe { std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<RawFd>()) };
    if fd < 0 { return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid received pidfd")); }
    // SAFETY: SCM_RIGHTS installed a new descriptor owned by this process.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    let flags = unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PidFd(owned))
}

/// Accepts only the canonical logind session scope unit-name shape.
pub fn validate_scope_name(scope: &str) -> io::Result<()> {
    let component = scope.strip_prefix("session-").and_then(|s| s.strip_suffix(".scope"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid logind scope name"))?;
    if component.is_empty() || component.len() > MAX_UNIT_COMPONENT
        || !component.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        || scope.len() > MAX_SCOPE_LEN
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid logind scope name"));
    }
    Ok(())
}

/// Sends a bounded arm request containing only a validated scope name.
pub fn send_arm(channel: &mut UnixStream, scope: &str) -> io::Result<()> {
    validate_scope_name(scope)?;
    channel.write_all(ARM_MARKER)?;
    channel.write_all(&(scope.len() as u16).to_be_bytes())?;
    channel.write_all(scope.as_bytes())
}

/// Reads and validates the bounded arm request.
pub fn receive_arm(channel: &mut UnixStream) -> io::Result<String> {
    let mut marker = [0_u8; 8];
    channel.read_exact(&mut marker)?;
    if &marker != ARM_MARKER { return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid guardian arm marker")); }
    let mut length = [0_u8; 2];
    channel.read_exact(&mut length)?;
    let length = u16::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_SCOPE_LEN { return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid guardian scope length")); }
    let mut bytes = vec![0_u8; length];
    channel.read_exact(&mut bytes)?;
    let scope = String::from_utf8(bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "scope is not UTF-8"))?;
    validate_scope_name(&scope)?;
    Ok(scope)
}

pub fn send_armed(channel: &mut UnixStream) -> io::Result<()> { channel.write_all(ARMED_MARKER) }
pub fn receive_armed(channel: &mut UnixStream) -> io::Result<()> {
    let mut marker = [0_u8; 8];
    channel.read_exact(&mut marker)?;
    if &marker == ARMED_MARKER { Ok(()) } else { Err(io::Error::new(io::ErrorKind::InvalidData, "guardian did not acknowledge arming")) }
}
pub fn send_normal_completion(channel: &mut UnixStream) -> io::Result<()> { channel.write_all(COMPLETE_MARKER) }
pub fn receive_normal_completion(channel: &mut UnixStream) -> io::Result<()> {
    let mut marker = [0_u8; 8];
    channel.read_exact(&mut marker)?;
    if &marker == COMPLETE_MARKER { Ok(()) } else { Err(io::Error::new(io::ErrorKind::InvalidData, "invalid guardian completion marker")) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn validates_only_session_scope_names() {
        for valid in ["session-c1.scope", "session-abc_123.scope", "session-4-test.scope"] {
            assert!(validate_scope_name(valid).is_ok(), "{valid}");
        }
        for invalid in ["", "user-1000.slice", "session-.scope", "session-a/b.scope",
            "session-../x.scope", "session-a.scope.extra", "session-a;stop.scope"] {
            assert!(validate_scope_name(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn arm_frame_round_trips_a_validated_scope() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        let thread = thread::spawn(move || send_arm(&mut sender, "session-c1.scope").unwrap());
        assert_eq!(receive_arm(&mut receiver).unwrap(), "session-c1.scope");
        thread.join().unwrap();
    }

    #[test]
    fn rejects_invalid_arm_scope_before_sending() {
        let (mut sender, _receiver) = UnixStream::pair().unwrap();
        assert!(send_arm(&mut sender, "user-1000.slice").is_err());
    }

    #[test]
    fn pidfd_transfer_preserves_a_live_process_reference() {
        let child = std::process::Command::new("/bin/sleep").arg("0.1").spawn().unwrap();
        let pidfd = PidFd::open(child.id() as libc::pid_t).unwrap();
        let (sender, receiver) = UnixStream::pair().unwrap();
        send_pidfd(&sender, &pidfd).unwrap();
        let received = receive_pidfd(&receiver).unwrap();
        assert!(!received.wait_exit(Duration::ZERO).unwrap());
        let mut child = child;
        child.wait().unwrap();
        assert!(received.wait_exit(Duration::from_secs(1)).unwrap());
    }
}
