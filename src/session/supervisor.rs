//! Nonblocking owner for one supervised session-worker attempt.
//!
//! This first supervisor layer deliberately does not perform authentication,
//! logind discovery or session actions. It owns the worker process and private
//! channel, tracks the lifecycle, and enforces the startup deadline. Protocol
//! exchange and independent logind validation are added as separate stages.

use super::{
    control::{WorkerProcess},
    lifecycle::{SessionLifecycle, SessionPhase},
};
use std::{
    io,
    os::unix::net::UnixStream,
    process::ExitStatus,
    time::{Duration, Instant},
};

/// Result of a nonblocking startup health check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupStatus {
    /// The worker is still running and the handshake deadline has not expired.
    Pending,
    /// The worker exited before the startup handshake completed.
    WorkerExited(ExitStatus),
}

/// Owns all resources associated with one worker startup attempt.
#[derive(Debug)]
pub(crate) struct SessionSupervisor {
    lifecycle: SessionLifecycle,
    worker: Option<WorkerProcess>,
    control: UnixStream,
    handshake_deadline: Instant,
}

impl SessionSupervisor {
    /// Takes ownership of the worker and its supervisor-side control socket.
    ///
    /// The timeout is converted to an absolute deadline once, so repeated
    /// health checks cannot accidentally extend the startup window.
    pub(crate) fn new(
        worker: WorkerProcess,
        control: UnixStream,
        handshake_timeout: Duration,
    ) -> io::Result<Self> {
        let handshake_deadline = Instant::now()
            .checked_add(handshake_timeout)
            .ok_or_else(|| invalid_input("session handshake timeout is too large"))?;

        let mut lifecycle = SessionLifecycle::new();
        lifecycle
            .transition(SessionPhase::Handshaking)
            .map_err(|_| io::Error::other("invalid initial session lifecycle"))?;

        Ok(Self {
            lifecycle,
            worker: Some(worker),
            control,
            handshake_deadline,
        })
    }

    /// Returns the current lifecycle phase.
    pub(crate) const fn phase(&self) -> SessionPhase {
        self.lifecycle.phase()
    }

    /// Returns the worker's private control stream for the protocol layer.
    ///
    /// The caller must use bounded protocol operations and must not expose
    /// this stream to compositor clients.
    pub(crate) fn control_stream(&mut self) -> &mut UnixStream {
        &mut self.control
    }

    /// Checks worker liveness and the fixed startup deadline without blocking.
    pub(crate) fn check_startup(&mut self) -> io::Result<StartupStatus> {
        if self.lifecycle.phase() != SessionPhase::Handshaking {
            return Err(invalid_input("session is not in the handshaking phase"));
        }

        if let Some(worker) = self.worker.as_mut() {
            if let Some(status) = worker.try_wait()? {
                self.worker = None;
                self.lifecycle
                    .fail()
                    .map_err(|_| io::Error::other("failed to terminate session lifecycle"))?;
                return Ok(StartupStatus::WorkerExited(status));
            }
        } else {
            return Err(invalid_input("session worker is no longer available"));
        }

        if Instant::now() >= self.handshake_deadline {
            self.lifecycle
                .fail()
                .map_err(|_| io::Error::other("failed to terminate session lifecycle"))?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "session worker handshake deadline expired",
            ));
        }

        Ok(StartupStatus::Pending)
    }

    /// Begins orderly shutdown and reaps the worker within the supplied grace.
    pub(crate) fn stop(&mut self, grace: Duration) -> io::Result<Option<ExitStatus>> {
        if self.lifecycle.is_terminal() {
            return Err(invalid_input("session lifecycle is already terminal"));
        }

        self.lifecycle
            .begin_stopping()
            .map_err(|_| io::Error::other("invalid session shutdown transition"))?;

        let status = match self.worker.as_mut() {
            Some(worker) => {
                let status = worker.terminate_and_reap(grace)?;
                self.worker = None;
                Some(status)
            }
            None => None,
        };

        self.lifecycle
            .transition(SessionPhase::Exited)
            .map_err(|_| io::Error::other("invalid completed session shutdown"))?;
        Ok(status)
    }

    /// Marks the attempt failed. Owned resources are still reaped on drop.
    pub(crate) fn fail(&mut self) -> io::Result<()> {
        self.lifecycle
            .fail()
            .map_err(|_| invalid_input("session lifecycle is already terminal"))
    }
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::control::{private_channel_pair, spawn_worker_with_channel};
    use std::{ffi::{OsStr, OsString}, thread};

    fn spawn(command: &str) -> (WorkerProcess, UnixStream) {
        let (supervisor, worker_channel) = private_channel_pair().unwrap();
        let worker = spawn_worker_with_channel(
            OsStr::new("/bin/sh"),
            &[OsString::from("-c"), OsString::from(command)],
            worker_channel,
        )
        .unwrap();
        (worker, supervisor)
    }

    #[test]
    fn supervisor_owns_worker_and_starts_handshaking() {
        let (worker, control) = spawn("sleep 30");
        let supervisor =
            SessionSupervisor::new(worker, control, Duration::from_secs(2)).unwrap();
        assert_eq!(supervisor.phase(), SessionPhase::Handshaking);
    }

    #[test]
    fn startup_health_check_is_nonblocking_while_worker_runs() {
        let (worker, control) = spawn("sleep 30");
        let mut supervisor =
            SessionSupervisor::new(worker, control, Duration::from_secs(2)).unwrap();

        assert_eq!(supervisor.check_startup().unwrap(), StartupStatus::Pending);
        assert_eq!(supervisor.phase(), SessionPhase::Handshaking);
        supervisor.stop(Duration::from_millis(100)).unwrap();
        assert_eq!(supervisor.phase(), SessionPhase::Exited);
    }

    #[test]
    fn early_worker_exit_fails_the_startup_attempt() {
        let (worker, control) = spawn("exit 7");
        let mut supervisor =
            SessionSupervisor::new(worker, control, Duration::from_secs(2)).unwrap();

        for _ in 0..100 {
            match supervisor.check_startup().unwrap() {
                StartupStatus::Pending => thread::sleep(Duration::from_millis(2)),
                StartupStatus::WorkerExited(status) => {
                    assert_eq!(status.code(), Some(7));
                    assert_eq!(supervisor.phase(), SessionPhase::Failed);
                    return;
                }
            }
        }
        panic!("worker did not exit within the test bound");
    }

    #[test]
    fn handshake_deadline_is_absolute_and_terminal_on_expiry() {
        let (worker, control) = spawn("sleep 30");
        let mut supervisor =
            SessionSupervisor::new(worker, control, Duration::ZERO).unwrap();

        let error = supervisor.check_startup().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(supervisor.phase(), SessionPhase::Failed);
    }

    #[test]
    fn rejects_unrepresentable_handshake_deadline() {
        let (worker, control) = spawn("sleep 30");
        let error = SessionSupervisor::new(worker, control, Duration::MAX).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
