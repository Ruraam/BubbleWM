//! Background worker for read-only systemd-logind discovery.
//!
//! All D-Bus operations run on a dedicated thread. The worker publishes one
//! result through a bounded channel; polling it never waits for D-Bus.

use std::{
    io,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use anyhow::{Context, Result};

use super::logind::{ActionAvailability, LogindAvailability, LogindProbe, LogindSession};

/// A read-only snapshot collected from systemd-logind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogindSnapshot {
    pub session: LogindSession,
    pub availability: LogindAvailability,
}

/// Owns the receiving side of a one-shot logind probe.
///
/// Drop this value to stop observing the result. The worker is detached and
/// exits after its single probe; an in-flight D-Bus call is never joined from
/// the compositor thread.
pub struct LogindWorker {
    receiver: Receiver<Result<LogindSnapshot>>,
    finished: bool,
}

impl LogindWorker {
    /// Starts logind discovery on a dedicated background thread.
    ///
    /// Thread creation is the only synchronous operation performed here. The
    /// actual system-bus connection, session validation and capability
    /// queries all happen on the worker thread.
    pub fn spawn() -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(1);

        thread::Builder::new()
            .name("bubblewm-logind".to_owned())
            .spawn(move || {
                let result = collect_snapshot();
                let _ = sender.send(result);
            })?;

        Ok(Self {
            receiver,
            finished: false,
        })
    }

    /// Returns immediately with the completed snapshot, or None while the
    /// worker is still running. A logind failure is returned as an error.
    ///
    /// This method uses try_recv and never waits for the D-Bus worker.
    pub fn try_result(&mut self) -> Result<Option<LogindSnapshot>> {
        if self.finished {
            return Ok(None);
        }

        match self.receiver.try_recv() {
            Ok(result) => {
                self.finished = true;
                result.map(Some)
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                self.finished = true;
                Err(anyhow::anyhow!(
                    "logind worker exited without publishing a result"
                ))
            }
        }
    }
}

fn collect_snapshot() -> Result<LogindSnapshot> {
    let probe = LogindProbe::connect().context("cannot initialize logind probe")?;
    let session = probe
        .current_session()
        .context("cannot validate the current logind session")?;
    let availability = probe
        .availability()
        .context("cannot query logind action availability")?;

    Ok(LogindSnapshot {
        session,
        availability,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn worker_with_result(result: Result<LogindSnapshot>) -> LogindWorker {
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.send(result).expect("test receiver is alive");
        LogindWorker {
            receiver,
            finished: false,
        }
    }

    fn snapshot() -> LogindSnapshot {
        LogindSnapshot {
            session: LogindSession {
                id: "c2".to_owned(),
                uid: 1000,
                session_type: "wayland".to_owned(),
                class: "user".to_owned(),
                active: true,
            },
            availability: LogindAvailability {
                suspend: ActionAvailability::Yes,
                reboot: ActionAvailability::No,
                power_off: ActionAvailability::Challenge,
            },
        }
    }

    #[test]
    fn returns_a_completed_snapshot_without_waiting() {
        let expected = snapshot();
        let mut worker = worker_with_result(Ok(expected.clone()));

        assert_eq!(worker.try_result().unwrap(), Some(expected));
        assert_eq!(worker.try_result().unwrap(), None);
    }

    #[test]
    fn forwards_probe_errors_to_the_caller() {
        let mut worker = worker_with_result(Err(anyhow::anyhow!("logind unavailable")));

        assert!(worker.try_result().is_err());
        assert_eq!(worker.try_result().unwrap(), None);
    }

    #[test]
    fn reports_a_disconnected_worker_without_a_result() {
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(sender);
        let mut worker = LogindWorker {
            receiver,
            finished: false,
        };

        assert!(worker.try_result().is_err());
        assert_eq!(worker.try_result().unwrap(), None);
    }

    #[test]
    fn reports_pending_when_no_result_is_available() {
        let (_sender, receiver) = mpsc::sync_channel(1);
        let mut worker = LogindWorker {
            receiver,
            finished: false,
        };

        assert_eq!(worker.try_result().unwrap(), None);
    }
}
