//! Built-in wallpaper renderer (replaces swaybg).

use anyhow::{Context, Result};
use image::{DynamicImage, imageops::FilterType};
use smithay::{
    backend::renderer::Color32F,
    utils::{Logical, Size},
};
use tracing::warn;

use crate::{
    config::{EffectsConfig, WallpaperConfig},
    render::{Pixmap, parse_color},
    ui::GlassBackdrop,
};

/// Full-resolution wallpaper pixels kept for window corner masks.
pub struct WallpaperSource {
    pub rgba: Vec<u8>,
    pub size: Size<i32, Logical>,
}

/// A decoded and scaled wallpaper ready for compositing.
pub struct Wallpaper {
    pub pixmap: Option<Pixmap>,
    pub color: Color32F,
    /// Blurred, downscaled wallpaper sampled by glass panels.
    pub glass: Option<GlassBackdrop>,
    /// Unblurred pixels used to make rounded window corners match the desktop.
    pub source: Option<WallpaperSource>,
}

impl Wallpaper {
    pub fn load(
        config: &WallpaperConfig,
        effects: &EffectsConfig,
        output: Size<i32, Logical>,
    ) -> Result<Self> {
        let color = parse_color(&config.color, 1.0);
        if !config.enabled {
            return Ok(Self {
                pixmap: None,
                color,
                glass: None,
                source: None,
            });
        }

        let path = std::path::Path::new(&config.path);
        if !path.exists() {
            warn!(path = %path.display(), "wallpaper image is missing; using the background color");
            return Ok(Self {
                pixmap: None,
                color,
                glass: None,
                source: None,
            });
        }

        let image = image::open(path)
            .with_context(|| format!("cannot decode wallpaper: {}", path.display()))?;
        let rgba = render(&image, config, output, color);
        let glass = (effects.enabled && effects.blur)
            .then(|| GlassBackdrop::from_rgba(&rgba, output, 6, 12))
            .flatten();
        Ok(Self {
            pixmap: Some(Pixmap::from_rgba(&rgba, (0, 0).into(), output)),
            color,
            glass,
            source: Some(WallpaperSource { rgba, size: output }),
        })
    }
}

fn render(
    image: &DynamicImage,
    config: &WallpaperConfig,
    output: Size<i32, Logical>,
    background: Color32F,
) -> Vec<u8> {
    let output_width = output.w.max(1) as u32;
    let output_height = output.h.max(1) as u32;
    let color = [
        (background.r() * 255.0).round() as u8,
        (background.g() * 255.0).round() as u8,
        (background.b() * 255.0).round() as u8,
        255,
    ];
    let mut canvas = vec![0u8; (output_width * output_height * 4) as usize];
    for pixel in canvas.chunks_exact_mut(4) {
        pixel.copy_from_slice(&color);
    }

    match config.mode.as_str() {
        "stretch" => {
            let scaled = image.resize_exact(output_width, output_height, FilterType::Lanczos3);
            blit(
                &mut canvas,
                output_width,
                output_height,
                &scaled.to_rgba8(),
                0,
                0,
            );
        }
        "fit" => {
            let scaled = image.resize(output_width, output_height, FilterType::Lanczos3);
            let rgba = scaled.to_rgba8();
            let x = (output_width as i32 - rgba.width() as i32) / 2;
            let y = (output_height as i32 - rgba.height() as i32) / 2;
            blit(&mut canvas, output_width, output_height, &rgba, x, y);
        }
        "center" => {
            let rgba = image.to_rgba8();
            let x = (output_width as i32 - rgba.width() as i32) / 2;
            let y = (output_height as i32 - rgba.height() as i32) / 2;
            blit(&mut canvas, output_width, output_height, &rgba, x, y);
        }
        "tile" => {
            let rgba = image.to_rgba8();
            if rgba.width() == 0 || rgba.height() == 0 {
                return canvas;
            }
            let mut y = 0;
            while y < output_height as i32 {
                let mut x = 0;
                while x < output_width as i32 {
                    blit(&mut canvas, output_width, output_height, &rgba, x, y);
                    x += rgba.width() as i32;
                }
                y += rgba.height() as i32;
            }
        }
        _ => {
            // Fill: scale to cover the output and crop the overflow.
            let scale = (output_width as f64 / image.width() as f64)
                .max(output_height as f64 / image.height() as f64);
            let width = (image.width() as f64 * scale).ceil() as u32;
            let height = (image.height() as f64 * scale).ceil() as u32;
            let scaled = image.resize_exact(width.max(1), height.max(1), FilterType::Lanczos3);
            let rgba = scaled.to_rgba8();
            let x = (output_width as i32 - rgba.width() as i32) / 2;
            let y = (output_height as i32 - rgba.height() as i32) / 2;
            blit(&mut canvas, output_width, output_height, &rgba, x, y);
        }
    }
    canvas
}

fn blit(
    canvas: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    source: &image::RgbaImage,
    x: i32,
    y: i32,
) {
    let source_width = source.width() as i32;
    let source_height = source.height() as i32;
    for source_y in 0..source_height {
        let destination_y = y + source_y;
        if destination_y < 0 || destination_y >= canvas_height as i32 {
            continue;
        }
        for source_x in 0..source_width {
            let destination_x = x + source_x;
            if destination_x < 0 || destination_x >= canvas_width as i32 {
                continue;
            }
            let source_offset = ((source_y as u32 * source.width() + source_x as u32) * 4) as usize;
            let destination_offset =
                ((destination_y as u32 * canvas_width + destination_x as u32) * 4) as usize;
            canvas[destination_offset..destination_offset + 4]
                .copy_from_slice(&source.as_raw()[source_offset..source_offset + 4]);
        }
    }
}
