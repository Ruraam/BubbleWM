//! Built-in BubbleOS top bar.
//!
//! The bar is rendered by the compositor itself and consumes the BubbleColor
//! palette, so it follows the wallpaper without an external shell process.
//! Only the active Wi-Fi SSID is displayed; no addresses or interface names.

use smithay::utils::{Logical, Size};

use crate::{
    config::Config,
    render::Pixmap,
    status::SessionStatus,
    theme::{Rgb, ThemeDocument},
    ui::{Canvas, Fonts, GlassBackdrop, GlassRegion, TRANSPARENT, UiColor, glass_tint},
};

const PILL_GAP: i32 = 8;
const PILL_PADDING: i32 = 12;
const WORKSPACE_WIDTH: i32 = 30;
const CLOCK_WIDTH: i32 = 96;
const POWER_WIDTH: i32 = 36;
const VOLUME_PADDING: i32 = 10;

/// A clickable region of the bar and the action it triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarAction {
    Launcher,
    Workspace(u32),
    Network,
    WindowMenu,
    ToggleMute,
    Lock,
}

#[derive(Debug, Clone, Copy)]
pub struct BarHit {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub action: BarAction,
}

impl BarHit {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x as f64
            && x < (self.x + self.width) as f64
            && y >= self.y as f64
            && y < (self.y + self.height) as f64
    }
}

/// Dynamic values rendered by the bar.
pub struct BarModel<'a> {
    pub workspaces: u32,
    pub active_workspace: u32,
    pub occupied: &'a [bool],
    pub title: Option<&'a str>,
    pub status: &'a SessionStatus,
    pub clock: &'a str,
}

pub struct BarFrame {
    pub pixmap: Pixmap,
    pub hits: Vec<BarHit>,
    pub regions: Vec<GlassRegion>,
}

struct BarColors {
    glass: UiColor,
    outline: UiColor,
    text: UiColor,
    muted: UiColor,
    accent: UiColor,
    accent_soft: UiColor,
    on_accent: UiColor,
    warning: UiColor,
    error: UiColor,
    highlight: UiColor,
}

impl BarColors {
    fn from_config(config: &Config, theme: Option<&ThemeDocument>) -> Self {
        let glass_alpha = config.effects.bar_opacity.clamp(0.0, 1.0);
        if let Some(theme) = theme {
            let colors = &theme.colors;
            return Self {
                glass: glass_tint(config, Some(theme), glass_alpha),
                outline: rgba(colors.outline, 128),
                text: rgba(colors.on_surface, 255),
                muted: rgba(colors.on_surface_variant, 190),
                accent: rgba(colors.primary, 255),
                accent_soft: rgba(colors.primary, 90),
                on_accent: rgba(colors.on_primary, 255),
                warning: rgba(colors.warning, 255),
                error: rgba(colors.error, 255),
                highlight: rgba(colors.on_surface, 26),
            };
        }
        Self {
            glass: glass_tint(config, None, glass_alpha),
            outline: UiColor::from_css(&config.theme.accent, 0.42),
            text: UiColor::from_css(&config.theme.foreground, 1.0),
            muted: UiColor::from_css(&config.theme.foreground, 0.62),
            accent: UiColor::from_css(&config.theme.accent, 1.0),
            accent_soft: UiColor::from_css(&config.theme.accent, 0.35),
            on_accent: UiColor::from_css(&config.theme.background, 1.0),
            warning: UiColor::rgba(240, 184, 64, 255),
            error: UiColor::rgba(220, 76, 86, 255),
            highlight: UiColor::rgba(255, 255, 255, 26),
        }
    }
}

fn rgba(color: Rgb, alpha: u8) -> UiColor {
    UiColor::rgba(color.r, color.g, color.b, alpha)
}

pub fn render(
    fonts: &Fonts,
    config: &Config,
    output: Size<i32, Logical>,
    model: &BarModel<'_>,
    theme: Option<&ThemeDocument>,
    glass: Option<&GlassBackdrop>,
) -> Option<BarFrame> {
    if !config.bar.enabled || output.w <= 0 || output.h <= 0 {
        return None;
    }
    let margin = config.bar.margin.clamp(0, 64);
    let height = config.bar.height.clamp(20, 64);
    let canvas_height = margin * 2 + height;
    let mut canvas = Canvas::new(output.w, canvas_height)?;
    canvas.fill(TRANSPARENT);

    let colors = BarColors::from_config(config, theme);
    let glow = config.effects.enabled && config.effects.glow;
    let y = margin;
    let radius = height / 2;
    let mut hits = Vec::new();
    let mut x = margin + 6;

    if config.bar.show_logo {
        let text = "BubbleOS";
        let text_width = fonts_text_width(fonts, text, 12.5, true);
        let width = 22 + text_width + PILL_PADDING * 2;
        draw_pill(
            &mut canvas,
            x,
            y,
            width,
            height,
            radius,
            &colors,
            glass,
            glow,
        );
        if glow {
            canvas.glow_round_rect(x, y, width, height, radius, colors.accent, 5);
        }
        canvas.fill_circle(x + PILL_PADDING + 4, y + height / 2, 4, colors.accent);
        canvas.fill_circle(
            x + PILL_PADDING + 10,
            y + height / 2 - 3,
            2,
            colors.accent.with_alpha(180),
        );
        canvas.text(
            fonts,
            text,
            x + PILL_PADDING + 18,
            y + (height - 16) / 2,
            12.5,
            colors.accent,
            true,
        );
        hits.push(BarHit {
            x,
            y,
            width,
            height,
            action: BarAction::Launcher,
        });
        x += width + PILL_GAP;
    }

    if config.bar.show_wifi {
        let ssid = model.status.ssid.as_deref().unwrap_or("hors ligne");
        let text_width = fonts_text_width(fonts, ssid, 11.0, false);
        let width = 20 + 6 + text_width + PILL_PADDING * 2 + 14;
        draw_pill(
            &mut canvas,
            x,
            y,
            width,
            height,
            radius,
            &colors,
            glass,
            false,
        );
        let connected = model.status.ssid.is_some();
        draw_wifi_icon(
            &mut canvas,
            x + PILL_PADDING,
            y + height / 2,
            if connected {
                colors.accent
            } else {
                colors.muted
            },
        );
        canvas.text(
            fonts,
            ssid,
            x + PILL_PADDING + 20 + 6,
            y + (height - 14) / 2,
            11.0,
            if connected { colors.text } else { colors.muted },
            false,
        );
        // A discreet chevron shows that the capsule opens the connectivity
        // panel, without adding another visible label.
        let chevron_x = x + width - PILL_PADDING - 4;
        let chevron_y = y + height / 2 - 1;
        canvas.line(
            ((chevron_x - 3) as f32, (chevron_y - 2) as f32),
            (chevron_x as f32, (chevron_y + 2) as f32),
            1.4,
            colors.muted,
        );
        canvas.line(
            (chevron_x as f32, (chevron_y + 2) as f32),
            ((chevron_x + 3) as f32, (chevron_y - 2) as f32),
            1.4,
            colors.muted,
        );
        hits.push(BarHit {
            x,
            y,
            width,
            height,
            action: BarAction::Network,
        });
        x += width + PILL_GAP;
    }

    if config.bar.show_workspaces {
        let clock_left = output.w / 2 - CLOCK_WIDTH / 2;
        for index in 0..model.workspaces {
            if x + WORKSPACE_WIDTH + PILL_GAP > clock_left - PILL_GAP {
                break;
            }
            let active = index == model.active_workspace;
            let occupied = model.occupied.get(index as usize).copied().unwrap_or(false);
            if active {
                if let Some(glass) = glass {
                    canvas.fill_round_rect_glass(
                        x,
                        y,
                        WORKSPACE_WIDTH,
                        height,
                        radius,
                        Some(glass),
                        (0, 0),
                        colors.accent_soft,
                    );
                } else {
                    canvas.fill_round_rect(x, y, WORKSPACE_WIDTH, height, radius, colors.accent);
                }
                canvas.fill_round_rect_gradient(
                    x,
                    y,
                    WORKSPACE_WIDTH,
                    height,
                    radius,
                    colors.accent.with_alpha(210),
                    colors.accent.with_alpha(150),
                );
                if glow {
                    canvas.glow_round_rect(x, y, WORKSPACE_WIDTH, height, radius, colors.accent, 4);
                }
            } else {
                draw_pill(
                    &mut canvas,
                    x,
                    y,
                    WORKSPACE_WIDTH,
                    height,
                    radius,
                    &colors,
                    glass,
                    false,
                );
            }
            let outline = if active {
                colors.accent
            } else if occupied {
                colors.outline
            } else {
                colors.outline.with_alpha(70)
            };
            canvas.stroke_round_rect(x, y, WORKSPACE_WIDTH, height, radius, 1, outline);
            if occupied && !active {
                canvas.fill_circle(x + WORKSPACE_WIDTH / 2, y + height - 5, 2, colors.accent);
            }
            canvas.text_center(
                fonts,
                &(index + 1).to_string(),
                x + WORKSPACE_WIDTH / 2,
                y + (height - 14) / 2,
                11.0,
                if active {
                    colors.on_accent
                } else if occupied {
                    colors.text
                } else {
                    colors.muted
                },
                true,
            );
            hits.push(BarHit {
                x,
                y,
                width: WORKSPACE_WIDTH,
                height,
                action: BarAction::Workspace(index),
            });
            x += WORKSPACE_WIDTH + 6;
        }
        x += PILL_GAP - 6;
    }

    if config.bar.show_title {
        let clock_left = output.w / 2 - CLOCK_WIDTH / 2;
        let right_limit = output.w
            - margin
            - POWER_WIDTH
            - PILL_GAP
            - if config.bar.show_volume { 96 } else { 0 }
            - PILL_GAP;
        let available = (clock_left.min(right_limit) - PILL_GAP - x).max(0);
        let width = available.min(320);
        if width >= 96 {
            let title = model.title.unwrap_or("Bureau");
            let text = canvas.ellipsize(fonts, title, 11.0, width - PILL_PADDING * 2 - 12);
            draw_pill(
                &mut canvas,
                x,
                y,
                width,
                height,
                radius,
                &colors,
                glass,
                false,
            );
            canvas.fill_circle(x + PILL_PADDING, y + height / 2, 3, colors.accent);
            canvas.text(
                fonts,
                &text,
                x + PILL_PADDING + 12,
                y + (height - 14) / 2,
                11.0,
                colors.muted,
                false,
            );
            hits.push(BarHit {
                x,
                y,
                width,
                height,
                action: BarAction::WindowMenu,
            });
        }
    }

    if config.bar.show_clock {
        let clock_x = output.w / 2 - CLOCK_WIDTH / 2;
        draw_pill(
            &mut canvas,
            clock_x,
            y,
            CLOCK_WIDTH,
            height,
            radius,
            &colors,
            glass,
            true,
        );
        canvas.text_center(
            fonts,
            model.clock,
            output.w / 2,
            y + (height - 17) / 2,
            14.0,
            colors.text,
            true,
        );
    }

    let mut right = output.w - margin - 6;

    if config.bar.show_power {
        right -= POWER_WIDTH;
        draw_pill(
            &mut canvas,
            right,
            y,
            POWER_WIDTH,
            height,
            radius,
            &colors,
            glass,
            false,
        );
        draw_power_icon(
            &mut canvas,
            right + POWER_WIDTH / 2,
            y + height / 2,
            colors.error,
        );
        hits.push(BarHit {
            x: right,
            y,
            width: POWER_WIDTH,
            height,
            action: BarAction::Lock,
        });
        right -= PILL_GAP;
    }

    if config.bar.show_volume {
        let volume = model.status.volume.unwrap_or(0);
        let label = if model.status.muted {
            "muet".to_owned()
        } else {
            format!("{volume}%")
        };
        let text_width = fonts_text_width(fonts, &label, 11.0, false);
        let width = 20 + 6 + text_width + VOLUME_PADDING * 2;
        right -= width;
        draw_pill(
            &mut canvas,
            right,
            y,
            width,
            height,
            radius,
            &colors,
            glass,
            false,
        );
        draw_volume_icon(
            &mut canvas,
            right + VOLUME_PADDING,
            y + height / 2,
            if model.status.muted {
                colors.warning
            } else {
                colors.accent
            },
            model.status.muted,
        );
        canvas.text(
            fonts,
            &label,
            right + VOLUME_PADDING + 20 + 6,
            y + (height - 14) / 2,
            11.0,
            if model.status.muted {
                colors.warning
            } else {
                colors.text
            },
            false,
        );
        hits.push(BarHit {
            x: right,
            y,
            width,
            height,
            action: BarAction::ToggleMute,
        });
    }

    let mut regions = hits
        .iter()
        .map(|hit| GlassRegion::new(hit.x, hit.y, hit.width, hit.height, radius))
        .collect::<Vec<_>>();
    if config.bar.show_clock {
        regions.push(GlassRegion::new(
            output.w / 2 - CLOCK_WIDTH / 2,
            y,
            CLOCK_WIDTH,
            height,
            radius,
        ));
    }
    Some(BarFrame {
        pixmap: canvas.into_pixmap((0, 0)),
        hits,
        regions,
    })
}

#[allow(clippy::too_many_arguments)]
fn draw_pill(
    canvas: &mut Canvas,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    radius: i32,
    colors: &BarColors,
    glass: Option<&GlassBackdrop>,
    glow: bool,
) {
    canvas.fill_round_rect_glass(x, y, width, height, radius, glass, (0, 0), colors.glass);
    canvas.fill_round_rect_gradient(x, y, width, height, radius, colors.highlight, TRANSPARENT);
    if glow {
        canvas.glow_round_rect(x, y, width, height, radius, colors.accent, 3);
    }
    canvas.stroke_round_rect(x, y, width, height, radius, 1, colors.outline);
}

fn fonts_text_width(fonts: &Fonts, text: &str, size: f32, display: bool) -> i32 {
    let canvas = Canvas::new(1, 1).expect("one-pixel canvas");
    canvas.text_width(fonts, text, size, display)
}

fn draw_wifi_icon(canvas: &mut Canvas, x: i32, center_y: i32, color: UiColor) {
    for (index, height) in [4, 8, 12].into_iter().enumerate() {
        canvas.fill_round_rect(
            x + index as i32 * 6,
            center_y + 6 - height,
            4,
            height,
            1,
            color,
        );
    }
}

fn draw_volume_icon(canvas: &mut Canvas, x: i32, center_y: i32, color: UiColor, muted: bool) {
    canvas.fill_rect(x, center_y - 3, 4, 6, color);
    canvas.fill_polygon(
        &[
            (x as f32 + 4.0, center_y as f32 - 3.0),
            (x as f32 + 9.0, center_y as f32 - 7.0),
            (x as f32 + 9.0, center_y as f32 + 7.0),
            (x as f32 + 4.0, center_y as f32 + 3.0),
        ],
        color,
    );
    if muted {
        canvas.line(
            (x as f32 + 12.0, center_y as f32 - 3.0),
            (x as f32 + 16.0, center_y as f32 + 3.0),
            1.5,
            color,
        );
        canvas.line(
            (x as f32 + 16.0, center_y as f32 - 3.0),
            (x as f32 + 12.0, center_y as f32 + 3.0),
            1.5,
            color,
        );
    } else {
        canvas.stroke_circle(x + 11, center_y, 5, 1, color);
    }
}

fn draw_power_icon(canvas: &mut Canvas, center_x: i32, center_y: i32, color: UiColor) {
    canvas.stroke_circle(center_x, center_y, 6, 1, color);
    canvas.line(
        (center_x as f32, center_y as f32 - 9.0),
        (center_x as f32, center_y as f32 - 1.0),
        2.0,
        color,
    );
}
