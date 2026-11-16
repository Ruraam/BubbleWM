//! Safe session-management policy and capability reporting.
//!
//! This layer never executes operating-system commands. A backend-specific
//! provider must be added before any session action can be advertised or run.
//! The nested Winit backend is intentionally read-only with respect to the
//! host session, so power and logout actions are always rejected.

pub mod lifecycle;
pub mod logind;
pub mod worker;

use bubble_ipc::{
    ErrorCode, ErrorResponse, Response, SessionAction, SessionCapabilities, SessionEnvironment,
    SessionState,
};

/// Transport-neutral description of the current user session.
///
/// This is a data contract shared by session discovery and session policy; it
/// does not itself establish trust. Values received from a session worker must
/// be treated as untrusted until the identity fields have been independently
/// checked against logind. In particular, a worker-provided UID or session ID
/// must never be accepted as proof of the caller's identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSnapshot {
    /// Effective UID that owns the session.
    pub uid: u32,
    /// Logind session identifier.
    pub session_id: String,
    /// Session type reported by logind (for example, `wayland`).
    pub session_type: String,
    /// Logind session class (for example, `user`).
    pub class: String,
    /// Whether logind currently considers the session active.
    pub active: bool,
    /// Runtime directory associated with the session.
    pub runtime_dir: String,
    /// Logind seat associated with the session, when available.
    pub seat: Option<String>,
    /// Virtual terminal number associated with the session, when available.
    pub vt_number: Option<u32>,
}

impl SessionSnapshot {
    /// Builds the normalized snapshot from a session already validated by logind.
    ///
    /// This conversion must only be used with a value returned by
    /// `LogindProbe::session` or `LogindProbe::current_session`. It does not
    /// independently authenticate arbitrary worker-provided metadata.
    pub(crate) fn from_validated_logind(session: logind::LogindSession) -> Self {
        Self {
            uid: session.uid,
            session_id: session.id,
            session_type: session.session_type,
            class: session.class,
            active: session.active,
            runtime_dir: session.runtime_dir,
            seat: session.seat,
            vt_number: session.vt_number,
        }
    }
}

/// Session policy exposed to the compositor and shell clients.
#[derive(Debug, Clone, Copy)]
pub struct SessionManager {
    environment: SessionEnvironment,
}

impl SessionManager {
    /// Creates the safe policy used by the nested development compositor.
    pub const fn nested_development() -> Self {
        Self {
            environment: SessionEnvironment::NestedDevelopment,
        }
    }

    /// Returns the current session environment.
    pub const fn environment(&self) -> SessionEnvironment {
        self.environment
    }

    /// Returns a stable status snapshot for IPC clients.
    pub const fn state(&self) -> SessionState {
        SessionState {
            environment: self.environment,
            action_in_progress: false,
        }
    }

    /// Reports only actions backed by a real, initialized provider.
    ///
    /// The logind probe is currently read-only and cannot authorize or execute
    /// actions, so no session action is advertised as available.
    pub const fn capabilities(&self) -> SessionCapabilities {
        SessionCapabilities {
            lock: false,
            logout: false,
            suspend: false,
            reboot: false,
            power_off: false,
        }
    }

    /// Rejects actions until a trusted native session provider is integrated.
    ///
    /// In particular, requests received by the nested Winit backend must never
    /// affect the host machine's power state or desktop session.
    pub fn request_action(&self, action: SessionAction) -> Response {
        let action_name = match action {
            SessionAction::Lock => "lock",
            SessionAction::Logout => "logout",
            SessionAction::Suspend => "suspend",
            SessionAction::Reboot => "reboot",
            SessionAction::PowerOff => "power off",
        };

        let message = match self.environment {
            SessionEnvironment::NestedDevelopment => format!(
                "session action '{action_name}' is disabled in the nested Winit development backend"
            ),
            SessionEnvironment::Native => {
                format!("session action '{action_name}' has no initialized native provider")
            }
        };

        Response::Failure {
            error: ErrorResponse {
                code: ErrorCode::Unavailable,
                message,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_development_reports_its_environment_without_active_actions() {
        let manager = SessionManager::nested_development();

        assert_eq!(manager.environment(), SessionEnvironment::NestedDevelopment);
        assert_eq!(
            manager.state(),
            SessionState {
                environment: SessionEnvironment::NestedDevelopment,
                action_in_progress: false,
            }
        );
    }

    #[test]
    fn no_host_session_actions_are_advertised_in_nested_mode() {
        let capabilities = SessionManager::nested_development().capabilities();

        assert!(!capabilities.lock);
        assert!(!capabilities.logout);
        assert!(!capabilities.suspend);
        assert!(!capabilities.reboot);
        assert!(!capabilities.power_off);
    }

    #[test]
    fn every_session_action_is_rejected_without_a_native_provider() {
        let manager = SessionManager::nested_development();
        let actions = [
            SessionAction::Lock,
            SessionAction::Logout,
            SessionAction::Suspend,
            SessionAction::Reboot,
            SessionAction::PowerOff,
        ];

        for action in actions {
            let response = manager.request_action(action);
            assert!(matches!(
                response,
                Response::Failure {
                    error: ErrorResponse {
                        code: ErrorCode::Unavailable,
                        ..
                    }
                }
            ));
        }
    }
}
