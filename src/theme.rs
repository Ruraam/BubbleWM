//! BubbleColor: wallpaper-derived semantic colors for BubbleWM.
//!
//! This module deliberately has no Smithay or UI dependency. It produces a
//! deterministic, versioned palette that the compositor, panel and settings
//! application can consume through one shared contract.

use anyhow::{Context, Result, ensure};
use image::{ImageReader, Limits};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const THEME_SCHEMA_VERSION: u32 = 1;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_IMAGE_EDGE: u32 = 16_384;
const SAMPLE_EDGE: u32 = 96;
const CLUSTER_COUNT: usize = 5;
const KMEANS_ITERATIONS: usize = 8;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ColorMode {
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PaletteStyle {
    Balanced,
    Vivid,
    Muted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Self = Self { r: 0, g: 0, b: 0 };
    pub const WHITE: Self = Self {
        r: 255,
        g: 255,
        b: 255,
    };

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemeOptions {
    pub mode: ColorMode,
    pub style: PaletteStyle,
    /// Requested minimum contrast for normal text (WCAG ratio).
    pub contrast_target: f32,
}

impl Default for ThemeOptions {
    fn default() -> Self {
        Self {
            mode: ColorMode::Dark,
            style: PaletteStyle::Balanced,
            contrast_target: 4.5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticColors {
    pub primary: Rgb,
    pub on_primary: Rgb,
    pub primary_container: Rgb,
    pub on_primary_container: Rgb,
    pub secondary: Rgb,
    pub on_secondary: Rgb,
    pub tertiary: Rgb,
    pub on_tertiary: Rgb,
    pub background: Rgb,
    pub on_background: Rgb,
    pub surface: Rgb,
    pub on_surface: Rgb,
    pub surface_variant: Rgb,
    pub on_surface_variant: Rgb,
    pub outline: Rgb,
    pub error: Rgb,
    pub on_error: Rgb,
    pub success: Rgb,
    pub warning: Rgb,
    pub info: Rgb,
    pub glass_tint: Rgb,
    pub glass_border: Rgb,
    pub glass_highlight: Rgb,
    pub neon_cyan: Rgb,
    pub neon_magenta: Rgb,
    pub neon_violet: Rgb,
    pub neon_lime: Rgb,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemeDiagnostics {
    pub sampled_pixels: usize,
    pub selected_wallpaper_color: Rgb,
    pub on_surface_contrast: f32,
    pub on_background_contrast: f32,
    pub on_primary_contrast: f32,
    pub on_secondary_contrast: f32,
    pub on_tertiary_contrast: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemeDocument {
    pub schema_version: u32,
    pub mode: ColorMode,
    pub style: PaletteStyle,
    pub contrast_target: f32,
    pub source_file: String,
    pub colors: SemanticColors,
    pub diagnostics: ThemeDiagnostics,
}

/// Generate a theme from a local wallpaper. Decoding and sampling are bounded
/// so a malformed or unusually large image cannot consume unbounded memory.
pub fn generate_from_path(path: impl AsRef<Path>, options: &ThemeOptions) -> Result<ThemeDocument> {
    validate_options(options)?;
    let path = path.as_ref();
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("cannot inspect wallpaper {}", path.display()))?;
    ensure!(metadata.len() <= MAX_FILE_BYTES, "wallpaper exceeds 64 MiB");

    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_EDGE);
    limits.max_image_height = Some(MAX_IMAGE_EDGE);
    limits.max_alloc = Some(256 * 1024 * 1024);
    let mut reader = ImageReader::open(path)
        .with_context(|| format!("cannot open wallpaper {}", path.display()))?
        .with_guessed_format()
        .context("cannot identify wallpaper format")?;
    reader.limits(limits);
    let image = reader.decode().context("cannot decode wallpaper")?;

    generate_from_pixels(
        image
            .thumbnail(SAMPLE_EDGE, SAMPLE_EDGE)
            .to_rgb8()
            .pixels()
            .map(|pixel| Rgb::new(pixel[0], pixel[1], pixel[2]))
            .collect(),
        options,
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("wallpaper"),
    )
}

/// Generate a palette from RGB samples. Public to make the color engine easy
/// to test and reusable by wallpaper providers other than local image files.
pub fn generate_from_pixels(
    pixels: Vec<Rgb>,
    options: &ThemeOptions,
    source_file: &str,
) -> Result<ThemeDocument> {
    validate_options(options)?;
    ensure!(!pixels.is_empty(), "wallpaper has no usable color samples");

    let selected = dominant_color(&pixels).context("cannot select wallpaper color")?;
    let mut accent = Hsl::from_rgb(selected);
    if accent.saturation < 0.08 {
        // Grayscale wallpapers need a deliberate chromatic seed, not hue noise.
        accent.hue = 190.0;
        accent.saturation = 0.52;
    } else {
        accent.saturation = match options.style {
            PaletteStyle::Balanced => accent.saturation.clamp(0.42, 0.72),
            PaletteStyle::Vivid => accent.saturation.clamp(0.68, 0.92),
            PaletteStyle::Muted => accent.saturation.clamp(0.24, 0.48),
        };
    }

    let colors = build_semantic_colors(accent, options);
    let diagnostics = ThemeDiagnostics {
        sampled_pixels: pixels.len(),
        selected_wallpaper_color: selected,
        on_surface_contrast: contrast_ratio(colors.on_surface, colors.surface),
        on_background_contrast: contrast_ratio(colors.on_background, colors.background),
        on_primary_contrast: contrast_ratio(colors.on_primary, colors.primary),
        on_secondary_contrast: contrast_ratio(colors.on_secondary, colors.secondary),
        on_tertiary_contrast: contrast_ratio(colors.on_tertiary, colors.tertiary),
    };

    Ok(ThemeDocument {
        schema_version: THEME_SCHEMA_VERSION,
        mode: options.mode,
        style: options.style,
        contrast_target: options.contrast_target,
        source_file: source_file.to_owned(),
        colors,
        diagnostics,
    })
}

fn validate_options(options: &ThemeOptions) -> Result<()> {
    ensure!(
        options.contrast_target.is_finite() && (4.5..=7.0).contains(&options.contrast_target),
        "contrast_target must be between 4.5 and 7.0"
    );
    Ok(())
}

fn build_semantic_colors(base: Hsl, options: &ThemeOptions) -> SemanticColors {
    let dark = options.mode == ColorMode::Dark;
    let primary = accessible_tone(
        base,
        if dark { 0.78 } else { 0.40 },
        options.contrast_target,
        dark,
    );
    let primary_container = accessible_tone(
        base,
        if dark { 0.30 } else { 0.88 },
        options.contrast_target,
        !dark,
    );
    let secondary_hsl = Hsl {
        hue: (base.hue + 36.0).rem_euclid(360.0),
        saturation: (base.saturation * 0.72).clamp(0.25, 0.68),
        lightness: base.lightness,
    };
    let tertiary_hsl = Hsl {
        hue: (base.hue + 300.0).rem_euclid(360.0),
        saturation: (base.saturation * 0.82).clamp(0.30, 0.78),
        lightness: base.lightness,
    };
    let secondary = accessible_tone(
        secondary_hsl,
        if dark { 0.76 } else { 0.42 },
        options.contrast_target,
        dark,
    );
    let tertiary = accessible_tone(
        tertiary_hsl,
        if dark { 0.78 } else { 0.40 },
        options.contrast_target,
        dark,
    );
    let background = if dark {
        Rgb::new(12, 14, 22)
    } else {
        Rgb::new(248, 247, 252)
    };
    let surface = if dark {
        Rgb::new(20, 23, 34)
    } else {
        Rgb::WHITE
    };
    let surface_variant = if dark {
        Rgb::new(39, 43, 58)
    } else {
        Rgb::new(231, 229, 239)
    };
    let error = accessible_tone(
        Hsl::from_rgb(Rgb::new(186, 26, 26)),
        0.42,
        options.contrast_target,
        dark,
    );

    SemanticColors {
        primary,
        on_primary: readable_foreground(primary, options.contrast_target),
        primary_container,
        on_primary_container: readable_foreground(primary_container, options.contrast_target),
        secondary,
        on_secondary: readable_foreground(secondary, options.contrast_target),
        tertiary,
        on_tertiary: readable_foreground(tertiary, options.contrast_target),
        background,
        on_background: readable_foreground(background, options.contrast_target),
        surface,
        on_surface: readable_foreground(surface, options.contrast_target),
        surface_variant,
        on_surface_variant: readable_foreground(surface_variant, options.contrast_target),
        outline: if dark {
            Rgb::new(143, 147, 164)
        } else {
            Rgb::new(104, 101, 117)
        },
        error,
        on_error: readable_foreground(error, options.contrast_target),
        success: Rgb::new(46, 160, 105),
        warning: Rgb::new(210, 143, 25),
        info: Rgb::new(50, 130, 210),
        glass_tint: if dark { primary_container } else { primary },
        glass_border: if dark {
            Rgb::new(95, 105, 133)
        } else {
            Rgb::new(180, 176, 199)
        },
        glass_highlight: if dark {
            Rgb::new(185, 205, 255)
        } else {
            Rgb::WHITE
        },
        neon_cyan: Rgb::new(45, 225, 235),
        neon_magenta: Rgb::new(245, 65, 190),
        neon_violet: Rgb::new(165, 95, 255),
        neon_lime: Rgb::new(180, 245, 70),
    }
}

/// Shift an accent's lightness only as far as needed to make either black
/// or white text meet the requested contrast ratio.
fn accessible_tone(base: Hsl, preferred: f32, target: f32, prefer_light: bool) -> Rgb {
    let steps = 100;
    for step in 0..=steps {
        let delta = step as f32 / steps as f32;
        let lightness = if prefer_light {
            preferred + (1.0 - preferred) * delta
        } else {
            preferred * (1.0 - delta)
        };
        let candidate = base.with_lightness(lightness).to_rgb();
        if contrast_ratio(Rgb::BLACK, candidate).max(contrast_ratio(Rgb::WHITE, candidate))
            >= target
        {
            return candidate;
        }
    }
    // Black or white always provides at least 1:1 and the loop reaches both
    // extremes in the requested direction; this is a defensive fallback.
    base.with_lightness(if prefer_light { 1.0 } else { 0.0 })
        .to_rgb()
}

fn readable_foreground(background: Rgb, target: f32) -> Rgb {
    let black_ratio = contrast_ratio(Rgb::BLACK, background);
    let white_ratio = contrast_ratio(Rgb::WHITE, background);
    if black_ratio >= target && black_ratio >= white_ratio {
        Rgb::BLACK
    } else if white_ratio >= target || white_ratio >= black_ratio {
        Rgb::WHITE
    } else {
        Rgb::BLACK
    }
}

pub fn contrast_ratio(foreground: Rgb, background: Rgb) -> f32 {
    let a = luminance(foreground);
    let b = luminance(background);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn luminance(color: Rgb) -> f32 {
    fn linear(channel: u8) -> f32 {
        let value = channel as f32 / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

#[derive(Clone, Copy)]
struct Hsl {
    hue: f32,
    saturation: f32,
    lightness: f32,
}

impl Hsl {
    fn from_rgb(color: Rgb) -> Self {
        let r = color.r as f32 / 255.0;
        let g = color.g as f32 / 255.0;
        let b = color.b as f32 / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;
        let lightness = (max + min) / 2.0;
        if delta == 0.0 {
            return Self {
                hue: 0.0,
                saturation: 0.0,
                lightness,
            };
        }
        let saturation = delta / (1.0 - (2.0 * lightness - 1.0).abs());
        let hue = if max == r {
            60.0 * ((g - b) / delta).rem_euclid(6.0)
        } else if max == g {
            60.0 * ((b - r) / delta + 2.0)
        } else {
            60.0 * ((r - g) / delta + 4.0)
        };
        Self {
            hue: hue.rem_euclid(360.0),
            saturation,
            lightness,
        }
    }

    fn with_lightness(self, lightness: f32) -> Self {
        Self { lightness, ..self }
    }

    fn to_rgb(self) -> Rgb {
        let chroma = (1.0 - (2.0 * self.lightness - 1.0).abs()) * self.saturation;
        let x = chroma * (1.0 - ((self.hue / 60.0).rem_euclid(2.0) - 1.0).abs());
        let m = self.lightness - chroma / 2.0;
        let (r, g, b) = match self.hue.rem_euclid(360.0) as u32 {
            0..=59 => (chroma, x, 0.0),
            60..=119 => (x, chroma, 0.0),
            120..=179 => (0.0, chroma, x),
            180..=239 => (0.0, x, chroma),
            240..=299 => (x, 0.0, chroma),
            _ => (chroma, 0.0, x),
        };
        let encode = |value: f32| ((value + m) * 255.0).round().clamp(0.0, 255.0) as u8;
        Rgb::new(encode(r), encode(g), encode(b))
    }
}

fn rgb_to_oklab(color: Rgb) -> [f32; 3] {
    fn linear(channel: u8) -> f32 {
        let value = channel as f32 / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }
    let r = linear(color.r);
    let g = linear(color.g);
    let b = linear(color.b);
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

fn oklab_to_rgb(lab: [f32; 3]) -> Rgb {
    let l = (lab[0] + 0.3963377774 * lab[1] + 0.2158037573 * lab[2]).powi(3);
    let m = (lab[0] - 0.1055613458 * lab[1] - 0.0638541728 * lab[2]).powi(3);
    let s = (lab[0] - 0.0894841775 * lab[1] - 1.2914855480 * lab[2]).powi(3);
    fn encode(value: f32) -> u8 {
        let srgb = if value <= 0.0031308 {
            12.92 * value
        } else {
            1.055 * value.max(0.0).powf(1.0 / 2.4) - 0.055
        };
        (srgb * 255.0).round().clamp(0.0, 255.0) as u8
    }
    Rgb::new(
        encode(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
        encode(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
        encode(-0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s),
    )
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// Deterministic, chroma-aware k-means clustering in perceptual OKLab space.
fn dominant_color(pixels: &[Rgb]) -> Option<Rgb> {
    if pixels.is_empty() {
        return None;
    }
    let points: Vec<[f32; 3]> = pixels.iter().copied().map(rgb_to_oklab).collect();
    let first = points
        .iter()
        .enumerate()
        .max_by(|(ia, a), (ib, b)| {
            a[1].hypot(a[2])
                .total_cmp(&b[1].hypot(b[2]))
                .then_with(|| ib.cmp(ia))
        })?
        .0;
    let mut centers = vec![points[first]];

    while centers.len() < CLUSTER_COUNT.min(points.len()) {
        let next = points
            .iter()
            .enumerate()
            .map(|(index, point)| {
                let nearest = centers
                    .iter()
                    .map(|center| distance(*point, *center))
                    .fold(f32::INFINITY, f32::min);
                (index, nearest)
            })
            .max_by(|(ia, da), (ib, db)| da.total_cmp(db).then_with(|| ib.cmp(ia)))?
            .0;
        if centers
            .iter()
            .any(|center| distance(*center, points[next]) <= f32::EPSILON)
        {
            break;
        }
        centers.push(points[next]);
    }

    let mut counts = vec![0usize; centers.len()];
    for _ in 0..KMEANS_ITERATIONS {
        let mut sums = vec![[0.0f64; 3]; centers.len()];
        counts.fill(0);
        for point in &points {
            let nearest = centers
                .iter()
                .enumerate()
                .min_by(|(ia, a), (ib, b)| {
                    distance(*point, **a)
                        .total_cmp(&distance(*point, **b))
                        .then_with(|| ia.cmp(ib))
                })?
                .0;
            counts[nearest] += 1;
            for channel in 0..3 {
                sums[nearest][channel] += point[channel] as f64;
            }
        }
        for (index, count) in counts.iter().copied().enumerate() {
            if count > 0 {
                for channel in 0..3 {
                    centers[index][channel] = (sums[index][channel] / count as f64) as f32;
                }
            }
        }
    }

    centers
        .iter()
        .enumerate()
        .filter(|(index, _)| counts[*index] > 0)
        .map(|(index, center)| {
            let chroma = center[1].hypot(center[2]);
            let score = counts[index] as f32 * (0.08 + chroma * 5.0);
            (oklab_to_rgb(*center), score, index)
        })
        .max_by(|a, b| a.1.total_cmp(&b.1).then_with(|| b.2.cmp(&a.2)))
        .map(|best| best.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageFormat, Rgb as ImageRgb, RgbImage};
    use std::{io::Cursor, path::PathBuf};

    fn write_test_image(directory: &std::path::Path, name: &str, format: ImageFormat) -> PathBuf {
        let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(8, 8, ImageRgb([32, 128, 220])));
        let mut encoded = Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, format)
            .expect("encode test image");
        let path = directory.join(name);
        std::fs::write(&path, encoded.into_inner()).expect("write test image");
        path
    }

    #[test]
    fn decodes_all_supported_wallpaper_formats() {
        let directory = tempfile::tempdir().expect("create temporary directory");
        for (name, format) in [
            ("wallpaper.png", ImageFormat::Png),
            ("wallpaper.jpg", ImageFormat::Jpeg),
            ("wallpaper.webp", ImageFormat::WebP),
        ] {
            let path = write_test_image(directory.path(), name, format);
            let theme = generate_from_path(&path, &ThemeOptions::default())
                .unwrap_or_else(|error| panic!("failed to decode {name}: {error:#}"));
            assert_eq!(theme.schema_version, THEME_SCHEMA_VERSION);
            assert!(theme.diagnostics.sampled_pixels > 0);
        }
    }

    #[test]
    fn rejects_corrupt_and_truncated_image_data() {
        let directory = tempfile::tempdir().expect("create temporary directory");
        for (name, bytes) in [
            ("corrupt.png", b"\x89PNG\r\n\x1a\nnot-image-data".as_slice()),
            ("truncated.png", b"\x89PNG\r\n\x1a\n".as_slice()),
        ] {
            let path = directory.path().join(name);
            std::fs::write(&path, bytes).expect("write invalid image");
            assert!(
                generate_from_path(&path, &ThemeOptions::default()).is_err(),
                "{name} must be rejected"
            );
        }
    }

    #[test]
    fn rejects_images_exceeding_dimension_limits() {
        let directory = tempfile::tempdir().expect("create temporary directory");
        let path = write_test_image(directory.path(), "oversized.png", ImageFormat::Png);
        let mut bytes = std::fs::read(&path).expect("read encoded PNG");
        // PNG IHDR stores width and height as big-endian u32 values.
        bytes[16..20].copy_from_slice(&(MAX_IMAGE_EDGE + 1).to_be_bytes());
        std::fs::write(&path, bytes).expect("write oversized PNG header");

        assert!(generate_from_path(&path, &ThemeOptions::default()).is_err());
    }

    #[test]
    fn rejects_files_larger_than_the_decode_limit_before_reading() {
        let directory = tempfile::tempdir().expect("create temporary directory");
        let path = directory.path().join("oversized.png");
        let file = std::fs::File::create(&path).expect("create sparse image");
        file.set_len(MAX_FILE_BYTES + 1)
            .expect("extend sparse image");

        let error = generate_from_path(&path, &ThemeOptions::default())
            .expect_err("oversized image must be rejected");
        assert!(format!("{error:#}").contains("wallpaper exceeds 64 MiB"));
    }

    #[test]
    fn ok_lab_round_trip_is_close_for_srgb_colors() {
        for color in [
            Rgb::new(0, 0, 0),
            Rgb::WHITE,
            Rgb::new(255, 0, 0),
            Rgb::new(0, 255, 0),
            Rgb::new(0, 0, 255),
            Rgb::new(30, 180, 220),
            Rgb::new(128, 128, 128),
        ] {
            let restored = oklab_to_rgb(rgb_to_oklab(color));
            assert!(
                restored.r.abs_diff(color.r) <= 1,
                "{color:?} -> {restored:?}"
            );
            assert!(
                restored.g.abs_diff(color.g) <= 1,
                "{color:?} -> {restored:?}"
            );
            assert!(
                restored.b.abs_diff(color.b) <= 1,
                "{color:?} -> {restored:?}"
            );
        }
    }

    #[test]
    fn colorful_subject_is_not_lost_in_a_large_neutral_background() {
        let mut pixels = vec![Rgb::new(245, 245, 245); 3072];
        pixels.extend(vec![Rgb::new(20, 190, 210); 1024]);
        let selected = dominant_color(&pixels).unwrap();
        assert!(Hsl::from_rgb(selected).saturation > 0.5);
    }

    #[test]
    fn palette_styles_have_distinct_saturation() {
        let pixels = vec![Rgb::new(30, 80, 220); 256];
        let make = |style| {
            generate_from_pixels(
                pixels.clone(),
                &ThemeOptions {
                    style,
                    ..ThemeOptions::default()
                },
                "unit-test",
            )
            .unwrap()
        };
        let balanced = Hsl::from_rgb(make(PaletteStyle::Balanced).colors.primary).saturation;
        let vivid = Hsl::from_rgb(make(PaletteStyle::Vivid).colors.primary).saturation;
        let muted = Hsl::from_rgb(make(PaletteStyle::Muted).colors.primary).saturation;
        assert!(vivid > balanced);
        assert!(balanced > muted);
    }

    #[test]
    fn semantic_text_pairs_meet_requested_contrast() {
        for mode in [ColorMode::Dark, ColorMode::Light] {
            let options = ThemeOptions {
                mode,
                contrast_target: 7.0,
                ..ThemeOptions::default()
            };
            let theme =
                generate_from_pixels(vec![Rgb::new(220, 40, 120); 64], &options, "contrast-test")
                    .unwrap();
            let c = &theme.colors;
            for (foreground, background) in [
                (c.on_background, c.background),
                (c.on_surface, c.surface),
                (c.on_primary, c.primary),
                (c.on_primary_container, c.primary_container),
                (c.on_secondary, c.secondary),
                (c.on_tertiary, c.tertiary),
                (c.on_surface_variant, c.surface_variant),
                (c.on_error, c.error),
            ] {
                assert!(contrast_ratio(foreground, background) >= 7.0);
            }
        }
    }

    #[test]
    fn grayscale_wallpaper_gets_a_stable_chromatic_accent() {
        let theme = generate_from_pixels(
            vec![Rgb::new(128, 128, 128); 16],
            &ThemeOptions::default(),
            "gray",
        )
        .unwrap();
        assert_ne!(theme.colors.primary, Rgb::new(128, 128, 128));
        assert!(Hsl::from_rgb(theme.colors.primary).saturation > 0.4);
    }

    #[test]
    fn empty_samples_and_invalid_contrast_are_rejected() {
        assert!(generate_from_pixels(vec![], &ThemeOptions::default(), "empty").is_err());
        let options = ThemeOptions {
            contrast_target: 3.0,
            ..ThemeOptions::default()
        };
        assert!(generate_from_pixels(vec![Rgb::WHITE], &options, "invalid").is_err());
    }

    #[test]
    fn output_schema_is_versioned_and_stable() {
        let theme = generate_from_pixels(
            vec![Rgb::new(50, 100, 200); 16],
            &ThemeOptions::default(),
            "wallpaper.png",
        )
        .unwrap();
        assert_eq!(theme.schema_version, THEME_SCHEMA_VERSION);
        assert_eq!(theme.colors.primary.to_hex().len(), 7);
    }
}
