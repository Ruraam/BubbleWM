//! BubbleGreeter presentation model.
//!
//! This unprivileged state model contains no PAM calls, password storage,
//! privileged process control, or session launching. Those belong behind a
//! separately reviewed authentication boundary.

pub mod view;

pub const MAX_VISIBLE_USERS: usize = 64;
pub const MAX_USERNAME_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalUser {
    username: String,
    display_name: String,
    available: bool,
}

impl LocalUser {
    pub fn new(
        username: impl Into<String>,
        display_name: impl Into<String>,
    ) -> Result<Self, UserError> {
        let username = username.into();
        let display_name = display_name.into();
        if !valid_username(&username) {
            return Err(UserError::InvalidUsername);
        }
        if display_name.len() > 128 || display_name.chars().any(char::is_control) {
            return Err(UserError::InvalidDisplayName);
        }
        Ok(Self {
            username,
            display_name,
            available: true,
        })
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Whether this account may currently be selected for login.
    pub fn is_available(&self) -> bool {
        self.available
    }

    /// Updates the presentation availability supplied by the account provider.
    pub fn set_available(&mut self, available: bool) {
        self.available = available;
    }
}

fn valid_username(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_USERNAME_BYTES {
        return false;
    }
    (bytes[0].is_ascii_lowercase() || bytes[0] == b'_')
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserError {
    InvalidUsername,
    InvalidDisplayName,
    TooManyUsers,
    DuplicateUser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GreeterError {
    AuthenticationFailed,
    AccountUnavailable,
    SessionStartFailed,
    ServiceUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GreeterStatus {
    Ready,
    Authenticating,
    StartingSession,
    Error(GreeterError),
    SessionEnded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GreeterAction {
    None,
    SubmitLogin { username: String },
}

/// Keyboard commands understood by the renderer-independent greeter model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GreeterKey {
    Up,
    Down,
    Enter,
}

/// UI state only. It never accepts or stores a password.
#[derive(Debug, Clone)]
pub struct GreeterModel {
    users: Vec<LocalUser>,
    selected: Option<usize>,
    status: GreeterStatus,
}

impl GreeterModel {
    pub fn new(users: Vec<LocalUser>) -> Result<Self, UserError> {
        if users.len() > MAX_VISIBLE_USERS {
            return Err(UserError::TooManyUsers);
        }
        for (i, user) in users.iter().enumerate() {
            if users[..i]
                .iter()
                .any(|prior| prior.username == user.username)
            {
                return Err(UserError::DuplicateUser);
            }
        }
        let selected = (!users.is_empty()).then_some(0);
        Ok(Self {
            users,
            selected,
            status: GreeterStatus::Ready,
        })
    }

    pub fn users(&self) -> &[LocalUser] {
        &self.users
    }

    pub fn selected_user(&self) -> Option<&LocalUser> {
        self.selected.and_then(|i| self.users.get(i))
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    pub fn status(&self) -> &GreeterStatus {
        &self.status
    }

    /// Handles basic account navigation and submit keys without reading secrets.
    ///
    /// Up/down skip unavailable accounts and wrap around the visible list.
    /// Enter emits the same username-only action as the primary button.
    pub fn handle_key(&mut self, key: GreeterKey) -> GreeterAction {
        match key {
            GreeterKey::Enter => self.submit(),
            GreeterKey::Up | GreeterKey::Down => {
                if !matches!(
                    self.status,
                    GreeterStatus::Ready | GreeterStatus::Error(_) | GreeterStatus::SessionEnded
                ) || self.users.is_empty()
                {
                    return GreeterAction::None;
                }

                let len = self.users.len();
                let start =
                    self.selected
                        .unwrap_or(if key == GreeterKey::Down { len - 1 } else { 0 });

                for step in 1..=len {
                    let index = match key {
                        GreeterKey::Down => (start + step) % len,
                        GreeterKey::Up => (start + len - (step % len)) % len,
                        GreeterKey::Enter => unreachable!(),
                    };
                    if self.users[index].available {
                        self.selected = Some(index);
                        self.status = GreeterStatus::Ready;
                        break;
                    }
                }
                GreeterAction::None
            }
        }
    }

    /// Applies a pointer hit target to the model without handling credentials.
    ///
    /// Account hits update the selected account; the primary action emits the
    /// same username-only login intent as keyboard Enter.
    pub fn handle_pointer(&mut self, target: view::GreeterHitTarget) -> GreeterAction {
        match target {
            view::GreeterHitTarget::Account(index) => {
                self.select_user(index);
                GreeterAction::None
            }
            view::GreeterHitTarget::Submit => self.submit(),
        }
    }

    pub fn select_user(&mut self, index: usize) -> bool {
        if index >= self.users.len()
            || !self.users[index].available
            || !matches!(
                self.status,
                GreeterStatus::Ready | GreeterStatus::Error(_) | GreeterStatus::SessionEnded
            )
        {
            return false;
        }
        self.selected = Some(index);
        self.status = GreeterStatus::Ready;
        true
    }

    /// Emits a username-only intent. Secret collection is deliberately outside this model.
    pub fn submit(&mut self) -> GreeterAction {
        if !matches!(
            self.status,
            GreeterStatus::Ready | GreeterStatus::Error(_) | GreeterStatus::SessionEnded
        ) {
            return GreeterAction::None;
        }
        let Some(user) = self.selected_user().filter(|user| user.available) else {
            return GreeterAction::None;
        };
        let action = GreeterAction::SubmitLogin {
            username: user.username.clone(),
        };
        self.status = GreeterStatus::Authenticating;
        action
    }

    pub fn authentication_failed(&mut self, error: GreeterError) {
        if self.status == GreeterStatus::Authenticating {
            self.status = GreeterStatus::Error(error);
        }
    }

    pub fn authentication_succeeded(&mut self) {
        if self.status == GreeterStatus::Authenticating {
            self.status = GreeterStatus::StartingSession;
        }
    }

    pub fn session_ended(&mut self) {
        self.status = GreeterStatus::SessionEnded;
    }

    pub fn service_unavailable(&mut self) {
        self.status = GreeterStatus::Error(GreeterError::ServiceUnavailable);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn users() -> Vec<LocalUser> {
        vec![
            LocalUser::new("alice", "Alice").unwrap(),
            LocalUser::new("bob", "Bob").unwrap(),
        ]
    }

    #[test]
    fn validates_names_and_labels() {
        assert!(LocalUser::new("alice-2", "Alice").is_ok());
        assert_eq!(
            LocalUser::new("Alice", "Alice").unwrap_err(),
            UserError::InvalidUsername
        );
        assert_eq!(
            LocalUser::new("alice", "bad\nlabel").unwrap_err(),
            UserError::InvalidDisplayName
        );
        assert!(LocalUser::new("a".repeat(33), "Alice").is_err());
    }

    #[test]
    fn selection_emits_only_username_and_prevents_double_submit() {
        let mut model = GreeterModel::new(users()).unwrap();
        assert_eq!(model.selected_user().unwrap().username(), "alice");
        assert!(model.select_user(1));
        assert_eq!(
            model.submit(),
            GreeterAction::SubmitLogin {
                username: "bob".into()
            }
        );
        assert_eq!(model.status(), &GreeterStatus::Authenticating);
        assert_eq!(model.submit(), GreeterAction::None);
    }

    #[test]
    fn pointer_account_selection_and_submit_follow_model_rules() {
        let mut model = GreeterModel::new(users()).unwrap();

        assert_eq!(
            model.handle_pointer(view::GreeterHitTarget::Account(1)),
            GreeterAction::None
        );
        assert_eq!(model.selected_user().unwrap().username(), "bob");
        assert_eq!(
            model.handle_pointer(view::GreeterHitTarget::Submit),
            GreeterAction::SubmitLogin {
                username: "bob".into()
            }
        );
        assert_eq!(
            model.handle_pointer(view::GreeterHitTarget::Account(0)),
            GreeterAction::None
        );
        assert_eq!(model.selected_user().unwrap().username(), "bob");
    }

    #[test]
    fn pointer_cannot_select_unavailable_or_out_of_range_accounts() {
        let mut accounts = users();
        accounts[1].set_available(false);
        let mut model = GreeterModel::new(accounts).unwrap();

        assert_eq!(
            model.handle_pointer(view::GreeterHitTarget::Account(1)),
            GreeterAction::None
        );
        assert_eq!(
            model.handle_pointer(view::GreeterHitTarget::Account(99)),
            GreeterAction::None
        );
        assert_eq!(model.selected_user().unwrap().username(), "alice");
    }

    #[test]
    fn unavailable_accounts_cannot_be_selected_or_submitted() {
        let mut accounts = users();
        accounts[1].set_available(false);
        let mut model = GreeterModel::new(accounts).unwrap();

        assert!(!model.select_user(1));
        assert_eq!(model.selected_user().unwrap().username(), "alice");
        assert_eq!(
            model.submit(),
            GreeterAction::SubmitLogin {
                username: "alice".into()
            }
        );
    }

    #[test]
    fn keyboard_navigation_skips_unavailable_accounts_and_wraps() {
        let mut accounts = vec![
            LocalUser::new("alice", "Alice").unwrap(),
            LocalUser::new("bob", "Bob").unwrap(),
            LocalUser::new("carol", "Carol").unwrap(),
        ];
        accounts[1].set_available(false);
        let mut model = GreeterModel::new(accounts).unwrap();

        assert_eq!(model.handle_key(GreeterKey::Down), GreeterAction::None);
        assert_eq!(model.selected_user().unwrap().username(), "carol");
        assert_eq!(model.handle_key(GreeterKey::Down), GreeterAction::None);
        assert_eq!(model.selected_user().unwrap().username(), "alice");
        assert_eq!(model.handle_key(GreeterKey::Up), GreeterAction::None);
        assert_eq!(model.selected_user().unwrap().username(), "carol");

        assert_eq!(
            model.handle_key(GreeterKey::Enter),
            GreeterAction::SubmitLogin {
                username: "carol".into()
            }
        );
        assert_eq!(model.handle_key(GreeterKey::Down), GreeterAction::None);
        assert_eq!(model.selected_user().unwrap().username(), "carol");
    }

    #[test]
    fn keyboard_navigation_handles_empty_and_all_unavailable_lists() {
        let mut empty = GreeterModel::new(Vec::new()).unwrap();
        assert_eq!(empty.handle_key(GreeterKey::Down), GreeterAction::None);

        let mut unavailable = LocalUser::new("alice", "Alice").unwrap();
        unavailable.set_available(false);
        let mut model = GreeterModel::new(vec![unavailable]).unwrap();
        assert_eq!(model.handle_key(GreeterKey::Down), GreeterAction::None);
        assert_eq!(model.selected_index(), Some(0));
        assert_eq!(model.submit(), GreeterAction::None);
    }

    #[test]
    fn failed_login_is_retryable() {
        let mut model = GreeterModel::new(users()).unwrap();
        model.submit();
        model.authentication_failed(GreeterError::AuthenticationFailed);
        assert_eq!(
            model.status(),
            &GreeterStatus::Error(GreeterError::AuthenticationFailed)
        );
        assert_eq!(
            model.submit(),
            GreeterAction::SubmitLogin {
                username: "alice".into()
            }
        );
    }

    #[test]
    fn successful_login_and_logout_have_explicit_states() {
        let mut model = GreeterModel::new(users()).unwrap();
        model.submit();
        model.authentication_succeeded();
        assert_eq!(model.status(), &GreeterStatus::StartingSession);
        model.session_ended();
        assert_eq!(model.status(), &GreeterStatus::SessionEnded);
        assert!(model.select_user(0));
    }

    #[test]
    fn rejects_duplicate_and_excessive_account_lists() {
        let duplicate = vec![
            LocalUser::new("alice", "A").unwrap(),
            LocalUser::new("alice", "B").unwrap(),
        ];
        assert_eq!(
            GreeterModel::new(duplicate).unwrap_err(),
            UserError::DuplicateUser
        );
        let many = (0..=MAX_VISIBLE_USERS)
            .map(|i| LocalUser::new(format!("user{i}"), "User").unwrap())
            .collect();
        assert_eq!(
            GreeterModel::new(many).unwrap_err(),
            UserError::TooManyUsers
        );
    }
}
