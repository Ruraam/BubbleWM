//! BubbleUI — the native visual toolkit for BubbleOS.
//!
//! BubbleUI provides a BubbleOS-owned API and visual identity on top of its
//! initial GTK4 backend. Applications should prefer these modules over
//! constructing styled GTK widgets directly.

pub use gtk;

pub mod icons;
pub mod theme;
pub mod widgets;

/// Convenient imports for BubbleUI applications.
pub mod prelude {
    pub use crate::widgets::*;
    pub use gtk::prelude::*;
}
