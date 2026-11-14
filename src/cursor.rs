//! Pointer cursor rendering.
//!
//! The default `bubble` cursor theme is generated at runtime so its thin neon
//! outline always follows the live BubbleColor palette. External xcursor themes
//! remain available by name through `theme.cursor_theme`.

use std::{io::Read, time::Duration};

use smithay::{
    input::pointer::CursorIcon,
    utils::{Logical, Size},
};
use tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap as SkPixmap, Stroke, Transform,
};
use tracing::warn;
use xcursor::{
    CursorTheme,
    parser::{Image, parse_xcursor},
};

use crate::{
    config::Config,
    render::Pixmap,
    theme::{Rgb, ThemeDocument},
    ui::UiColor,
};

/// Name of the generated cursor theme.
pub const BUILTIN_THEME: &str = "bubble";

const DESIGN: f32 = 24.0;

#[derive(Debug, Clone, Copy)]
struct CursorPalette {
    outline: [u8; 3],
    fill: [u8; 3],
    shadow: [u8; 3],
}

impl CursorPalette {
    fn from_config(config: &Config) -> Self {
        let outline = UiColor::from_css(&config.theme.accent, 1.0);
        let fill = UiColor::from_css(&config.theme.foreground, 1.0);
        let shadow = UiColor::from_css(&config.theme.background, 1.0);
        Self {
            outline: [outline.r, outline.g, outline.b],
            fill: [fill.r, fill.g, fill.b],
            shadow: [shadow.r, shadow.g, shadow.b],
        }
    }

    fn from_theme(colors: &crate::theme::SemanticColors) -> Self {
        Self {
            outline: rgb(colors.primary),
            fill: rgb(colors.on_surface),
            shadow: rgb(colors.background),
        }
    }
}

fn rgb(color: Rgb) -> [u8; 3] {
    [color.r, color.g, color.b]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorShape {
    Arrow,
    Text,
    VerticalText,
    Pointer,
    Crosshair,
    Cell,
    Move,
    Ew,
    Ns,
    Nwse,
    Nesw,
    Wait,
    NotAllowed,
    Grab,
    Grabbing,
}

impl CursorShape {
    fn from_icon(icon: CursorIcon) -> Self {
        match icon {
            CursorIcon::Text => Self::Text,
            CursorIcon::VerticalText => Self::VerticalText,
            CursorIcon::Pointer
            | CursorIcon::Alias
            | CursorIcon::Copy
            | CursorIcon::ContextMenu => Self::Pointer,
            CursorIcon::Crosshair => Self::Crosshair,
            CursorIcon::Cell => Self::Cell,
            CursorIcon::Move | CursorIcon::AllScroll => Self::Move,
            CursorIcon::EResize | CursorIcon::WResize | CursorIcon::EwResize => Self::Ew,
            CursorIcon::NResize | CursorIcon::SResize | CursorIcon::NsResize => Self::Ns,
            CursorIcon::SeResize | CursorIcon::NwResize | CursorIcon::NwseResize => Self::Nwse,
            CursorIcon::NeResize | CursorIcon::SwResize | CursorIcon::NeswResize => Self::Nesw,
            CursorIcon::Wait | CursorIcon::Progress => Self::Wait,
            CursorIcon::NotAllowed | CursorIcon::NoDrop => Self::NotAllowed,
            CursorIcon::Grab => Self::Grab,
            CursorIcon::Grabbing => Self::Grabbing,
            _ => Self::Arrow,
        }
    }

    /// Hotspot in the 24×24 design grid.
    fn hotspot(self) -> (f32, f32) {
        match self {
            Self::Arrow => (2.0, 2.0),
            Self::Pointer | Self::Grab | Self::Grabbing => (10.0, 4.0),
            Self::Text | Self::VerticalText | Self::Crosshair | Self::Cell | Self::Move => {
                (12.0, 12.0)
            }
            Self::Ew | Self::Ns | Self::Nwse | Self::Nesw | Self::Wait | Self::NotAllowed => {
                (12.0, 12.0)
            }
        }
    }
}

/// Loads either the generated BubbleColor cursor or an external xcursor theme.
pub struct CursorThemeImages {
    size: u32,
    custom: bool,
    palette: CursorPalette,
    theme: Option<CursorTheme>,
    icon: CursorIcon,
}

impl CursorThemeImages {
    pub fn load(config: &Config) -> Self {
        let name = config.theme.cursor_theme.trim();
        let custom = name.is_empty() || name.eq_ignore_ascii_case(BUILTIN_THEME);
        let theme = if custom {
            None
        } else {
            Some(CursorTheme::load(name))
        };
        Self {
            size: config.theme.cursor_size.max(8),
            custom,
            palette: CursorPalette::from_config(config),
            theme,
            icon: CursorIcon::Default,
        }
    }

    /// Rebuilds the generated palette from an accepted BubbleColor snapshot.
    pub fn apply_theme(&mut self, theme: Option<&ThemeDocument>, config: &Config) -> bool {
        let palette = theme
            .map(|theme| CursorPalette::from_theme(&theme.colors))
            .unwrap_or_else(|| CursorPalette::from_config(config));
        let changed = self.palette.outline != palette.outline
            || self.palette.fill != palette.fill
            || self.palette.shadow != palette.shadow;
        self.palette = palette;
        changed
    }

    /// Selects the shape requested by the focused client.
    pub fn set_icon(&mut self, icon: CursorIcon) {
        self.icon = icon;
    }

    pub fn icon(&self) -> CursorIcon {
        self.icon
    }

    pub fn pixmap(&self, scale: f64) -> Option<(Pixmap, i32, i32)> {
        let size = (self.size as f64 * scale.max(1.0)).round() as u32;
        if self.custom {
            let shape = CursorShape::from_icon(self.icon);
            return Some(render_shape(shape, size, &self.palette));
        }

        let theme = self.theme.as_ref()?;
        let images = load_icon(theme, self.icon).unwrap_or_else(|error| {
            warn!(%error, "cannot load the xcursor theme; using the built-in cursor");
            vec![fallback_image(size)]
        });
        let image = nearest_image(size, &images);
        let width = image.width as i32;
        let height = image.height as i32;
        let pixels = image.pixels_rgba.clone();
        let pixmap = Pixmap::from_rgba(&pixels, (0, 0).into(), Size::from((width, height)));
        Some((
            pixmap,
            (image.xhot as f64 * scale).round() as i32,
            (image.yhot as f64 * scale).round() as i32,
        ))
    }
}

fn nearest_image(size: u32, images: &[Image]) -> Image {
    let cache = size.max(8);
    images
        .iter()
        .min_by_key(|image| (cache as i32 - image.size as i32).abs())
        .cloned()
        .unwrap_or_else(|| fallback_image(cache))
}

fn load_icon(
    theme: &CursorTheme,
    icon: CursorIcon,
) -> Result<Vec<Image>, Box<dyn std::error::Error>> {
    let path = theme
        .load_icon(icon.name())
        .or_else(|| theme.load_icon("default"))
        .ok_or_else(|| format!("the cursor theme has no {} cursor", icon.name()))?;
    let mut file = std::fs::File::open(path)?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    parse_xcursor(&data).ok_or_else(|| "invalid xcursor file".into())
}

/// Renders a cursor shape at `size` physical pixels.
fn render_shape(shape: CursorShape, size: u32, palette: &CursorPalette) -> (Pixmap, i32, i32) {
    let (rgba, hot_x, hot_y) = render_shape_pixels(shape, size, palette);
    (
        Pixmap::from_rgba(
            &rgba,
            (0, 0).into(),
            Size::from((size.max(8) as i32, size.max(8) as i32)),
        ),
        hot_x,
        hot_y,
    )
}

fn render_shape_pixels(
    shape: CursorShape,
    size: u32,
    palette: &CursorPalette,
) -> (Vec<u8>, i32, i32) {
    let size = size.max(8);
    let mut pixmap = SkPixmap::new(size, size).expect("valid cursor pixmap size");
    let scale = size as f32 / DESIGN;
    let fill = solid(palette.fill, 250);
    let outline = solid(palette.outline, 255);
    let shadow = solid(palette.shadow, 130);
    let highlight = solid(palette.fill, 190);

    let line = (1.55 * scale).max(1.1);
    let outline_width = (2.9 * scale).max(1.8);
    let shadow_width = (3.8 * scale).max(2.4);

    match shape {
        CursorShape::Arrow => {
            let points = [
                (2.0, 2.0),
                (2.0, 20.2),
                (6.8, 15.6),
                (10.0, 22.0),
                (13.3, 20.5),
                (10.0, 14.3),
                (16.4, 14.3),
            ];
            if let Some(path) = polygon(&points, scale) {
                fill_path(
                    &mut pixmap,
                    &path,
                    &shadow,
                    Transform::from_translate(0.8, 1.0),
                );
                fill_path(&mut pixmap, &path, &fill, Transform::identity());
                stroke_path(
                    &mut pixmap,
                    &path,
                    &outline,
                    line * 0.95,
                    Transform::identity(),
                );
            }
            // Small glass highlight along the leading edge.
            let glint = line_path(&[(3.4, 4.0), (3.4, 15.0)], scale);
            if let Some(path) = glint {
                stroke_path(
                    &mut pixmap,
                    &path,
                    &highlight,
                    line * 0.55,
                    Transform::identity(),
                );
            }
        }
        CursorShape::Text => {
            let path = line_path(
                &[
                    (8.0, 4.0),
                    (16.0, 4.0),
                    (12.0, 4.0),
                    (12.0, 20.0),
                    (8.0, 20.0),
                    (16.0, 20.0),
                ],
                scale,
            );
            stroke_layered(
                &mut pixmap,
                path.as_ref(),
                &shadow,
                &outline,
                &fill,
                shadow_width,
                outline_width,
                line,
            );
        }
        CursorShape::VerticalText => {
            let path = line_path(
                &[
                    (4.0, 8.0),
                    (4.0, 16.0),
                    (4.0, 12.0),
                    (20.0, 12.0),
                    (20.0, 8.0),
                    (20.0, 16.0),
                ],
                scale,
            );
            stroke_layered(
                &mut pixmap,
                path.as_ref(),
                &shadow,
                &outline,
                &fill,
                shadow_width,
                outline_width,
                line,
            );
        }
        CursorShape::Pointer => {
            let path = pointer_path(scale);
            fill_path(
                &mut pixmap,
                &path,
                &shadow,
                Transform::from_translate(0.8, 1.0),
            );
            fill_path(&mut pixmap, &path, &fill, Transform::identity());
            stroke_path(
                &mut pixmap,
                &path,
                &outline,
                line * 0.95,
                Transform::identity(),
            );
        }
        CursorShape::Grab | CursorShape::Grabbing => {
            let path = hand_path(scale);
            fill_path(
                &mut pixmap,
                &path,
                &shadow,
                Transform::from_translate(0.8, 1.0),
            );
            fill_path(&mut pixmap, &path, &fill, Transform::identity());
            stroke_path(
                &mut pixmap,
                &path,
                &outline,
                line * 0.95,
                Transform::identity(),
            );
        }
        CursorShape::Crosshair => {
            let path = line_path(
                &[(12.0, 3.0), (12.0, 21.0), (3.0, 12.0), (21.0, 12.0)],
                scale,
            );
            stroke_layered(
                &mut pixmap,
                path.as_ref(),
                &shadow,
                &outline,
                &fill,
                shadow_width,
                outline_width,
                line,
            );
        }
        CursorShape::Cell => {
            let path = line_path(
                &[(12.0, 3.0), (12.0, 21.0), (3.0, 12.0), (21.0, 12.0)],
                scale,
            );
            stroke_layered(
                &mut pixmap,
                path.as_ref(),
                &shadow,
                &outline,
                &fill,
                shadow_width,
                outline_width,
                line,
            );
            let dot = circle(12.0, 12.0, 1.8, scale);
            fill_path(&mut pixmap, &dot, &outline, Transform::identity());
        }
        CursorShape::Move => {
            let points = [
                (12.0, 2.0),
                (15.6, 5.6),
                (13.2, 5.6),
                (13.2, 10.8),
                (18.4, 10.8),
                (18.4, 8.4),
                (22.0, 12.0),
                (18.4, 15.6),
                (18.4, 13.2),
                (13.2, 13.2),
                (13.2, 18.4),
                (15.6, 18.4),
                (12.0, 22.0),
                (8.4, 18.4),
                (10.8, 18.4),
                (10.8, 13.2),
                (5.6, 13.2),
                (5.6, 15.6),
                (2.0, 12.0),
                (5.6, 8.4),
                (5.6, 10.8),
                (10.8, 10.8),
                (10.8, 5.6),
                (8.4, 5.6),
            ];
            if let Some(path) = polygon(&points, scale) {
                fill_path(
                    &mut pixmap,
                    &path,
                    &shadow,
                    Transform::from_translate(0.7, 0.9),
                );
                fill_path(&mut pixmap, &path, &fill, Transform::identity());
                stroke_path(
                    &mut pixmap,
                    &path,
                    &outline,
                    line * 0.8,
                    Transform::identity(),
                );
            }
        }
        CursorShape::Ew => {
            let points = [
                (2.0, 12.0),
                (6.2, 7.8),
                (6.2, 10.6),
                (17.8, 10.6),
                (17.8, 7.8),
                (22.0, 12.0),
                (17.8, 16.2),
                (17.8, 13.4),
                (6.2, 13.4),
                (6.2, 16.2),
            ];
            if let Some(path) = polygon(&points, scale) {
                fill_path(
                    &mut pixmap,
                    &path,
                    &shadow,
                    Transform::from_translate(0.7, 0.9),
                );
                fill_path(&mut pixmap, &path, &fill, Transform::identity());
                stroke_path(
                    &mut pixmap,
                    &path,
                    &outline,
                    line * 0.8,
                    Transform::identity(),
                );
            }
        }
        CursorShape::Ns => {
            let points = [
                (12.0, 2.0),
                (16.2, 6.2),
                (13.4, 6.2),
                (13.4, 17.8),
                (16.2, 17.8),
                (12.0, 22.0),
                (7.8, 17.8),
                (10.6, 17.8),
                (10.6, 6.2),
                (7.8, 6.2),
            ];
            if let Some(path) = polygon(&points, scale) {
                fill_path(
                    &mut pixmap,
                    &path,
                    &shadow,
                    Transform::from_translate(0.7, 0.9),
                );
                fill_path(&mut pixmap, &path, &fill, Transform::identity());
                stroke_path(
                    &mut pixmap,
                    &path,
                    &outline,
                    line * 0.8,
                    Transform::identity(),
                );
            }
        }
        CursorShape::Nwse => {
            let points = [
                (3.0, 3.0),
                (9.2, 3.0),
                (7.0, 5.2),
                (17.8, 16.0),
                (20.0, 13.8),
                (20.0, 20.0),
                (13.8, 20.0),
                (16.0, 17.8),
                (5.2, 7.0),
                (3.0, 9.2),
            ];
            if let Some(path) = polygon(&points, scale) {
                fill_path(
                    &mut pixmap,
                    &path,
                    &shadow,
                    Transform::from_translate(0.7, 0.9),
                );
                fill_path(&mut pixmap, &path, &fill, Transform::identity());
                stroke_path(
                    &mut pixmap,
                    &path,
                    &outline,
                    line * 0.8,
                    Transform::identity(),
                );
            }
        }
        CursorShape::Nesw => {
            let points = [
                (21.0, 3.0),
                (21.0, 9.2),
                (18.8, 7.0),
                (8.0, 17.8),
                (10.2, 20.0),
                (4.0, 20.0),
                (4.0, 13.8),
                (6.2, 16.0),
                (17.0, 5.2),
                (14.8, 3.0),
            ];
            if let Some(path) = polygon(&points, scale) {
                fill_path(
                    &mut pixmap,
                    &path,
                    &shadow,
                    Transform::from_translate(0.7, 0.9),
                );
                fill_path(&mut pixmap, &path, &fill, Transform::identity());
                stroke_path(
                    &mut pixmap,
                    &path,
                    &outline,
                    line * 0.8,
                    Transform::identity(),
                );
            }
        }
        CursorShape::Wait => {
            let ring = circle(12.0, 12.0, 8.0, scale);
            stroke_path(
                &mut pixmap,
                &ring,
                &shadow,
                shadow_width,
                Transform::identity(),
            );
            stroke_path(
                &mut pixmap,
                &ring,
                &outline,
                outline_width,
                Transform::identity(),
            );
            stroke_path(&mut pixmap, &ring, &fill, line, Transform::identity());
            let hands = line_path(&[(12.0, 12.0), (12.0, 7.2), (15.2, 14.0)], scale);
            if let Some(path) = hands {
                stroke_path(
                    &mut pixmap,
                    &path,
                    &shadow,
                    shadow_width * 0.8,
                    Transform::identity(),
                );
                stroke_path(
                    &mut pixmap,
                    &path,
                    &outline,
                    outline_width * 0.8,
                    Transform::identity(),
                );
                stroke_path(&mut pixmap, &path, &fill, line * 0.8, Transform::identity());
            }
        }
        CursorShape::NotAllowed => {
            let ring = circle(12.0, 12.0, 8.0, scale);
            stroke_path(
                &mut pixmap,
                &ring,
                &shadow,
                shadow_width,
                Transform::identity(),
            );
            stroke_path(
                &mut pixmap,
                &ring,
                &outline,
                outline_width,
                Transform::identity(),
            );
            stroke_path(&mut pixmap, &ring, &fill, line, Transform::identity());
            let slash = line_path(&[(6.6, 6.6), (17.4, 17.4)], scale);
            if let Some(path) = slash {
                stroke_path(
                    &mut pixmap,
                    &path,
                    &shadow,
                    shadow_width,
                    Transform::identity(),
                );
                stroke_path(
                    &mut pixmap,
                    &path,
                    &outline,
                    outline_width,
                    Transform::identity(),
                );
                stroke_path(&mut pixmap, &path, &fill, line, Transform::identity());
            }
        }
    }

    let (hot_x, hot_y) = shape.hotspot();
    let mut rgba = pixmap.data().to_vec();
    unpremultiply(&mut rgba);
    (
        rgba,
        (hot_x * scale).round() as i32,
        (hot_y * scale).round() as i32,
    )
}

fn solid(rgb: [u8; 3], alpha: u8) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(rgb[0], rgb[1], rgb[2], alpha);
    paint.anti_alias = true;
    paint
}

fn fill_path(pixmap: &mut SkPixmap, path: &Path, paint: &Paint<'_>, transform: Transform) {
    pixmap.fill_path(path, paint, FillRule::Winding, transform, None);
}

fn stroke_path(
    pixmap: &mut SkPixmap,
    path: &Path,
    paint: &Paint<'_>,
    width: f32,
    transform: Transform,
) {
    let stroke = Stroke {
        width,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    };
    pixmap.stroke_path(path, paint, &stroke, transform, None);
}

fn stroke_layered(
    pixmap: &mut SkPixmap,
    path: Option<&Path>,
    shadow: &Paint<'_>,
    outline: &Paint<'_>,
    fill: &Paint<'_>,
    shadow_width: f32,
    outline_width: f32,
    line: f32,
) {
    let Some(path) = path else {
        return;
    };
    stroke_path(
        pixmap,
        path,
        shadow,
        shadow_width,
        Transform::from_translate(0.7, 0.9),
    );
    stroke_path(pixmap, path, outline, outline_width, Transform::identity());
    stroke_path(pixmap, path, fill, line, Transform::identity());
}

fn polygon(points: &[(f32, f32)], scale: f32) -> Option<Path> {
    let mut builder = PathBuilder::new();
    builder.move_to(points[0].0 * scale, points[0].1 * scale);
    for point in &points[1..] {
        builder.line_to(point.0 * scale, point.1 * scale);
    }
    builder.close();
    builder.finish()
}

fn line_path(points: &[(f32, f32)], scale: f32) -> Option<Path> {
    let mut builder = PathBuilder::new();
    builder.move_to(points[0].0 * scale, points[0].1 * scale);
    for point in &points[1..] {
        builder.line_to(point.0 * scale, point.1 * scale);
    }
    builder.finish()
}

fn circle(cx: f32, cy: f32, radius: f32, scale: f32) -> Path {
    let mut builder = PathBuilder::new();
    builder.push_circle(cx * scale, cy * scale, radius * scale);
    builder.finish().expect("valid circle path")
}

fn pointer_path(scale: f32) -> Path {
    let s = scale;
    let mut builder = PathBuilder::new();
    builder.move_to(10.0 * s, 21.8 * s);
    builder.cubic_to(7.9 * s, 21.8 * s, 6.2 * s, 20.2 * s, 6.2 * s, 18.2 * s);
    builder.line_to(6.2 * s, 13.0 * s);
    builder.cubic_to(6.2 * s, 11.7 * s, 7.2 * s, 10.8 * s, 8.4 * s, 10.8 * s);
    builder.cubic_to(9.6 * s, 10.8 * s, 10.6 * s, 11.7 * s, 10.6 * s, 13.0 * s);
    builder.line_to(10.6 * s, 6.2 * s);
    builder.cubic_to(10.6 * s, 4.9 * s, 11.6 * s, 4.0 * s, 12.8 * s, 4.0 * s);
    builder.cubic_to(14.0 * s, 4.0 * s, 15.0 * s, 4.9 * s, 15.0 * s, 6.2 * s);
    builder.line_to(15.0 * s, 11.4 * s);
    builder.line_to(15.0 * s, 8.8 * s);
    builder.cubic_to(15.0 * s, 7.5 * s, 16.0 * s, 6.6 * s, 17.2 * s, 6.6 * s);
    builder.cubic_to(18.4 * s, 6.6 * s, 19.4 * s, 7.5 * s, 19.4 * s, 8.8 * s);
    builder.line_to(19.4 * s, 12.2 * s);
    builder.line_to(19.4 * s, 10.8 * s);
    builder.cubic_to(19.4 * s, 9.5 * s, 20.4 * s, 8.6 * s, 21.6 * s, 8.6 * s);
    builder.cubic_to(22.8 * s, 8.6 * s, 23.8 * s, 9.5 * s, 23.8 * s, 10.8 * s);
    builder.line_to(23.8 * s, 17.0 * s);
    builder.cubic_to(23.8 * s, 20.0 * s, 21.2 * s, 21.8 * s, 18.0 * s, 21.8 * s);
    builder.close();
    builder.finish().expect("valid pointer path")
}

fn hand_path(scale: f32) -> Path {
    let s = scale;
    let mut builder = PathBuilder::new();
    builder.move_to(9.0 * s, 21.5 * s);
    builder.cubic_to(7.0 * s, 21.5 * s, 5.6 * s, 20.0 * s, 5.6 * s, 18.0 * s);
    builder.line_to(5.6 * s, 13.5 * s);
    builder.cubic_to(5.6 * s, 12.2 * s, 6.6 * s, 11.2 * s, 7.8 * s, 11.2 * s);
    builder.cubic_to(9.0 * s, 11.2 * s, 10.0 * s, 12.2 * s, 10.0 * s, 13.5 * s);
    builder.line_to(10.0 * s, 7.5 * s);
    builder.cubic_to(10.0 * s, 6.2 * s, 11.0 * s, 5.2 * s, 12.2 * s, 5.2 * s);
    builder.cubic_to(13.4 * s, 5.2 * s, 14.4 * s, 6.2 * s, 14.4 * s, 7.5 * s);
    builder.line_to(14.4 * s, 11.8 * s);
    builder.line_to(14.4 * s, 9.8 * s);
    builder.cubic_to(14.4 * s, 8.5 * s, 15.4 * s, 7.5 * s, 16.6 * s, 7.5 * s);
    builder.cubic_to(17.8 * s, 7.5 * s, 18.8 * s, 8.5 * s, 18.8 * s, 9.8 * s);
    builder.line_to(18.8 * s, 13.0 * s);
    builder.line_to(18.8 * s, 11.8 * s);
    builder.cubic_to(18.8 * s, 10.5 * s, 19.8 * s, 9.5 * s, 21.0 * s, 9.5 * s);
    builder.cubic_to(22.2 * s, 9.5 * s, 23.2 * s, 10.5 * s, 23.2 * s, 11.8 * s);
    builder.line_to(23.2 * s, 17.0 * s);
    builder.cubic_to(23.2 * s, 20.0 * s, 20.6 * s, 21.8 * s, 17.4 * s, 21.8 * s);
    builder.close();
    builder.finish().expect("valid hand path")
}

fn unpremultiply(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = pixel[3] as u32;
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((*channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
        }
    }
}

fn fallback_image(size: u32) -> Image {
    let size = size.max(16);
    let mut pixels = vec![0u8; (size * size * 4) as usize];
    let outline = [
        (2, 2),
        (2, 3),
        (3, 3),
        (3, 4),
        (4, 4),
        (4, 5),
        (5, 5),
        (5, 6),
        (6, 6),
        (6, 7),
        (7, 7),
        (7, 8),
        (8, 8),
        (8, 9),
        (9, 9),
        (9, 10),
        (10, 10),
        (10, 11),
        (11, 11),
        (11, 12),
        (12, 12),
        (2, 13),
        (3, 13),
        (4, 14),
        (5, 14),
        (6, 15),
        (7, 15),
        (8, 16),
        (9, 16),
    ];
    for (x, y) in outline {
        if x < size as i32 && y < size as i32 {
            set_pixel(&mut pixels, size, x, y, [255, 255, 255, 255]);
            set_pixel(&mut pixels, size, x + 1, y, [2, 6, 23, 255]);
            set_pixel(&mut pixels, size, x, y + 1, [2, 6, 23, 255]);
        }
    }
    for y in 2..14 {
        for x in 2..12 {
            if x <= y - 1 {
                set_pixel(&mut pixels, size, x, y, [255, 255, 255, 235]);
            }
        }
    }
    Image {
        size,
        width: size,
        height: size,
        xhot: 2,
        yhot: 2,
        delay: 1000,
        pixels_rgba: pixels,
        pixels_argb: vec![],
    }
}

fn set_pixel(pixels: &mut [u8], size: u32, x: i32, y: i32, color: [u8; 4]) {
    if x < 0 || y < 0 || x >= size as i32 || y >= size as i32 {
        return;
    }
    let offset = ((y as u32 * size + x as u32) * 4) as usize;
    pixels[offset..offset + 4].copy_from_slice(&color);
}

/// Unused duration helper kept for animated xcursor support.
pub fn animation_frame(millis: u32, images: &[Image]) -> usize {
    if images.is_empty() {
        return 0;
    }
    let total = images.iter().map(|image| image.delay.max(1)).sum::<u32>();
    let mut remaining = millis % total.max(1);
    for (index, image) in images.iter().enumerate() {
        if remaining < image.delay.max(1) {
            return index;
        }
        remaining -= image.delay.max(1);
    }
    0
}

/// Cursor size used for the built-in pixmap.
pub fn nominal_size(config: &Config) -> Size<i32, Logical> {
    Size::from((
        config.theme.cursor_size as i32,
        config.theme.cursor_size as i32,
    ))
}

/// A static cursor animation timestamp.
pub fn elapsed_millis(start: std::time::Instant) -> u32 {
    start.elapsed().as_millis() as u32
}

/// Kept for the winit backend, which needs a duration for the cursor cache.
pub fn frame_delay() -> Duration {
    Duration::from_millis(16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_cursor_has_a_colored_outline_and_valid_hotspot() {
        let palette = CursorPalette {
            outline: [34, 211, 238],
            fill: [248, 250, 252],
            shadow: [2, 6, 23],
        };
        let (data, hot_x, hot_y) = render_shape_pixels(CursorShape::Arrow, 32, &palette);
        assert!(hot_x > 0 && hot_y > 0);
        let mut found_outline = false;
        let mut found_fill = false;
        for pixel in data.chunks_exact(4) {
            if pixel[3] == 0 {
                continue;
            }
            let close = |rgb: [u8; 3]| {
                pixel[..3]
                    .iter()
                    .zip(rgb)
                    .all(|(value, target)| (*value as i32 - target as i32).abs() <= 6)
            };
            found_outline |= close(palette.outline);
            found_fill |= close(palette.fill);
        }
        assert!(found_outline, "the neon outline must be present");
        assert!(found_fill, "the cursor body must be present");
    }

    #[test]
    fn unpremultiply_round_trips_alpha_blending() {
        let mut pixels = vec![128, 64, 32, 128, 10, 20, 30, 255, 0, 0, 0, 0];
        unpremultiply(&mut pixels);
        assert_eq!(pixels[0], 255);
        assert_eq!(pixels[1], 128);
        assert_eq!(pixels[2], 64);
        assert_eq!(pixels[3], 128);
        assert_eq!(&pixels[4..8], &[10, 20, 30, 255]);
    }
}
