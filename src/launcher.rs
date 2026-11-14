//! Built-in application launcher (replaces fuzzel).

use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use smithay::{
    input::keyboard::Keysym,
    utils::{Logical, Size},
};
use tracing::{info, warn};

use crate::{
    config::Config,
    theme::{Rgb, ThemeDocument},
    ui::{Canvas, Fonts, GlassBackdrop, GlassRegion, TRANSPARENT, UiColor, glass_tint},
};

const ITEM_HEIGHT: i32 = 48;
const SEARCH_HEIGHT: i32 = 58;
const PADDING: i32 = 10;
const FOOTER_HEIGHT: i32 = 28;
const RADIUS: i32 = 24;

#[derive(Debug, Clone)]
pub struct DesktopEntry {
    pub name: String,
    pub comment: String,
    pub exec: String,
    pub icon: String,
    pub terminal: bool,
    pub categories: String,
    pub keywords: String,
}

impl DesktopEntry {
    fn matches(&self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let query = query.to_lowercase();
        let haystack = format!(
            "{} {} {} {}",
            self.name, self.comment, self.categories, self.keywords
        )
        .to_lowercase();
        haystack.contains(&query)
    }

    fn score(&self, query: &str) -> i32 {
        if query.is_empty() {
            return 0;
        }
        let name = self.name.to_lowercase();
        let query = query.to_lowercase();
        if name == query {
            0
        } else if name.starts_with(&query) {
            1
        } else if name.contains(&query) {
            2
        } else {
            3
        }
    }
}

#[derive(Debug, Default)]
pub struct Launcher {
    entries: Vec<DesktopEntry>,
    pub visible: bool,
    query: String,
    selected: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherAction {
    None,
    Close,
    Redraw,
}

impl Launcher {
    pub fn load() -> Self {
        let mut entries = Vec::new();
        let mut seen = BTreeSet::new();
        for directory in application_directories() {
            let Ok(read) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in read.flatten() {
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("desktop") {
                    continue;
                }
                if let Some(desktop) = parse_desktop_entry(&path) {
                    let key = format!("{}\u{0}{}", desktop.name, desktop.exec);
                    if seen.insert(key) {
                        entries.push(desktop);
                    }
                }
            }
        }
        entries.sort_by(|left, right| left.name.to_lowercase().cmp(&right.name.to_lowercase()));
        info!(
            applications = entries.len(),
            "launcher application index ready"
        );
        Self {
            entries,
            visible: false,
            query: String::new(),
            selected: 0,
        }
    }

    pub fn open(&mut self) {
        self.visible = true;
        self.query.clear();
        self.selected = 0;
    }

    pub fn close(&mut self) {
        self.visible = false;
    }

    pub fn toggle(&mut self) {
        if self.visible {
            self.close();
        } else {
            self.open();
        }
    }

    pub fn handle_key(
        &mut self,
        keysym: Keysym,
        modifiers: smithay::input::keyboard::ModifiersState,
        config: &Config,
    ) -> LauncherAction {
        if !self.visible {
            return LauncherAction::None;
        }
        match keysym {
            Keysym::Escape => {
                self.close();
                LauncherAction::Close
            }
            Keysym::Return | Keysym::KP_Enter => {
                self.launch_selected(config);
                LauncherAction::Close
            }
            Keysym::BackSpace => {
                self.query.pop();
                self.selected = 0;
                LauncherAction::Redraw
            }
            Keysym::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                LauncherAction::Redraw
            }
            Keysym::Down => {
                let count = self.filtered().len();
                if self.selected + 1 < count {
                    self.selected += 1;
                }
                LauncherAction::Redraw
            }
            _ => {
                if modifiers.ctrl || modifiers.alt || modifiers.logo {
                    return LauncherAction::None;
                }
                if let Some(character) = keysym.key_char() {
                    if !character.is_control() && !character.is_whitespace() || character == ' ' {
                        self.query.push(character);
                        self.selected = 0;
                        return LauncherAction::Redraw;
                    }
                }
                LauncherAction::None
            }
        }
    }

    pub fn handle_click(
        &mut self,
        x: i32,
        y: i32,
        output: Size<i32, Logical>,
        config: &Config,
    ) -> LauncherAction {
        if !self.visible {
            return LauncherAction::None;
        }
        let geometry = self.geometry(output, config);
        if !geometry.contains((x, y)) {
            self.close();
            return LauncherAction::Close;
        }
        let list_top = geometry.loc.y + SEARCH_HEIGHT + PADDING;
        let index = (y - list_top) / ITEM_HEIGHT;
        if index >= 0 && (index as usize) < self.filtered().len() {
            self.selected = index as usize;
            self.launch_selected(config);
            return LauncherAction::Close;
        }
        LauncherAction::None
    }

    /// Geometry of the visible launcher panel, used for the live backdrop blur.
    pub fn glass_region(&self, config: &Config, output: Size<i32, Logical>) -> Option<GlassRegion> {
        if !self.visible {
            return None;
        }
        let width = config.launcher.width.min(output.w - 64).max(320);
        let results = self.filtered();
        let visible_count = results.len().min(config.launcher.max_results);
        let height =
            SEARCH_HEIGHT + PADDING + visible_count as i32 * ITEM_HEIGHT + PADDING + FOOTER_HEIGHT;
        let x = (output.w - width) / 2;
        let y = (output.h / 6).max(48);
        Some(GlassRegion::new(x, y, width, height, RADIUS))
    }

    pub fn render(
        &self,
        fonts: &Fonts,
        config: &Config,
        theme: Option<&ThemeDocument>,
        output: Size<i32, Logical>,
        glass: Option<&GlassBackdrop>,
    ) -> Option<crate::render::Pixmap> {
        if !self.visible {
            return None;
        }
        let width = config.launcher.width.min(output.w - 64).max(320);
        let results = self.filtered();
        let visible_count = results.len().min(config.launcher.max_results);
        let height =
            SEARCH_HEIGHT + PADDING + visible_count as i32 * ITEM_HEIGHT + PADDING + FOOTER_HEIGHT;
        let x = (output.w - width) / 2;
        let y = (output.h / 6).max(48);
        let mut canvas = Canvas::new(width, height)?;

        let glass_enabled = config.effects.enabled && config.effects.blur;
        let glow = config.effects.enabled && config.effects.glow;
        let glass_active = glass_enabled && glass.is_some();
        let panel_alpha = if glass_active {
            config.effects.launcher_opacity
        } else {
            0.96
        };
        let panel = glass_tint(config, theme, panel_alpha);
        let search_alpha = if glass_active {
            panel_alpha * 0.72
        } else {
            0.72
        };
        let (search_tint, border, text, muted, accent, highlight, selected_fill, selected_text) =
            if let Some(theme) = theme {
                let colors = &theme.colors;
                (
                    rgba(
                        colors.surface_elevated,
                        (search_alpha * 255.0).round() as u8,
                    ),
                    rgba(colors.glass_border, 158),
                    rgba(colors.on_surface, 255),
                    rgba(colors.on_surface_variant, 190),
                    rgba(colors.primary, 255),
                    rgba(colors.glass_highlight, 24),
                    rgba(colors.primary_container, 205),
                    rgba(colors.on_primary_container, 255),
                )
            } else {
                (
                    glass_tint(config, None, search_alpha),
                    UiColor::from_css(&config.theme.accent, 0.62),
                    UiColor::from_css(&config.theme.foreground, 1.0),
                    UiColor::from_css(&config.theme.foreground, 0.55),
                    UiColor::from_css(&config.theme.accent, 1.0),
                    UiColor::from_css(&config.theme.foreground, 0.10),
                    UiColor::from_css(&config.theme.accent, 0.24),
                    UiColor::from_css(&config.theme.background, 1.0),
                )
            };

        canvas.fill_round_rect_glass(
            0,
            0,
            width,
            height,
            RADIUS,
            glass.filter(|_| glass_enabled),
            (x, y),
            panel,
        );
        canvas.fill_round_rect_gradient(0, 0, width, height, RADIUS, highlight, TRANSPARENT);
        if glow {
            canvas.glow_round_rect(0, 0, width, height, RADIUS, accent, 12);
        }
        canvas.stroke_round_rect(0, 0, width, height, RADIUS, 1, border);

        // Cyberpunk HUD corner brackets.
        let bracket = 16;
        let inset = 9;
        for (corner_x, corner_y, dx, dy) in [
            (inset, inset, 1, 1),
            (width - inset, inset, -1, 1),
            (inset, height - inset, 1, -1),
            (width - inset, height - inset, -1, -1),
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

        // Accent light bar centered on the top edge.
        canvas.fill_round_rect(width / 2 - 56, 1, 112, 2, 1, accent.with_alpha(190));

        canvas.fill_round_rect_glass(
            16,
            10,
            width - 32,
            SEARCH_HEIGHT - 20,
            12,
            glass.filter(|_| glass_enabled),
            (x, y),
            search_tint,
        );
        canvas.stroke_round_rect(
            16,
            10,
            width - 32,
            SEARCH_HEIGHT - 20,
            12,
            1,
            border.with_alpha(80),
        );
        canvas.text_right(
            fonts,
            "BubbleOS",
            width - 30,
            21,
            11.0,
            accent.with_alpha(190),
            true,
        );

        let query = if self.query.is_empty() {
            "Rechercher une application…".to_owned()
        } else {
            self.query.clone()
        };
        let query_color = if self.query.is_empty() { muted } else { text };
        canvas.text(fonts, &query, 30, 18, 19.0, query_color, false);
        if self.query.is_empty() {
            canvas.fill_rect(24, SEARCH_HEIGHT - 12, 2, 22, accent);
        }

        if results.is_empty() {
            canvas.text(
                fonts,
                "Aucun résultat",
                24,
                SEARCH_HEIGHT + 16,
                14.0,
                muted,
                false,
            );
        }

        let list_top = SEARCH_HEIGHT + PADDING;
        let max_rows = config.launcher.max_results;
        for (row, entry) in results.iter().take(max_rows).enumerate() {
            let row_y = list_top + row as i32 * ITEM_HEIGHT;
            if row == self.selected {
                canvas.fill_round_rect_gradient(
                    PADDING,
                    row_y,
                    width - PADDING * 2,
                    ITEM_HEIGHT,
                    12,
                    selected_fill.with_alpha(86),
                    selected_fill.with_alpha(28),
                );
                if glow {
                    canvas.glow_round_rect(
                        PADDING,
                        row_y,
                        width - PADDING * 2,
                        ITEM_HEIGHT,
                        12,
                        accent,
                        5,
                    );
                }
                canvas.fill_round_rect(PADDING + 1, row_y + 10, 3, ITEM_HEIGHT - 20, 2, accent);
            }
            let badge = entry
                .name
                .chars()
                .next()
                .map(|character| character.to_uppercase().to_string())
                .unwrap_or_else(|| "?".into());
            let badge_color = if row == self.selected {
                accent.with_alpha(225)
            } else {
                accent.with_alpha(120)
            };
            canvas.fill_round_rect_gradient(
                22,
                row_y + 10,
                28,
                28,
                9,
                badge_color,
                accent.with_alpha(if row == self.selected { 150 } else { 70 }),
            );
            canvas.text_center(
                fonts,
                &badge,
                36,
                row_y + 15,
                14.0,
                if row == self.selected {
                    selected_text
                } else {
                    UiColor::from_css(&config.theme.background, 1.0)
                },
                true,
            );
            let name = canvas.ellipsize(fonts, &entry.name, 15.0, width - 130);
            canvas.text(fonts, &name, 62, row_y + 8, 15.0, text, false);
            if !entry.comment.is_empty() {
                let comment = canvas.ellipsize(fonts, &entry.comment, 11.0, width - 130);
                canvas.text(fonts, &comment, 62, row_y + 27, 11.0, muted, false);
            }
            if row == self.selected {
                canvas.text_right(fonts, "Entrée", width - 24, row_y + 17, 11.0, muted, false);
            }
        }

        let footer_y = height - FOOTER_HEIGHT;
        canvas.fill_rect(16, footer_y, width - 32, 1, border.with_alpha(90));
        canvas.text(fonts, "↑↓ naviguer", 26, footer_y + 7, 10.5, muted, false);
        canvas.text_center(
            fonts,
            "Entrée lancer",
            width / 2,
            footer_y + 7,
            10.5,
            muted,
            false,
        );
        canvas.text_right(
            fonts,
            "Échap fermer",
            width - 26,
            footer_y + 7,
            10.5,
            muted,
            false,
        );

        Some(canvas.into_pixmap((x, y)))
    }

    pub fn geometry(
        &self,
        output: Size<i32, Logical>,
        config: &Config,
    ) -> smithay::utils::Rectangle<i32, Logical> {
        let width = config.launcher.width.min(output.w - 64).max(320);
        let visible_count = self.filtered().len().min(config.launcher.max_results);
        let height =
            SEARCH_HEIGHT + PADDING + visible_count as i32 * ITEM_HEIGHT + PADDING + FOOTER_HEIGHT;
        smithay::utils::Rectangle::new(
            ((output.w - width) / 2, (output.h / 6).max(48)).into(),
            (width, height).into(),
        )
    }

    pub fn filtered(&self) -> Vec<&DesktopEntry> {
        let mut matching = self
            .entries
            .iter()
            .filter(|entry| entry.matches(&self.query))
            .collect::<Vec<_>>();
        matching.sort_by(|left, right| {
            left.score(&self.query)
                .cmp(&right.score(&self.query))
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        });
        matching
    }

    fn launch_selected(&self, config: &Config) {
        let Some(entry) = self.filtered().get(self.selected).copied() else {
            return;
        };
        let command = sanitize_exec(&entry.exec);
        let process = if entry.terminal {
            let terminal = if config.launcher.terminal.is_empty() {
                config.core.terminal.clone()
            } else {
                config.launcher.terminal.clone()
            };
            Command::new("sh")
                .arg("-c")
                .arg(format!("{terminal} -e {command}"))
                .spawn()
        } else {
            Command::new("sh").arg("-c").arg(command).spawn()
        };
        match process {
            Ok(_) => info!(application = %entry.name, "launched"),
            Err(error) => warn!(application = %entry.name, %error, "cannot launch application"),
        }
    }
}

fn rgba(color: Rgb, alpha: u8) -> UiColor {
    UiColor::rgba(color.r, color.g, color.b, alpha)
}

fn application_directories() -> Vec<PathBuf> {
    let mut directories = vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
    ];
    if let Some(data_home) = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
    {
        directories.push(data_home.join("applications"));
    }
    if let Some(data_dirs) = env::var_os("XDG_DATA_DIRS") {
        for directory in env::split_paths(&data_dirs) {
            directories.push(directory.join("applications"));
        }
    }
    directories
}

fn parse_desktop_entry(path: &Path) -> Option<DesktopEntry> {
    let content = fs::read_to_string(path).ok()?;
    let mut in_desktop_entry = false;
    let mut name = None;
    let mut comment = None;
    let mut exec = None;
    let mut icon = None;
    let mut terminal = false;
    let mut hidden = false;
    let mut no_display = false;
    let mut categories = String::new();
    let mut keywords = String::new();
    let mut only_show_in = String::new();

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "Name" if name.is_none() => name = Some(value.trim().to_owned()),
            "Comment" if comment.is_none() => comment = Some(value.trim().to_owned()),
            "Exec" => exec = Some(value.trim().to_owned()),
            "Icon" => icon = Some(value.trim().to_owned()),
            "Terminal" => terminal = value.trim() == "true",
            "Hidden" => hidden = value.trim() == "true",
            "NoDisplay" => no_display = value.trim() == "true",
            "Categories" => categories = value.trim().to_owned(),
            "Keywords" => keywords = value.trim().to_owned(),
            "OnlyShowIn" => only_show_in = value.trim().to_owned(),
            _ => {}
        }
    }

    if hidden || no_display {
        return None;
    }
    if !only_show_in.is_empty()
        && !only_show_in.split(';').any(|desktop| {
            desktop.eq_ignore_ascii_case("bubbleos") || desktop.eq_ignore_ascii_case("bubblewm")
        })
    {
        return None;
    }
    Some(DesktopEntry {
        name: name?,
        comment: comment.unwrap_or_default(),
        exec: exec?,
        icon: icon.unwrap_or_default(),
        terminal,
        categories,
        keywords,
    })
}

fn sanitize_exec(exec: &str) -> String {
    let mut result = String::new();
    let mut characters = exec.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '%' {
            if let Some(code) = characters.next() {
                match code {
                    'f' | 'F' | 'u' | 'U' | 'i' | 'c' | 'k' | 'd' | 'D' | 'n' | 'N' | 'v' | 'm' => {
                        continue;
                    }
                    '%' => result.push('%'),
                    other => {
                        result.push('%');
                        result.push(other);
                    }
                }
            }
        } else {
            result.push(character);
        }
    }
    result
}
