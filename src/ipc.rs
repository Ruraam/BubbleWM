//! Bridge between the Unix IPC worker and the compositor-owned state thread.
//!
//! The compositor must call [`CoreCommandReceiver::dispatch_pending`] from its
//! own event loop. The IPC worker never reads or mutates compositor state.

use std::{
    io,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use bubble_ipc::{
    ErrorCode, ErrorResponse, Request, Response, unix::{ServerError, UnixIpcServer},
};

const COMMAND_QUEUE_CAPACITY: usize = 64;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);

/// Errors returned while starting or stopping the compositor IPC worker.
#[derive(Debug)]
pub enum IpcRuntimeError {
    Server(ServerError),
    ThreadSpawn(io::Error),
    WorkerPanicked,
}

impl std::fmt::Display for IpcRuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Server(error) => write!(formatter, "{error}"),
            Self::ThreadSpawn(error) => write!(formatter, "cannot spawn BubbleWM IPC worker: {error}"),
            Self::WorkerPanicked => write!(formatter, "BubbleWM IPC worker panicked"),
        }
    }
}

impl std::error::Error for IpcRuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Server(error) => Some(error),
            Self::ThreadSpawn(error) => Some(error),
            Self::WorkerPanicked => None,
        }
    }
}

impl From<ServerError> for IpcRuntimeError {
    fn from(error: ServerError) -> Self {
        Self::Server(error)
    }
}

struct CoreCommand {
    request: Request,
    reply: SyncSender<Response>,
}

/// Receiver owned by the compositor thread.
///
/// Call `dispatch_pending` regularly from the compositor event loop. The
/// supplied handler executes synchronously on the calling thread, which keeps
/// all compositor state access on its owner thread.
pub struct CoreCommandReceiver {
    receiver: Receiver<CoreCommand>,
}

impl CoreCommandReceiver {
    /// Processes all currently queued requests and returns the number handled.
    ///
    /// The handler must be quick and must not perform blocking I/O. A response
    /// that cannot be delivered because the client timed out is safely dropped.
    pub fn dispatch_pending<F>(&self, mut handler: F) -> usize
    where
        F: FnMut(Request) -> Response,
    {
        let mut handled = 0;
        loop {
            match self.receiver.try_recv() {
                Ok(command) => {
                    let response = handler(command.request);
                    let _ = command.reply.send(response);
                    handled += 1;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        handled
    }
}

/// Owns the IPC worker lifecycle. Dropping it requests shutdown and joins the
/// worker, ensuring the socket is removed before the owner finishes shutdown.
pub struct IpcRuntime {
    shutdown: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<Result<(), ServerError>>>>,
}

impl IpcRuntime {
    /// Starts the IPC worker at the standard XDG_RUNTIME_DIR socket path.
    pub fn start_default() -> Result<(Self, CoreCommandReceiver), IpcRuntimeError> {
        let server = UnixIpcServer::bind_default()?;
        Self::start_server(server)
    }

    /// Starts the IPC worker at an explicit socket path. The parent directory
    /// must already exist, be owned by the current user and have private
    /// permissions. This is primarily useful for tests and embedding.
    pub fn start_at(
        socket_path: impl AsRef<Path>,
    ) -> Result<(Self, CoreCommandReceiver), IpcRuntimeError> {
        let server = UnixIpcServer::bind(socket_path)?;
        Self::start_server(server)
    }

    fn start_server(
        server: UnixIpcServer,
    ) -> Result<(Self, CoreCommandReceiver), IpcRuntimeError> {
        let (sender, receiver) = mpsc::sync_channel::<CoreCommand>(COMMAND_QUEUE_CAPACITY);
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);

        let worker = thread::Builder::new()
            .name("bubblewm-ipc".into())
            .spawn(move || {
                server.serve_until(&worker_shutdown, move |request| {
                    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
                    let command = CoreCommand {
                        request,
                        reply: reply_tx,
                    };

                    match sender.try_send(command) {
                        Ok(()) => {}
                        Err(TrySendError::Full(_)) => {
                            return unavailable("compositor command queue is full");
                        }
                        Err(TrySendError::Disconnected(_)) => {
                            return unavailable("compositor command receiver is unavailable");
                        }
                    }

                    match reply_rx.recv_timeout(RESPONSE_TIMEOUT) {
                        Ok(response) => response,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            unavailable("compositor did not process the request in time")
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            unavailable("compositor dropped the command response")
                        }
                    }
                })
            })
            .map_err(IpcRuntimeError::ThreadSpawn)?;

        Ok((
            Self {
                shutdown,
                worker: Mutex::new(Some(worker)),
            },
            CoreCommandReceiver { receiver },
        ))
    }

    /// Requests worker shutdown and joins it. Safe to call more than once.
    pub fn shutdown(&self) -> Result<(), IpcRuntimeError> {
        self.shutdown.store(true, Ordering::Release);
        let worker = self.worker.lock().map_err(|_| IpcRuntimeError::WorkerPanicked)?.take();
        if let Some(worker) = worker {
            match worker.join() {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(IpcRuntimeError::Server(error)),
                Err(_) => Err(IpcRuntimeError::WorkerPanicked),
            }
        } else {
            Ok(())
        }
    }
}

impl Drop for IpcRuntime {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Ok(worker) = self.worker.get_mut() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
}

fn unavailable(message: &str) -> Response {
    Response::Failure {
        error: ErrorResponse {
            code: ErrorCode::Unavailable,
            message: message.to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bubble_ipc::{
        PROTOCOL_VERSION, RequestEnvelope, ResponseData, ResponseEnvelope, read_frame, write_frame,
    };
    use std::{
        io::BufReader,
        os::unix::fs::DirBuilderExt,
        os::unix::net::UnixStream,
        time::Instant,
    };

    fn private_socket_path(base: &Path) -> std::path::PathBuf {
        let directory = base.join("private");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .expect("create private directory");
        directory.join("control.sock")
    }

    #[test]
    fn compositor_thread_dispatches_requests_and_worker_shuts_down_cleanly() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let socket_path = private_socket_path(directory.path());
        let (runtime, commands) = IpcRuntime::start_at(&socket_path).expect("start IPC runtime");

        let client_path = socket_path.clone();
        let client = thread::spawn(move || {
            let mut stream = UnixStream::connect(client_path).expect("connect IPC client");
            let request = RequestEnvelope {
                protocol_version: PROTOCOL_VERSION,
                request_id: 91,
                request: Request::GetCapabilities,
            };
            write_frame(&mut stream, &request).expect("send IPC request");
            let response: ResponseEnvelope =
                read_frame(&mut BufReader::new(stream)).expect("read IPC response");
            assert_eq!(response.request_id, request.request_id);
            response.response
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        while commands.dispatch_pending(|request| match request {
            Request::GetCapabilities => Response::Success {
                data: ResponseData::Capabilities {
                    workspace_control: false,
                },
            },
            _ => unavailable("unexpected test command"),
        }) == 0
        {
            assert!(Instant::now() < deadline, "IPC request was not dispatched");
            thread::sleep(Duration::from_millis(2));
        }

        assert_eq!(
            client.join().expect("join IPC client"),
            Response::Success {
                data: ResponseData::Capabilities {
                    workspace_control: false
                }
            }
        );

        runtime.shutdown().expect("stop IPC runtime");
        assert!(!socket_path.exists());
    }

    #[test]
    fn returns_unavailable_when_compositor_receiver_is_gone() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let socket_path = private_socket_path(directory.path());
        let (runtime, commands) = IpcRuntime::start_at(&socket_path).expect("start IPC runtime");
        drop(commands);

        let mut stream = UnixStream::connect(&socket_path).expect("connect IPC client");
        let request = RequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: 12,
            request: Request::GetWorkspaceState,
        };
        write_frame(&mut stream, &request).expect("send IPC request");
        let response: ResponseEnvelope =
            read_frame(&mut BufReader::new(stream)).expect("read IPC response");
        assert_eq!(response.request_id, 12);
        assert_eq!(
            response.response,
            unavailable("compositor command receiver is unavailable")
        );

        runtime.shutdown().expect("stop IPC runtime");
    }

    #[test]
    fn dropping_runtime_removes_socket() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let socket_path = private_socket_path(directory.path());
        let (runtime, _commands) = IpcRuntime::start_at(&socket_path).expect("start IPC runtime");
        assert!(socket_path.exists());
        drop(runtime);
        assert!(!socket_path.exists());
    }
}
