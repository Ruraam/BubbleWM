//! BubbleUI theme installation.

/// Installs the default BubbleUI stylesheet on the current display.
///
/// Call this after GTK has initialized a display and before building widgets.
pub fn install() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(include_str!("theme.css"));

    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
