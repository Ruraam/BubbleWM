//! Private process and descriptor primitives for supervised session workers.
//!
//! This module does not define the worker protocol or authorize session
//! metadata. It only creates a private Unix stream and owns the child process.
//! The supervisor must validate all worker-reported data independently.

use std::{
    ffi::{CString, OsStr, OsString},
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::{ffi::OsStrExt, net::UnixStream, process::ExitStatusExt},
    },
    path::Path,
    process::ExitStatus,
    time::{Duration, Instant},
};

/// Creates a connected, close-on-exec supervisor/worker stream pair.
pub(crate) fn private_channel_pair() -> io::Result<(UnixStream, UnixStream)> {
    let (supervisor, worker) = UnixStream::pair()?;
    set_close_on_exec(&supervisor)?;
    set_close_on_exec(&worker)?;
    Ok((supervisor, worker))
}

fn set_close_on_exec(stream: &UnixStream) -> io::Result<()> {
    let fd = stream.as_raw_fd();
    // SAFETY: fcntl operates on the live descriptor borrowed from stream.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: same live descriptor; only its descriptor flags are changed.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Takes ownership of the worker socket inherited on descriptor 3.
///
/// Call this at worker startup, before creating threads or spawning programs.
pub(crate) fn take_worker_channel() -> io::Result<UnixStream> {
    take_worker_channel_from_fd(3)
}

fn take_worker_channel_from_fd(fd: RawFd) -> io::Result<UnixStream> {
    // SAFETY: F_GETFD checks whether fd is open; ownership is transferred below.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: callers transfer ownership of this open descriptor to this function.
    let owned_fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let raw_fd = owned_fd.as_raw_fd();

    let mut domain: libc::c_int = 0;
    let mut domain_len = std::mem::size_of_val(&domain) as libc::socklen_t;
    // SAFETY: pointers reference correctly sized writable storage.
    if unsafe {
        libc::getsockopt(
            raw_fd,
            libc::SOL_SOCKET,
            libc::SO_DOMAIN,
            &mut domain as *mut _ as *mut libc::c_void,
            &mut domain_len,
        )
    } < 0
    {
        return Err(invalid_input(
            "worker descriptor is not a connected Unix stream",
        ));
    }

    let mut socket_type: libc::c_int = 0;
    let mut type_len = std::mem::size_of_val(&socket_type) as libc::socklen_t;
    // SAFETY: pointers reference correctly sized writable storage.
    if unsafe {
        libc::getsockopt(
            raw_fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            &mut socket_type as *mut _ as *mut libc::c_void,
            &mut type_len,
        )
    } < 0
        || domain != libc::AF_UNIX
        || socket_type != libc::SOCK_STREAM
    {
        return Err(invalid_input(
            "worker descriptor is not a connected Unix stream",
        ));
    }

    let mut peer: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut peer_len = std::mem::size_of_val(&peer) as libc::socklen_t;
    // SAFETY: pointers reference correctly sized writable storage.
    if unsafe {
        libc::getpeername(
            raw_fd,
            &mut peer as *mut _ as *mut libc::sockaddr,
            &mut peer_len,
        )
    } < 0
    {
        return Err(invalid_input("worker control socket has no connected peer"));
    }

    // SAFETY: raw_fd remains owned by owned_fd.
    if unsafe { libc::fcntl(raw_fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(UnixStream::from(owned_fd))
}

/// Spawns an absolute-path worker with its private channel mapped to fd 3.
///
/// The child starts with an empty environment. Arguments must be fixed,
/// non-secret values selected by the trusted supervisor.
pub(crate) fn spawn_worker_with_channel(
    program: &OsStr,
    arguments: &[OsString],
    worker_channel: UnixStream,
) -> io::Result<WorkerProcess> {
    if !Path::new(program).is_absolute() {
        return Err(invalid_input("worker executable path must be absolute"));
    }

    let program = CString::new(program.as_bytes())
        .map_err(|_| invalid_input("worker executable path contains a NUL byte"))?;
    let mut argv = Vec::with_capacity(arguments.len() + 1);
    argv.push(program.clone());
    for argument in arguments {
        argv.push(
            CString::new(argument.as_bytes())
                .map_err(|_| invalid_input("worker argument contains a NUL byte"))?,
        );
    }
    let mut argv_ptrs: Vec<*mut libc::c_char> = argv
        .iter()
        .map(|value| value.as_ptr() as *mut libc::c_char)
        .collect();
    argv_ptrs.push(std::ptr::null_mut());
    let mut envp = [std::ptr::null_mut()];

    // Keep the duplicate above fd 3 so the child can safely dup it to fd 3.
    // SAFETY: worker_channel owns a valid socket descriptor.
    let duplicate = unsafe { libc::fcntl(worker_channel.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 4) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    let duplicate = OwnedFdGuard(duplicate);

    let mut actions_storage = std::mem::MaybeUninit::<libc::posix_spawn_file_actions_t>::uninit();
    // SAFETY: actions_storage points to uninitialized storage for this API.
    let status = unsafe { libc::posix_spawn_file_actions_init(actions_storage.as_mut_ptr()) };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status));
    }
    // SAFETY: init succeeded and initialized the file-actions structure.
    let mut actions = SpawnFileActions(unsafe { actions_storage.assume_init() });

    // SAFETY: actions is initialized and duplicate is an open descriptor.
    let status = unsafe { libc::posix_spawn_file_actions_adddup2(&mut actions.0, duplicate.0, 3) };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status));
    }
    // SAFETY: duplicate is closed in the child after fd 3 has been established.
    let status = unsafe { libc::posix_spawn_file_actions_addclose(&mut actions.0, duplicate.0) };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status));
    }

    let mut pid = 0;
    // SAFETY: all C strings and pointer arrays remain alive for the call;
    // file actions establish the only inherited worker descriptor.
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

    drop(worker_channel);
    Ok(WorkerProcess::new(pid))
}

/// Owns a worker PID until its exit status has been collected.
#[derive(Debug)]
pub(crate) struct WorkerProcess {
    pid: libc::pid_t,
    reaped: bool,
}

impl WorkerProcess {
    fn new(pid: libc::pid_t) -> Self {
        Self { pid, reaped: false }
    }

    pub(crate) fn id(&self) -> libc::pid_t {
        self.pid
    }

    /// Checks for process exit without blocking.
    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.reaped {
            return Err(already_reaped());
        }

        let mut status = 0;
        loop {
            // SAFETY: pid is owned by this handle and status is writable.
            let result = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
            if result == self.pid {
                self.reaped = true;
                return Ok(Some(ExitStatus::from_raw(status)));
            }
            if result == 0 {
                return Ok(None);
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.raw_os_error() == Some(libc::ECHILD) {
                self.reaped = true;
            }
            return Err(error);
        }
    }

    /// Waits for process exit, retrying interrupted waitpid calls.
    pub(crate) fn wait(&mut self) -> io::Result<ExitStatus> {
        if self.reaped {
            return Err(already_reaped());
        }

        let mut status = 0;
        loop {
            // SAFETY: pid is owned by this handle and status is writable.
            let result = unsafe { libc::waitpid(self.pid, &mut status, 0) };
            if result == self.pid {
                self.reaped = true;
                return Ok(ExitStatus::from_raw(status));
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.raw_os_error() == Some(libc::ECHILD) {
                self.reaped = true;
            }
            return Err(error);
        }
    }

    /// Waits up to timeout for normal worker termination.
    pub(crate) fn wait_timeout(&mut self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(10)));
        }
    }

    /// Sends SIGTERM, then SIGKILL if needed, and always attempts to reap.
    pub(crate) fn terminate_and_reap(&mut self, grace: Duration) -> io::Result<ExitStatus> {
        if self.reaped {
            return Err(already_reaped());
        }
        // SAFETY: signal is sent only to the child PID owned by this handle.
        if unsafe { libc::kill(self.pid, libc::SIGTERM) } < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        if let Some(status) = self.wait_timeout(grace)? {
            return Ok(status);
        }
        // SAFETY: same exclusively owned child PID.
        if unsafe { libc::kill(self.pid, libc::SIGKILL) } < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        self.wait()
    }
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        // SAFETY: this handle exclusively owns the unreaped child PID.
        unsafe { libc::kill(self.pid, libc::SIGKILL) };
        let mut status = 0;
        loop {
            // SAFETY: pid is owned by this handle and status is writable.
            let result = unsafe { libc::waitpid(self.pid, &mut status, 0) };
            if result == self.pid
                || (result < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD))
            {
                self.reaped = true;
                break;
            }
            if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                break;
            }
        }
    }
}

struct OwnedFdGuard(RawFd);

impl Drop for OwnedFdGuard {
    fn drop(&mut self) {
        // SAFETY: this guard owns the duplicated descriptor.
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
        // SAFETY: this wrapper owns an initialized file-actions structure.
        unsafe { libc::posix_spawn_file_actions_destroy(&mut self.0) };
    }
}

fn already_reaped() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "worker has already been reaped",
    )
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        os::fd::IntoRawFd,
    };

    #[test]
    fn private_channel_is_connected_and_close_on_exec() {
        let (mut supervisor, mut worker) = private_channel_pair().unwrap();
        worker.write_all(b"ok").unwrap();
        let mut bytes = [0_u8; 2];
        supervisor.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"ok");

        for stream in [&supervisor, &worker] {
            // SAFETY: stream owns a valid descriptor.
            let flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFD) };
            assert!(flags >= 0);
            assert_ne!(flags & libc::FD_CLOEXEC, 0);
        }
    }

    #[test]
    fn worker_channel_take_owns_connected_socket_and_sets_close_on_exec() {
        let (mut supervisor, worker) = private_channel_pair().unwrap();
        let raw_fd = worker.into_raw_fd();
        let mut worker = take_worker_channel_from_fd(raw_fd).unwrap();

        // SAFETY: worker owns a valid descriptor.
        let flags = unsafe { libc::fcntl(worker.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0);
        assert_ne!(flags & libc::FD_CLOEXEC, 0);

        supervisor.write_all(b"ready").unwrap();
        let mut bytes = [0_u8; 5];
        worker.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"ready");
    }

    #[test]
    fn worker_channel_take_rejects_non_socket_and_closes_descriptor() {
        let mut pipe_fds = [-1; 2];
        // SAFETY: pipe_fds points to two writable descriptor slots.
        assert_eq!(
            unsafe { libc::pipe2(pipe_fds.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        let error = take_worker_channel_from_fd(pipe_fds[0]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        // SAFETY: pipe_fds[0] was transferred and closed by the function.
        assert_eq!(unsafe { libc::fcntl(pipe_fds[0], libc::F_GETFD) }, -1);
        // SAFETY: the test still owns the write end.
        unsafe { libc::close(pipe_fds[1]) };
    }

    #[test]
    fn spawn_maps_private_channel_to_descriptor_three() {
        let (mut supervisor, worker_channel) = private_channel_pair().unwrap();
        let mut worker = spawn_worker_with_channel(
            OsStr::new("/bin/sh"),
            &[OsString::from("-c"), OsString::from("printf ok >&3")],
            worker_channel,
        )
        .unwrap();

        let mut bytes = [0_u8; 2];
        supervisor.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"ok");
        assert!(worker.wait().unwrap().success());
    }

    #[test]
    fn worker_wait_timeout_and_terminate_reap_the_child() {
        let (_supervisor, worker_channel) = private_channel_pair().unwrap();
        let mut worker = spawn_worker_with_channel(
            OsStr::new("/bin/sleep"),
            &[OsString::from("30")],
            worker_channel,
        )
        .unwrap();

        assert!(
            worker
                .wait_timeout(Duration::from_millis(20))
                .unwrap()
                .is_none()
        );
        let status = worker
            .terminate_and_reap(Duration::from_millis(100))
            .unwrap();
        assert_eq!(status.signal(), Some(libc::SIGTERM));
        assert!(worker.try_wait().is_err());
    }

    #[test]
    fn dropping_unreaped_worker_kills_and_reaps_child() {
        let (_supervisor, worker_channel) = private_channel_pair().unwrap();
        let worker = spawn_worker_with_channel(
            OsStr::new("/bin/sleep"),
            &[OsString::from("30")],
            worker_channel,
        )
        .unwrap();
        let pid = worker.id();

        drop(worker);

        let mut status = 0;
        // SAFETY: checking that the child was already reaped.
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[test]
    fn spawn_rejects_relative_executable_paths() {
        let (_supervisor, worker_channel) = private_channel_pair().unwrap();
        let error = spawn_worker_with_channel(OsStr::new("sh"), &[], worker_channel).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
