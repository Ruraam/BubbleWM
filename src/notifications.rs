//! Freedesktop notification service and overlay (replaces mako).

use std::{
    collections::HashMap,
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use smithay::utils::{Logical, Size};
use tracing::{info, warn};
use zbus::zvariant::OwnedValue;

use crate::{
    config::{Config, NotificationsConfig},
    theme::{Rgb, ThemeDocument},
    ui::{Canvas, Fonts, GlassBackdrop, GlassRegion, TRANSPARENT, UiColor, glass_tint},
};

#[derive(Debug, Clone)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    pub summary: String,
    pub body: String,
    pub created: Instant,
    pub timeout: Option<Duration>,
}

#[derive(Debug)]
pub enum NotificationEvent {
    Notify {
        app_name: String,
        summary: String,
        body: String,
        expire_timeout: i32,
    },
    Close(u32),
}

#[derive(Debug, Default)]
pub struct NotificationCenter {
    notifications: Vec<Notification>,
    next_id: u32,
    dirty: bool,
}

impl NotificationCenter {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            dirty: true,
            ..Default::default()
        }
    }

    pub fn handle_event(&mut self, event: NotificationEvent, config: &NotificationsConfig) {
        match event {
            NotificationEvent::Notify {
                app_name,
                summary,
                body,
                expire_timeout,
            } => {
                let id = self.next_id;
                self.next_id = self.next_id.wrapping_add(1).max(1);
                let timeout = match expire_timeout {
                    0 => None,
                    value if value > 0 => Some(Duration::from_millis(value as u64)),
                    _ => Some(Duration::from_millis(config.timeout as u64)),
                };
                self.notifications.push(Notification {
                    id,
                    app_name,
                    summary,
                    body,
                    created: Instant::now(),
                    timeout,
                });
                while self.notifications.len() > config.max_visible.max(1) + 16 {
                    self.notifications.remove(0);
                }
                self.dirty = true;
            }
            NotificationEvent::Close(id) => {
                self.notifications
                    .retain(|notification| notification.id != id);
                self.dirty = true;
            }
        }
    }

    pub fn expire(&mut self) -> bool {
        let before = self.notifications.len();
        self.notifications.retain(|notification| {
            notification
                .timeout
                .map(|timeout| notification.created.elapsed() < timeout)
                .unwrap_or(true)
        });
        if before != self.notifications.len() {
            self.dirty = true;
        }
        self.dirty
    }

    pub fn clear_dirty(&mut self) -> bool {
        let dirty = self.dirty;
        self.dirty = false;
        dirty
    }

    pub fn is_empty(&self) -> bool {
        self.notifications.is_empty()
    }

    /// Card geometry used by the live backdrop blur.
    pub fn glass_regions(&self, config: &Config, output: Size<i32, Logical>) -> Vec<GlassRegion> {
        if !config.notifications.enabled || self.notifications.is_empty() {
            return Vec::new();
        }
        let width = config.notifications.width.min(output.w - 32).max(200);
        let margin = config.notifications.margin.max(0);
        let visible = self
            .notifications
            .iter()
            .rev()
            .take(config.notifications.max_visible.max(1))
            .collect::<Vec<_>>();
        let mut heights = Vec::with_capacity(visible.len());
        for notification in &visible {
            let body_lines = if notification.body.is_empty() { 0 } else { 3 };
            heights.push(64 + body_lines * 18);
        }
        let total_height: i32 = heights.iter().sum::<i32>() + (visible.len() as i32 - 1) * 10;
        if total_height <= 0 {
            return Vec::new();
        }
        let x = match config.notifications.position.as_str() {
            "top-left" | "bottom-left" => margin,
            _ => output.w - width - margin,
        };
        let y = match config.notifications.position.as_str() {
            "bottom-left" | "bottom-right" => output.h - total_height - margin,
            _ => margin,
        };
        let mut regions = Vec::with_capacity(heights.len());
        let mut card_y = 0;
        for height in heights {
            regions.push(GlassRegion::new(
                x.max(0),
                (y + card_y).max(0),
                width,
                height,
                20,
            ));
            card_y += height + 10;
        }
        regions
    }

    pub fn render(
        &self,
        fonts: &Fonts,
        config: &Config,
        theme: Option<&ThemeDocument>,
        output: Size<i32, Logical>,
        glass: Option<&GlassBackdrop>,
    ) -> Option<crate::render::Pixmap> {
        if !config.notifications.enabled || self.notifications.is_empty() {
            return None;
        }
        let width = config.notifications.width.min(output.w - 32).max(200);
        let margin = config.notifications.margin.max(0);
        let visible = self
            .notifications
            .iter()
            .rev()
            .take(config.notifications.max_visible.max(1))
            .collect::<Vec<_>>();
        let mut heights = Vec::with_capacity(visible.len());
        for notification in &visible {
            let body_lines = if notification.body.is_empty() { 0 } else { 3 };
            heights.push(64 + body_lines * 18);
        }
        let total_height: i32 = heights.iter().sum::<i32>() + (visible.len() as i32 - 1) * 10;
        if total_height <= 0 {
            return None;
        }
        let x = match config.notifications.position.as_str() {
            "top-left" | "bottom-left" => margin,
            _ => output.w - width - margin,
        };
        let y = match config.notifications.position.as_str() {
            "bottom-left" | "bottom-right" => output.h - total_height - margin,
            _ => margin,
        };
        let origin = (x.max(0), y.max(0));
        let mut canvas = Canvas::new(width, total_height)?;

        let glass_enabled = config.effects.enabled && config.effects.blur;
        let glow = config.effects.enabled && config.effects.glow;
        let panel = glass_tint(
            config,
            theme,
            if glass_enabled && glass.is_some() {
                config.effects.notification_opacity
            } else {
                0.96
            },
        );
        let (accent, text, muted, highlight, border, badge_text) = if let Some(theme) = theme {
            let colors = &theme.colors;
            (
                rgba(colors.primary, 255),
                rgba(colors.on_surface, 255),
                rgba(colors.on_surface_variant, 200),
                rgba(colors.glass_highlight, 22),
                rgba(colors.glass_border, 160),
                rgba(colors.on_primary, 255),
            )
        } else {
            (
                UiColor::from_css(&config.theme.accent, 1.0),
                UiColor::from_css(&config.theme.foreground, 1.0),
                UiColor::from_css(&config.theme.foreground, 0.60),
                UiColor::from_css(&config.theme.foreground, 0.10),
                UiColor::from_css(&config.theme.accent, 0.55),
                UiColor::from_css(&config.theme.background, 1.0),
            )
        };
        let mut card_y = 0;
        for (notification, height) in visible.iter().zip(heights.iter()) {
            canvas.fill_round_rect_glass(
                0,
                card_y,
                width,
                *height,
                20,
                glass.filter(|_| glass_enabled),
                origin,
                panel,
            );
            canvas.fill_round_rect_gradient(0, card_y, width, *height, 20, highlight, TRANSPARENT);
            if glow {
                canvas.glow_round_rect(0, card_y, width, *height, 20, accent, 8);
            }
            canvas.stroke_round_rect(0, card_y, width, *height, 16, 1, border);
            canvas.fill_round_rect(1, card_y + 12, 3, height - 24, 2, accent);
            canvas.glow_round_rect(1, card_y + 12, 3, height - 24, 2, accent, 4);

            let badge = notification
                .app_name
                .chars()
                .next()
                .map(|character| character.to_uppercase().to_string())
                .unwrap_or_else(|| "?".into());
            canvas.fill_round_rect_gradient(
                16,
                card_y + 16,
                30,
                30,
                10,
                accent.with_alpha(220),
                accent.with_alpha(120),
            );
            canvas.text_center(fonts, &badge, 31, card_y + 22, 14.0, badge_text, true);

            let app = canvas.ellipsize(fonts, &notification.app_name, 11.0, width - 76);
            canvas.text(fonts, &app, 56, card_y + 10, 11.0, accent, false);
            let summary = canvas.ellipsize(fonts, &notification.summary, 15.0, width - 76);
            canvas.text(fonts, &summary, 56, card_y + 26, 15.0, text, false);
            if !notification.body.is_empty() {
                let lines = canvas.wrap(fonts, &notification.body, 11.5, width - 76, 3);
                for (line, content) in lines.iter().enumerate() {
                    canvas.text(
                        fonts,
                        content,
                        56,
                        card_y + 50 + line as i32 * 18,
                        11.5,
                        muted,
                        false,
                    );
                }
            }
            card_y += height + 10;
        }

        Some(canvas.into_pixmap(origin))
    }
}

fn rgba(color: Rgb, alpha: u8) -> UiColor {
    UiColor::rgba(color.r, color.g, color.b, alpha)
}

struct Service {
    sender: Mutex<mpsc::Sender<NotificationEvent>>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Service {
    #[zbus(name = "GetCapabilities")]
    fn get_capabilities(&self) -> Vec<String> {
        vec![
            "body".into(),
            "body-markup".into(),
            "persistence".into(),
            "icon-static".into(),
            "actions".into(),
        ]
    }

    #[zbus(name = "GetServerInformation")]
    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "BubbleWM".into(),
            "BubbleOS".into(),
            env!("CARGO_PKG_VERSION").into(),
            "1.2".into(),
        )
    }

    #[zbus(name = "Notify")]
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        _replaces_id: u32,
        _app_icon: String,
        summary: String,
        body: String,
        _actions: Vec<String>,
        _hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        self.send(NotificationEvent::Notify {
            app_name,
            summary,
            body,
            expire_timeout,
        });
        0
    }

    #[zbus(name = "CloseNotification")]
    fn close_notification(&self, id: u32) {
        self.send(NotificationEvent::Close(id));
    }
}

impl Service {
    fn send(&self, event: NotificationEvent) {
        if let Ok(sender) = self.sender.lock() {
            let _ = sender.send(event);
        }
    }
}

/// Starts the session-bus notification service on a dedicated thread.
pub fn start_service(sender: mpsc::Sender<NotificationEvent>) {
    thread::Builder::new()
        .name("bubblewm-notifications".into())
        .spawn(move || {
            let connection = match zbus::blocking::Connection::session() {
                Ok(connection) => connection,
                Err(error) => {
                    warn!(%error, "cannot connect to the session bus; notifications are disabled");
                    return;
                }
            };
            let service = Service {
                sender: Mutex::new(sender),
            };
            if let Err(error) = connection
                .object_server()
                .at("/org/freedesktop/Notifications", service)
            {
                warn!(%error, "cannot export the notification object");
                return;
            }
            if let Err(error) = connection.request_name("org.freedesktop.Notifications") {
                warn!(%error, "cannot own org.freedesktop.Notifications");
                return;
            }
            info!("notification service ready on org.freedesktop.Notifications");
            loop {
                thread::park_timeout(Duration::from_secs(3600));
            }
        })
        .ok();
}
