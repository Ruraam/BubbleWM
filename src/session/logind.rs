//! Read-only systemd-logind discovery and capability probing.
//!
//! This module deliberately does not execute session actions. It is not wired
//! into the nested Winit backend. Callers must run probes away from the
//! compositor event loop because D-Bus round trips are blocking.

use anyhow::{Context, Result, ensure};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedObjectPath;

const LOGIND_DESTINATION: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";
const LOGIND_SESSION: &str = "org.freedesktop.login1.Session";
const LOGIND_USER: &str = "org.freedesktop.login1.User";
const LOGIND_SEAT: &str = "org.freedesktop.login1.Seat";

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

/// Validated identity and state of a logind user Wayland session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogindSession {
    pub id: String,
    pub uid: u32,
    pub session_type: String,
    pub class: String,
    pub active: bool,
    /// Runtime directory reported by the owning logind user object.
    pub runtime_dir: String,
    /// Seat identifier reported by logind, when the session has a seat.
    pub seat: Option<String>,
    /// Virtual terminal number reported by logind, when assigned.
    pub vt_number: Option<u32>,
}

impl LogindSession {
    fn validate(&self, requested_id: &str, expected_uid: u32) -> Result<()> {
        ensure!(
            self.id == requested_id,
            "logind returned a different session ID"
        );
        ensure!(
            self.uid == expected_uid,
            "logind session belongs to a different UID"
        );
        ensure!(
            self.session_type == "wayland",
            "logind session is not a Wayland session"
        );
        ensure!(
            self.class == "user",
            "logind session is not a regular user session"
        );
        ensure!(self.active, "logind session is not active");
        Ok(())
    }
}

/// A read-only connection to the systemd-logind manager.
pub struct LogindProbe {
    connection: Connection,
}

impl LogindProbe {
    /// Connects to the system bus and verifies that logind's manager is present.
    pub fn connect() -> Result<Self> {
        let connection = Connection::system().context("cannot connect to the system D-Bus")?;
        let _ = manager_proxy(&connection)
            .context("systemd-logind manager is unavailable on the system bus")?;
        Ok(Self { connection })
    }

    /// Resolves and validates the session named by XDG_SESSION_ID.
    ///
    /// The environment variable is only a lookup hint: the session ID, owner
    /// UID, type, class and active state are checked against logind itself.
    /// This performs blocking D-Bus I/O and must run on a worker thread, never
    /// from the compositor event loop.
    pub fn current_session(&self) -> Result<LogindSession> {
        let session_id = std::env::var("XDG_SESSION_ID")
            .context("XDG_SESSION_ID is not set or is not valid Unicode")?;
        let expected_uid = effective_uid()?;
        self.session(&session_id, expected_uid)
    }

    /// Resolves and validates a specific logind session against an expected UID.
    ///
    /// This is a blocking D-Bus operation. The caller must not invoke it from
    /// the compositor event loop.
    pub fn session(&self, session_id: &str, expected_uid: u32) -> Result<LogindSession> {
        validate_session_id(session_id)?;

        let manager = manager_proxy(&self.connection)?;
        let session_path: OwnedObjectPath = manager
            .call("GetSession", &(session_id,))
            .context("logind GetSession lookup failed")?;
        let proxy = Proxy::new(
            &self.connection,
            LOGIND_DESTINATION,
            session_path.as_str(),
            LOGIND_SESSION,
        )
        .context("cannot create logind session proxy")?;

        let id: String = proxy
            .get_property("Id")
            .context("cannot read logind session ID")?;
        let (uid, user_path): (u32, OwnedObjectPath) = proxy
            .get_property("User")
            .context("cannot read logind session owner")?;
        let user_proxy = Proxy::new(
            &self.connection,
            LOGIND_DESTINATION,
            user_path.as_str(),
            LOGIND_USER,
        )
        .context("cannot create logind user proxy")?;
        let runtime_dir: String = user_proxy
            .get_property("RuntimePath")
            .context("cannot read logind user runtime directory")?;
        validate_runtime_dir(&runtime_dir, uid)?;

        let seat_path: OwnedObjectPath = proxy
            .get_property("Seat")
            .context("cannot read logind session seat")?;
        let seat = if seat_path.as_str() == "/" {
            None
        } else {
            let seat_proxy = Proxy::new(
                &self.connection,
                LOGIND_DESTINATION,
                seat_path.as_str(),
                LOGIND_SEAT,
            )
            .context("cannot create logind seat proxy")?;
            let seat_id: String = seat_proxy
                .get_property("Id")
                .context("cannot read logind seat ID")?;
            validate_seat_id(&seat_id)?;
            Some(seat_id)
        };
        let vt_number: u32 = proxy
            .get_property("VTNr")
            .context("cannot read logind virtual terminal number")?;
        let session_type: String = proxy
            .get_property("Type")
            .context("cannot read logind session type")?;
        let class: String = proxy
            .get_property("Class")
            .context("cannot read logind session class")?;
        let active: bool = proxy
            .get_property("Active")
            .context("cannot read logind session active state")?;

        let session = LogindSession {
            id,
            uid,
            session_type,
            class,
            active,
            runtime_dir,
            seat,
            vt_number: (vt_number != 0).then_some(vt_number),
        };
        session.validate(session_id, expected_uid)?;
        Ok(session)
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
    Proxy::new(connection, LOGIND_DESTINATION, LOGIND_PATH, LOGIND_MANAGER)
        .context("cannot create systemd-logind manager proxy")
}

fn validate_runtime_dir(runtime_dir: &str, uid: u32) -> Result<()> {
    let expected = format!("/run/user/{uid}");
    ensure!(
        runtime_dir == expected,
        "logind runtime directory does not match the expected user runtime path"
    );
    Ok(())
}

fn validate_seat_id(seat_id: &str) -> Result<()> {
    ensure!(
        !seat_id.is_empty()
            && seat_id.len() <= 64
            && seat_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
        "logind returned a malformed seat identifier"
    );
    Ok(())
}

fn validate_session_id(session_id: &str) -> Result<()> {
    ensure!(
        !session_id.is_empty()
            && session_id.len() <= 256
            && session_id.bytes().all(|byte| byte.is_ascii_graphic()),
        "XDG_SESSION_ID is empty or malformed"
    );
    Ok(())
}

fn effective_uid() -> Result<u32> {
    let status = std::fs::read_to_string("/proc/self/status")
        .context("cannot read effective UID from /proc/self/status")?;
    effective_uid_from_status(&status)
}

fn effective_uid_from_status(status: &str) -> Result<u32> {
    let uid_line = status
        .lines()
        .find(|line| line.starts_with("Uid:"))
        .context("process status does not contain a Uid field")?;
    uid_line
        .split_whitespace()
        .nth(2)
        .context("process Uid field has no effective UID")?
        .parse()
        .context("process effective UID is malformed")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_session() -> LogindSession {
        LogindSession {
            id: "c2".to_owned(),
            uid: 1000,
            session_type: "wayland".to_owned(),
            class: "user".to_owned(),
            active: true,
            runtime_dir: "/run/user/1000".to_owned(),
            seat: Some("seat0".to_owned()),
            vt_number: Some(2),
        }
    }

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

    #[test]
    fn accepts_a_matching_active_user_wayland_session() {
        assert!(valid_session().validate("c2", 1000).is_ok());
    }

    #[test]
    fn rejects_a_session_owned_by_another_uid() {
        assert!(valid_session().validate("c2", 1001).is_err());
    }

    #[test]
    fn rejects_a_different_session_id() {
        assert!(valid_session().validate("c3", 1000).is_err());
    }

    #[test]
    fn rejects_non_wayland_or_non_user_sessions() {
        let mut session = valid_session();
        session.session_type = "x11".to_owned();
        assert!(session.validate("c2", 1000).is_err());

        let mut session = valid_session();
        session.class = "greeter".to_owned();
        assert!(session.validate("c2", 1000).is_err());
    }

    #[test]
    fn rejects_inactive_sessions() {
        let mut session = valid_session();
        session.active = false;
        assert!(session.validate("c2", 1000).is_err());
    }

    #[test]
    fn validates_runtime_directory_against_the_logind_uid() {
        assert!(validate_runtime_dir("/run/user/1000", 1000).is_ok());
        assert!(validate_runtime_dir("/run/user/1001", 1000).is_err());
        assert!(validate_runtime_dir("/tmp/user-1000", 1000).is_err());
    }

    #[test]
    fn validates_optional_seat_identifier_syntax() {
        assert!(validate_seat_id("seat0").is_ok());
        assert!(validate_seat_id("seat-1").is_ok());
        assert!(validate_seat_id("").is_err());
        assert!(validate_seat_id("seat/0").is_err());
    }

    #[test]
    fn rejects_empty_or_malformed_session_ids() {
        assert!(validate_session_id("").is_err());
        assert!(validate_session_id("c2\n").is_err());
        assert!(validate_session_id(&"x".repeat(257)).is_err());
        assert!(validate_session_id("c2").is_ok());
    }

    #[test]
    fn reads_the_effective_uid_from_proc_status() {
        let status = "Name:\tbubblewm\nUid:\t1000\t1001\t1002\t1003\n";
        assert_eq!(effective_uid_from_status(status).unwrap(), 1001);
    }

    #[test]
    fn rejects_a_missing_or_malformed_uid_field() {
        assert!(effective_uid_from_status("Name:\tbubblewm\n").is_err());
        assert!(effective_uid_from_status("Uid:\t1000\tbad\t1002\t1003\n").is_err());
    }
}
