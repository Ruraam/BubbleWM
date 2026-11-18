//! Testable orchestration for one privileged BubbleWM login session.
//!
//! This layer owns the ordering between worker metadata, trusted logind
//! identity, explicit approval, worker/PAM completion and final scope cleanup.
//! OS-facing operations are injected so the complete state machine can be
//! tested without PAM, root privileges or a running systemd instance.

use std::{io, process::ExitStatus, time::Duration};

const WORKER_EXIT_TIMEOUT: Duration = Duration::from_secs(30);
const WORKER_TERMINATION_GRACE: Duration = Duration::from_secs(1);

use crate::{
    logind::ResolvedLogindSession,
    session_control::{SessionReady, SessionWorker, validate_session_ready_against},
};

/// The narrow worker interface required by the session supervisor.
pub trait SessionWorkerLifecycle {
    fn id(&self) -> libc::pid_t;
    fn receive_session_ready(&mut self, timeout: Duration) -> io::Result<SessionReady>;
    fn approve_session(&mut self) -> io::Result<()>;
    fn wait(&mut self) -> io::Result<ExitStatus>;

    fn wait_with_timeout(
        &mut self,
        timeout: Duration,
        termination_grace: Duration,
    ) -> io::Result<(ExitStatus, bool)> {
        let _ = (timeout, termination_grace);
        self.wait().map(|status| (status, false))
    }
}

impl SessionWorkerLifecycle for SessionWorker {
    fn id(&self) -> libc::pid_t {
        SessionWorker::id(self)
    }

    fn receive_session_ready(&mut self, timeout: Duration) -> io::Result<SessionReady> {
        SessionWorker::receive_session_ready(self, timeout)
    }

    fn approve_session(&mut self) -> io::Result<()> {
        SessionWorker::approve_session(self)
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        SessionWorker::wait(self)
    }

    fn wait_with_timeout(
        &mut self,
        timeout: Duration,
        termination_grace: Duration,
    ) -> io::Result<(ExitStatus, bool)> {
        SessionWorker::wait_with_timeout(self, timeout, termination_grace)
    }
}

/// Runs the privileged half of the worker/logind session lifecycle.
///
/// The worker's metadata is treated as an untrusted claim. The resolver must
/// obtain identity independently from logind using the worker PID. Approval
/// is sent only after an exact identity match. The retained logind scope is
/// stopped only after the worker exits and has completed PAM cleanup.
///
/// On every early return the caller still owns the worker; dropping the real
/// `SessionWorker` closes its private channel and performs bounded cleanup.
pub fn supervise_session<W, Resolve, Stop>(
    worker: &mut W,
    metadata_timeout: Duration,
    scope_stop_timeout: Duration,
    resolve_session: Resolve,
    stop_scope: Stop,
) -> Result<i32, String>
where
    W: SessionWorkerLifecycle,
    Resolve: FnOnce(u32) -> io::Result<ResolvedLogindSession>,
    Stop: FnOnce(&str, Duration) -> io::Result<()>,
{
    let ready = worker
        .receive_session_ready(metadata_timeout)
        .map_err(|error| format!("le worker n'a pas fourni de métadonnées valides: {error}"))?;

    let worker_pid = u32::try_from(worker.id())
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| "identifiant de processus worker invalide".to_owned())?;

    // The resolver must inspect the worker PID, never the supervisor PID.
    let trusted = resolve_session(worker_pid)
        .map_err(|error| format!("impossible de vérifier la session logind du worker: {error}"))?;

    validate_session_ready_against(&ready, &trusted.identity)
        .map_err(|error| format!("identité de session refusée: {error}"))?;

    worker
        .approve_session()
        .map_err(|error| format!("impossible d'autoriser le démarrage de la session: {error}"))?;

    let (status, worker_timed_out) = worker
        .wait_with_timeout(WORKER_EXIT_TIMEOUT, WORKER_TERMINATION_GRACE)
        .map_err(|error| format!("impossible d'attendre le nettoyage PAM du worker: {error}"))?;

    // The worker has now exited. Stop the trusted scope from outside it.
    stop_scope(&trusted.scope, scope_stop_timeout).map_err(|error| {
        format!(
            "impossible de terminer le scope logind {}: {error}",
            trusted.scope
        )
    })?;

    if worker_timed_out {
        return Err(format!(
            "le worker n'a pas terminé son nettoyage PAM dans le délai de {} secondes; arrêt forcé effectué",
            WORKER_EXIT_TIMEOUT.as_secs()
        ));
    }

    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_control::SessionIdentity;
    use std::{cell::RefCell, io, os::unix::process::ExitStatusExt, rc::Rc};

    struct MockWorker {
        events: Rc<RefCell<Vec<&'static str>>>,
        ready: io::Result<SessionReady>,
        approval: io::Result<()>,
        exit_status: io::Result<ExitStatus>,
        wait_timed_out: bool,
        pid: libc::pid_t,
    }

    impl SessionWorkerLifecycle for MockWorker {
        fn id(&self) -> libc::pid_t {
            self.pid
        }

        fn receive_session_ready(&mut self, timeout: Duration) -> io::Result<SessionReady> {
            assert_eq!(timeout, Duration::from_secs(10));
            self.events.borrow_mut().push("metadata");
            std::mem::replace(
                &mut self.ready,
                Err(io::Error::other("metadata already consumed")),
            )
        }

        fn approve_session(&mut self) -> io::Result<()> {
            self.events.borrow_mut().push("approve");
            std::mem::replace(
                &mut self.approval,
                Err(io::Error::other("approval already consumed")),
            )
        }

        fn wait(&mut self) -> io::Result<ExitStatus> {
            self.events.borrow_mut().push("wait");
            std::mem::replace(
                &mut self.exit_status,
                Err(io::Error::other("worker already waited")),
            )
        }

        fn wait_with_timeout(
            &mut self,
            timeout: Duration,
            termination_grace: Duration,
        ) -> io::Result<(ExitStatus, bool)> {
            assert_eq!(timeout, WORKER_EXIT_TIMEOUT);
            assert_eq!(termination_grace, WORKER_TERMINATION_GRACE);
            self.events.borrow_mut().push("wait");
            std::mem::replace(
                &mut self.exit_status,
                Err(io::Error::other("worker already waited")),
            )
            .map(|status| (status, self.wait_timed_out))
        }
    }

    fn sample_ready() -> SessionReady {
        SessionReady {
            uid: 1000,
            session_id: "c7".into(),
            runtime_dir: "/run/user/1000".into(),
            seat: Some("seat0".into()),
            vt_number: Some(2),
        }
    }

    fn trusted(ready: &SessionReady) -> ResolvedLogindSession {
        ResolvedLogindSession {
            identity: SessionIdentity {
                uid: ready.uid,
                session_id: ready.session_id.clone(),
                runtime_dir: ready.runtime_dir.clone(),
                seat: ready.seat.clone(),
                vt_number: ready.vt_number,
            },
            scope: format!("session-{}.scope", ready.session_id),
        }
    }

    fn mock_worker(events: Rc<RefCell<Vec<&'static str>>>) -> MockWorker {
        MockWorker {
            events,
            ready: Ok(sample_ready()),
            approval: Ok(()),
            exit_status: Ok(ExitStatus::from_raw(0)),
            wait_timed_out: false,
            pid: 4242,
        }
    }

    #[test]
    fn successful_lifecycle_resolves_approves_waits_then_stops_scope() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut worker = mock_worker(events.clone());
        let expected = trusted(&sample_ready());

        let result = supervise_session(
            &mut worker,
            Duration::from_secs(10),
            Duration::from_secs(15),
            |pid| {
                assert_eq!(pid, 4242);
                events.borrow_mut().push("resolve");
                Ok(expected)
            },
            |scope, timeout| {
                assert_eq!(scope, "session-c7.scope");
                assert_eq!(timeout, Duration::from_secs(15));
                events.borrow_mut().push("stop_scope");
                Ok(())
            },
        );

        assert_eq!(result.unwrap(), 0);
        assert_eq!(
            *events.borrow(),
            ["metadata", "resolve", "approve", "wait", "stop_scope"]
        );
    }

    #[test]
    fn metadata_failure_never_resolves_or_approves_a_session() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut worker = mock_worker(events.clone());
        worker.ready = Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "worker exited",
        ));

        let result = supervise_session(
            &mut worker,
            Duration::from_secs(10),
            Duration::from_secs(15),
            |_| {
                events.borrow_mut().push("resolve");
                Ok(trusted(&sample_ready()))
            },
            |_, _| {
                events.borrow_mut().push("stop_scope");
                Ok(())
            },
        );

        assert!(result.unwrap_err().contains("métadonnées"));
        assert_eq!(*events.borrow(), ["metadata"]);
    }

    #[test]
    fn logind_lookup_failure_never_approves_or_stops_an_untrusted_scope() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut worker = mock_worker(events.clone());

        let result = supervise_session(
            &mut worker,
            Duration::from_secs(10),
            Duration::from_secs(15),
            |pid| {
                assert_eq!(pid, 4242);
                events.borrow_mut().push("resolve");
                Err(io::Error::new(io::ErrorKind::NotFound, "no logind session"))
            },
            |_, _| {
                events.borrow_mut().push("stop_scope");
                Ok(())
            },
        );

        assert!(result.unwrap_err().contains("logind"));
        assert_eq!(*events.borrow(), ["metadata", "resolve"]);
    }

    #[test]
    fn mismatched_worker_identity_is_rejected_before_approval() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut worker = mock_worker(events.clone());
        let mut identity = trusted(&sample_ready());
        identity.identity.uid = 1001;

        let result = supervise_session(
            &mut worker,
            Duration::from_secs(10),
            Duration::from_secs(15),
            |_| {
                events.borrow_mut().push("resolve");
                Ok(identity)
            },
            |_, _| {
                events.borrow_mut().push("stop_scope");
                Ok(())
            },
        );

        assert!(result.unwrap_err().contains("identité"));
        assert_eq!(*events.borrow(), ["metadata", "resolve"]);
    }

    #[test]
    fn approval_failure_does_not_wait_or_stop_scope() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut worker = mock_worker(events.clone());
        worker.approval = Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "supervisor channel closed",
        ));

        let result = supervise_session(
            &mut worker,
            Duration::from_secs(10),
            Duration::from_secs(15),
            |_| {
                events.borrow_mut().push("resolve");
                Ok(trusted(&sample_ready()))
            },
            |_, _| {
                events.borrow_mut().push("stop_scope");
                Ok(())
            },
        );

        assert!(result.unwrap_err().contains("autoriser"));
        assert_eq!(*events.borrow(), ["metadata", "resolve", "approve"]);
    }

    #[test]
    fn nonzero_worker_exit_still_stops_the_trusted_scope() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut worker = mock_worker(events.clone());
        worker.exit_status = Ok(ExitStatus::from_raw(7 << 8));

        let result = supervise_session(
            &mut worker,
            Duration::from_secs(10),
            Duration::from_secs(15),
            |_| {
                events.borrow_mut().push("resolve");
                Ok(trusted(&sample_ready()))
            },
            |scope, _| {
                assert_eq!(scope, "session-c7.scope");
                events.borrow_mut().push("stop_scope");
                Ok(())
            },
        );

        assert_eq!(result.unwrap(), 7);
        assert_eq!(
            *events.borrow(),
            ["metadata", "resolve", "approve", "wait", "stop_scope"]
        );
    }

    #[test]
    fn scope_stop_failure_is_reported_after_worker_and_pam_cleanup() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut worker = mock_worker(events.clone());

        let result = supervise_session(
            &mut worker,
            Duration::from_secs(10),
            Duration::from_secs(15),
            |_| {
                events.borrow_mut().push("resolve");
                Ok(trusted(&sample_ready()))
            },
            |_, _| {
                events.borrow_mut().push("stop_scope");
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "scope remained active",
                ))
            },
        );

        assert!(result.unwrap_err().contains("scope logind"));
        assert_eq!(
            *events.borrow(),
            ["metadata", "resolve", "approve", "wait", "stop_scope"]
        );
    }

    #[test]
    fn invalid_worker_pid_fails_before_logind_lookup() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut worker = mock_worker(events.clone());
        worker.pid = 0;

        let result = supervise_session(
            &mut worker,
            Duration::from_secs(10),
            Duration::from_secs(15),
            |_| {
                events.borrow_mut().push("resolve");
                Ok(trusted(&sample_ready()))
            },
            |_, _| {
                events.borrow_mut().push("stop_scope");
                Ok(())
            },
        );

        assert!(result.unwrap_err().contains("processus worker"));
        assert_eq!(*events.borrow(), ["metadata"]);
    }

    #[test]
    fn worker_wait_failure_never_stops_the_logind_scope() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut worker = mock_worker(events.clone());
        worker.exit_status = Err(io::Error::new(io::ErrorKind::Other, "waitpid failed"));

        let result = supervise_session(
            &mut worker,
            Duration::from_secs(10),
            Duration::from_secs(15),
            |_| {
                events.borrow_mut().push("resolve");
                Ok(trusted(&sample_ready()))
            },
            |_, _| {
                events.borrow_mut().push("stop_scope");
                Ok(())
            },
        );

        assert!(result.unwrap_err().contains("nettoyage PAM"));
        assert_eq!(*events.borrow(), ["metadata", "resolve", "approve", "wait"]);
    }

    #[test]
    fn signaled_worker_exit_maps_to_failure_status_after_scope_cleanup() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut worker = mock_worker(events.clone());
        worker.exit_status = Ok(ExitStatus::from_raw(libc::SIGTERM));

        let result = supervise_session(
            &mut worker,
            Duration::from_secs(10),
            Duration::from_secs(15),
            |_| {
                events.borrow_mut().push("resolve");
                Ok(trusted(&sample_ready()))
            },
            |scope, _| {
                assert_eq!(scope, "session-c7.scope");
                events.borrow_mut().push("stop_scope");
                Ok(())
            },
        );

        assert_eq!(result.unwrap(), 1);
        assert_eq!(
            *events.borrow(),
            ["metadata", "resolve", "approve", "wait", "stop_scope"]
        );
    }

    #[test]
    fn worker_timeout_still_stops_scope_before_reporting_forced_termination() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut worker = mock_worker(events.clone());
        worker.wait_timed_out = true;

        let result = supervise_session(
            &mut worker,
            Duration::from_secs(10),
            Duration::from_secs(15),
            |_| {
                events.borrow_mut().push("resolve");
                Ok(trusted(&sample_ready()))
            },
            |scope, _| {
                assert_eq!(scope, "session-c7.scope");
                events.borrow_mut().push("stop_scope");
                Ok(())
            },
        );

        assert!(result.unwrap_err().contains("nettoyage PAM"));
        assert_eq!(
            *events.borrow(),
            ["metadata", "resolve", "approve", "wait", "stop_scope"]
        );
    }

    #[test]
    fn negative_worker_pid_fails_before_logind_lookup() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut worker = mock_worker(events.clone());
        worker.pid = -1;

        let result = supervise_session(
            &mut worker,
            Duration::from_secs(10),
            Duration::from_secs(15),
            |_| {
                events.borrow_mut().push("resolve");
                Ok(trusted(&sample_ready()))
            },
            |_, _| {
                events.borrow_mut().push("stop_scope");
                Ok(())
            },
        );

        assert!(result.unwrap_err().contains("processus worker"));
        assert_eq!(*events.borrow(), ["metadata"]);
    }
}
