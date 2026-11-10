//! Linux Unix-domain socket transport for the BubbleWM IPC protocol.
//!
//! Run the blocking accept loop on a dedicated worker thread, never on the
//! compositor's Wayland event loop. The request handler should forward commands
//! to the compositor thread through a bounded channel when shared state is needed.

use std::{
    fs::{self, DirBuilder},
    io::{BufReader, ErrorKind},
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use super::{
    ErrorCode, ErrorResponse, PROTOCOL_VERSION, ProtocolError, Request, RequestEnvelope,
    Response, ResponseEnvelope, read_frame, write_frame,
};

use std::os::fd::AsRawFd;

/// Socket location relative to XDG_RUNTIME_DIR.
pub const SOCKET_RELATIVE_PATH: &str = "bubblewm/control.sock";
const SOCKET_MODE: u32 = 0o600;
const PRIVATE_DIR_MODE: u32 = 0o700;
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(20);
const CLIENT_IO_TIMEOUT: Duration = Duration::from_secs(2);

/// Errors raised while preparing or running the local IPC listener.
#[derive(Debug)]
pub enum ServerError {
    Io(std::io::Error),
    RuntimeDirectoryUnavailable,
    UnsafeDirectory(PathBuf),
    SocketPathOccupied(PathBuf),
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "BubbleWM IPC socket error: {error}"),
            Self::RuntimeDirectoryUnavailable => {
                write!(formatter, "XDG_RUNTIME_DIR is missing or invalid")
            }
            Self::UnsafeDirectory(path) => {
                write!(
                    formatter,
                    "refusing to use an unsafe IPC directory: {}",
                    path.display()
                )
            }
            Self::SocketPathOccupied(path) => {
                write!(
                    formatter,
                    "another BubbleWM IPC server owns {}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for ServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ServerError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Returns the default socket path after validating and preparing its private
/// per-user runtime subdirectory. XDG_RUNTIME_DIR itself is never modified.
pub fn default_socket_path() -> Result<PathBuf, ServerError> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or(ServerError::RuntimeDirectoryUnavailable)?;

    validate_directory(&runtime_dir, false)?;
    let app_dir = runtime_dir.join("bubblewm");

    match DirBuilder::new().mode(PRIVATE_DIR_MODE).create(&app_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => return Err(ServerError::Io(error)),
    }

    validate_directory(&app_dir, true)?;
    Ok(app_dir.join("control.sock"))
}

/// Validates a directory without following a symbolic link.
fn validate_directory(path: &Path, tighten_permissions: bool) -> Result<(), ServerError> {
    let metadata = fs::symlink_metadata(path).map_err(ServerError::Io)?;
    if !metadata.file_type().is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(ServerError::UnsafeDirectory(path.to_path_buf()));
    }

    let mode = metadata.mode() & 0o777;
    if mode & 0o077 != 0 {
        if !tighten_permissions {
            return Err(ServerError::UnsafeDirectory(path.to_path_buf()));
        }
        fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_DIR_MODE))
            .map_err(ServerError::Io)?;
        let updated = fs::symlink_metadata(path).map_err(ServerError::Io)?;
        if !updated.file_type().is_dir()
            || updated.uid() != unsafe { libc::geteuid() }
            || updated.mode() & 0o077 != 0
        {
            return Err(ServerError::UnsafeDirectory(path.to_path_buf()));
        }
    }
    Ok(())
}

/// Owns the BubbleWM local IPC listening socket.
///
/// The server handles one request per connection. Its accept loop is blocking
/// at the API level and must be run away from the compositor event loop.
pub struct UnixIpcServer {
    listener: UnixListener,
    socket_path: PathBuf,
    socket_device: u64,
    socket_inode: u64,
}

impl UnixIpcServer {
    /// Binds the default per-user socket beneath XDG_RUNTIME_DIR.
    pub fn bind_default() -> Result<Self, ServerError> {
        Self::bind(default_socket_path()?)
    }

    /// Binds a socket inside an existing private directory owned by this user.
    /// A stale socket is removed, but regular files and live sockets are never
    /// unlinked or replaced.
    pub fn bind(path: impl AsRef<Path>) -> Result<Self, ServerError> {
        let socket_path = path.as_ref().to_path_buf();
        let parent = socket_path
            .parent()
            .ok_or_else(|| ServerError::UnsafeDirectory(socket_path.clone()))?;
        validate_directory(parent, false)?;

        match fs::symlink_metadata(&socket_path) {
            Ok(metadata) => {
                if !metadata.file_type().is_socket() {
                    return Err(ServerError::SocketPathOccupied(socket_path));
                }
                match UnixStream::connect(&socket_path) {
                    Ok(_) => return Err(ServerError::SocketPathOccupied(socket_path)),
                    Err(error)
                        if matches!(
                            error.kind(),
                            ErrorKind::ConnectionRefused | ErrorKind::NotFound
                        ) =>
                    {
                        fs::remove_file(&socket_path).map_err(ServerError::Io)?;
                    }
                    Err(error) => return Err(ServerError::Io(error)),
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(ServerError::Io(error)),
        }

        let listener = UnixListener::bind(&socket_path).map_err(ServerError::Io)?;
        if let Err(error) =
            fs::set_permissions(&socket_path, fs::Permissions::from_mode(SOCKET_MODE))
        {
            let _ = fs::remove_file(&socket_path);
            return Err(ServerError::Io(error));
        }
        if let Err(error) = listener.set_nonblocking(true) {
            let _ = fs::remove_file(&socket_path);
            return Err(ServerError::Io(error));
        }

        let metadata = fs::symlink_metadata(&socket_path).map_err(ServerError::Io)?;
        if !metadata.file_type().is_socket()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o777 != SOCKET_MODE
        {
            let _ = fs::remove_file(&socket_path);
            return Err(ServerError::UnsafeDirectory(socket_path));
        }

        Ok(Self {
            listener,
            socket_path,
            socket_device: metadata.dev(),
            socket_inode: metadata.ino(),
        })
    }

    /// Returns the filesystem path used by this listener.
    pub fn path(&self) -> &Path {
        &self.socket_path
    }

    /// Accepts and serves requests until shutdown is set.
    ///
    /// Only peers with the same effective UID are served. Each connection gets
    /// bounded read/write timeouts and exactly one request/response exchange.
    /// Per-client protocol or authentication failures close that connection
    /// without terminating the listener.
    pub fn serve_until<F>(&self, shutdown: &AtomicBool, handler: F) -> Result<(), ServerError>
    where
        F: Fn(Request) -> Response,
    {
        while !shutdown.load(Ordering::Acquire) {
            match self.listener.accept() {
                Ok((mut stream, _address)) => {
                    let _ = self.serve_connection(&mut stream, &handler);
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(ACCEPT_POLL_INTERVAL);
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(ServerError::Io(error)),
            }
        }
        Ok(())
    }

    fn serve_connection<F>(&self, stream: &mut UnixStream, handler: &F) -> Result<(), ProtocolError>
    where
        F: Fn(Request) -> Response,
    {
        stream.set_read_timeout(Some(CLIENT_IO_TIMEOUT))?;
        stream.set_write_timeout(Some(CLIENT_IO_TIMEOUT))?;
        if !peer_has_current_uid(stream).unwrap_or(false) {
            return Ok(());
        }

        let request: RequestEnvelope = {
            let mut reader = BufReader::new(&mut *stream);
            read_frame(&mut reader)?
        };

        let response = if request.protocol_version != PROTOCOL_VERSION {
            Response::Failure {
                error: ErrorResponse {
                    code: ErrorCode::UnsupportedVersion,
                    message: format!(
                        "protocol version {} is not supported; expected {}",
                        request.protocol_version, PROTOCOL_VERSION
                    ),
                },
            }
        } else {
            handler(request.request)
        };

        let envelope = ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id,
            response,
        };
        write_frame(stream, &envelope)
    }
}

impl Drop for UnixIpcServer {
    fn drop(&mut self) {
        // Do not unlink a path that another process may have replaced.
        if let Ok(metadata) = fs::symlink_metadata(&self.socket_path) {
            if metadata.file_type().is_socket()
                && metadata.dev() == self.socket_device
                && metadata.ino() == self.socket_inode
                && metadata.uid() == unsafe { libc::geteuid() }
            {
                let _ = fs::remove_file(&self.socket_path);
            }
        }
    }
}

/// Checks Linux SO_PEERCRED before accepting a local IPC request.
fn peer_has_current_uid(stream: &UnixStream) -> std::io::Result<bool> {
    let mut credentials = std::mem::MaybeUninit::<libc::ucred>::zeroed();
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the kernel fills the initialized-size ucred buffer on success.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of::<libc::ucred>() {
        return Ok(false);
    }
    // SAFETY: getsockopt succeeded and returned exactly sizeof(ucred).
    let credentials = unsafe { credentials.assume_init() };
    Ok(credentials.uid == unsafe { libc::geteuid() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PROTOCOL_VERSION, Request, ResponseData};
    use std::{
        io::BufReader,
        os::unix::fs::PermissionsExt,
        sync::{Arc, atomic::AtomicBool},
    };

    #[test]
    fn binds_private_socket_and_removes_it_on_drop() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("control.sock");
        {
            let server = UnixIpcServer::bind(&path).expect("bind private socket");
            let metadata = fs::symlink_metadata(&path).expect("socket metadata");
            assert_eq!(metadata.permissions().mode() & 0o777, SOCKET_MODE);
            assert_eq!(server.path(), path);
        }
        assert!(!path.exists());
    }

    #[test]
    fn refuses_to_replace_a_regular_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("control.sock");
        fs::write(&path, "keep me").expect("create sentinel file");
        assert!(matches!(
            UnixIpcServer::bind(&path),
            Err(ServerError::SocketPathOccupied(_))
        ));
        assert_eq!(fs::read_to_string(path).expect("read sentinel"), "keep me");
    }

    #[test]
    fn server_authenticates_and_answers_a_versioned_request() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let server = Arc::new(
            UnixIpcServer::bind(directory.path().join("control.sock")).expect("bind server"),
        );
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_server = Arc::clone(&server);
        let worker_shutdown = Arc::clone(&shutdown);
        let worker = thread::spawn(move || {
            worker_server
                .serve_until(&worker_shutdown, |request| match request {
                    Request::GetCapabilities => Response::Success {
                        data: ResponseData::Capabilities {
                            workspace_control: false,
                        },
                    },
                    _ => Response::Failure {
                        error: ErrorResponse {
                            code: ErrorCode::Unavailable,
                            message: "workspace manager is not ready".into(),
                        },
                    },
                })
                .expect("serve loop");
        });

        let mut stream = UnixStream::connect(server.path()).expect("connect to server");
        let request = RequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: 7,
            request: Request::GetCapabilities,
        };
        write_frame(&mut stream, &request).expect("send request");
        let response: ResponseEnvelope =
            read_frame(&mut BufReader::new(stream)).expect("read response");
        assert_eq!(response.request_id, 7);
        assert_eq!(
            response.response,
            Response::Success {
                data: ResponseData::Capabilities {
                    workspace_control: false
                }
            }
        );

        shutdown.store(true, Ordering::Release);
        worker.join().expect("join server worker");
    }

    #[test]
    fn rejects_insecure_directory_permissions() {
        let directory = tempfile::tempdir().expect("temporary directory");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755))
            .expect("relax directory permissions");
        assert!(matches!(
            UnixIpcServer::bind(directory.path().join("control.sock")),
            Err(ServerError::UnsafeDirectory(_))
        ));
    }
}
