//! BubbleUI — the native visual toolkit for BubbleOS.

pub use gtk;

pub mod icons;
pub mod theme;
pub mod widgets;

/// Convenient imports for BubbleUI applications.
pub mod prelude {
    pub use crate::widgets::*;
    pub use gtk::prelude::*;
}
