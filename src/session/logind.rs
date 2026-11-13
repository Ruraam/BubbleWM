//! Read-only systemd-logind discovery and capability probing.
//!
//! This module deliberately does not execute session actions. It is not wired
//! into the nested Winit backend. Callers must run probes away from the
//! compositor event loop because D-Bus round trips are blocking.

use anyhow::{Context, Result};
use zbus::blocking::{Connection, Proxy};

const LOGIND_DESTINATION: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";

/// The result returned by logind's Can* methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionAvailability {
    Yes,
    No,
    Challenge,
    NotApplicable,
    Unknown,
}

impl ActionAvailability {
    fn parse(value: &str) -> Self {
        match value {
            "yes" => Self::Yes,
            "no" => Self::No,
            "challenge" => Self::Challenge,
            "na" => Self::NotApplicable,
            _ => Self::Unknown,
        }
    }

    /// Whether logind reports the action as immediately permitted.
    pub const fn is_permitted(self) -> bool {
        matches!(self, Self::Yes)
    }
}

/// Read-only snapshot of the logind power-action policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogindAvailability {
    pub suspend: ActionAvailability,
    pub reboot: ActionAvailability,
    pub power_off: ActionAvailability,
}

/// A read-only connection to the systemd-logind manager.
pub struct LogindProbe {
    connection: Connection,
}

impl LogindProbe {
    /// Connects to the system bus and verifies that logind's manager is present.
    pub fn connect() -> Result<Self> {
        let connection =
            Connection::system().context("cannot connect to the system D-Bus")?;
        let _ = manager_proxy(&connection)
            .context("systemd-logind manager is unavailable on the system bus")?;
        Ok(Self { connection })
    }

    /// Queries logind without requesting or executing any session action.
    ///
    /// The returned values are advisory only. BubbleWM must keep its public
    /// session capabilities disabled until authorization, confirmation and
    /// action execution are implemented as a separate, audited flow.
    pub fn availability(&self) -> Result<LogindAvailability> {
        let proxy = manager_proxy(&self.connection)?;
        Ok(LogindAvailability {
            suspend: ActionAvailability::parse(
                &proxy
                    .call::<_, _, String>("CanSuspend", &())
                    .context("logind CanSuspend query failed")?,
            ),
            reboot: ActionAvailability::parse(
                &proxy
                    .call::<_, _, String>("CanReboot", &())
                    .context("logind CanReboot query failed")?,
            ),
            power_off: ActionAvailability::parse(
                &proxy
                    .call::<_, _, String>("CanPowerOff", &())
                    .context("logind CanPowerOff query failed")?,
            ),
        })
    }
}

fn manager_proxy(connection: &Connection) -> Result<Proxy<'_>> {
    Proxy::new(
        connection,
        LOGIND_DESTINATION,
        LOGIND_PATH,
        LOGIND_MANAGER,
    )
    .context("cannot create systemd-logind manager proxy")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_logind_action_availability_values() {
        assert_eq!(ActionAvailability::parse("yes"), ActionAvailability::Yes);
        assert_eq!(ActionAvailability::parse("no"), ActionAvailability::No);
        assert_eq!(
            ActionAvailability::parse("challenge"),
            ActionAvailability::Challenge
        );
        assert_eq!(
            ActionAvailability::parse("na"),
            ActionAvailability::NotApplicable
        );
        assert_eq!(
            ActionAvailability::parse("unexpected"),
            ActionAvailability::Unknown
        );
    }

    #[test]
    fn only_explicit_yes_is_immediately_permitted() {
        assert!(ActionAvailability::Yes.is_permitted());
        assert!(!ActionAvailability::No.is_permitted());
        assert!(!ActionAvailability::Challenge.is_permitted());
        assert!(!ActionAvailability::NotApplicable.is_permitted());
        assert!(!ActionAvailability::Unknown.is_permitted());
    }
}
