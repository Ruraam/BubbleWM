//! Explicit lifecycle model for a supervised BubbleWM session.
//!
//! This module contains no process, D-Bus, PAM or I/O operations. It is the
//! small state contract shared by the future session supervisor and the
//! session policy layer.

use std::fmt;

/// Observable phase of one supervised session attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Starting,
    Handshaking,
    Validating,
    Ready,
    Stopping,
    Exited,
    Failed,
}

impl SessionPhase {
    /// Whether this phase represents a completed session attempt.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Exited | Self::Failed)
    }

    const fn allows(self, next: Self) -> bool {
        use SessionPhase::*;

        matches!(
            (self, next),
            (Starting, Handshaking | Stopping | Failed)
                | (Handshaking, Validating | Stopping | Failed)
                | (Validating, Ready | Stopping | Failed)
                | (Ready, Stopping | Failed)
                | (Stopping, Exited | Failed)
        )
    }
}

/// Error returned when a lifecycle transition is not permitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidSessionTransition {
    pub from: SessionPhase,
    pub to: SessionPhase,
}

impl fmt::Display for InvalidSessionTransition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid session lifecycle transition: {:?} -> {:?}",
            self.from, self.to
        )
    }
}

impl std::error::Error for InvalidSessionTransition {}

/// Tracks the legal state transitions for one session attempt.
///
/// A new value must be created for every attempt. Terminal states cannot be
/// restarted or reused; a subsequent login starts with a fresh lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionLifecycle {
    phase: SessionPhase,
}

impl Default for SessionLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionLifecycle {
    /// Creates a lifecycle in the initial Starting phase.
    pub const fn new() -> Self {
        Self {
            phase: SessionPhase::Starting,
        }
    }

    /// Returns the current lifecycle phase.
    pub const fn phase(&self) -> SessionPhase {
        self.phase
    }

    /// Advances to the next phase if the transition is legal.
    pub fn transition(&mut self, next: SessionPhase) -> Result<(), InvalidSessionTransition> {
        if !self.phase.allows(next) {
            return Err(InvalidSessionTransition {
                from: self.phase,
                to: next,
            });
        }

        self.phase = next;
        Ok(())
    }

    /// Marks the attempt as failed from any non-terminal phase.
    pub fn fail(&mut self) -> Result<(), InvalidSessionTransition> {
        self.transition(SessionPhase::Failed)
    }

    /// Begins orderly shutdown from any active phase.
    pub fn begin_stopping(&mut self) -> Result<(), InvalidSessionTransition> {
        self.transition(SessionPhase::Stopping)
    }

    /// Returns whether this attempt has reached Exited or Failed.
    pub const fn is_terminal(&self) -> bool {
        self.phase.is_terminal()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_complete_startup_and_shutdown_path() {
        let mut lifecycle = SessionLifecycle::new();

        assert_eq!(lifecycle.phase(), SessionPhase::Starting);
        lifecycle.transition(SessionPhase::Handshaking).unwrap();
        lifecycle.transition(SessionPhase::Validating).unwrap();
        lifecycle.transition(SessionPhase::Ready).unwrap();
        lifecycle.begin_stopping().unwrap();
        lifecycle.transition(SessionPhase::Exited).unwrap();

        assert!(lifecycle.is_terminal());
    }

    #[test]
    fn permits_shutdown_before_the_session_becomes_ready() {
        for phase in [
            SessionPhase::Starting,
            SessionPhase::Handshaking,
            SessionPhase::Validating,
            SessionPhase::Ready,
        ] {
            let mut lifecycle = SessionLifecycle::new();
            for next in [
                SessionPhase::Handshaking,
                SessionPhase::Validating,
                SessionPhase::Ready,
            ] {
                if lifecycle.phase() == phase {
                    break;
                }
                lifecycle.transition(next).unwrap();
            }

            lifecycle.begin_stopping().unwrap();
            lifecycle.transition(SessionPhase::Exited).unwrap();
            assert!(lifecycle.is_terminal());
        }
    }

    #[test]
    fn failure_is_terminal_from_every_active_phase() {
        let paths = [
            vec![],
            vec![SessionPhase::Handshaking],
            vec![SessionPhase::Handshaking, SessionPhase::Validating],
            vec![
                SessionPhase::Handshaking,
                SessionPhase::Validating,
                SessionPhase::Ready,
            ],
            vec![SessionPhase::Stopping],
        ];

        for path in paths {
            let mut lifecycle = SessionLifecycle::new();
            for phase in path {
                lifecycle.transition(phase).unwrap();
            }

            lifecycle.fail().unwrap();
            assert_eq!(lifecycle.phase(), SessionPhase::Failed);
            assert!(lifecycle.is_terminal());
        }
    }

    #[test]
    fn rejects_skipped_or_reversed_startup_phases() {
        let mut lifecycle = SessionLifecycle::new();

        assert_eq!(
            lifecycle.transition(SessionPhase::Ready),
            Err(InvalidSessionTransition {
                from: SessionPhase::Starting,
                to: SessionPhase::Ready,
            })
        );
        assert_eq!(lifecycle.phase(), SessionPhase::Starting);
    }

    #[test]
    fn terminal_states_cannot_be_reused() {
        let mut exited = SessionLifecycle::new();
        exited.begin_stopping().unwrap();
        exited.transition(SessionPhase::Exited).unwrap();
        assert!(exited.transition(SessionPhase::Starting).is_err());
        assert!(exited.fail().is_err());

        let mut failed = SessionLifecycle::new();
        failed.fail().unwrap();
        assert!(failed.begin_stopping().is_err());
        assert!(failed.transition(SessionPhase::Handshaking).is_err());
    }
}
