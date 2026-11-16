//! Reusable BubbleUI widgets.

use gtk::prelude::*;

/// Semantic status used by the status_label helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Success,
    Warning,
    Error,
    Info,
}

impl Status {
    const fn css_class(self) -> &'static str {
        match self {
            Self::Success => "bubble-status-success",
            Self::Warning => "bubble-status-warning",
            Self::Error => "bubble-status-error",
            Self::Info => "bubble-status-info",
        }
    }
}

/// Creates a Bubble-styled button.
pub fn button(label: &str) -> gtk::Button {
    let widget = gtk::Button::with_label(label);
    widget.add_css_class("bubble-button");
    widget
}

/// Creates a Bubble-styled primary action button.
pub fn primary_button(label: &str) -> gtk::Button {
    let widget = button(label);
    widget.add_css_class("bubble-button-primary");
    widget
}

/// Creates a Bubble-styled label.
pub fn label(text: &str) -> gtk::Label {
    let widget = gtk::Label::new(Some(text));
    widget.add_css_class("bubble-label");
    widget
}

/// Creates a heading label with the BubbleUI typography.
pub fn heading(text: &str) -> gtk::Label {
    let widget = label(text);
    widget.set_halign(gtk::Align::Start);
    widget.add_css_class("bubble-heading");
    widget
}

/// Creates a secondary, visually muted label.
pub fn muted_label(text: &str) -> gtk::Label {
    let widget = label(text);
    widget.set_halign(gtk::Align::Start);
    widget.add_css_class("bubble-text-muted");
    widget
}

/// Creates a label styled for a semantic status.
pub fn status_label(text: &str, status: Status) -> gtk::Label {
    let widget = label(text);
    widget.set_halign(gtk::Align::Start);
    widget.set_wrap(true);
    widget.add_css_class(status.css_class());
    widget
}

/// Creates a vertical BubbleUI surface container.
///
/// Negative spacing values are clamped to zero.
pub fn surface(spacing: i32) -> gtk::Box {
    let widget = column(spacing);
    widget.add_css_class("bubble-surface");
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
    use super::{Status, normalize_spacing};

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

    #[test]
    fn status_maps_to_stable_semantic_css_classes() {
        assert_eq!(Status::Success.css_class(), "bubble-status-success");
        assert_eq!(Status::Warning.css_class(), "bubble-status-warning");
        assert_eq!(Status::Error.css_class(), "bubble-status-error");
        assert_eq!(Status::Info.css_class(), "bubble-status-info");
    }
}
