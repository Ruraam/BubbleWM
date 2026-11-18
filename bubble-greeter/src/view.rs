//! Geometry and presentation data for the BubbleGreeter frontend.
//!
//! This module is deliberately renderer- and authentication-independent. It
//! describes a responsive login surface without opening a display, handling
//! credentials, or starting a session.

/// A rectangle in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn contains(self, x: u32, y: u32) -> bool {
        x >= self.x && y >= self.y && x - self.x < self.width && y - self.y < self.height
    }
}

/// Responsive regions used by a renderer to draw the login screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GreeterLayout {
    pub card: Rect,
    pub header: Rect,
    pub accounts: Rect,
    pub status: Rect,
    pub action: Rect,
}

impl GreeterLayout {
    /// Computes a centered layout, clamped to small display sizes.
    pub fn for_viewport(width: u32, height: u32) -> Self {
        let margin = 16;
        let card_width = 440.min(width.saturating_sub(margin * 2));
        let card_height = 520.min(height.saturating_sub(margin * 2));
        let card = Rect {
            x: width.saturating_sub(card_width) / 2,
            y: height.saturating_sub(card_height) / 2,
            width: card_width,
            height: card_height,
        };

        let inset = 24.min(card_width / 2);
        let inner_x = card.x + inset;
        let inner_width = card.width.saturating_sub(inset * 2);
        let header_height = 104.min(card.height);
        let action_height = 48.min(card.height.saturating_sub(header_height));
        let status_height = 48.min(
            card.height
                .saturating_sub(header_height)
                .saturating_sub(action_height),
        );
        let accounts_y = card.y + header_height;
        let action_y = card.y + card.height.saturating_sub(action_height + 20);
        let status_y = action_y.saturating_sub(status_height + 12);

        Self {
            card,
            header: Rect {
                x: inner_x,
                y: card.y + 20,
                width: inner_width,
                height: header_height.saturating_sub(20),
            },
            accounts: Rect {
                x: inner_x,
                y: accounts_y,
                width: inner_width,
                height: status_y.saturating_sub(accounts_y).saturating_sub(12),
            },
            status: Rect {
                x: inner_x,
                y: status_y,
                width: inner_width,
                height: status_height,
            },
            action: Rect {
                x: inner_x,
                y: action_y,
                width: inner_width,
                height: action_height,
            },
        }
    }

    /// Returns the clickable rectangle for an account row.
    ///
    /// Rows divide the available account region evenly. The final row absorbs
    /// any remainder, so hit testing covers the full region without gaps.
    pub fn account_rect(self, index: usize, account_count: usize) -> Option<Rect> {
        if account_count == 0
            || account_count > crate::MAX_VISIBLE_USERS
            || index >= account_count
            || self.accounts.height == 0
        {
            return None;
        }

        let count = account_count as u64;
        let height = u64::from(self.accounts.height);
        let top = height * index as u64 / count;
        let bottom = height * (index as u64 + 1) / count;
        if bottom == top {
            return None;
        }

        Some(Rect {
            x: self.accounts.x,
            y: self.accounts.y + top as u32,
            width: self.accounts.width,
            height: (bottom - top) as u32,
        })
    }

    /// Resolves pointer coordinates to an account or the primary action.
    pub fn hit_test(self, x: u32, y: u32, account_count: usize) -> Option<GreeterHitTarget> {
        for index in 0..account_count.min(crate::MAX_VISIBLE_USERS) {
            if self.account_rect(index, account_count)?.contains(x, y) {
                return Some(GreeterHitTarget::Account(index));
            }
        }
        self.action
            .contains(x, y)
            .then_some(GreeterHitTarget::Submit)
    }
}

/// Render-ready account row data; contains no credentials or process state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRowPresentation {
    pub username: String,
    pub display_name: String,
    pub bounds: Rect,
    pub selected: bool,
    pub disabled: bool,
}

/// Builds visible account rows from the model and current responsive layout.
pub fn account_rows(
    model: &crate::GreeterModel,
    layout: GreeterLayout,
) -> Vec<AccountRowPresentation> {
    let interaction_locked = matches!(
        model.status(),
        crate::GreeterStatus::Authenticating | crate::GreeterStatus::StartingSession
    );

    model
        .users()
        .iter()
        .enumerate()
        .filter_map(|(index, user)| {
            Some(AccountRowPresentation {
                username: user.username().to_owned(),
                display_name: user.display_name().to_owned(),
                bounds: layout.account_rect(index, model.users().len())?,
                selected: model.selected_index() == Some(index),
                disabled: !user.is_available() || interaction_locked,
            })
        })
        .collect()
}

/// Interactive regions exposed to a frontend event handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GreeterHitTarget {
    Account(usize),
    Submit,
}

/// User-facing copy for a state in the presentation model.
pub fn status_message(status: &crate::GreeterStatus) -> &'static str {
    match status {
        crate::GreeterStatus::Ready => "Select an account to continue",
        crate::GreeterStatus::Authenticating => "Signing in…",
        crate::GreeterStatus::StartingSession => "Starting your session…",
        crate::GreeterStatus::Error(crate::GreeterError::AuthenticationFailed) => {
            "The password was not accepted"
        }
        crate::GreeterStatus::Error(crate::GreeterError::AccountUnavailable) => {
            "This account is unavailable"
        }
        crate::GreeterStatus::Error(crate::GreeterError::SessionStartFailed) => {
            "The session could not be started"
        }
        crate::GreeterStatus::Error(crate::GreeterError::ServiceUnavailable) => {
            "The login service is unavailable"
        }
        crate::GreeterStatus::SessionEnded => "Your session has ended",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_centered_and_fits_wide_viewport() {
        let layout = GreeterLayout::for_viewport(1920, 1080);
        assert_eq!(layout.card.width, 440);
        assert_eq!(layout.card.height, 520);
        assert_eq!(layout.card.x, (1920 - 440) / 2);
        assert_eq!(layout.card.y, (1080 - 520) / 2);
        assert!(layout.card.contains(layout.action.x, layout.action.y));
    }

    #[test]
    fn layout_stays_inside_small_viewport() {
        let layout = GreeterLayout::for_viewport(320, 240);
        assert!(layout.card.x + layout.card.width <= 320);
        assert!(layout.card.y + layout.card.height <= 240);
        assert!(layout.header.width <= layout.card.width);
        assert!(layout.accounts.width <= layout.card.width);
        assert!(layout.status.width <= layout.card.width);
        assert!(layout.action.width <= layout.card.width);
    }

    #[test]
    fn account_rows_cover_region_and_hit_testing_selects_correct_target() {
        let layout = GreeterLayout::for_viewport(1280, 800);
        let first = layout.account_rect(0, 3).unwrap();
        let second = layout.account_rect(1, 3).unwrap();
        let third = layout.account_rect(2, 3).unwrap();

        assert_eq!(first.y, layout.accounts.y);
        assert_eq!(first.y + first.height, second.y);
        assert_eq!(second.y + second.height, third.y);
        assert_eq!(
            third.y + third.height,
            layout.accounts.y + layout.accounts.height
        );

        assert_eq!(
            layout.hit_test(first.x, first.y, 3),
            Some(GreeterHitTarget::Account(0))
        );
        assert_eq!(
            layout.hit_test(second.x, second.y, 3),
            Some(GreeterHitTarget::Account(1))
        );
        assert_eq!(
            layout.hit_test(layout.action.x, layout.action.y, 3),
            Some(GreeterHitTarget::Submit)
        );
        assert_eq!(layout.hit_test(layout.card.x, layout.card.y, 3), None);
    }

    #[test]
    fn account_hit_testing_rejects_empty_excessive_and_unavailable_rows() {
        let layout = GreeterLayout::for_viewport(320, 240);
        assert_eq!(layout.account_rect(0, 0), None);
        assert_eq!(layout.account_rect(0, crate::MAX_VISIBLE_USERS + 1), None);
        assert_eq!(layout.account_rect(0, 1), None);
        assert_eq!(layout.hit_test(0, 0, 0), None);
        assert_eq!(layout.hit_test(0, 0, crate::MAX_VISIBLE_USERS + 1), None);
    }

    #[test]
    fn account_rows_expose_names_selection_and_disabled_state() {
        let mut accounts = vec![
            crate::LocalUser::new("alice", "Alice Example").unwrap(),
            crate::LocalUser::new("bob", "Bob Example").unwrap(),
        ];
        accounts[1].set_available(false);
        let mut model = crate::GreeterModel::new(accounts).unwrap();
        let layout = GreeterLayout::for_viewport(1280, 800);

        let rows = account_rows(&model, layout);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].display_name, "Alice Example");
        assert_eq!(rows[0].username, "alice");
        assert!(rows[0].selected);
        assert!(!rows[0].disabled);
        assert!(!rows[1].selected);
        assert!(rows[1].disabled);
        assert!(rows[0].bounds.width > 0);

        model.submit();
        let busy_rows = account_rows(&model, layout);
        assert!(busy_rows.iter().all(|row| row.disabled));
        assert!(busy_rows[0].selected);
    }

    #[test]
    fn status_copy_covers_authentication_states() {
        assert_eq!(
            status_message(&crate::GreeterStatus::Ready),
            "Select an account to continue"
        );
        assert_eq!(
            status_message(&crate::GreeterStatus::Error(
                crate::GreeterError::AuthenticationFailed
            )),
            "The password was not accepted"
        );
    }
}
