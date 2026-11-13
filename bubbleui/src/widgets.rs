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
pub fn column(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

/// Creates a horizontal BubbleUI container.
pub fn row(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}
