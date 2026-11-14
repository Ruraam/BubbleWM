//! Compositor-native popup menus.
//!
//! One renderer serves the desktop context menu, the taskbar window list and
//! the network configuration menu. Every menu uses the BubbleColor palette,
//! the blurred wallpaper backdrop and the configured corner radius.

use std::time::Instant;

use smithay::{
    input::keyboard::Keysym,
    utils::{Logical, Size},
};

use crate::{
    config::Config,
    render::Pixmap,
    status::{SavedConnection, SessionStatus, WifiNetwork},
    theme::ThemeDocument,
    ui::{Canvas, Fonts, GlassBackdrop, GlassRegion, TRANSPARENT, UiColor, glass_tint},
};

const ROW_HEIGHT: i32 = 38;
const HEADER_HEIGHT: i32 = 48;
const FOOTER_HEIGHT: i32 = 26;
const PADDING: i32 = 8;
const SEPARATOR_HEIGHT: i32 = 9;
const SECTION_HEIGHT: i32 = 24;
const RADIUS: i32 = 22;
/// Opening animation duration, in seconds.
const OPEN_DURATION: f32 = 0.22;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    None,
    Close,
    Terminal,
    Launcher,
    Settings,
    Lock,
    Reload,
    FocusWindow(u64),
    MinimizeWindow(u64),
    CloseWindow(u64),
    WifiConnect(String),
    WifiConnectAsk(String),
    WifiConnectSaved(String),
    WifiDisconnect,
    WifiToggle,
    BluetoothConnect(String),
    BluetoothDisconnect(String),
    BluetoothToggle,
    BluetoothScan,
    NetworkSettings,
}

#[derive(Debug, Clone, Copy)]
pub enum Icon {
    Terminal,
    Search,
    Settings,
    Lock,
    Reload,
    Window,
    Wifi {
        signal: u8,
        secured: bool,
        active: bool,
    },
    WifiToggle(bool),
    BluetoothToggle(bool),
    BluetoothDevice {
        connected: bool,
    },
    Scan,
}

#[derive(Debug, Clone)]
pub struct PopupItem {
    pub label: String,
    pub detail: String,
    pub icon: Icon,
    pub action: MenuAction,
    pub minimize: Option<MenuAction>,
    pub close: Option<MenuAction>,
    pub enabled: bool,
}

impl PopupItem {
    pub fn new(
        label: impl Into<String>,
        detail: impl Into<String>,
        icon: Icon,
        action: MenuAction,
    ) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            icon,
            action,
            minimize: None,
            close: None,
            enabled: true,
        }
    }

    pub fn with_window_buttons(mut self, minimize: MenuAction, close: MenuAction) -> Self {
        self.minimize = Some(minimize);
        self.close = Some(close);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HitRegion {
    Row,
    Minimize,
    Close,
}

pub struct PopupMenu {
    pub visible: bool,
    pub x: i32,
    pub y: i32,
    pub selected: usize,
    width: i32,
    title: Option<String>,
    subtitle: Option<String>,
    footer: Option<String>,
    items: Vec<PopupItem>,
    separators: Vec<usize>,
    sections: Vec<(usize, String)>,
    opened_at: Option<Instant>,
}

impl PopupMenu {
    fn empty() -> Self {
        Self {
            visible: false,
            x: 0,
            y: 0,
            selected: 0,
            width: 276,
            title: None,
            subtitle: None,
            footer: None,
            items: Vec::new(),
            separators: Vec::new(),
            sections: Vec::new(),
            opened_at: None,
        }
    }

    /// A hidden menu ready to be replaced by one of the constructors.
    pub fn hidden() -> Self {
        Self::empty()
    }

    pub fn desktop(x: i32, y: i32, output: Size<i32, Logical>) -> Self {
        let mut menu = Self::empty();
        menu.items = vec![
            PopupItem::new(
                "Terminal",
                "Super + Entrée",
                Icon::Terminal,
                MenuAction::Terminal,
            ),
            PopupItem::new(
                "Lanceur d’applications",
                "Super + D",
                Icon::Search,
                MenuAction::Launcher,
            ),
            PopupItem::new(
                "Paramètres BubbleOS",
                "",
                Icon::Settings,
                MenuAction::Settings,
            ),
            PopupItem::new(
                "Verrouiller la session",
                "Super + L",
                Icon::Lock,
                MenuAction::Lock,
            ),
            PopupItem::new(
                "Recharger BubbleWM",
                "SIGHUP",
                Icon::Reload,
                MenuAction::Reload,
            ),
        ];
        menu.separators = vec![2];
        menu.open(x, y, output);
        menu
    }

    pub fn windows(
        x: i32,
        y: i32,
        output: Size<i32, Logical>,
        title: String,
        subtitle: String,
        items: Vec<PopupItem>,
    ) -> Self {
        let mut menu = Self::empty();
        menu.width = 380;
        menu.title = Some(title);
        menu.subtitle = Some(subtitle);
        menu.footer = Some("Clic : activer   ·   − réduire   ·   × fermer".into());
        menu.items = items;
        menu.open(x, y, output);
        menu
    }

    pub fn network(x: i32, y: i32, output: Size<i32, Logical>, status: &SessionStatus) -> Self {
        let mut menu = Self::empty();
        menu.width = 384;
        menu.title = Some("Connectivité".into());
        let wifi_summary = if let Some(ssid) = &status.ssid {
            let signal = status
                .signal
                .map(|signal| format!(" · {signal}%"))
                .unwrap_or_default();
            format!("Wi-Fi · {ssid}{signal}")
        } else if !status.network_manager {
            "NetworkManager indisponible".into()
        } else if !status.wifi_enabled {
            "Wi-Fi désactivé".into()
        } else {
            "Wi-Fi · aucun réseau".into()
        };
        let bluetooth_summary = if !status.bluetooth_available {
            "Bluetooth indisponible".into()
        } else if !status.bluetooth_enabled {
            "Bluetooth désactivé".into()
        } else {
            let connected = status
                .bluetooth_devices
                .iter()
                .filter(|device| device.connected)
                .count();
            if connected > 0 {
                format!(
                    "Bluetooth · {connected} connecté{}",
                    if connected > 1 { "s" } else { "" }
                )
            } else {
                "Bluetooth · prêt".into()
            }
        };
        menu.subtitle = Some(format!("{wifi_summary}   ·   {bluetooth_summary}"));

        menu.sections.push((0, "Wi-Fi".into()));
        menu.items.push(PopupItem::new(
            "Wi-Fi",
            if status.wifi_enabled {
                "activé"
            } else {
                "désactivé"
            },
            Icon::WifiToggle(status.wifi_enabled),
            MenuAction::WifiToggle,
        ));

        if status.network_manager && status.wifi_enabled {
            let mut networks = status.networks.clone();
            networks.sort_by(|left, right| {
                right
                    .active
                    .cmp(&left.active)
                    .then(right.signal.cmp(&left.signal))
                    .then(left.ssid.cmp(&right.ssid))
            });
            let mut seen = std::collections::BTreeSet::new();
            let mut listed = 0;
            for WifiNetwork {
                ssid,
                signal,
                secured,
                active,
            } in networks
            {
                if ssid.is_empty() || !seen.insert(ssid.clone()) {
                    continue;
                }
                if listed >= 5 {
                    break;
                }
                listed += 1;
                let detail = if active {
                    format!("connecté · {signal}%")
                } else if secured {
                    format!("{signal}% · sécurisé")
                } else {
                    format!("{signal}% · ouvert")
                };
                let action = if secured {
                    MenuAction::WifiConnectAsk(ssid.clone())
                } else {
                    MenuAction::WifiConnect(ssid.clone())
                };
                menu.items.push(PopupItem::new(
                    ssid,
                    detail,
                    Icon::Wifi {
                        signal,
                        secured,
                        active,
                    },
                    action,
                ));
            }

            let mut profiles = 0;
            for SavedConnection { name, kind, active } in &status.saved {
                if kind != "802-11-wireless" || *active || profiles >= 2 {
                    continue;
                }
                profiles += 1;
                menu.items.push(PopupItem::new(
                    name.clone(),
                    "profil enregistré".to_owned(),
                    Icon::Window,
                    MenuAction::WifiConnectSaved(name.clone()),
                ));
            }
        }

        menu.separators.push(menu.items.len().saturating_sub(1));
        menu.sections.push((menu.items.len(), "Bluetooth".into()));
        menu.items.push(PopupItem::new(
            "Bluetooth",
            if status.bluetooth_enabled {
                "activé"
            } else {
                "désactivé"
            },
            Icon::BluetoothToggle(status.bluetooth_enabled),
            MenuAction::BluetoothToggle,
        ));

        if status.bluetooth_available && status.bluetooth_enabled {
            let mut devices = status.bluetooth_devices.clone();
            devices.sort_by(|left, right| {
                right
                    .connected
                    .cmp(&left.connected)
                    .then(left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            });
            for device in devices.iter().take(4) {
                let detail = if device.connected {
                    "connecté"
                } else {
                    "appairé"
                };
                let action = if device.connected {
                    MenuAction::BluetoothDisconnect(device.address.clone())
                } else {
                    MenuAction::BluetoothConnect(device.address.clone())
                };
                menu.items.push(PopupItem::new(
                    device.name.clone(),
                    detail,
                    Icon::BluetoothDevice {
                        connected: device.connected,
                    },
                    action,
                ));
            }
            menu.items.push(PopupItem::new(
                "Rechercher des appareils…",
                "8 s",
                Icon::Scan,
                MenuAction::BluetoothScan,
            ));
        }

        menu.separators.push(menu.items.len().saturating_sub(1));
        menu.items.push(PopupItem::new(
            "Paramètres réseau…",
            "nmtui",
            Icon::Settings,
            MenuAction::NetworkSettings,
        ));
        if status.ssid.is_some() {
            menu.items.push(PopupItem::new(
                "Se déconnecter du Wi-Fi",
                "",
                Icon::Wifi {
                    signal: 0,
                    secured: false,
                    active: false,
                },
                MenuAction::WifiDisconnect,
            ));
        }
        menu.footer = Some("Clic : connecter   ·   NetworkManager   ·   BlueZ".into());
        menu.open(x, y, output);
        menu
    }

    fn open(&mut self, x: i32, y: i32, output: Size<i32, Logical>) {
        let height = self.height();
        self.visible = true;
        self.selected = 0;
        self.opened_at = Some(Instant::now());
        self.x = x.clamp(8, (output.w - self.width - 8).max(8));
        self.y = y.clamp(8, (output.h - height - 8).max(8));
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.selected = 0;
        self.opened_at = None;
    }

    /// Opening progress in the `0.0..=1.0` range while the menu grows from the
    /// taskbar capsule.
    pub fn opening_progress(&self) -> Option<f32> {
        self.opened_at
            .map(|started| (started.elapsed().as_secs_f32() / OPEN_DURATION).clamp(0.0, 1.0))
    }

    pub fn animating(&self) -> bool {
        self.visible
            && self
                .opening_progress()
                .is_some_and(|progress| progress < 1.0)
    }

    pub fn width(&self) -> i32 {
        self.width
    }

    pub fn height(&self) -> i32 {
        self.header_height()
            + self.footer_height()
            + PADDING * 2
            + self.items.len() as i32 * ROW_HEIGHT
            + self.separator_count() * SEPARATOR_HEIGHT
            + self.sections.len() as i32 * SECTION_HEIGHT
    }

    fn header_height(&self) -> i32 {
        if self.title.is_some() || self.subtitle.is_some() {
            HEADER_HEIGHT
        } else {
            0
        }
    }

    fn footer_height(&self) -> i32 {
        if self.footer.is_some() {
            FOOTER_HEIGHT
        } else {
            0
        }
    }

    fn separator_count(&self) -> i32 {
        self.separators
            .iter()
            .filter(|index| **index < self.items.len().saturating_sub(1))
            .count() as i32
    }

    pub fn glass_region(&self) -> GlassRegion {
        GlassRegion::new(self.x, self.y, self.width, self.height(), RADIUS)
    }

    fn row_y(&self, index: usize) -> i32 {
        let mut y = self.header_height() + PADDING + index as i32 * ROW_HEIGHT;
        for separator in &self.separators {
            if *separator < index {
                y += SEPARATOR_HEIGHT;
            }
        }
        for (at, _) in &self.sections {
            if *at <= index {
                y += SECTION_HEIGHT;
            }
        }
        y
    }

    fn section_y(&self, index: usize) -> i32 {
        self.row_y(index) - SECTION_HEIGHT
    }

    fn hit_at(&self, x: f64, y: f64) -> Option<(usize, HitRegion)> {
        if !self.visible
            || x < self.x as f64
            || x >= (self.x + self.width) as f64
            || y < self.y as f64
            || y >= (self.y + self.height()) as f64
        {
            return None;
        }
        let local_x = x as i32 - self.x;
        let local_y = y as i32 - self.y;
        for index in 0..self.items.len() {
            let top = self.row_y(index);
            if local_y >= top && local_y < top + ROW_HEIGHT {
                let item = &self.items[index];
                if item.minimize.is_some()
                    && local_x >= self.width - 70
                    && local_x < self.width - 42
                {
                    return Some((index, HitRegion::Minimize));
                }
                if item.close.is_some() && local_x >= self.width - 40 && local_x < self.width - 12 {
                    return Some((index, HitRegion::Close));
                }
                return Some((index, HitRegion::Row));
            }
        }
        None
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        self.visible
            && x >= self.x as f64
            && x < (self.x + self.width) as f64
            && y >= self.y as f64
            && y < (self.y + self.height()) as f64
    }

    /// Updates the highlighted row. Returns `true` when a redraw is needed.
    pub fn handle_motion(&mut self, x: f64, y: f64) -> bool {
        if !self.visible {
            return false;
        }
        let row = self
            .hit_at(x, y)
            .map(|(index, _)| index)
            .unwrap_or(self.selected);
        if row != self.selected {
            self.selected = row;
            return true;
        }
        false
    }

    pub fn handle_click(&mut self, x: f64, y: f64) -> MenuAction {
        if !self.visible {
            return MenuAction::None;
        }
        match self.hit_at(x, y) {
            Some((index, HitRegion::Minimize)) => {
                let action = self.items[index]
                    .minimize
                    .clone()
                    .unwrap_or(MenuAction::None);
                self.close();
                action
            }
            Some((index, HitRegion::Close)) => {
                let action = self.items[index].close.clone().unwrap_or(MenuAction::None);
                self.close();
                action
            }
            Some((index, HitRegion::Row)) => {
                let action = if self.items[index].enabled {
                    self.items[index].action.clone()
                } else {
                    MenuAction::None
                };
                self.close();
                action
            }
            None => {
                self.close();
                MenuAction::Close
            }
        }
    }

    pub fn handle_key(&mut self, keysym: Keysym) -> MenuAction {
        if !self.visible || self.items.is_empty() {
            return MenuAction::None;
        }
        match keysym {
            Keysym::Up => {
                self.selected = self.selected.checked_sub(1).unwrap_or(self.items.len() - 1);
                MenuAction::None
            }
            Keysym::Down => {
                self.selected = (self.selected + 1) % self.items.len();
                MenuAction::None
            }
            Keysym::Return | Keysym::KP_Enter => {
                let action = self.items[self.selected].action.clone();
                self.close();
                action
            }
            Keysym::Escape => {
                self.close();
                MenuAction::Close
            }
            _ => MenuAction::None,
        }
    }

    pub fn render(
        &self,
        fonts: &Fonts,
        config: &Config,
        theme: Option<&ThemeDocument>,
        glass: Option<&GlassBackdrop>,
    ) -> Option<Pixmap> {
        if !self.visible {
            return None;
        }
        let height = self.height();
        let width = self.width;
        let mut canvas = Canvas::new(width, height)?;

        let glass_enabled = config.effects.enabled && config.effects.blur;
        let glow = config.effects.enabled && config.effects.glow;
        let panel = glass_tint(config, theme, config.effects.menu_opacity);
        let (outline, text, muted, accent, highlight, error) = if let Some(theme) = theme {
            let colors = &theme.colors;
            (
                rgba(colors.outline, 120),
                rgba(colors.on_surface, 255),
                rgba(colors.on_surface_variant, 190),
                rgba(colors.primary, 255),
                rgba(colors.on_surface, 24),
                rgba(colors.error, 255),
            )
        } else {
            (
                UiColor::from_css(&config.theme.accent, 0.38),
                UiColor::from_css(&config.theme.foreground, 1.0),
                UiColor::from_css(&config.theme.foreground, 0.62),
                UiColor::from_css(&config.theme.accent, 1.0),
                UiColor::rgba(255, 255, 255, 24),
                UiColor::rgba(248, 113, 113, 255),
            )
        };

        canvas.fill_round_rect_glass(
            0,
            0,
            width,
            height,
            RADIUS,
            glass.filter(|_| glass_enabled),
            (self.x, self.y),
            panel,
        );
        canvas.fill_round_rect_gradient(0, 0, width, height, RADIUS, highlight, TRANSPARENT);
        if glow {
            canvas.glow_round_rect(0, 0, width, height, RADIUS, accent, 6);
        }
        canvas.stroke_round_rect(0, 0, width, height, RADIUS, 1, outline);
        canvas.fill_round_rect(width / 2 - 48, 1, 96, 2, 1, accent.with_alpha(190));

        if let Some(title) = &self.title {
            canvas.text(fonts, title, 18, 12, 13.0, accent, true);
        }
        if let Some(subtitle) = &self.subtitle {
            let subtitle = canvas.ellipsize(fonts, subtitle, 10.5, width - 36);
            canvas.text(fonts, &subtitle, 18, 28, 10.5, muted, false);
        }

        for (index, label) in &self.sections {
            let section_y = self.section_y(*index);
            let label = label.to_uppercase();
            canvas.text(
                fonts,
                &label,
                PADDING + 14,
                section_y + 8,
                9.0,
                accent.with_alpha(200),
                true,
            );
            let label_width = canvas.text_width(fonts, &label, 9.0, true);
            let line_x = PADDING + 24 + label_width;
            if line_x < width - PADDING - 14 {
                canvas.fill_rect(
                    line_x,
                    section_y + 12,
                    width - PADDING - 14 - line_x,
                    1,
                    outline.with_alpha(45),
                );
            }
        }

        for (index, item) in self.items.iter().enumerate() {
            let row_y = self.row_y(index);
            if index == self.selected {
                canvas.fill_round_rect_gradient(
                    PADDING,
                    row_y,
                    width - PADDING * 2,
                    ROW_HEIGHT,
                    11,
                    accent.with_alpha(62),
                    accent.with_alpha(16),
                );
                if glow {
                    canvas.glow_round_rect(
                        PADDING,
                        row_y,
                        width - PADDING * 2,
                        ROW_HEIGHT,
                        11,
                        accent,
                        4,
                    );
                }
                canvas.fill_round_rect(PADDING + 1, row_y + 9, 3, ROW_HEIGHT - 18, 2, accent);
            }
            let icon_color = if !item.enabled {
                muted.with_alpha(90)
            } else if index == self.selected {
                accent
            } else {
                muted
            };
            draw_icon(
                &mut canvas,
                item.icon,
                PADDING + 20,
                row_y + ROW_HEIGHT / 2,
                icon_color,
            );
            let text_color = if item.enabled {
                if index == self.selected {
                    text
                } else {
                    text.with_alpha(220)
                }
            } else {
                muted.with_alpha(110)
            };
            let detail_width = if item.minimize.is_some() || item.close.is_some() {
                96
            } else {
                128
            };
            let label = canvas.ellipsize(
                fonts,
                &item.label,
                12.5,
                width - 42 - PADDING - detail_width,
            );
            canvas.text(
                fonts,
                &label,
                PADDING + 42,
                row_y + 12,
                12.5,
                text_color,
                false,
            );
            if !item.detail.is_empty() {
                canvas.text_right(
                    fonts,
                    &item.detail,
                    width - PADDING - 14,
                    row_y + 14,
                    9.5,
                    muted.with_alpha(160),
                    false,
                );
            }
            if item.minimize.is_some() {
                draw_row_button(
                    &mut canvas,
                    width - 58,
                    row_y + ROW_HEIGHT / 2,
                    false,
                    index == self.selected,
                    accent,
                    muted,
                );
            }
            if item.close.is_some() {
                draw_row_button(
                    &mut canvas,
                    width - 28,
                    row_y + ROW_HEIGHT / 2,
                    true,
                    index == self.selected,
                    error,
                    muted,
                );
            }
            if self.separators.contains(&index) && index + 1 < self.items.len() {
                let separator_y = row_y + ROW_HEIGHT + SEPARATOR_HEIGHT / 2;
                canvas.fill_rect(
                    PADDING + 14,
                    separator_y,
                    width - PADDING * 2 - 28,
                    1,
                    outline.with_alpha(70),
                );
            }
        }

        if let Some(footer) = &self.footer {
            let footer_y = height - self.footer_height() + 7;
            canvas.fill_rect(16, footer_y - 5, width - 32, 1, outline.with_alpha(60));
            canvas.text(fonts, footer, 18, footer_y, 9.5, muted, false);
        }

        Some(canvas.into_pixmap((self.x, self.y)))
    }
}

fn rgba(color: crate::theme::Rgb, alpha: u8) -> UiColor {
    UiColor::rgba(color.r, color.g, color.b, alpha)
}

fn draw_row_button(
    canvas: &mut Canvas,
    center_x: i32,
    center_y: i32,
    close: bool,
    selected: bool,
    close_color: UiColor,
    muted: UiColor,
) {
    let color = if close {
        if selected {
            close_color
        } else {
            close_color.with_alpha(170)
        }
    } else {
        muted.with_alpha(if selected { 230 } else { 160 })
    };
    canvas.fill_round_rect(center_x - 9, center_y - 9, 18, 18, 6, color.with_alpha(26));
    if close {
        canvas.line(
            (center_x as f32 - 3.0, center_y as f32 - 3.0),
            (center_x as f32 + 3.0, center_y as f32 + 3.0),
            1.4,
            color,
        );
        canvas.line(
            (center_x as f32 + 3.0, center_y as f32 - 3.0),
            (center_x as f32 - 3.0, center_y as f32 + 3.0),
            1.4,
            color,
        );
    } else {
        canvas.line(
            (center_x as f32 - 4.0, center_y as f32),
            (center_x as f32 + 4.0, center_y as f32),
            1.4,
            color,
        );
    }
}

fn draw_icon(canvas: &mut Canvas, icon: Icon, x: i32, y: i32, color: UiColor) {
    match icon {
        Icon::Terminal => {
            canvas.stroke_round_rect(x - 8, y - 8, 16, 16, 4, 1, color);
            canvas.line(
                (x as f32 - 4.0, y as f32 - 3.0),
                (x as f32 - 1.0, y as f32),
                1.2,
                color,
            );
            canvas.line(
                (x as f32 - 1.0, y as f32),
                (x as f32 - 4.0, y as f32 + 3.0),
                1.2,
                color,
            );
            canvas.fill_rect(x + 1, y + 3, 4, 1, color);
        }
        Icon::Search => {
            canvas.stroke_circle(x - 1, y - 1, 5, 1, color);
            canvas.line(
                (x as f32 + 3.0, y as f32 + 3.0),
                (x as f32 + 7.0, y as f32 + 7.0),
                1.5,
                color,
            );
        }
        Icon::Settings => {
            canvas.stroke_circle(x, y, 6, 1, color);
            canvas.stroke_circle(x, y, 2, 1, color);
            for (dx, dy) in [(0, -8), (0, 8), (-8, 0), (8, 0)] {
                canvas.line(
                    ((x + dx) as f32, (y + dy) as f32),
                    ((x + dx / 2) as f32, (y + dy / 2) as f32),
                    1.3,
                    color,
                );
            }
        }
        Icon::Lock => {
            canvas.stroke_circle(x, y - 2, 4, 1, color);
            canvas.fill_round_rect(x - 6, y, 12, 9, 3, color);
        }
        Icon::Reload => {
            canvas.stroke_circle(x, y, 6, 1, color);
            canvas.fill_polygon(
                &[
                    (x as f32 + 4.0, y as f32 - 8.0),
                    (x as f32 + 9.0, y as f32 - 6.0),
                    (x as f32 + 5.0, y as f32 - 3.0),
                ],
                color,
            );
        }
        Icon::Window => {
            canvas.stroke_round_rect(x - 8, y - 7, 16, 14, 3, 1, color);
            canvas.fill_rect(x - 8, y - 4, 16, 2, color);
            canvas.fill_circle(x - 4, y + 2, 1, color);
            canvas.fill_rect(x - 1, y + 1, 7, 1, color);
        }
        Icon::Wifi {
            signal,
            secured,
            active,
        } => {
            let bars = if signal == 0 {
                1
            } else {
                ((signal as i32 + 24) / 25).clamp(1, 4) as usize
            };
            for (index, height) in [4, 7, 10, 13].into_iter().enumerate() {
                let bar_color = if index < bars {
                    color
                } else {
                    color.with_alpha(50)
                };
                canvas.fill_round_rect(
                    x - 8 + index as i32 * 4,
                    y + 6 - height,
                    3,
                    height,
                    1,
                    bar_color,
                );
            }
            if secured {
                canvas.stroke_circle(x + 6, y + 1, 2, 1, color);
                canvas.fill_round_rect(x + 3, y + 2, 6, 5, 1, color);
            }
            if active {
                canvas.fill_circle(x + 7, y + 7, 2, color);
            }
        }
        Icon::WifiToggle(enabled) => {
            canvas.stroke_round_rect(x - 9, y - 6, 18, 12, 6, 1, color);
            let knob_x = if enabled { x + 4 } else { x - 4 };
            canvas.fill_circle(knob_x, y, 4, color);
        }
        Icon::BluetoothToggle(enabled) => {
            canvas.stroke_round_rect(x - 9, y - 6, 18, 12, 6, 1, color);
            let knob_x = if enabled { x + 4 } else { x - 4 };
            canvas.fill_circle(knob_x, y, 4, color);
        }
        Icon::BluetoothDevice { connected } => {
            draw_bluetooth_rune(canvas, x - 1, y, color);
            if connected {
                canvas.fill_circle(x + 8, y + 7, 2, color);
            }
        }
        Icon::Scan => {
            canvas.stroke_circle(x - 1, y - 1, 5, 1, color);
            canvas.line(
                (x as f32 + 3.0, y as f32 + 3.0),
                (x as f32 + 7.0, y as f32 + 7.0),
                1.5,
                color,
            );
            canvas.line(
                (x as f32 - 4.0, y as f32 + 6.0),
                (x as f32 + 2.0, y as f32 + 6.0),
                1.0,
                color.with_alpha(140),
            );
        }
    }
}

fn draw_bluetooth_rune(canvas: &mut Canvas, x: i32, y: i32, color: UiColor) {
    canvas.line(
        (x as f32, (y - 8) as f32),
        (x as f32, (y + 8) as f32),
        1.5,
        color,
    );
    canvas.line(
        (x as f32, (y - 8) as f32),
        ((x + 6) as f32, (y - 3) as f32),
        1.5,
        color,
    );
    canvas.line(
        ((x + 6) as f32, (y - 3) as f32),
        (x as f32, (y + 2) as f32),
        1.5,
        color,
    );
    canvas.line(
        (x as f32, (y + 2) as f32),
        ((x + 6) as f32, (y + 7) as f32),
        1.5,
        color,
    );
    canvas.line(
        ((x + 6) as f32, (y + 7) as f32),
        (x as f32, (y + 8) as f32),
        1.5,
        color,
    );
}
