//! Live GPU backdrop blur.
//!
//! The compositor renders the scene behind its glass surfaces into an
//! offscreen texture, downsamples it and runs a Dual Kawase blur. The
//! resulting texture is sampled by the bar, menus, launcher, notifications and
//! lock screen, so those surfaces blur the actual windows behind them instead
//! of a pre-baked wallpaper copy.

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, Color32F, Frame, Offscreen, Renderer, Texture,
            gles::{
                GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform,
                UniformName, UniformType,
            },
            utils::draw_render_elements,
        },
    },
    utils::{Buffer, Physical, Point, Rectangle, Size, Transform},
};
use tracing::warn;

use crate::{render::BubbleRenderElement, ui::GlassRegion};

/// The blur source is rendered at a fraction of the output resolution. A
/// quarter-resolution source keeps the extra GPU cost tiny even at 4K.
const DOWNSCALE: i32 = 4;
const DEFAULT_BLUR_RADIUS: f32 = 3.0;

const KAWASE_SHADER: &str = r#"
#version 100
//_DEFINES_
precision mediump float;
uniform sampler2D tex;
uniform float alpha;
uniform vec2 texel_size;
uniform float offset;
varying vec2 v_coords;
void main() {
    vec2 d = texel_size * offset;
    vec4 color = texture2D(tex, v_coords + vec2(-d.x, -d.y));
    color += texture2D(tex, v_coords + vec2( d.x, -d.y));
    color += texture2D(tex, v_coords + vec2(-d.x,  d.y));
    color += texture2D(tex, v_coords + vec2( d.x,  d.y));
    gl_FragColor = color * (0.25 * alpha);
}
"#;

const MASK_SHADER: &str = r#"
#version 100
//_DEFINES_
precision mediump float;
uniform sampler2D tex;
uniform float alpha;
uniform vec4 rect;
uniform vec2 size;
uniform float radius;
varying vec2 v_coords;
void main() {
    vec4 color = texture2D(tex, v_coords);
    vec2 local = (v_coords - rect.xy) / rect.zw * size;
    vec2 half_size = size * 0.5;
    vec2 q = abs(local - half_size) - (half_size - vec2(radius));
    float dist = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
    float mask = 1.0 - smoothstep(-1.0, 1.0, dist);
    gl_FragColor = vec4(color.rgb, color.a * mask * alpha);
}
"#;

pub struct BlurPipeline {
    scene: Option<GlesTexture>,
    down: Option<GlesTexture>,
    temp: Option<GlesTexture>,
    blurred: Option<GlesTexture>,
    scene_size: Size<i32, Buffer>,
    blur_size: Size<i32, Buffer>,
    kawase_size: Size<i32, Buffer>,
    blur_program: Option<GlesTexProgram>,
    mask_program: Option<GlesTexProgram>,
    radius: f32,
}

impl Default for BlurPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl BlurPipeline {
    pub fn new() -> Self {
        Self {
            scene: None,
            down: None,
            temp: None,
            blurred: None,
            scene_size: (0, 0).into(),
            blur_size: (0, 0).into(),
            kawase_size: (0, 0).into(),
            blur_program: None,
            mask_program: None,
            radius: DEFAULT_BLUR_RADIUS,
        }
    }

    /// Updates the blur strength live, without recreating the GL resources.
    pub fn set_radius(&mut self, radius: f32) {
        self.radius = radius.clamp(0.0, 16.0);
    }

    pub fn blurred_texture(&self) -> Option<&GlesTexture> {
        self.blurred.as_ref()
    }

    /// Captures `elements` and produces a blurred copy of the scene.
    pub fn prepare(
        &mut self,
        renderer: &mut GlesRenderer,
        elements: &[BubbleRenderElement],
        size: Size<i32, Physical>,
        scale: f64,
    ) -> Result<(), GlesError> {
        if size.w <= 0 || size.h <= 0 {
            return Ok(());
        }
        self.ensure_programs(renderer)?;
        let buffer_size = Size::<i32, Buffer>::from((size.w, size.h));
        let blur_size =
            Size::<i32, Buffer>::from(((size.w / DOWNSCALE).max(1), (size.h / DOWNSCALE).max(1)));
        let kawase_size =
            Size::<i32, Buffer>::from(((blur_size.w / 2).max(1), (blur_size.h / 2).max(1)));
        if self.scene.is_none()
            || self.scene_size != buffer_size
            || self.blur_size != blur_size
            || self.kawase_size != kawase_size
        {
            self.scene = Some(Offscreen::<GlesTexture>::create_buffer(
                renderer,
                Fourcc::Abgr8888,
                buffer_size,
            )?);
            self.down = Some(Offscreen::<GlesTexture>::create_buffer(
                renderer,
                Fourcc::Abgr8888,
                blur_size,
            )?);
            self.temp = Some(Offscreen::<GlesTexture>::create_buffer(
                renderer,
                Fourcc::Abgr8888,
                kawase_size,
            )?);
            self.blurred = Some(Offscreen::<GlesTexture>::create_buffer(
                renderer,
                Fourcc::Abgr8888,
                blur_size,
            )?);
            self.scene_size = buffer_size;
            self.blur_size = blur_size;
            self.kawase_size = kawase_size;
        }

        // 1. Capture the live scene.
        {
            let scene = self.scene.as_mut().expect("scene texture exists");
            let mut target = renderer.bind(scene)?;
            let mut frame = renderer.render(&mut target, size, Transform::Normal)?;
            let damage = [Rectangle::from_size(size)];
            frame.clear(Color32F::new(0.0, 0.0, 0.0, 1.0), &damage)?;
            draw_render_elements(&mut frame, scale, elements, &damage)?;
            let _ = frame.finish()?;
        }

        // 2. Downsample to the blur resolution.
        {
            let source = self.scene.clone().expect("scene texture exists");
            let down = self.down.as_mut().expect("down texture exists");
            let mut target = renderer.bind(down)?;
            let physical_size = Size::<i32, Physical>::from((blur_size.w, blur_size.h));
            let mut frame = renderer.render(&mut target, physical_size, Transform::Normal)?;
            let damage = [Rectangle::from_size(physical_size)];
            frame.clear(Color32F::new(0.0, 0.0, 0.0, 1.0), &damage)?;
            frame.render_texture_from_to(
                &source,
                Rectangle::new(
                    Point::from((0.0, 0.0)),
                    Size::from((size.w as f64, size.h as f64)),
                ),
                Rectangle::from_size(physical_size),
                &damage,
                &[],
                Transform::Normal,
                1.0,
                None,
                &[],
            )?;
            let _ = frame.finish()?;
        }

        // Dual Kawase: gather at half the quarter-resolution target, then
        // gather back while upsampling. This gives a broad blur with two
        // inexpensive passes and avoids a large Gaussian kernel.
        self.kawase_pass(renderer, true)?;
        self.kawase_pass(renderer, false)?;
        Ok(())
    }

    /// Draws the blurred scene into one rounded glass region.
    pub fn draw_region(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        region: GlassRegion,
        scale: f64,
        output: Size<i32, Physical>,
    ) {
        let Some(blurred) = self.blurred.as_ref() else {
            return;
        };
        let Some(program) = self.mask_program.as_ref() else {
            return;
        };
        let panel = Rectangle::<i32, Physical>::new(
            Point::from((
                (region.x as f64 * scale).round() as i32,
                (region.y as f64 * scale).round() as i32,
            )),
            Size::from((
                (region.width as f64 * scale).round().max(1.0) as i32,
                (region.height as f64 * scale).round().max(1.0) as i32,
            )),
        );
        let Some(panel) = panel.intersection(Rectangle::from_size(output)) else {
            return;
        };
        if panel.size.w <= 0 || panel.size.h <= 0 {
            return;
        }
        let texture_size = blurred.size();
        let inv = 1.0 / DOWNSCALE as f64;
        let source = Rectangle::<f64, Buffer>::new(
            Point::from((panel.loc.x as f64 * inv, panel.loc.y as f64 * inv)),
            Size::from((panel.size.w as f64 * inv, panel.size.h as f64 * inv)),
        );
        let rect = (
            source.loc.x as f32 / texture_size.w as f32,
            source.loc.y as f32 / texture_size.h as f32,
            source.size.w as f32 / texture_size.w as f32,
            source.size.h as f32 / texture_size.h as f32,
        );
        let damage = [panel];
        let uniforms = [
            Uniform::new("rect", rect),
            Uniform::new("size", (panel.size.w as f32, panel.size.h as f32)),
            Uniform::new("radius", (region.radius as f64 * scale) as f32),
        ];
        if let Err(error) = frame.render_texture_from_to(
            blurred,
            source,
            panel,
            &damage,
            &[],
            Transform::Normal,
            1.0,
            Some(program),
            &uniforms,
        ) {
            warn!(%error, "cannot draw the backdrop blur");
        }
    }

    fn ensure_programs(&mut self, renderer: &mut GlesRenderer) -> Result<(), GlesError> {
        if self.blur_program.is_none() {
            self.blur_program = Some(renderer.compile_custom_texture_shader(
                KAWASE_SHADER,
                &[
                    UniformName::new("texel_size", UniformType::_2f),
                    UniformName::new("offset", UniformType::_1f),
                ],
            )?);
        }
        if self.mask_program.is_none() {
            self.mask_program = Some(renderer.compile_custom_texture_shader(
                MASK_SHADER,
                &[
                    UniformName::new("rect", UniformType::_4f),
                    UniformName::new("size", UniformType::_2f),
                    UniformName::new("radius", UniformType::_1f),
                ],
            )?);
        }
        Ok(())
    }

    fn kawase_pass(
        &mut self,
        renderer: &mut GlesRenderer,
        down_pass: bool,
    ) -> Result<(), GlesError> {
        let source = if down_pass {
            self.down.clone().expect("down texture exists")
        } else {
            self.temp.clone().expect("temp texture exists")
        };
        let program = self.blur_program.clone().expect("blur program exists");
        let source_size = if down_pass {
            self.blur_size
        } else {
            self.kawase_size
        };
        let target_size = if down_pass {
            self.kawase_size
        } else {
            self.blur_size
        };
        let target = if down_pass {
            self.temp.as_mut().expect("temp texture exists")
        } else {
            self.blurred.as_mut().expect("blurred texture exists")
        };
        let mut target = renderer.bind(target)?;
        let physical_size = Size::<i32, Physical>::from((target_size.w, target_size.h));
        let mut frame = renderer.render(&mut target, physical_size, Transform::Normal)?;
        let damage = [Rectangle::from_size(physical_size)];
        frame.clear(Color32F::new(0.0, 0.0, 0.0, 1.0), &damage)?;
        let texel = (1.0 / source_size.w as f32, 1.0 / source_size.h as f32);
        frame.render_texture_from_to(
            &source,
            Rectangle::new(
                Point::from((0.0, 0.0)),
                Size::from((source_size.w as f64, source_size.h as f64)),
            ),
            Rectangle::from_size(physical_size),
            &damage,
            &[],
            Transform::Normal,
            1.0,
            Some(&program),
            &[
                Uniform::new("texel_size", texel),
                Uniform::new(
                    "offset",
                    if down_pass {
                        self.radius.clamp(0.5, 16.0)
                    } else {
                        (self.radius * 0.5).clamp(0.5, 8.0)
                    },
                ),
            ],
        )?;
        let _ = frame.finish()?;
        Ok(())
    }
}
