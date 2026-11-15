//! Built-in lock screen for an already-running user session.

use std::{
    ffi::CStr,
    time::{Duration, Instant},
};

use smithay::{
    input::keyboard::{Keysym, ModifiersState},
    utils::{Logical, Size},
};

use crate::{
    auth,
    config::Config,
    theme::{Rgb, ThemeDocument},
    ui::{Canvas, Fonts, GlassBackdrop, GlassRegion, TRANSPARENT, UiColor, glass_tint},
};

#[derive(Debug)]
pub struct LockScreen {
    pub active: bool,
    password: String,
    message: Option<String>,
    message_is_error: bool,
    last_input: Instant,
    attempts: u32,
    cooldown_until: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockAction {
    None,
    Redraw,
    Unlocked,
}

impl LockScreen {
    pub fn new(active: bool) -> Self {
        Self {
            active,
            password: String::new(),
            message: None,
            message_is_error: false,
            last_input: Instant::now(),
            attempts: 0,
            cooldown_until: None,
        }
    }

    pub fn lock(&mut self, config: &Config) {
        if !config.lock.enabled {
            return;
        }
        self.active = true;
        self.password.clear();
        self.message = None;
        self.message_is_error = false;
        self.attempts = 0;
        self.cooldown_until = None;
        self.last_input = Instant::now();
    }

    pub fn handle_key(
        &mut self,
        keysym: Keysym,
        modifiers: ModifiersState,
        config: &Config,
    ) -> LockAction {
        if !self.active {
            return LockAction::None;
        }
        if self
            .cooldown_until
            .is_some_and(|deadline| Instant::now() < deadline)
        {
            return LockAction::Redraw;
        }
        match keysym {
            Keysym::Return | Keysym::KP_Enter => {
                self.last_input = Instant::now();
                match auth::authenticate(&self.password, &config.lock.pam_service) {
                    Ok(()) => {
                        self.active = false;
                        self.password.clear();
                        self.message = None;
                        self.attempts = 0;
                        return LockAction::Unlocked;
                    }
                    Err(error) => {
                        self.attempts = self.attempts.saturating_add(1);
                        self.password.clear();
                        self.message = Some(error);
                        self.message_is_error = true;
                        if self.attempts >= 5 {
                            self.cooldown_until = Some(Instant::now() + Duration::from_secs(10));
                            self.message = Some("Trop de tentatives, patientez 10 secondes".into());
                        }
                        return LockAction::Redraw;
                    }
                }
            }
            Keysym::BackSpace => {
                self.password.pop();
                self.last_input = Instant::now();
                LockAction::Redraw
            }
            Keysym::Escape => {
                self.password.clear();
                self.message = None;
                LockAction::Redraw
            }
            _ => {
                if modifiers.ctrl || modifiers.alt || modifiers.logo {
                    return LockAction::None;
                }
                if let Some(character) = keysym.key_char() {
                    if !character.is_control() {
                        self.password.push(character);
                        self.last_input = Instant::now();
                        return LockAction::Redraw;
                    }
                }
                LockAction::None
            }
        }
    }

    /// Full-screen dim layer and lock card geometry for the live backdrop blur.
    pub fn glass_regions(&self, config: &Config, output: Size<i32, Logical>) -> Vec<GlassRegion> {
        if !self.active || !config.effects.enabled || !config.effects.blur {
            return Vec::new();
        }
        let card_width = 460.min(output.w - 64).max(320);
        let card_height = 330;
        let x = (output.w - card_width) / 2;
        let y = (output.h - card_height) / 2;
        vec![
            GlassRegion::new(0, 0, output.w, output.h, 0),
            GlassRegion::new(x, y, card_width, card_height, 28),
        ]
    }

    pub fn render(
        &self,
        fonts: &Fonts,
        config: &Config,
        theme: Option<&ThemeDocument>,
        output: Size<i32, Logical>,
        glass: Option<&GlassBackdrop>,
    ) -> Option<crate::render::Pixmap> {
        if !self.active {
            return None;
        }
        let mut canvas = Canvas::new(output.w, output.h)?;
        let glass_enabled = config.effects.enabled && config.effects.blur;
        let glow = config.effects.enabled && config.effects.glow;
        let dim = config.lock.background_dim.clamp(0.0, 1.0);
        if glass_enabled && glass.is_some() {
            canvas.fill_round_rect_glass(0, 0, output.w, output.h, 0, glass, (0, 0), TRANSPARENT);
        }
        canvas.fill_rect(
            0,
            0,
            output.w,
            output.h,
            theme
                .map(|theme| rgba(theme.colors.background, (dim * 0.82 * 255.0).round() as u8))
                .unwrap_or_else(|| UiColor::from_css(&config.theme.background, dim * 0.82)),
        );

        let card_width = 460.min(output.w - 64).max(320);
        let card_height = 330;
        let x = (output.w - card_width) / 2;
        let y = (output.h - card_height) / 2;
        let panel = glass_tint(
            config,
            theme,
            if glass_enabled && glass.is_some() {
                config.effects.lock_opacity
            } else {
                0.92
            },
        );
        let (accent, text, muted, highlight, border, field_tint, error_color) =
            if let Some(theme) = theme {
                let colors = &theme.colors;
                (
                    rgba(colors.primary, 255),
                    rgba(colors.on_surface, 255),
                    rgba(colors.on_surface_variant, 200),
                    rgba(colors.glass_highlight, 28),
                    rgba(colors.glass_border, 170),
                    rgba(colors.surface_overlay, 140),
                    rgba(colors.error, 255),
                )
            } else {
                (
                    UiColor::from_css(&config.theme.accent, 1.0),
                    UiColor::from_css(&config.theme.foreground, 1.0),
                    UiColor::from_css(&config.theme.foreground, 0.58),
                    UiColor::from_css(&config.theme.foreground, 0.12),
                    UiColor::from_css(&config.theme.accent, 0.38),
                    UiColor::from_css(&config.theme.background, 0.55),
                    UiColor::rgba(248, 113, 113, 255),
                )
            };
        canvas.fill_round_rect_glass(
            x,
            y,
            card_width,
            card_height,
            28,
            glass.filter(|_| glass_enabled),
            (0, 0),
            panel,
        );
        canvas.fill_round_rect_gradient(x, y, card_width, card_height, 28, highlight, TRANSPARENT);
        if glow {
            canvas.glow_round_rect(x, y, card_width, card_height, 24, accent, 14);
        }
        canvas.stroke_round_rect(x, y, card_width, card_height, 24, 1, border);
        canvas.fill_round_rect(x + card_width / 2 - 56, y + 1, 112, 2, 1, accent);

        // HUD corner brackets around the login card.
        let bracket = 18;
        let inset = 10;
        for (corner_x, corner_y, dx, dy) in [
            (x + inset, y + inset, 1, 1),
            (x + card_width - inset, y + inset, -1, 1),
            (x + inset, y + card_height - inset, 1, -1),
            (x + card_width - inset, y + card_height - inset, -1, -1),
        ] {
            canvas.line(
                (corner_x as f32, corner_y as f32),
                ((corner_x + dx * bracket) as f32, corner_y as f32),
                1.5,
                accent.with_alpha(150),
            );
            canvas.line(
                (corner_x as f32, corner_y as f32),
                (corner_x as f32, (corner_y + dy * bracket) as f32),
                1.5,
                accent.with_alpha(150),
            );
        }

        canvas.text_center(fonts, "BUBBLEOS", output.w / 2, y + 34, 22.0, accent, true);
        canvas.text_center(
            fonts,
            "Session verrouillée",
            output.w / 2,
            y + 66,
            11.0,
            muted,
            false,
        );

        if config.lock.show_clock {
            let (time, date) = clock_strings();
            canvas.text_center(fonts, &time, output.w / 2, y + 96, 42.0, text, true);
            canvas.text_center(fonts, &date, output.w / 2, y + 142, 12.0, muted, false);
        }

        let user = auth::current_user().unwrap_or_else(|| "bubble".into());
        canvas.text_center(fonts, &user, output.w / 2, y + 172, 13.0, text, false);

        let field_x = x + 48;
        let field_width = card_width - 96;
        let field_y = y + 202;
        canvas.fill_round_rect_glass(
            field_x,
            field_y,
            field_width,
            46,
            12,
            glass.filter(|_| glass_enabled),
            (0, 0),
            field_tint,
        );
        canvas.stroke_round_rect(
            field_x,
            field_y,
            field_width,
            46,
            12,
            1,
            border.with_alpha(180),
        );
        if glow {
            canvas.glow_round_rect(field_x, field_y, field_width, 46, 12, accent, 5);
        }
        let bullets = "●".repeat(self.password.chars().count().min(32));
        let display = if bullets.is_empty() {
            "Mot de passe".to_owned()
        } else {
            bullets
        };
        let color = if self.password.is_empty() {
            muted
        } else {
            text
        };
        canvas.text(
            fonts,
            &display,
            field_x + 16,
            field_y + 12,
            16.0,
            color,
            false,
        );

        if let Some(message) = &self.message {
            let color = if self.message_is_error {
                error_color
            } else {
                muted
            };
            canvas.text_center(
                fonts,
                message,
                output.w / 2,
                field_y + 54,
                11.0,
                color,
                false,
            );
        } else {
            canvas.text_center(
                fonts,
                "Entrée pour déverrouiller",
                output.w / 2,
                field_y + 54,
                10.5,
                muted,
                false,
            );
        }

        let dots = self.password.chars().count().min(12) as i32;
        for index in 0..dots {
            canvas.fill_round_rect(
                output.w / 2 - dots * 7 + index * 14,
                y + card_height - 34,
                8,
                8,
                4,
                accent.with_alpha(180),
            );
        }

        Some(canvas.into_pixmap((0, 0)))
    }
}

fn rgba(color: Rgb, alpha: u8) -> UiColor {
    UiColor::rgba(color.r, color.g, color.b, alpha)
}

/// Returns the local time and date using libc so no chrono dependency is needed.
pub fn clock_strings() -> (String, String) {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut local: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut local).is_null() {
            return ("--:--".into(), String::new());
        }
        let mut time = [0i8; 16];
        let mut date = [0i8; 64];
        libc::strftime(time.as_mut_ptr(), time.len(), c"%H:%M".as_ptr(), &local);
        libc::strftime(
            date.as_mut_ptr(),
            date.len(),
            c"%A %d %B %Y".as_ptr(),
            &local,
        );
        (
            CStr::from_ptr(time.as_ptr()).to_string_lossy().into_owned(),
            CStr::from_ptr(date.as_ptr()).to_string_lossy().into_owned(),
        )
    }
}
