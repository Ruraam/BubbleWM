//! BubbleUI theme installation and semantic palette.

use std::cell::RefCell;

thread_local! {
    static INSTALLED_PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
}

/// An opaque sRGB color used by the BubbleUI theme contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Color {
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }

    fn to_css(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.red, self.green, self.blue)
    }
}

/// Semantic colors shared by BubbleUI components.
///
/// The roles intentionally mirror BubbleColor's semantic palette without
/// coupling this UI crate to the compositor or its IPC protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BubblePalette {
    pub primary: Color,
    pub on_primary: Color,
    pub primary_container: Color,
    pub on_primary_container: Color,
    pub secondary: Color,
    pub on_secondary: Color,
    pub tertiary: Color,
    pub on_tertiary: Color,
    pub background: Color,
    pub on_background: Color,
    pub surface: Color,
    pub on_surface: Color,
    pub surface_variant: Color,
    pub on_surface_variant: Color,
    pub outline: Color,
    pub error: Color,
    pub on_error: Color,
    pub success: Color,
    pub warning: Color,
    pub info: Color,
    pub glass_tint: Color,
    pub glass_border: Color,
    pub glass_highlight: Color,
    pub neon_cyan: Color,
    pub neon_magenta: Color,
    pub neon_violet: Color,
    pub neon_lime: Color,
}

impl Default for BubblePalette {
    /// A dark fallback palette matching BubbleUI's original appearance.
    fn default() -> Self {
        Self {
            primary: Color::new(52, 120, 212),
            on_primary: Color::new(255, 255, 255),
            primary_container: Color::new(48, 59, 88),
            on_primary_container: Color::new(237, 241, 251),
            secondary: Color::new(91, 111, 153),
            on_secondary: Color::new(255, 255, 255),
            tertiary: Color::new(165, 95, 255),
            on_tertiary: Color::new(255, 255, 255),
            background: Color::new(17, 19, 28),
            on_background: Color::new(237, 241, 251),
            surface: Color::new(27, 31, 46),
            on_surface: Color::new(237, 241, 251),
            surface_variant: Color::new(37, 43, 62),
            on_surface_variant: Color::new(226, 232, 245),
            outline: Color::new(58, 69, 98),
            error: Color::new(220, 60, 70),
            on_error: Color::new(255, 255, 255),
            success: Color::new(46, 160, 105),
            warning: Color::new(210, 143, 25),
            info: Color::new(50, 130, 210),
            glass_tint: Color::new(48, 59, 88),
            glass_border: Color::new(95, 105, 133),
            glass_highlight: Color::new(185, 205, 255),
            neon_cyan: Color::new(45, 225, 235),
            neon_magenta: Color::new(245, 65, 190),
            neon_violet: Color::new(165, 95, 255),
            neon_lime: Color::new(180, 245, 70),
        }
    }
}

impl BubblePalette {
    fn css_definitions(self) -> String {
        let colors = [
            ("bubble-primary", self.primary),
            ("bubble-on-primary", self.on_primary),
            ("bubble-primary-container", self.primary_container),
            ("bubble-on-primary-container", self.on_primary_container),
            ("bubble-secondary", self.secondary),
            ("bubble-on-secondary", self.on_secondary),
            ("bubble-tertiary", self.tertiary),
            ("bubble-on-tertiary", self.on_tertiary),
            ("bubble-background", self.background),
            ("bubble-on-background", self.on_background),
            ("bubble-surface", self.surface),
            ("bubble-on-surface", self.on_surface),
            ("bubble-surface-variant", self.surface_variant),
            ("bubble-on-surface-variant", self.on_surface_variant),
            ("bubble-outline", self.outline),
            ("bubble-error", self.error),
            ("bubble-on-error", self.on_error),
            ("bubble-success", self.success),
            ("bubble-warning", self.warning),
            ("bubble-info", self.info),
            ("bubble-glass-tint", self.glass_tint),
            ("bubble-glass-border", self.glass_border),
            ("bubble-glass-highlight", self.glass_highlight),
            ("bubble-neon-cyan", self.neon_cyan),
            ("bubble-neon-magenta", self.neon_magenta),
            ("bubble-neon-violet", self.neon_violet),
            ("bubble-neon-lime", self.neon_lime),
        ];

        let mut css = String::new();
        for (name, color) in colors {
            use std::fmt::Write as _;
            let _ = writeln!(css, "@define-color {name} {};", color.to_css());
        }
        css
    }

    fn stylesheet(self) -> String {
        format!("{}\n{}", self.css_definitions(), include_str!("theme.css"))
    }
}

/// Installs the default BubbleUI stylesheet on the current display.
///
/// The provider is retained and replaced on subsequent calls, allowing the
/// active palette to be updated without accumulating GTK style providers.
pub fn install() {
    install_with_palette(&BubblePalette::default());
}

/// Installs BubbleUI styles using the supplied semantic palette.
pub fn install_with_palette(palette: &BubblePalette) {
    install_with_palette_and_css(palette, "");
}

/// Installs BubbleUI and application styles in one provider.
///
/// Keeping semantic color definitions and their consumers in the same CSS
/// provider makes named colors available consistently across GTK versions.
pub fn install_with_palette_and_css(palette: &BubblePalette, application_css: &str) {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };

    let provider = gtk::CssProvider::new();
    let mut stylesheet = palette.stylesheet();
    if !application_css.is_empty() {
        stylesheet.push('\n');
        stylesheet.push_str(application_css);
    }
    provider.load_from_data(&stylesheet);

    INSTALLED_PROVIDER.with(|installed| {
        let mut installed = installed.borrow_mut();
        if let Some(previous) = installed.take() {
            gtk::style_context_remove_provider_for_display(&display, &previous);
        }
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        *installed = Some(provider);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_formats_as_css_hex() {
        assert_eq!(Color::new(52, 120, 212).to_css(), "#3478D4");
    }

    #[test]
    fn default_palette_defines_every_semantic_color() {
        let css = BubblePalette::default().css_definitions();
        for role in [
            "bubble-primary",
            "bubble-on-primary",
            "bubble-primary-container",
            "bubble-on-primary-container",
            "bubble-secondary",
            "bubble-on-secondary",
            "bubble-tertiary",
            "bubble-on-tertiary",
            "bubble-background",
            "bubble-on-background",
            "bubble-surface",
            "bubble-on-surface",
            "bubble-surface-variant",
            "bubble-on-surface-variant",
            "bubble-outline",
            "bubble-error",
            "bubble-on-error",
            "bubble-success",
            "bubble-warning",
            "bubble-info",
            "bubble-glass-tint",
            "bubble-glass-border",
            "bubble-glass-highlight",
            "bubble-neon-cyan",
            "bubble-neon-magenta",
            "bubble-neon-violet",
            "bubble-neon-lime",
        ] {
            assert!(css.contains(&format!("@define-color {role} ")), "{role}");
        }
    }

    #[test]
    fn custom_palette_values_are_reflected_in_css() {
        let palette = BubblePalette {
            primary: Color::new(1, 2, 3),
            ..BubblePalette::default()
        };
        assert!(
            palette
                .css_definitions()
                .contains("@define-color bubble-primary #010203;")
        );
    }
}
