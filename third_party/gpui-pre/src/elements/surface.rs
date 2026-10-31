use crate::{
    App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    ObjectFit, Pixels, Style, StyleRefinement, Styled, Window,
};
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
#[cfg(any(target_os = "macos", target_os = "ios"))]
use objc2_core_foundation::CFRetained;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use objc2_core_video::{CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth};
use refineable::Refineable;

/// A source of a surface's content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SurfaceSource {
    /// A CoreVideo image buffer
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    Surface(CFRetained<CVPixelBuffer>),
    /// An NV12 picture in CPU memory, uploaded by the renderer when its generation changes
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    Nv12(Nv12Frame),
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
impl From<CFRetained<CVPixelBuffer>> for SurfaceSource {
    fn from(value: CFRetained<CVPixelBuffer>) -> Self {
        SurfaceSource::Surface(value)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
impl From<Nv12Frame> for SurfaceSource {
    fn from(value: Nv12Frame) -> Self {
        SurfaceSource::Nv12(value)
    }
}

/// Identifies one video stream drawn with [`surface`]. Renderers keep one set of textures per
/// id and reuse them for every frame of that stream.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SurfaceId(u64);

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
impl SurfaceId {
    /// A new id, distinct from every other in this process.
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
impl Default for SurfaceId {
    fn default() -> Self {
        Self::new()
    }
}

/// The pixels of an NV12 picture: a full-resolution Y plane, then a plane of interleaved Cb
/// and Cr at half the width and half the height, rounded up. Rows are tightly packed.
/// Full range, BT.709.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub trait Nv12Planes: Send + Sync {
    /// The Y plane, `width * height` bytes.
    fn y(&self) -> &[u8];
    /// The interleaved CbCr plane, `2 * width.div_ceil(2) * height.div_ceil(2)` bytes.
    fn uv(&self) -> &[u8];
}

/// One frame of a video stream, for [`surface`].
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
#[derive(Clone)]
pub struct Nv12Frame {
    /// The stream this frame belongs to.
    pub id: SurfaceId,
    /// Changes whenever the pixels do. A renderer that has uploaded this generation for `id`
    /// draws its textures again without uploading.
    pub generation: u64,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The pixels.
    pub planes: Arc<dyn Nv12Planes>,
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
impl Nv12Frame {
    /// The width and height of the CbCr plane, in CbCr pairs.
    pub fn chroma_size(&self) -> (u32, u32) {
        (self.width.div_ceil(2), self.height.div_ceil(2))
    }

    /// The Y and CbCr planes cut to this frame's size, or `None` when either is too short.
    pub fn planes(&self) -> Option<(&[u8], &[u8])> {
        let (chroma_width, chroma_height) = self.chroma_size();
        let y_len = self.width as usize * self.height as usize;
        let uv_len = 2 * chroma_width as usize * chroma_height as usize;
        let y = self.planes.y().get(..y_len)?;
        let uv = self.planes.uv().get(..uv_len)?;
        Some((y, uv))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
impl fmt::Debug for Nv12Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Nv12Frame")
            .field("id", &self.id)
            .field("generation", &self.generation)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
impl PartialEq for Nv12Frame {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.generation == other.generation
            && self.width == other.width
            && self.height == other.height
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
impl Eq for Nv12Frame {}

/// A surface element.
pub struct Surface {
    source: SurfaceSource,
    object_fit: ObjectFit,
    style: StyleRefinement,
}

/// Create a new surface element.
pub fn surface(source: impl Into<SurfaceSource>) -> Surface {
    Surface {
        source: source.into(),
        object_fit: ObjectFit::Contain,
        style: Default::default(),
    }
}

impl Surface {
    /// Set the object fit for the image.
    pub fn object_fit(mut self, object_fit: ObjectFit) -> Self {
        self.object_fit = object_fit;
        self
    }
}

impl Element for Surface {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.refine(&self.style);
        let layout_id = window.request_layout(style, [], cx);
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        _: &mut App,
    ) {
        let mut style = Style::default();
        style.refine(&self.style);
        let corner_radii = style.corner_radii.to_pixels(window.rem_size());
        match &self.source {
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            SurfaceSource::Surface(surface) => {
                let size = crate::size(
                    CVPixelBufferGetWidth(surface).into(),
                    CVPixelBufferGetHeight(surface).into(),
                );
                let new_bounds = self.object_fit.get_bounds(bounds, size);
                window.paint_surface(new_bounds, corner_radii, surface.clone());
            }
            #[cfg(not(any(target_os = "macos", target_os = "ios")))]
            SurfaceSource::Nv12(frame) => {
                let size = crate::size(
                    crate::DevicePixels(frame.width as i32),
                    crate::DevicePixels(frame.height as i32),
                );
                let new_bounds = self.object_fit.get_bounds(bounds, size);
                window.paint_surface(new_bounds, corner_radii, frame.clone());
            }
        }
    }
}

impl IntoElement for Surface {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for Surface {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}
