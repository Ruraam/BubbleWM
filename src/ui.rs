//! Small software-rendered user interface toolkit.
//!
//! BubbleWM draws the launcher, notifications and lock screen itself so the
//! session does not need an extra GUI process. `tiny-skia` provides the 2D
//! primitives and `fontdue` rasterizes glyphs; the resulting RGBA pixmap is
//! uploaded as a single texture by the compositor.

use std::path::Path;

use fontdue::{Font, FontSettings};
use smithay::utils::{Logical, Size};
use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LinearGradient, Paint, PathBuilder, Pattern,
    Pixmap, Rect as SkRect, SpreadMode, Transform as SkTransform,
};

use crate::{
    config::Config,
    render::{Pixmap as CompositorPixmap, parse_color},
    theme::ThemeDocument,
};

const EMBEDDED_BODY: &[u8] = include_bytes!("../assets/DejaVuSans.ttf");
const EMBEDDED_DISPLAY: &[u8] = include_bytes!("../assets/Orbitron.ttf");

/// Font collection used by every built-in overlay.
pub struct Fonts {
    body: Font,
    display: Font,
}

impl Fonts {
    pub fn load(preferred_body: Option<&str>) -> Self {
        let body = preferred_body
            .and_then(load_font_file)
            .or_else(|| {
                find_font(&[
                    "/usr/share/fonts/truetype/inter/Inter_18pt-Regular.ttf",
                    "/usr/share/fonts/truetype/inter/Inter-Regular.ttf",
                    "/usr/share/fonts/truetype/inter/Inter.ttf",
                    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
                ])
            })
            .unwrap_or_else(|| {
                Font::from_bytes(EMBEDDED_BODY, FontSettings::default())
                    .expect("the embedded body font is valid")
            });
        let display =
            find_font(&["/usr/share/fonts/truetype/orbitron/Orbitron-VariableFont_wght.ttf"])
                .unwrap_or_else(|| {
                    Font::from_bytes(EMBEDDED_DISPLAY, FontSettings::default())
                        .expect("the embedded display font is valid")
                });
        Self { body, display }
    }

    fn body(&self) -> &Font {
        &self.body
    }

    fn display(&self) -> &Font {
        &self.display
    }
}

fn load_font_file(path: &str) -> Option<Font> {
    let data = std::fs::read(Path::new(path)).ok()?;
    Font::from_bytes(data, FontSettings::default()).ok()
}

fn find_font(paths: &[&str]) -> Option<Font> {
    paths.iter().find_map(|path| load_font_file(path))
}

/// Straight RGBA color used by the overlay toolkit.
#[derive(Clone, Copy, Debug)]
pub struct UiColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl UiColor {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub fn from_css(value: &str, alpha: f32) -> Self {
        let color = parse_color(value, alpha);
        Self::rgba(
            (color.r() * 255.0).round() as u8,
            (color.g() * 255.0).round() as u8,
            (color.b() * 255.0).round() as u8,
            (color.a() * 255.0).round() as u8,
        )
    }

    pub fn with_alpha(self, alpha: u8) -> Self {
        Self { a: alpha, ..self }
    }
}

pub const TRANSPARENT: UiColor = UiColor::rgba(0, 0, 0, 0);
pub const WHITE: UiColor = UiColor::rgba(255, 255, 255, 255);

/// Resolves the glass surface tint.
///
/// `effects.glass_tint` overrides the BubbleColor `surface` role so users can
/// color every glass surface live, without restarting the compositor.
pub fn glass_tint(config: &Config, theme: Option<&ThemeDocument>, alpha: f32) -> UiColor {
    let alpha = alpha.clamp(0.0, 1.0);
    if !config.effects.glass_tint.trim().is_empty() {
        return UiColor::from_css(&config.effects.glass_tint, alpha);
    }
    if let Some(theme) = theme {
        let color = theme.colors.surface;
        return UiColor::rgba(color.r, color.g, color.b, (alpha * 255.0).round() as u8);
    }
    UiColor::from_css(&config.theme.background, alpha)
}

/// A downscaled, strongly blurred copy of the wallpaper.
///
/// Panels sample this pixmap instead of the wallpaper itself. The result is a
/// frosted-glass backdrop that follows the wallpaper and therefore the
/// BubbleColor palette, without requiring a GPU scene capture.
pub struct GlassBackdrop {
    pixmap: Pixmap,
    scale_x: f32,
    scale_y: f32,
    /// When true the compositor supplies a live GPU-blurred backdrop and this
    /// CPU copy is only used as a fallback for window corner masks.
    pub gpu: bool,
}

impl GlassBackdrop {
    /// Builds the backdrop from opaque RGBA wallpaper pixels.
    ///
    /// `downscale` controls the resolution of the blur source and `radius` the
    /// blur strength. Three box-blur passes approximate a Gaussian kernel.
    pub fn from_rgba(
        pixels: &[u8],
        size: Size<i32, Logical>,
        downscale: u32,
        radius: u32,
    ) -> Option<Self> {
        let width = size.w.max(1) as u32;
        let height = size.h.max(1) as u32;
        if pixels.len() < (width as usize * height as usize * 4) {
            return None;
        }
        let factor = downscale.clamp(1, 32);
        let small_width = width.div_ceil(factor);
        let small_height = height.div_ceil(factor);
        let mut data = vec![0u8; (small_width * small_height * 4) as usize];

        // Box-average the wallpaper before blurring. This keeps the blur cheap
        // even on a 4K output while preserving the dominant colors.
        for small_y in 0..small_height {
            let source_y0 = small_y * factor;
            let source_y1 = ((small_y + 1) * factor).min(height);
            for small_x in 0..small_width {
                let source_x0 = small_x * factor;
                let source_x1 = ((small_x + 1) * factor).min(width);
                let mut channels = [0u32; 4];
                let mut count = 0u32;
                for source_y in source_y0..source_y1 {
                    for source_x in source_x0..source_x1 {
                        let offset = ((source_y * width + source_x) * 4) as usize;
                        for channel in 0..4 {
                            channels[channel] += pixels[offset + channel] as u32;
                        }
                        count += 1;
                    }
                }
                let offset = ((small_y * small_width + small_x) * 4) as usize;
                let count = count.max(1);
                for channel in 0..4 {
                    data[offset + channel] = (channels[channel] / count) as u8;
                }
            }
        }

        let radius = radius.clamp(1, 128);
        box_blur(&mut data, small_width, small_height, radius);
        box_blur(&mut data, small_width, small_height, radius);
        box_blur(&mut data, small_width, small_height, radius);

        let mut pixmap = Pixmap::new(small_width, small_height)?;
        pixmap.data_mut().copy_from_slice(&data);
        Some(Self {
            pixmap,
            scale_x: small_width as f32 / width as f32,
            scale_y: small_height as f32 / height as f32,
            gpu: false,
        })
    }

    pub fn pixmap(&self) -> &Pixmap {
        &self.pixmap
    }

    pub fn scale(&self) -> (f32, f32) {
        (self.scale_x, self.scale_y)
    }

    /// Samples the blurred wallpaper at an output-space position.
    pub fn sample(&self, x: f32, y: f32) -> UiColor {
        let width = self.pixmap.width();
        let height = self.pixmap.height();
        if width == 0 || height == 0 {
            return TRANSPARENT;
        }
        let fx = (x * self.scale_x - 0.5).clamp(0.0, width as f32 - 1.0);
        let fy = (y * self.scale_y - 0.5).clamp(0.0, height as f32 - 1.0);
        let x0 = fx.floor() as u32;
        let y0 = fy.floor() as u32;
        let x1 = (x0 + 1).min(width - 1);
        let y1 = (y0 + 1).min(height - 1);
        let tx = fx - x0 as f32;
        let ty = fy - y0 as f32;
        let data = self.pixmap.data();
        let at = |px: u32, py: u32, channel: usize| {
            data[((py * width + px) * 4) as usize + channel] as f32
        };
        let mut channels = [0u8; 4];
        for (channel, value) in channels.iter_mut().enumerate() {
            let top = at(x0, y0, channel) * (1.0 - tx) + at(x1, y0, channel) * tx;
            let bottom = at(x0, y1, channel) * (1.0 - tx) + at(x1, y1, channel) * tx;
            *value = (top * (1.0 - ty) + bottom * ty).round().clamp(0.0, 255.0) as u8;
        }
        UiColor::rgba(channels[0], channels[1], channels[2], channels[3])
    }
}

/// A rounded rectangle that should receive a live backdrop blur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlassRegion {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub radius: i32,
}

impl GlassRegion {
    pub fn new(x: i32, y: i32, width: i32, height: i32, radius: i32) -> Self {
        Self {
            x,
            y,
            width: width.max(1),
            height: height.max(1),
            radius: radius.max(0),
        }
    }

    pub fn rect(self) -> smithay::utils::Rectangle<i32, smithay::utils::Logical> {
        smithay::utils::Rectangle::new(
            smithay::utils::Point::from((self.x, self.y)),
            smithay::utils::Size::from((self.width, self.height)),
        )
    }
}

/// Separable box blur over straight RGBA pixels. The wallpaper is opaque, so
/// alpha stays at 255 and no premultiplication is required.
fn box_blur(data: &mut [u8], width: u32, height: u32, radius: u32) {
    if width == 0 || height == 0 || radius == 0 {
        return;
    }
    let mut temp = vec![0u8; data.len()];
    let window = (radius * 2 + 1) as u32;

    for y in 0..height {
        for channel in 0..4 {
            let mut sum = 0u32;
            for index in 0..window {
                let x = (index as i32 - radius as i32).clamp(0, width as i32 - 1) as u32;
                sum += data[((y * width + x) * 4) as usize + channel] as u32;
            }
            for x in 0..width {
                temp[((y * width + x) * 4) as usize + channel] = (sum / window) as u8;
                let add = (x as i32 + radius as i32 + 1).clamp(0, width as i32 - 1) as u32;
                let remove = (x as i32 - radius as i32).clamp(0, width as i32 - 1) as u32;
                sum += data[((y * width + add) * 4) as usize + channel] as u32;
                sum -= data[((y * width + remove) * 4) as usize + channel] as u32;
            }
        }
    }

    for x in 0..width {
        for channel in 0..4 {
            let mut sum = 0u32;
            for index in 0..window {
                let y = (index as i32 - radius as i32).clamp(0, height as i32 - 1) as u32;
                sum += temp[((y * width + x) * 4) as usize + channel] as u32;
            }
            for y in 0..height {
                data[((y * width + x) * 4) as usize + channel] = (sum / window) as u8;
                let add = (y as i32 + radius as i32 + 1).clamp(0, height as i32 - 1) as u32;
                let remove = (y as i32 - radius as i32).clamp(0, height as i32 - 1) as u32;
                sum += temp[((add * width + x) * 4) as usize + channel] as u32;
                sum -= temp[((remove * width + x) * 4) as usize + channel] as u32;
            }
        }
    }
}

/// A CPU-rendered overlay surface.
pub struct Canvas {
    pixmap: Pixmap,
}

impl Canvas {
    pub fn new(width: i32, height: i32) -> Option<Self> {
        let width = width.max(1) as u32;
        let height = height.max(1) as u32;
        Some(Self {
            pixmap: Pixmap::new(width, height)?,
        })
    }

    pub fn width(&self) -> i32 {
        self.pixmap.width() as i32
    }

    pub fn height(&self) -> i32 {
        self.pixmap.height() as i32
    }

    pub fn fill(&mut self, color: UiColor) {
        self.pixmap.fill(to_skia(color));
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: UiColor) {
        if width <= 0 || height <= 0 {
            return;
        }
        let Some(rect) = SkRect::from_xywh(x as f32, y as f32, width as f32, height as f32) else {
            return;
        };
        let mut paint = Paint::default();
        paint.set_color(to_skia(color));
        paint.anti_alias = true;
        self.pixmap
            .fill_rect(rect, &paint, SkTransform::identity(), None);
    }

    pub fn fill_round_rect(
        &mut self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        radius: i32,
        color: UiColor,
    ) {
        if width <= 0 || height <= 0 {
            return;
        }
        let Some(path) = round_rect_path(x, y, width, height, radius) else {
            return;
        };
        let mut paint = Paint::default();
        paint.set_color(to_skia(color));
        paint.anti_alias = true;
        self.pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            SkTransform::identity(),
            None,
        );
    }

    pub fn stroke_round_rect(
        &mut self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        radius: i32,
        thickness: i32,
        color: UiColor,
    ) {
        if width <= 0 || height <= 0 || thickness <= 0 {
            return;
        }
        let Some(path) = round_rect_path(x, y, width, height, radius) else {
            return;
        };
        let mut paint = Paint::default();
        paint.set_color(to_skia(color));
        paint.anti_alias = true;
        let stroke = tiny_skia::Stroke {
            width: thickness as f32,
            ..Default::default()
        };
        self.pixmap
            .stroke_path(&path, &paint, &stroke, SkTransform::identity(), None);
    }

    pub fn fill_circle(&mut self, center_x: i32, center_y: i32, radius: i32, color: UiColor) {
        if radius <= 0 {
            return;
        }
        let mut builder = PathBuilder::new();
        builder.push_circle(center_x as f32, center_y as f32, radius as f32);
        let Some(path) = builder.finish() else {
            return;
        };
        let mut paint = Paint::default();
        paint.set_color(to_skia(color));
        paint.anti_alias = true;
        self.pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            SkTransform::identity(),
            None,
        );
    }

    pub fn stroke_circle(
        &mut self,
        center_x: i32,
        center_y: i32,
        radius: i32,
        thickness: i32,
        color: UiColor,
    ) {
        if radius <= 0 || thickness <= 0 {
            return;
        }
        let mut builder = PathBuilder::new();
        builder.push_circle(center_x as f32, center_y as f32, radius as f32);
        let Some(path) = builder.finish() else {
            return;
        };
        let mut paint = Paint::default();
        paint.set_color(to_skia(color));
        paint.anti_alias = true;
        let stroke = tiny_skia::Stroke {
            width: thickness as f32,
            ..Default::default()
        };
        self.pixmap
            .stroke_path(&path, &paint, &stroke, SkTransform::identity(), None);
    }

    pub fn line(&mut self, from: (f32, f32), to: (f32, f32), thickness: f32, color: UiColor) {
        if thickness <= 0.0 {
            return;
        }
        let mut builder = PathBuilder::new();
        builder.move_to(from.0, from.1);
        builder.line_to(to.0, to.1);
        let Some(path) = builder.finish() else {
            return;
        };
        let mut paint = Paint::default();
        paint.set_color(to_skia(color));
        paint.anti_alias = true;
        let stroke = tiny_skia::Stroke {
            width: thickness,
            ..Default::default()
        };
        self.pixmap
            .stroke_path(&path, &paint, &stroke, SkTransform::identity(), None);
    }

    pub fn fill_polygon(&mut self, points: &[(f32, f32)], color: UiColor) {
        if points.len() < 3 {
            return;
        }
        let mut builder = PathBuilder::new();
        builder.move_to(points[0].0, points[0].1);
        for point in &points[1..] {
            builder.line_to(point.0, point.1);
        }
        builder.close();
        let Some(path) = builder.finish() else {
            return;
        };
        let mut paint = Paint::default();
        paint.set_color(to_skia(color));
        paint.anti_alias = true;
        self.pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            SkTransform::identity(),
            None,
        );
    }

    /// Fills a rounded rectangle with a blurred wallpaper sample and a tint.
    ///
    /// `origin` is the output-space position of the canvas' local `(0, 0)`, so
    /// panels that are later placed at `(x, y)` still sample the right part of
    /// the wallpaper.
    pub fn fill_round_rect_glass(
        &mut self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        radius: i32,
        backdrop: Option<&GlassBackdrop>,
        origin: (i32, i32),
        tint: UiColor,
    ) {
        if width <= 0 || height <= 0 {
            return;
        }
        if let Some(backdrop) = backdrop {
            if !backdrop.gpu
                && let Some(path) = round_rect_path(x, y, width, height, radius)
            {
                let (scale_x, scale_y) = backdrop.scale();
                let transform = SkTransform::from_translate(-origin.0 as f32, -origin.1 as f32)
                    .pre_scale(
                        1.0 / scale_x.max(f32::EPSILON),
                        1.0 / scale_y.max(f32::EPSILON),
                    );
                let pattern = Pattern::new(
                    backdrop.pixmap().as_ref(),
                    SpreadMode::Pad,
                    FilterQuality::Bilinear,
                    1.0,
                    transform,
                );
                let mut paint = Paint::default();
                paint.shader = pattern;
                paint.anti_alias = true;
                self.pixmap.fill_path(
                    &path,
                    &paint,
                    FillRule::Winding,
                    SkTransform::identity(),
                    None,
                );
            }
        }
        self.fill_round_rect(x, y, width, height, radius, tint);
    }

    /// Fills a rounded rectangle with a vertical two-color gradient.
    pub fn fill_round_rect_gradient(
        &mut self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        radius: i32,
        top: UiColor,
        bottom: UiColor,
    ) {
        if width <= 0 || height <= 0 {
            return;
        }
        let Some(path) = round_rect_path(x, y, width, height, radius) else {
            return;
        };
        let Some(shader) = LinearGradient::new(
            tiny_skia::Point::from_xy(x as f32, y as f32),
            tiny_skia::Point::from_xy(x as f32, (y + height) as f32),
            vec![
                GradientStop::new(0.0, to_skia(top)),
                GradientStop::new(1.0, to_skia(bottom)),
            ],
            SpreadMode::Pad,
            SkTransform::identity(),
        ) else {
            return;
        };
        let mut paint = Paint::default();
        paint.shader = shader;
        paint.anti_alias = true;
        self.pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            SkTransform::identity(),
            None,
        );
    }

    /// Draws a soft outer glow around a rounded rectangle.
    pub fn glow_round_rect(
        &mut self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        radius: i32,
        color: UiColor,
        spread: i32,
    ) {
        let spread = spread.max(1);
        let steps = 4;
        for step in (1..=steps).rev() {
            let expand = spread * step / steps;
            let alpha = (color.a as u32 * (steps + 1 - step) as u32 / (steps as u32 * 2))
                .clamp(0, 255) as u8;
            if alpha == 0 {
                continue;
            }
            self.stroke_round_rect(
                x - expand,
                y - expand,
                width + expand * 2,
                height + expand * 2,
                radius + expand,
                2,
                color.with_alpha(alpha),
            );
        }
    }

    pub fn text(
        &mut self,
        fonts: &Fonts,
        text: &str,
        x: i32,
        top: i32,
        size: f32,
        color: UiColor,
        display_font: bool,
    ) -> i32 {
        let font = if display_font {
            fonts.display()
        } else {
            fonts.body()
        };
        let baseline = top as f32
            + font
                .horizontal_line_metrics(size)
                .map(|metrics| metrics.ascent)
                .unwrap_or(size * 0.8);
        let mut pen_x = x as f32;
        for character in text.chars() {
            if character == '\n' {
                continue;
            }
            let (metrics, bitmap) = font.rasterize(character, size);
            if metrics.width > 0 && metrics.height > 0 {
                let glyph_x = pen_x.round() as i32 + metrics.xmin;
                let glyph_y = baseline.round() as i32 - metrics.ymin - metrics.height as i32;
                self.blend_glyph(
                    &bitmap,
                    metrics.width,
                    metrics.height,
                    glyph_x,
                    glyph_y,
                    color,
                );
            }
            pen_x += metrics.advance_width;
        }
        (pen_x - x as f32).round() as i32
    }

    pub fn text_center(
        &mut self,
        fonts: &Fonts,
        text: &str,
        center_x: i32,
        top: i32,
        size: f32,
        color: UiColor,
        display_font: bool,
    ) -> i32 {
        let width = self.text_width(fonts, text, size, display_font);
        self.text(
            fonts,
            text,
            center_x - width / 2,
            top,
            size,
            color,
            display_font,
        )
    }

    pub fn text_right(
        &mut self,
        fonts: &Fonts,
        text: &str,
        right: i32,
        top: i32,
        size: f32,
        color: UiColor,
        display_font: bool,
    ) -> i32 {
        let width = self.text_width(fonts, text, size, display_font);
        self.text(fonts, text, right - width, top, size, color, display_font)
    }

    pub fn text_width(&self, fonts: &Fonts, text: &str, size: f32, display_font: bool) -> i32 {
        let font = if display_font {
            fonts.display()
        } else {
            fonts.body()
        };
        text.chars()
            .map(|character| font.metrics(character, size).advance_width)
            .sum::<f32>()
            .round() as i32
    }

    pub fn text_height(&self, fonts: &Fonts, size: f32) -> i32 {
        fonts
            .body()
            .horizontal_line_metrics(size)
            .map(|metrics| (metrics.ascent - metrics.descent).ceil() as i32)
            .unwrap_or((size * 1.3) as i32)
    }

    pub fn ellipsize(&self, fonts: &Fonts, text: &str, size: f32, max_width: i32) -> String {
        if self.text_width(fonts, text, size, false) <= max_width {
            return text.to_owned();
        }
        let ellipsis = "…";
        let ellipsis_width = self.text_width(fonts, ellipsis, size, false);
        let mut result = String::new();
        let mut width = 0;
        for character in text.chars() {
            let advance = fonts.body().metrics(character, size).advance_width;
            if width + advance as i32 + ellipsis_width > max_width {
                break;
            }
            width += advance as i32;
            result.push(character);
        }
        result.push_str(ellipsis);
        result
    }

    /// Wraps text on a maximum width and returns at most `max_lines` lines.
    pub fn wrap(
        &self,
        fonts: &Fonts,
        text: &str,
        size: f32,
        max_width: i32,
        max_lines: usize,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        let mut current = String::new();
        for word in text.split_whitespace() {
            let candidate = if current.is_empty() {
                word.to_owned()
            } else {
                format!("{current} {word}")
            };
            if self.text_width(fonts, &candidate, size, false) <= max_width {
                current = candidate;
            } else {
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }
                if lines.len() >= max_lines {
                    break;
                }
                current = word.to_owned();
            }
        }
        if lines.len() < max_lines && !current.is_empty() {
            lines.push(current);
        }
        if lines.len() == max_lines {
            if let Some(last) = lines.last_mut() {
                *last = self.ellipsize(fonts, last, size, max_width);
            }
        }
        lines
    }

    pub fn into_pixmap(self, location: (i32, i32)) -> CompositorPixmap {
        let size = Size::from((self.width(), self.height()));
        CompositorPixmap::from_rgba(self.pixmap.data(), location.into(), size)
    }

    fn blend_glyph(
        &mut self,
        coverage: &[u8],
        width: usize,
        height: usize,
        x: i32,
        y: i32,
        color: UiColor,
    ) {
        let canvas_width = self.width();
        let canvas_height = self.height();
        let stride = canvas_width as usize * 4;
        let data = self.pixmap.data_mut();
        for row in 0..height {
            let destination_y = y + row as i32;
            if destination_y < 0 || destination_y >= canvas_height {
                continue;
            }
            for column in 0..width {
                let destination_x = x + column as i32;
                if destination_x < 0 || destination_x >= canvas_width {
                    continue;
                }
                let alpha = coverage[row * width + column];
                if alpha == 0 {
                    continue;
                }
                let offset = destination_y as usize * stride + destination_x as usize * 4;
                let source_alpha = (alpha as u32 * color.a as u32) / 255;
                blend_over(&mut data[offset..offset + 4], color, source_alpha as u8);
            }
        }
    }
}

fn blend_over(destination: &mut [u8], color: UiColor, alpha: u8) {
    let source_alpha = alpha as f32 / 255.0;
    let inverse = 1.0 - source_alpha;
    let source = [
        color.r as f32 * source_alpha,
        color.g as f32 * source_alpha,
        color.b as f32 * source_alpha,
        alpha as f32,
    ];
    for index in 0..4 {
        let value = source[index] + destination[index] as f32 * inverse;
        destination[index] = value.round().clamp(0.0, 255.0) as u8;
    }
}

fn round_rect_path(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    radius: i32,
) -> Option<tiny_skia::Path> {
    let radius = radius.max(0).min(width / 2).min(height / 2) as f32;
    let x = x as f32;
    let y = y as f32;
    let width = width as f32;
    let height = height as f32;
    let mut builder = PathBuilder::new();
    builder.move_to(x + radius, y);
    builder.line_to(x + width - radius, y);
    builder.quad_to(x + width, y, x + width, y + radius);
    builder.line_to(x + width, y + height - radius);
    builder.quad_to(x + width, y + height, x + width - radius, y + height);
    builder.line_to(x + radius, y + height);
    builder.quad_to(x, y + height, x, y + height - radius);
    builder.line_to(x, y + radius);
    builder.quad_to(x, y, x + radius, y);
    builder.close();
    builder.finish()
}

fn to_skia(color: UiColor) -> Color {
    Color::from_rgba8(color.r, color.g, color.b, color.a)
}

/// Returns a full-screen lock/notification scrim color.
pub fn scrim(alpha: f32) -> UiColor {
    UiColor::rgba(2, 6, 23, (alpha.clamp(0.0, 1.0) * 255.0) as u8)
}

/// A stable identifier for UI caching.
pub fn logical_size(width: i32, height: i32) -> Size<i32, Logical> {
    Size::from((width, height))
}
