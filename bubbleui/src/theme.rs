//! BubbleUI theme installation.

/// Installs the default BubbleUI stylesheet on the current display.
///
/// GTK retains the registered provider for the display, so the local
/// Rust wrapper does not need to be stored in a global static.
pub fn install() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };

    let provider = gtk::CssProvider::new();
    provider.load_from_data(include_str!("theme.css"));

    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
