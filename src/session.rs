//! Safe session-management policy and capability reporting.
//!
//! This layer never executes operating-system commands. A backend-specific
//! provider must be added before any session action can be advertised or run.
//! The nested Winit backend is intentionally read-only with respect to the
//! host session, so power and logout actions are always rejected.

use bubble_ipc::{
    ErrorCode, ErrorResponse, Response, SessionAction, SessionCapabilities, SessionEnvironment,
    SessionState,
};

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
    /// No provider is currently installed, so every action remains disabled.
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
