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

/// Reads the supervisor approval with a finite deadline. A stalled supervisor
/// must not leave an authenticated PAM session open indefinitely.
pub fn read_supervisor_approval_with_timeout(
    channel: &mut UnixStream,
    timeout: Duration,
) -> io::Result<()> {
    channel.set_read_timeout(Some(timeout))?;
    let approval_result = read_supervisor_approval(channel);
    let reset_result = channel.set_read_timeout(None);
    match (approval_result, reset_result) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

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

/// Trusted session identity resolved independently by the privileged supervisor.
///
/// Construct this only from an authoritative source such as logind. It is
/// deliberately separate from worker-reported `SessionReady` metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIdentity {
    pub uid: u32,
    pub session_id: String,
    pub runtime_dir: String,
    pub seat: Option<String>,
    pub vt_number: Option<u32>,
}

/// Validates worker metadata and requires an exact match with trusted identity.
///
/// A worker claim is never accepted as proof of its own session membership.
pub fn validate_session_ready_against(
    ready: &SessionReady,
    trusted: &SessionIdentity,
) -> io::Result<()> {
    validate(ready)?;
    if ready.uid != trusted.uid
        || ready.session_id != trusted.session_id
        || ready.runtime_dir != trusted.runtime_dir
        || ready.seat != trusted.seat
        || ready.vt_number != trusted.vt_number
    {
        return Err(invalid_data(
            "worker session metadata does not match the trusted session identity",
        ));
    }
    Ok(())
}

/// Requires the inherited password descriptor to be a read-only anonymous/FIFO pipe.
pub fn validate_password_pipe_fd(fd: RawFd) -> io::Result<()> {
    if fd < 0 {
        return Err(invalid_input("password descriptor is invalid"));
    }

    let mut metadata = std::mem::MaybeUninit::<libc::stat>::zeroed();
    if unsafe { libc::fstat(fd, metadata.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let metadata = unsafe { metadata.assume_init() };
    if metadata.st_mode & libc::S_IFMT != libc::S_IFIFO {
        return Err(invalid_input("password descriptor must be a pipe"));
    }
    // Anonymous Linux pipes live on pipefs; named FIFOs live on the
    // filesystem where they were created. Check the filesystem type rather
    // than st_nlink, which is not a reliable discriminator for pipe ends.
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    if unsafe { libc::fstatfs(fd, filesystem.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let filesystem = unsafe { filesystem.assume_init() };
    const PIPEFS_MAGIC: libc::c_long = 0x5049_5045;
    if filesystem.f_type != PIPEFS_MAGIC {
        return Err(invalid_input(
            "password descriptor must be an anonymous pipe",
        ));
    }

    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if flags & libc::O_ACCMODE != libc::O_RDONLY {
        return Err(invalid_input("password pipe must be open for reading"));
    }

    Ok(())
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

/// Takes ownership of the worker control socket inherited on descriptor 3.
///
/// Call this at the very beginning of the single-threaded worker, before
/// starting threads or executing other programs. The descriptor is marked
/// close-on-exec again immediately so it cannot leak into the desktop session.
pub fn take_worker_channel() -> io::Result<UnixStream> {
    take_worker_channel_from_fd(3)
}

fn take_worker_channel_from_fd(fd: RawFd) -> io::Result<UnixStream> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }

    // Take ownership as soon as we know this is an open descriptor. Every
    // validation failure below then closes it instead of leaking fd 3.
    // SAFETY: callers transfer ownership of this descriptor to this function.
    let owned_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
    let fd = owned_fd.as_raw_fd();

    let mut domain: libc::c_int = 0;
    let mut domain_len = std::mem::size_of_val(&domain) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_DOMAIN,
            &mut domain as *mut _ as *mut libc::c_void,
            &mut domain_len,
        )
    } < 0
    {
        return Err(invalid_input(
            "worker control descriptor is not a Unix stream socket",
        ));
    }

    let mut socket_type: libc::c_int = 0;
    let mut type_len = std::mem::size_of_val(&socket_type) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            &mut socket_type as *mut _ as *mut libc::c_void,
            &mut type_len,
        )
    } < 0
    {
        return Err(invalid_input(
            "worker control descriptor is not a Unix stream socket",
        ));
    }
    if domain != libc::AF_UNIX || socket_type != libc::SOCK_STREAM {
        return Err(invalid_input(
            "worker control descriptor is not a Unix stream socket",
        ));
    }

    let mut peer: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut peer_len = std::mem::size_of_val(&peer) as libc::socklen_t;
    if unsafe {
        libc::getpeername(
            fd,
            &mut peer as *mut _ as *mut libc::sockaddr,
            &mut peer_len,
        )
    } < 0
    {
        return Err(invalid_input(
            "worker control socket is not connected to a peer",
        ));
    }

    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(UnixStream::from(owned_fd))
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
) -> io::Result<WorkerProcess> {
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
    let mut argv_ptrs: Vec<*mut libc::c_char> = argv
        .iter()
        .map(|value| value.as_ptr() as *mut libc::c_char)
        .collect();
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

    let status = unsafe { libc::posix_spawn_file_actions_adddup2(&mut actions.0, duplicate.0, 3) };
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
    Ok(WorkerProcess::new(pid))
}

/// Starts a worker and accepts it only after its exact bootstrap handshake.
///
/// The read timeout bounds startup if the executable exits, hangs, or speaks
/// an unexpected protocol. On every handshake failure the child is terminated
/// and reaped before the error is returned.
pub fn start_worker_with_handshake(
    program: &std::ffi::OsStr,
    arguments: &[std::ffi::OsString],
    timeout: Duration,
) -> io::Result<(UnixStream, WorkerProcess)> {
    let (mut supervisor_channel, worker_channel) = private_channel_pair()?;
    let mut worker = spawn_worker_with_channel(program, arguments, worker_channel)?;

    let handshake_result = (|| {
        supervisor_channel.set_read_timeout(Some(timeout))?;
        read_worker_hello(&mut supervisor_channel)?;
        supervisor_channel.set_read_timeout(None)?;
        Ok(())
    })();

    if let Err(error) = handshake_result {
        let _ = worker.terminate_and_reap(Duration::from_secs(1));
        return Err(error);
    }

    Ok((supervisor_channel, worker))
}

/// Owns a spawned worker until its exit status has been collected.
///
/// The supervisor must explicitly wait for or terminate the worker. As a final
/// safety net, dropping an unreaped handle sends SIGKILL and reaps the child.
pub struct WorkerProcess {
    pid: libc::pid_t,
    reaped: bool,
}

impl WorkerProcess {
    fn new(pid: libc::pid_t) -> Self {
        Self { pid, reaped: false }
    }

    /// Returns the worker PID while the process is still owned by this handle.
    pub fn id(&self) -> libc::pid_t {
        self.pid
    }

    /// Checks for worker exit without blocking.
    pub fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        if self.reaped {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "worker has already been reaped",
            ));
        }

        let mut status = 0;
        loop {
            let result = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
            if result == self.pid {
                self.reaped = true;
                return Ok(Some(std::process::ExitStatus::from_raw(status)));
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

    /// Waits for the worker to exit, retrying when interrupted by a signal.
    pub fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        if self.reaped {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "worker has already been reaped",
            ));
        }

        let mut status = 0;
        loop {
            let result = unsafe { libc::waitpid(self.pid, &mut status, 0) };
            if result == self.pid {
                self.reaped = true;
                return Ok(std::process::ExitStatus::from_raw(status));
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

    /// Waits up to `timeout` for normal worker termination.
    pub fn wait_timeout(
        &mut self,
        timeout: Duration,
    ) -> io::Result<Option<std::process::ExitStatus>> {
        let started = Instant::now();
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return Ok(None);
            }
            std::thread::sleep((timeout - elapsed).min(Duration::from_millis(10)));
        }
    }

    /// Requests graceful termination, then force-kills and reaps after `grace`.
    pub fn terminate_and_reap(&mut self, grace: Duration) -> io::Result<std::process::ExitStatus> {
        if self.reaped {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "worker has already been reaped",
            ));
        }
        if unsafe { libc::kill(self.pid, libc::SIGTERM) } < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        if let Some(status) = self.wait_timeout(grace)? {
            return Ok(status);
        }
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

/// Owns the private channel and process handle for one bootstrapped worker.
///
/// Normal shutdown closes the channel first, allowing the worker to observe
/// EOF and exit cleanly. A timeout falls back to the bounded TERM/KILL path.
pub struct SessionWorker {
    channel: Option<UnixStream>,
    process: WorkerProcess,
    metadata_received: bool,
}

impl SessionWorker {
    /// Starts a worker and requires its exact bootstrap marker before returning.
    pub fn start(
        program: &std::ffi::OsStr,
        arguments: &[std::ffi::OsString],
        startup_timeout: Duration,
    ) -> io::Result<Self> {
        let (channel, process) = start_worker_with_handshake(program, arguments, startup_timeout)?;
        Ok(Self {
            channel: Some(channel),
            process,
            metadata_received: false,
        })
    }

    /// Returns the worker PID.
    pub fn id(&self) -> libc::pid_t {
        self.process.id()
    }

    /// Checks whether the worker has exited without blocking.
    pub fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        self.process.try_wait()
    }

    /// Waits for the worker to finish its PAM and desktop-session cleanup.
    pub fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        self.process.wait()
    }

    /// Waits for graceful worker/PAM cleanup up to `timeout`, then escalates
    /// through SIGTERM and SIGKILL using the supplied termination grace.
    ///
    /// Returns whether the graceful deadline was exceeded. The private
    /// channel is closed first so a worker blocked on approval can unwind.
    pub fn wait_with_timeout(
        &mut self,
        timeout: Duration,
        termination_grace: Duration,
    ) -> io::Result<(std::process::ExitStatus, bool)> {
        self.channel.take();
        if let Some(status) = self.process.wait_timeout(timeout)? {
            return Ok((status, false));
        }
        self.process
            .terminate_and_reap(termination_grace)
            .map(|status| (status, true))
    }

    /// Receives the worker's single bounded session-metadata frame.
    ///
    /// The returned values are untrusted claims: the privileged supervisor
    /// must independently validate them against logind before using them.
    /// Any framing, validation, timeout, or socket error fails closed by
    /// closing the channel, allowing PAM cleanup, then force-stopping only
    /// if the worker does not exit within the grace period.
    pub fn receive_session_ready(&mut self, timeout: Duration) -> io::Result<SessionReady> {
        if self.metadata_received {
            let _ = self.abort_and_reap(Duration::from_secs(1));
            return Err(invalid_data(
                "worker has already supplied its session metadata frame",
            ));
        }
        let Some(channel) = self.channel.as_ref() else {
            let _ = self.abort_and_reap(Duration::from_secs(1));
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "worker channel is closed",
            ));
        };
        if let Err(error) = channel.set_read_timeout(Some(timeout)) {
            let _ = self.abort_and_reap(Duration::from_secs(1));
            return Err(error);
        }
        let channel = self.channel.as_mut().expect("channel checked above");

        let result = read_session_ready(channel);
        let reset_result = channel.set_read_timeout(None);
        match (result, reset_result) {
            (Ok(ready), Ok(())) => {
                self.metadata_received = true;
                Ok(ready)
            }
            (Err(error), _) | (_, Err(error)) => {
                let _ = self.abort_and_reap(Duration::from_secs(1));
                Err(error)
            }
        }
    }

    /// Receives metadata and validates it against supervisor-trusted identity.
    ///
    /// A mismatch is fatal: the channel is closed, giving the worker time to
    /// close PAM cleanly, and the worker is reaped before the error is returned.
    /// Callers that need to consume
    /// worker metadata should prefer this method over the untrusted raw receive.
    pub fn receive_session_ready_against(
        &mut self,
        timeout: Duration,
        trusted: &SessionIdentity,
    ) -> io::Result<SessionReady> {
        let ready = self.receive_session_ready(timeout)?;
        if let Err(error) = validate_session_ready_against(&ready, trusted) {
            let _ = self.abort_and_reap(Duration::from_secs(1));
            return Err(error);
        }
        Ok(ready)
    }

    /// Approves compositor startup after the supervisor has independently
    /// validated the worker's metadata against logind. The channel is closed
    /// immediately after the one-way approval is sent.
    pub fn approve_session(&mut self) -> io::Result<()> {
        if !self.metadata_received {
            return Err(invalid_input(
                "cannot approve a session before receiving worker metadata",
            ));
        }
        let Some(mut channel) = self.channel.take() else {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "worker channel is closed",
            ));
        };
        if let Err(error) = channel
            .write_all(SUPERVISOR_APPROVAL)
            .and_then(|()| channel.flush())
        {
            drop(channel);
            let _ = self.abort_and_reap(Duration::from_secs(1));
            return Err(error);
        }
        Ok(())
    }

    /// Closes the control channel and waits for graceful worker shutdown.
    /// If the worker does not exit before the grace period, it is terminated.
    pub fn shutdown(&mut self, grace: Duration) -> io::Result<std::process::ExitStatus> {
        self.abort_and_reap(grace)
    }

    /// Closes the protocol channel before waiting, so a worker blocked on
    /// supervisor approval can observe EOF and run its PAM Drop cleanup.
    fn abort_and_reap(&mut self, grace: Duration) -> io::Result<std::process::ExitStatus> {
        self.channel.take();
        match self.process.wait_timeout(grace)? {
            Some(status) => Ok(status),
            None => self.process.terminate_and_reap(grace),
        }
    }
}

impl Drop for SessionWorker {
    fn drop(&mut self) {
        let _ = self.abort_and_reap(Duration::from_secs(1));
    }
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
    use std::os::fd::IntoRawFd;

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
    fn worker_hello_accepts_exact_bootstrap_marker() {
        let mut bytes = Cursor::new(WORKER_HELLO);
        read_worker_hello(&mut bytes).unwrap();
    }

    #[test]
    fn worker_hello_rejects_wrong_marker() {
        let mut bytes = Cursor::new(*b"BUBWORKX");
        assert_eq!(
            read_worker_hello(&mut bytes).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn worker_hello_rejects_truncated_marker() {
        let mut bytes = Cursor::new(&WORKER_HELLO[..7]);
        assert_eq!(
            read_worker_hello(&mut bytes).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn supervisor_approval_accepts_only_the_exact_marker() {
        let mut bytes = Cursor::new(SUPERVISOR_APPROVAL);
        read_supervisor_approval(&mut bytes).unwrap();
    }

    #[test]
    fn supervisor_approval_times_out_when_supervisor_stalls() {
        let (mut worker, _supervisor) = UnixStream::pair().unwrap();
        let error = read_supervisor_approval_with_timeout(&mut worker, Duration::from_millis(25))
            .unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
    }

    #[test]
    fn supervisor_approval_rejects_invalid_or_truncated_markers() {
        let mut invalid = Cursor::new(*b"BUBOKAYX");
        assert_eq!(
            read_supervisor_approval(&mut invalid).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        let mut truncated = Cursor::new(&SUPERVISOR_APPROVAL[..7]);
        assert_eq!(
            read_supervisor_approval(&mut truncated).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn password_descriptor_must_be_a_read_only_pipe() {
        let mut pipe_fds = [-1; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipe_fds.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        assert!(validate_password_pipe_fd(pipe_fds[0]).is_ok());
        assert_eq!(
            validate_password_pipe_fd(pipe_fds[1]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        unsafe {
            libc::close(pipe_fds[0]);
            libc::close(pipe_fds[1]);
        }

        let file = tempfile::tempfile().unwrap();
        assert_eq!(
            validate_password_pipe_fd(file.as_raw_fd())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );

        let directory = tempfile::tempdir().unwrap();
        let fifo_path = directory.path().join("password.fifo");
        let fifo_path_c = std::ffi::CString::new(fifo_path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_path_c.as_ptr(), 0o600) }, 0);
        let fifo_fd = unsafe {
            libc::open(
                fifo_path_c.as_ptr(),
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        assert!(fifo_fd >= 0);
        assert_eq!(
            validate_password_pipe_fd(fifo_fd).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        unsafe { libc::close(fifo_fd) };
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
    fn worker_channel_take_owns_connected_unix_stream_and_sets_cloexec() {
        let (mut supervisor, worker) = private_channel_pair().unwrap();
        let raw_fd = worker.into_raw_fd();
        let mut worker = take_worker_channel_from_fd(raw_fd).unwrap();

        let flags = unsafe { libc::fcntl(worker.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0);
        assert_ne!(flags & libc::FD_CLOEXEC, 0);

        supervisor.write_all(b"ready").unwrap();
        let mut bytes = [0_u8; 5];
        worker.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"ready");
    }

    #[test]
    fn worker_channel_take_closes_invalid_descriptors() {
        let mut pipe_fds = [-1; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipe_fds.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        let error = take_worker_channel_from_fd(pipe_fds[0]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(unsafe { libc::fcntl(pipe_fds[0], libc::F_GETFD) }, -1);
        unsafe { libc::close(pipe_fds[1]) };

        let socket =
            unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        assert!(socket >= 0);
        assert!(take_worker_channel_from_fd(socket).is_err());
        assert_eq!(unsafe { libc::fcntl(socket, libc::F_GETFD) }, -1);
    }

    #[test]
    fn start_worker_accepts_only_a_valid_handshake() {
        let (channel, mut worker) = start_worker_with_handshake(
            std::ffi::OsStr::new("/bin/sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from("printf BUBWORK1 >&3; exec /bin/sleep 30"),
            ],
            Duration::from_secs(1),
        )
        .unwrap();

        assert!(channel.set_read_timeout(None).is_ok());
        let status = worker
            .terminate_and_reap(Duration::from_millis(50))
            .unwrap();
        assert_eq!(status.signal(), Some(libc::SIGTERM));
    }

    #[test]
    fn start_worker_rejects_an_invalid_handshake_and_reaps_child() {
        let error = start_worker_with_handshake(
            std::ffi::OsStr::new("/bin/sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from("printf BADHELLO >&3; exec /bin/sleep 30"),
            ],
            Duration::from_secs(1),
        )
        .err()
        .expect("invalid worker handshake must be rejected");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn session_worker_receives_valid_session_metadata_frame() {
        let expected = sample();
        let mut frame = Vec::new();
        write_session_ready(&mut frame, &expected).unwrap();
        let escaped = frame
            .iter()
            .map(|byte| format!("\\{:03o}", byte))
            .collect::<String>();
        let command = format!("printf BUBWORK1 >&3; printf '{escaped}' >&3; cat <&3");

        let mut worker = SessionWorker::start(
            std::ffi::OsStr::new("/bin/sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from(command),
            ],
            Duration::from_secs(1),
        )
        .unwrap();

        assert_eq!(
            worker
                .receive_session_ready(Duration::from_secs(1))
                .unwrap(),
            expected
        );
        assert!(worker.shutdown(Duration::from_secs(1)).unwrap().success());
    }

    #[test]
    fn session_worker_waits_for_supervisor_approval_before_exit() {
        let expected = sample();
        let mut frame = Vec::new();
        write_session_ready(&mut frame, &expected).unwrap();
        let escaped = frame
            .iter()
            .map(|byte| format!("\\{:03o}", byte))
            .collect::<String>();
        let command =
            format!("printf BUBWORK1 >&3; printf '{escaped}' >&3; dd bs=8 count=1 <&3 >/dev/null");
        let mut worker = SessionWorker::start(
            std::ffi::OsStr::new("/bin/sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from(command),
            ],
            Duration::from_secs(1),
        )
        .unwrap();

        assert_eq!(
            worker
                .receive_session_ready(Duration::from_secs(1))
                .unwrap(),
            expected
        );
        worker.approve_session().unwrap();
        assert!(worker.wait().unwrap().success());
    }

    #[test]
    fn session_worker_cannot_be_approved_before_metadata() {
        let mut worker = SessionWorker::start(
            std::ffi::OsStr::new("/bin/sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from("printf BUBWORK1 >&3; cat <&3"),
            ],
            Duration::from_secs(1),
        )
        .unwrap();

        assert_eq!(
            worker.approve_session().unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(worker.shutdown(Duration::from_secs(1)).unwrap().success());
    }

    #[test]
    fn session_worker_rejects_invalid_metadata_and_reaps_worker() {
        let mut worker = SessionWorker::start(
            std::ffi::OsStr::new("/bin/sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from(
                    "printf BUBWORK1 >&3; printf BADFRAME0000 >&3; exec /bin/sleep 30",
                ),
            ],
            Duration::from_secs(1),
        )
        .unwrap();

        let error = worker
            .receive_session_ready(Duration::from_secs(1))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(worker.channel.is_none());
        assert!(worker.process.reaped);
    }

    #[test]
    fn session_worker_rejects_each_trusted_identity_mismatch_and_reaps_worker() {
        let expected = sample();
        let identities = [
            SessionIdentity {
                uid: expected.uid + 1,
                session_id: expected.session_id.clone(),
                runtime_dir: expected.runtime_dir.clone(),
                seat: expected.seat.clone(),
                vt_number: expected.vt_number,
            },
            SessionIdentity {
                uid: expected.uid,
                session_id: "different-session".into(),
                runtime_dir: expected.runtime_dir.clone(),
                seat: expected.seat.clone(),
                vt_number: expected.vt_number,
            },
            SessionIdentity {
                uid: expected.uid,
                session_id: expected.session_id.clone(),
                runtime_dir: "/run/user/1001".into(),
                seat: expected.seat.clone(),
                vt_number: expected.vt_number,
            },
            SessionIdentity {
                uid: expected.uid,
                session_id: expected.session_id.clone(),
                runtime_dir: expected.runtime_dir.clone(),
                seat: Some("seat1".into()),
                vt_number: expected.vt_number,
            },
            SessionIdentity {
                uid: expected.uid,
                session_id: expected.session_id.clone(),
                runtime_dir: expected.runtime_dir.clone(),
                seat: expected.seat.clone(),
                vt_number: Some(expected.vt_number.unwrap_or(1) + 1),
            },
        ];

        for trusted in identities {
            let mut frame = Vec::new();
            write_session_ready(&mut frame, &expected).unwrap();
            let escaped = frame
                .iter()
                .map(|byte| format!("\\{:03o}", byte))
                .collect::<String>();
            let command =
                format!("printf BUBWORK1 >&3; printf '{escaped}' >&3; exec /bin/sleep 30");
            let mut worker = SessionWorker::start(
                std::ffi::OsStr::new("/bin/sh"),
                &[
                    std::ffi::OsString::from("-c"),
                    std::ffi::OsString::from(command),
                ],
                Duration::from_secs(1),
            )
            .unwrap();

            let error = worker
                .receive_session_ready_against(Duration::from_secs(1), &trusted)
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(worker.channel.is_none());
            assert!(worker.process.reaped);
        }
    }
    #[test]
    fn session_worker_rejects_a_second_metadata_frame_and_reaps_worker() {
        let expected = sample();
        let mut frame = Vec::new();
        write_session_ready(&mut frame, &expected).unwrap();
        let escaped = frame
            .iter()
            .map(|byte| format!("\\{:03o}", byte))
            .collect::<String>();
        let command =
            format!("printf BUBWORK1 >&3; printf '{escaped}{escaped}' >&3; exec /bin/sleep 30");
        let mut worker = SessionWorker::start(
            std::ffi::OsStr::new("/bin/sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from(command),
            ],
            Duration::from_secs(1),
        )
        .unwrap();

        assert_eq!(
            worker
                .receive_session_ready(Duration::from_secs(1))
                .unwrap(),
            expected
        );
        assert_eq!(
            worker
                .receive_session_ready(Duration::from_secs(1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert!(worker.channel.is_none());
        assert!(worker.process.reaped);
    }

    #[test]
    fn session_worker_invalid_read_timeout_fails_closed_and_reaps_worker() {
        let mut worker = SessionWorker::start(
            std::ffi::OsStr::new("/bin/sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from("printf BUBWORK1 >&3; exec /bin/sleep 30"),
            ],
            Duration::from_secs(1),
        )
        .unwrap();

        let error = worker.receive_session_ready(Duration::ZERO).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(worker.channel.is_none());
        assert!(worker.process.reaped);
    }

    #[test]
    fn session_worker_metadata_timeout_fails_closed_and_reaps_worker() {
        let mut worker = SessionWorker::start(
            std::ffi::OsStr::new("/bin/sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from("printf BUBWORK1 >&3; exec /bin/sleep 30"),
            ],
            Duration::from_secs(1),
        )
        .unwrap();

        let error = worker
            .receive_session_ready(Duration::from_millis(30))
            .unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
        assert!(worker.channel.is_none());
        assert!(worker.process.reaped);
    }

    #[test]
    fn start_worker_times_out_when_handshake_is_missing() {
        let error = start_worker_with_handshake(
            std::ffi::OsStr::new("/bin/sleep"),
            &[std::ffi::OsString::from("30")],
            Duration::from_millis(30),
        )
        .err()
        .expect("missing worker handshake must time out");

        assert!(matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
    }

    #[test]
    fn session_worker_shutdown_closes_channel_and_allows_clean_exit() {
        let mut worker = SessionWorker::start(
            std::ffi::OsStr::new("/bin/sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from("printf BUBWORK1 >&3; cat <&3"),
            ],
            Duration::from_secs(1),
        )
        .unwrap();

        let status = worker.shutdown(Duration::from_secs(1)).unwrap();
        assert!(status.success());
        assert!(worker.try_wait().is_err());
    }

    #[test]
    fn session_worker_shutdown_terminates_worker_that_ignores_channel_eof() {
        let mut worker = SessionWorker::start(
            std::ffi::OsStr::new("/bin/sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from("printf BUBWORK1 >&3; exec /bin/sleep 30"),
            ],
            Duration::from_secs(1),
        )
        .unwrap();

        let status = worker.shutdown(Duration::from_millis(20)).unwrap();
        assert_eq!(status.signal(), Some(libc::SIGTERM));
    }
    #[test]
    fn worker_spawn_receives_channel_on_fd_three() {
        let (mut supervisor, worker) = private_channel_pair().unwrap();
        let mut worker = spawn_worker_with_channel(
            std::ffi::OsStr::new("/bin/sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from("printf ok >&3"),
            ],
            worker,
        )
        .unwrap();

        let mut bytes = [0_u8; 2];
        supervisor.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"ok");

        let status = worker.wait().unwrap();
        assert!(status.success());
    }

    #[test]
    fn worker_terminate_and_reap_allows_graceful_sigterm_exit() {
        use std::io::Read;

        let (mut supervisor, worker_channel) = private_channel_pair().unwrap();
        let mut worker = spawn_worker_with_channel(
            std::ffi::OsStr::new("/bin/sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from(
                    "trap 'exit 0' TERM; printf ready >&3; while :; do :; done",
                ),
            ],
            worker_channel,
        )
        .unwrap();

        let mut ready = [0_u8; 5];
        supervisor.read_exact(&mut ready).unwrap();
        assert_eq!(&ready, b"ready");

        let status = worker.terminate_and_reap(Duration::from_secs(1)).unwrap();
        assert_eq!(status.code(), Some(0));
        assert!(worker.try_wait().is_err());
    }

    #[test]
    fn worker_terminate_and_reap_escalates_to_sigkill() {
        use std::io::Read;

        let (mut supervisor, worker_channel) = private_channel_pair().unwrap();
        let mut worker = spawn_worker_with_channel(
            std::ffi::OsStr::new("/bin/sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from("trap '' TERM; printf ready >&3; exec /bin/sleep 30"),
            ],
            worker_channel,
        )
        .unwrap();

        let mut ready = [0_u8; 5];
        supervisor.read_exact(&mut ready).unwrap();
        assert_eq!(&ready, b"ready");

        let status = worker
            .terminate_and_reap(Duration::from_millis(30))
            .unwrap();
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert!(worker.wait().is_err());
    }

    #[test]
    fn worker_wait_timeout_returns_none_while_process_is_running() {
        let (_supervisor, worker_channel) = private_channel_pair().unwrap();
        let mut worker = spawn_worker_with_channel(
            std::ffi::OsStr::new("/bin/sleep"),
            &[std::ffi::OsString::from("30")],
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
            .terminate_and_reap(Duration::from_millis(20))
            .unwrap();
        assert_eq!(status.signal(), Some(libc::SIGTERM));
    }

    #[test]
    fn worker_wait_timeout_accepts_extremely_large_duration() {
        let (_supervisor, worker_channel) = private_channel_pair().unwrap();
        let mut worker = spawn_worker_with_channel(
            std::ffi::OsStr::new("/bin/sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from("exit 23"),
            ],
            worker_channel,
        )
        .unwrap();

        let status = worker
            .wait_timeout(Duration::MAX)
            .unwrap()
            .expect("worker should already have exited");
        assert_eq!(status.code(), Some(23));
    }

    #[test]
    fn worker_wait_timeout_collects_normal_exit_status() {
        let (_supervisor, worker_channel) = private_channel_pair().unwrap();
        let mut worker = spawn_worker_with_channel(
            std::ffi::OsStr::new("/bin/sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from("exit 23"),
            ],
            worker_channel,
        )
        .unwrap();

        let status = worker
            .wait_timeout(Duration::from_secs(1))
            .unwrap()
            .expect("worker should exit before the timeout");
        assert_eq!(status.code(), Some(23));
        assert!(worker.try_wait().is_err());
        assert!(worker.wait().is_err());
    }

    #[test]
    fn session_worker_shutdown_closes_channel_before_waiting_for_cleanup() {
        use std::io::Read;

        let (mut supervisor, worker_channel) = private_channel_pair().unwrap();
        let process = spawn_worker_with_channel(
            std::ffi::OsStr::new("/bin/sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from(
                    "printf BUBWORK1 >&3; dd bs=1 count=1 <&3 >/dev/null 2>&1; exit 23",
                ),
            ],
            worker_channel,
        )
        .unwrap();
        let mut hello = [0_u8; 8];
        supervisor.read_exact(&mut hello).unwrap();
        assert_eq!(&hello, b"BUBWORK1");

        let mut worker = SessionWorker {
            channel: Some(supervisor),
            process,
            metadata_received: false,
        };
        let status = worker.shutdown(Duration::from_secs(1)).unwrap();
        assert_eq!(status.code(), Some(23));
    }

    #[test]
    fn dropping_unreaped_worker_kills_and_reaps_child() {
        let (_supervisor, worker_channel) = private_channel_pair().unwrap();
        let worker = spawn_worker_with_channel(
            std::ffi::OsStr::new("/bin/sleep"),
            &[std::ffi::OsString::from("30")],
            worker_channel,
        )
        .unwrap();
        let pid = worker.id();

        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) }, 0);

        drop(worker);

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
        let bad_magic = [0_u8; HEADER_LEN];
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
    fn trusted_session_identity_accepts_an_exact_metadata_match() {
        let ready = sample();
        let trusted = SessionIdentity {
            uid: ready.uid,
            session_id: ready.session_id.clone(),
            runtime_dir: ready.runtime_dir.clone(),
            seat: ready.seat.clone(),
            vt_number: ready.vt_number,
        };

        validate_session_ready_against(&ready, &trusted).unwrap();
    }

    #[test]
    fn trusted_session_identity_rejects_each_worker_claim_mismatch() {
        let trusted = SessionIdentity {
            uid: 1000,
            session_id: "c7".into(),
            runtime_dir: "/run/user/1000".into(),
            seat: Some("seat0".into()),
            vt_number: Some(2),
        };

        let mut mismatched = sample();
        mismatched.uid = 1001;
        assert_eq!(
            validate_session_ready_against(&mismatched, &trusted)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );

        let mut mismatched = sample();
        mismatched.session_id = "c8".into();
        assert!(validate_session_ready_against(&mismatched, &trusted).is_err());

        let mut mismatched = sample();
        mismatched.runtime_dir = "/run/user/1001".into();
        assert!(validate_session_ready_against(&mismatched, &trusted).is_err());

        let mut mismatched = sample();
        mismatched.seat = Some("seat1".into());
        assert!(validate_session_ready_against(&mismatched, &trusted).is_err());

        let mut mismatched = sample();
        mismatched.vt_number = Some(3);
        assert!(validate_session_ready_against(&mismatched, &trusted).is_err());
    }

    #[test]
    fn trusted_session_identity_does_not_bypass_metadata_validation() {
        let mut invalid = sample();
        invalid.uid = 0;
        let trusted = SessionIdentity {
            uid: 0,
            session_id: invalid.session_id.clone(),
            runtime_dir: invalid.runtime_dir.clone(),
            seat: invalid.seat.clone(),
            vt_number: invalid.vt_number,
        };

        assert_eq!(
            validate_session_ready_against(&invalid, &trusted)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
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
