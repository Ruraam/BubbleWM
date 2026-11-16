//! Basic BubbleUI widgets.

use gtk::prelude::*;

/// Creates a Bubble-styled button.
pub fn button(label: &str) -> gtk::Button {
    let widget = gtk::Button::with_label(label);
    widget.add_css_class("bubble-button");
    widget
}

/// Creates a Bubble-styled label.
pub fn label(text: &str) -> gtk::Label {
    let widget = gtk::Label::new(Some(text));
    widget.add_css_class("bubble-label");
    widget
}

/// Creates a vertical BubbleUI container.
///
/// Negative spacing values are clamped to zero.
pub fn column(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, normalize_spacing(spacing))
}

/// Creates a horizontal BubbleUI container.
///
/// Negative spacing values are clamped to zero.
pub fn row(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, normalize_spacing(spacing))
}

fn normalize_spacing(spacing: i32) -> i32 {
    spacing.max(0)
}

#[cfg(test)]
mod tests {
    use super::normalize_spacing;

    #[test]
    fn spacing_is_clamped_to_zero_when_negative() {
        assert_eq!(normalize_spacing(i32::MIN), 0);
        assert_eq!(normalize_spacing(-1), 0);
    }

    #[test]
    fn spacing_preserves_zero_and_positive_values() {
        assert_eq!(normalize_spacing(0), 0);
        assert_eq!(normalize_spacing(1), 1);
        assert_eq!(normalize_spacing(12), 12);
        assert_eq!(normalize_spacing(i32::MAX), i32::MAX);
    }
}
