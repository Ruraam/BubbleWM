//! Asynchronous BubbleColor generation service.
//!
//! Requests are decoded and analyzed on a dedicated worker thread. The
//! compositor can submit work without blocking its event loop and poll for a
//! completed immutable ThemeDocument snapshot.

use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread::{self, JoinHandle},
};

use crate::theme::{ThemeDocument, ThemeOptions, generate_from_path};

/// A completed palette generation, tagged so callers can discard stale work.
#[derive(Debug)]
pub struct ThemeUpdate {
    pub generation: u64,
    pub result: Result<ThemeDocument, String>,
}

struct ThemeRequest {
    generation: u64,
    path: PathBuf,
    options: ThemeOptions,
}

/// Non-blocking request submission and result polling for BubbleColor.
///
/// The request queue is deliberately bounded to one item. The worker coalesces
/// queued requests and processes only the newest one, preventing rapid
/// wallpaper changes from creating an unbounded backlog.
pub struct ThemeRuntime {
    requests: Option<SyncSender<ThemeRequest>>,
    updates: Receiver<ThemeUpdate>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeRequestError {
    Busy,
    Stopped,
}

impl std::fmt::Display for ThemeRequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => formatter.write_str("BubbleColor worker queue is full"),
            Self::Stopped => formatter.write_str("BubbleColor worker has stopped"),
        }
    }
}

impl std::error::Error for ThemeRequestError {}

impl ThemeRuntime {
    /// Starts the dedicated BubbleColor worker.
    pub fn start() -> std::io::Result<Self> {
        let (request_tx, request_rx) = mpsc::sync_channel::<ThemeRequest>(1);
        let (update_tx, update_rx) = mpsc::sync_channel::<ThemeUpdate>(1);
        let worker = thread::Builder::new()
            .name("bubblecolor-worker".into())
            .spawn(move || {
                while let Ok(mut request) = request_rx.recv() {
                    // Keep only the newest queued wallpaper/options request.
                    while let Ok(newer) = request_rx.try_recv() {
                        request = newer;
                    }

                    let result = generate_from_path(Path::new(&request.path), &request.options)
                        .map_err(|error| format!("{error:#}"));
                    let update = ThemeUpdate {
                        generation: request.generation,
                        result,
                    };

                    // Never block the worker if the compositor has not polled
                    // the previous result. A newer result supersedes it.
                    match update_tx.try_send(update) {
                        Ok(()) | Err(TrySendError::Disconnected(_)) => {}
                        Err(TrySendError::Full(_)) => {}
                    }
                }
            })?;

        Ok(Self {
            requests: Some(request_tx),
            updates: update_rx,
            worker: Some(worker),
        })
    }

    /// Queues a wallpaper for analysis without waiting for decoding or sampling.
    pub fn request(
        &self,
        generation: u64,
        path: impl AsRef<Path>,
        options: ThemeOptions,
    ) -> Result<(), ThemeRequestError> {
        let Some(requests) = self.requests.as_ref() else {
            return Err(ThemeRequestError::Stopped);
        };
        let request = ThemeRequest {
            generation,
            path: path.as_ref().to_path_buf(),
            options,
        };
        match requests.try_send(request) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(ThemeRequestError::Busy),
            Err(TrySendError::Disconnected(_)) => Err(ThemeRequestError::Stopped),
        }
    }

    /// Returns a completed palette if one is ready; never waits.
    pub fn try_take_update(&self) -> Option<ThemeUpdate> {
        match self.updates.try_recv() {
            Ok(update) => Some(update),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    /// Stops the worker and waits for any in-flight bounded image decode to finish.
    pub fn shutdown(mut self) -> std::thread::Result<()> {
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            worker.join()
        } else {
            Ok(())
        }
    }
}

impl Drop for ThemeRuntime {
    fn drop(&mut self) {
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn invalid_wallpaper_returns_a_non_blocking_error_update() {
        let runtime = ThemeRuntime::start().expect("worker should start");
        runtime
            .request(7, "/definitely/not/a/bubblewm-wallpaper.png", ThemeOptions::default())
            .expect("request should be queued");

        let deadline = Instant::now() + Duration::from_secs(2);
        let update = loop {
            if let Some(update) = runtime.try_take_update() {
                break update;
            }
            assert!(Instant::now() < deadline, "worker did not return an update");
            thread::yield_now();
        };

        assert_eq!(update.generation, 7);
        assert!(update.result.is_err());
        runtime.shutdown().expect("worker should stop cleanly");
    }
}
