//! Shared render element types and small drawing helpers.
//!
//! Both the Winit and DRM backends use the same `GlesRenderer`, so the scene
//! assembled by the compositor is identical on both targets.

use smithay::{
    backend::renderer::{
        Color32F,
        element::{
            Element, Id, Kind, RenderElement, UnderlyingStorage,
            memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            solid::{SolidColorBuffer, SolidColorRenderElement},
            surface::WaylandSurfaceRenderElement,
        },
        gles::{GlesError, GlesFrame, GlesRenderer},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    desktop::space::SpaceRenderElements,
    utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform},
};

smithay::backend::renderer::element::render_elements! {
    /// Complete scene element used by both the Winit and DRM backends.
    ///
    /// The concrete `GlesRenderer` renderer type keeps the enum simple and lets
    /// the DRM backend hand the whole scene to `DrmCompositor` in one slice.
    pub BubbleRenderElement<=GlesRenderer>;
    Space=SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>,
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Memory=MemoryRenderBufferRenderElement<GlesRenderer>,
    Solid=SolidColorRenderElement,
    Animated=AnimatedElement<SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>>,
    AnimatedMemory=AnimatedElement<MemoryRenderBufferRenderElement<GlesRenderer>>,
    AnimatedSolid=AnimatedElement<SolidColorRenderElement>,
}

/// Wraps a render element with a compositor animation transform.
///
/// The wrapper changes only the destination rectangle and opacity. Wayland
/// buffers are therefore never resized, which keeps clients stable while their
/// window opens, closes, minimizes or slides between workspaces.
#[derive(Debug)]
pub struct AnimatedElement<E> {
    inner: E,
    scale: f64,
    alpha: f32,
    offset: Point<f64, Physical>,
}

impl<E> AnimatedElement<E> {
    pub fn new(inner: E, scale: f64, alpha: f32, offset: Point<f64, Physical>) -> Self {
        Self {
            inner,
            scale: scale.clamp(0.01, 8.0),
            alpha: alpha.clamp(0.0, 1.0),
            offset,
        }
    }

    fn animated(&self) -> bool {
        (self.scale - 1.0).abs() > f64::EPSILON
            || (self.alpha - 1.0).abs() > f32::EPSILON
            || self.offset.x.abs() > f64::EPSILON
            || self.offset.y.abs() > f64::EPSILON
    }
}

impl<E: Element> Element for AnimatedElement<E> {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.geometry(scale).loc
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        transform_geometry(self.inner.geometry(scale), self.scale, self.offset)
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        if self.animated() {
            DamageSet::from_slice(&[Rectangle::from_size(self.geometry(scale).size)])
        } else {
            self.inner.damage_since(scale, commit)
        }
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        if self.animated() {
            OpaqueRegions::default()
        } else {
            self.inner.opaque_regions(scale)
        }
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha() * self.alpha
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl<E> RenderElement<GlesRenderer> for AnimatedElement<E>
where
    E: Element + RenderElement<GlesRenderer>,
{
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        let animated = self.animated();
        let dst = if animated {
            transform_geometry(dst, self.scale, self.offset)
        } else {
            dst
        };
        self.inner.draw(
            frame,
            src,
            dst,
            if animated { &[] } else { damage },
            if animated { &[] } else { opaque_regions },
        )
    }

    fn underlying_storage(&self, renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        if self.animated() {
            None
        } else {
            self.inner.underlying_storage(renderer)
        }
    }
}

fn transform_geometry(
    rect: Rectangle<i32, Physical>,
    scale: f64,
    offset: Point<f64, Physical>,
) -> Rectangle<i32, Physical> {
    let center_x = rect.loc.x as f64 + rect.size.w as f64 / 2.0;
    let center_y = rect.loc.y as f64 + rect.size.h as f64 / 2.0;
    let width = (rect.size.w as f64 * scale).round().max(1.0) as i32;
    let height = (rect.size.h as f64 * scale).round().max(1.0) as i32;
    let x = (center_x - width as f64 / 2.0 + offset.x).round() as i32;
    let y = (center_y - height as f64 / 2.0 + offset.y).round() as i32;
    Rectangle::new(Point::from((x, y)), Size::from((width, height)))
}

/// Parses `#rrggbb` or `#rrggbbaa` into a floating point color.
pub fn parse_color(value: &str, alpha: f32) -> Color32F {
    let value = value.trim_start_matches('#');
    if value.len() < 6 {
        return Color32F::new(0.0, 0.0, 0.0, alpha);
    }
    let component = |range: std::ops::Range<usize>| {
        u8::from_str_radix(value.get(range).unwrap_or("00"), 16).unwrap_or(0) as f32 / 255.0
    };
    Color32F::new(component(0..2), component(2..4), component(4..6), alpha)
}

/// Builds a solid render element from a logical rectangle.
pub fn solid(
    buffer: &SolidColorBuffer,
    location: Point<i32, Logical>,
    scale: f64,
) -> SolidColorRenderElement {
    SolidColorRenderElement::from_buffer(
        buffer,
        location.to_physical_precise_round(scale),
        scale,
        1.0,
        Kind::Unspecified,
    )
}

/// Builds a memory render element from an RGBA pixmap.
pub fn memory_element(
    renderer: &mut GlesRenderer,
    buffer: &MemoryRenderBuffer,
    location: Point<i32, Logical>,
    scale: f64,
) -> Result<
    MemoryRenderBufferRenderElement<GlesRenderer>,
    smithay::backend::renderer::gles::GlesError,
> {
    MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        location.to_f64().to_physical(scale),
        buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    )
}

/// Creates a memory buffer containing straight RGBA pixels.
pub fn memory_buffer(pixels: &[u8], size: Size<i32, Logical>) -> MemoryRenderBuffer {
    MemoryRenderBuffer::from_slice(
        pixels,
        smithay::backend::allocator::Fourcc::Abgr8888,
        Size::<i32, Buffer>::from((size.w, size.h)),
        1,
        Transform::Normal,
        None,
    )
}

/// A cached RGBA pixmap positioned in logical output coordinates.
#[derive(Debug)]
pub struct Pixmap {
    pub buffer: MemoryRenderBuffer,
    pub location: Point<i32, Logical>,
    pub size: Size<i32, Logical>,
}

impl Pixmap {
    pub fn from_rgba(
        pixels: &[u8],
        location: Point<i32, Logical>,
        size: Size<i32, Logical>,
    ) -> Self {
        Self {
            buffer: memory_buffer(pixels, size),
            location,
            size,
        }
    }

    pub fn element(
        &self,
        renderer: &mut GlesRenderer,
        scale: f64,
    ) -> Option<MemoryRenderBufferRenderElement<GlesRenderer>> {
        memory_element(renderer, &self.buffer, self.location, scale).ok()
    }

    pub fn element_at(
        &self,
        renderer: &mut GlesRenderer,
        location: Point<f64, Logical>,
        scale: f64,
    ) -> Option<MemoryRenderBufferRenderElement<GlesRenderer>> {
        memory_element(
            renderer,
            &self.buffer,
            Point::from((location.x.round() as i32, location.y.round() as i32)),
            scale,
        )
        .ok()
    }
}
