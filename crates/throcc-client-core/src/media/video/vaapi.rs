use std::ffi::{c_int, c_uint, c_void};
use std::os::fd::AsRawFd;

use crate::media::capture::DmaBuf;

pub type Display = *mut c_void;
pub type SurfaceId = c_uint;
type Status = c_int;

const SUCCESS: Status = 0;
const INVALID_ID: c_uint = 0xffff_ffff;
const RT_FORMAT_RGB32: c_uint = 0x0002_0000;
const PROFILE_NONE: c_int = -1;
const ENTRYPOINT_VIDEO_PROC: c_int = 10;
const PROGRESSIVE: c_int = 1;
const PIPELINE_PARAMETER_BUFFER: c_int = 41;
const ATTRIB_MEMORY_TYPE: c_int = 6;
const ATTRIB_EXTERNAL_BUFFER_DESCRIPTOR: c_int = 7;
const ATTRIB_SETTABLE: c_uint = 2;
const VALUE_INTEGER: c_int = 1;
const VALUE_POINTER: c_int = 3;
const MEM_TYPE_DRM_PRIME_2: c_int = 0x4000_0000;
const COLOR_STANDARD_BT709: c_int = 2;
const COLOR_STANDARD_SRGB: c_int = 8;
const RANGE_FULL: u8 = 2;
const BT709: u8 = 1;
const BLACK: c_uint = 0xff00_0000;
const MAX_PLANES: usize = 4;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rectangle {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PrimeObject {
    fd: c_int,
    size: u32,
    drm_format_modifier: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PrimeLayer {
    drm_format: u32,
    num_planes: u32,
    object_index: [u32; MAX_PLANES],
    offset: [u32; MAX_PLANES],
    pitch: [u32; MAX_PLANES],
}

/// `VADRMPRIMESurfaceDescriptor`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PrimeDescriptor {
    fourcc: u32,
    width: u32,
    height: u32,
    num_objects: u32,
    objects: [PrimeObject; MAX_PLANES],
    num_layers: u32,
    layers: [PrimeLayer; MAX_PLANES],
}

#[repr(C)]
#[derive(Clone, Copy)]
union GenericValueData {
    i: c_int,
    p: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GenericValue {
    kind: c_int,
    value: GenericValueData,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SurfaceAttrib {
    kind: c_int,
    flags: c_uint,
    value: GenericValue,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ColorProperties {
    chroma_sample_location: u8,
    color_range: u8,
    colour_primaries: u8,
    transfer_characteristics: u8,
    matrix_coefficients: u8,
    reserved: [u8; 3],
}

/// `VAProcPipelineParameterBuffer`.
#[repr(C)]
struct PipelineParameters {
    surface: SurfaceId,
    surface_region: *const Rectangle,
    surface_color_standard: c_int,
    output_region: *const Rectangle,
    output_background_color: c_uint,
    output_color_standard: c_int,
    pipeline_flags: c_uint,
    filter_flags: c_uint,
    filters: *mut c_uint,
    num_filters: c_uint,
    forward_references: *mut SurfaceId,
    num_forward_references: c_uint,
    backward_references: *mut SurfaceId,
    num_backward_references: c_uint,
    rotation_state: c_uint,
    blend_state: *const c_void,
    mirror_state: c_uint,
    additional_outputs: *mut SurfaceId,
    num_additional_outputs: c_uint,
    input_surface_flag: c_uint,
    output_surface_flag: c_uint,
    input_color_properties: ColorProperties,
    output_color_properties: ColorProperties,
    processing_mode: c_int,
    output_hdr_metadata: *const c_void,
    va_reserved: [u32; 16],
}

#[link(name = "va")]
unsafe extern "C" {
    fn vaCreateConfig(
        display: Display,
        profile: c_int,
        entrypoint: c_int,
        attribs: *mut c_void,
        count: c_int,
        config: *mut c_uint,
    ) -> Status;
    fn vaDestroyConfig(display: Display, config: c_uint) -> Status;
    fn vaCreateContext(
        display: Display,
        config: c_uint,
        width: c_int,
        height: c_int,
        flag: c_int,
        targets: *mut SurfaceId,
        count: c_int,
        context: *mut c_uint,
    ) -> Status;
    fn vaDestroyContext(display: Display, context: c_uint) -> Status;
    fn vaCreateSurfaces(
        display: Display,
        format: c_uint,
        width: c_uint,
        height: c_uint,
        surfaces: *mut SurfaceId,
        count: c_uint,
        attribs: *mut SurfaceAttrib,
        attrib_count: c_uint,
    ) -> Status;
    fn vaDestroySurfaces(display: Display, surfaces: *mut SurfaceId, count: c_int) -> Status;
    fn vaCreateBuffer(
        display: Display,
        context: c_uint,
        kind: c_int,
        size: c_uint,
        elements: c_uint,
        data: *mut c_void,
        buffer: *mut c_uint,
    ) -> Status;
    fn vaDestroyBuffer(display: Display, buffer: c_uint) -> Status;
    fn vaBeginPicture(display: Display, context: c_uint, target: SurfaceId) -> Status;
    fn vaRenderPicture(
        display: Display,
        context: c_uint,
        buffers: *mut c_uint,
        count: c_int,
    ) -> Status;
    fn vaEndPicture(display: Display, context: c_uint) -> Status;
    fn vaSyncSurface(display: Display, surface: SurfaceId) -> Status;
    #[cfg(test)]
    fn vaExportSurfaceHandle(
        display: Display,
        surface: SurfaceId,
        memory_type: u32,
        flags: u32,
        descriptor: *mut c_void,
    ) -> Status;
}

/// Exports `surface` as one DMA-BUF layer, the way a compositor would share it.
#[cfg(test)]
pub(crate) fn export(display: Display, surface: SurfaceId) -> Result<DmaBuf, VaError> {
    use std::os::fd::{FromRawFd, OwnedFd};

    const READ_ONLY: u32 = 1;
    const COMPOSED_LAYERS: u32 = 8;
    let mut descriptor = PrimeDescriptor::default();
    // SAFETY: the descriptor has room for what the driver writes.
    unsafe {
        check("vaSyncSurface", vaSyncSurface(display, surface))?;
        check(
            "vaExportSurfaceHandle",
            vaExportSurfaceHandle(
                display,
                surface,
                MEM_TYPE_DRM_PRIME_2 as u32,
                READ_ONLY | COMPOSED_LAYERS,
                (&raw mut descriptor).cast(),
            ),
        )?;
    }
    let layer = descriptor.layers[0];
    let mut planes = [const { None }; MAX_PLANES];
    for (index, plane) in planes
        .iter_mut()
        .enumerate()
        .take(layer.num_planes as usize)
    {
        let object = descriptor.objects[layer.object_index[index] as usize];
        *plane = Some(crate::media::capture::DmaBufPlane {
            // SAFETY: each exported object's fd is ours; planes sharing one get duplicates.
            fd: unsafe { OwnedFd::from_raw_fd(libc::dup(object.fd)) },
            offset: layer.offset[index],
            stride: layer.pitch[index],
        });
    }
    for object in &descriptor.objects[..descriptor.num_objects as usize] {
        // SAFETY: the planes hold duplicates, so the originals are closed once here.
        unsafe { libc::close(object.fd) };
    }
    Ok(DmaBuf {
        width: descriptor.width,
        height: descriptor.height,
        fourcc: layer.drm_format,
        modifier: descriptor.objects[0].drm_format_modifier,
        planes,
        timestamp: 0,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum VaError {
    #[error("VA-API {call} failed with status {status}")]
    Call { call: &'static str, status: Status },
    #[error("VA-API can't import DRM format {0:#010x}")]
    Format(u32),
}

fn check(call: &'static str, status: Status) -> Result<(), VaError> {
    if status == SUCCESS {
        Ok(())
    } else {
        Err(VaError::Call { call, status })
    }
}

/// VA-API's fourcc for a DRM fourcc of packed 8-bit RGB.
fn va_fourcc(drm: u32) -> Option<u32> {
    let va = match &drm.to_le_bytes() {
        b"XR24" => b"BGRX",
        b"AR24" => b"BGRA",
        b"XB24" => b"RGBX",
        b"AB24" => b"RGBA",
        _ => return None,
    };
    Some(u32::from_le_bytes(*va))
}

/// Scales and converts pictures between surfaces of one VA display with its video
/// processing, sRGB in and full-range BT.709 out.
pub struct Vpp {
    display: Display,
    config: c_uint,
    context: c_uint,
}

// SAFETY: libva serialises calls on a display, and a Vpp is used by one thread at a time.
unsafe impl Send for Vpp {}

impl Vpp {
    /// # Safety
    /// `display` must be an initialised VA display that outlives the Vpp.
    pub unsafe fn new(display: Display) -> Result<Self, VaError> {
        let mut config = INVALID_ID;
        let mut context = INVALID_ID;
        // SAFETY: the out pointers are valid; `display` is live by the caller's promise.
        unsafe {
            check(
                "vaCreateConfig",
                vaCreateConfig(
                    display,
                    PROFILE_NONE,
                    ENTRYPOINT_VIDEO_PROC,
                    std::ptr::null_mut(),
                    0,
                    &mut config,
                ),
            )?;
            let created = vaCreateContext(
                display,
                config,
                0,
                0,
                PROGRESSIVE,
                std::ptr::null_mut(),
                0,
                &mut context,
            );
            if let Err(e) = check("vaCreateContext", created) {
                vaDestroyConfig(display, config);
                return Err(e);
            }
        }
        Ok(Self {
            display,
            config,
            context,
        })
    }

    /// Wraps `frame`'s planes in a surface, without copying them.
    pub fn import(&self, frame: &DmaBuf) -> Result<Surface<'_>, VaError> {
        let fourcc = va_fourcc(frame.fourcc).ok_or(VaError::Format(frame.fourcc))?;
        let mut descriptor = PrimeDescriptor {
            fourcc,
            width: frame.width,
            height: frame.height,
            num_layers: 1,
            ..Default::default()
        };
        descriptor.layers[0].drm_format = frame.fourcc;
        for (index, plane) in frame.planes().enumerate().take(MAX_PLANES) {
            descriptor.objects[index] = PrimeObject {
                fd: plane.fd.as_raw_fd(),
                size: 0,
                drm_format_modifier: frame.modifier,
            };
            let layer = &mut descriptor.layers[0];
            layer.object_index[index] = index as u32;
            layer.offset[index] = plane.offset;
            layer.pitch[index] = plane.stride;
            layer.num_planes += 1;
            descriptor.num_objects += 1;
        }
        let mut attribs = [
            SurfaceAttrib {
                kind: ATTRIB_MEMORY_TYPE,
                flags: ATTRIB_SETTABLE,
                value: GenericValue {
                    kind: VALUE_INTEGER,
                    value: GenericValueData {
                        i: MEM_TYPE_DRM_PRIME_2,
                    },
                },
            },
            SurfaceAttrib {
                kind: ATTRIB_EXTERNAL_BUFFER_DESCRIPTOR,
                flags: ATTRIB_SETTABLE,
                value: GenericValue {
                    kind: VALUE_POINTER,
                    value: GenericValueData {
                        p: (&raw mut descriptor).cast(),
                    },
                },
            },
        ];
        let mut id = INVALID_ID;
        // SAFETY: the descriptor and attributes live across the call, which only reads
        // them; the planes' descriptors stay open until it returns.
        let status = unsafe {
            vaCreateSurfaces(
                self.display,
                RT_FORMAT_RGB32,
                frame.width,
                frame.height,
                &mut id,
                1,
                attribs.as_mut_ptr(),
                attribs.len() as c_uint,
            )
        };
        check("vaCreateSurfaces", status)?;
        Ok(Surface { vpp: self, id })
    }

    /// Draws all of `source` into `region` of `target`, painting the rest of `target`
    /// black, and waits for the GPU to finish.
    pub fn convert(
        &self,
        source: &Surface<'_>,
        source_size: (u32, u32),
        target: SurfaceId,
        region: Rectangle,
    ) -> Result<(), VaError> {
        let whole = Rectangle {
            x: 0,
            y: 0,
            width: source_size.0 as u16,
            height: source_size.1 as u16,
        };
        let full_range_bt709 = ColorProperties {
            color_range: RANGE_FULL,
            colour_primaries: BT709,
            transfer_characteristics: BT709,
            matrix_coefficients: BT709,
            ..Default::default()
        };
        let mut parameters = PipelineParameters {
            surface: source.id,
            surface_region: &whole,
            surface_color_standard: COLOR_STANDARD_SRGB,
            output_region: &region,
            output_background_color: BLACK,
            output_color_standard: COLOR_STANDARD_BT709,
            output_color_properties: full_range_bt709,
            // SAFETY: every other field is an integer or a pointer, for which zero means
            // unset.
            ..unsafe { std::mem::zeroed() }
        };
        let mut buffer = INVALID_ID;
        // SAFETY: `parameters` and the rectangles it points to outlive vaEndPicture, after
        // which the buffer is destroyed; both surfaces belong to this display.
        unsafe {
            check(
                "vaCreateBuffer",
                vaCreateBuffer(
                    self.display,
                    self.context,
                    PIPELINE_PARAMETER_BUFFER,
                    size_of::<PipelineParameters>() as c_uint,
                    1,
                    (&raw mut parameters).cast(),
                    &mut buffer,
                ),
            )?;
            let rendered = check(
                "vaBeginPicture",
                vaBeginPicture(self.display, self.context, target),
            )
            .and_then(|()| {
                check(
                    "vaRenderPicture",
                    vaRenderPicture(self.display, self.context, &mut buffer, 1),
                )
            })
            .and_then(|()| check("vaEndPicture", vaEndPicture(self.display, self.context)));
            vaDestroyBuffer(self.display, buffer);
            rendered?;
            check("vaSyncSurface", vaSyncSurface(self.display, target))
        }
    }
}

impl Drop for Vpp {
    fn drop(&mut self) {
        // SAFETY: the context and config were created on this display and are not used
        // after this.
        unsafe {
            vaDestroyContext(self.display, self.context);
            vaDestroyConfig(self.display, self.config);
        }
    }
}

/// A surface made by [`Vpp::import`], destroyed when dropped.
pub struct Surface<'a> {
    vpp: &'a Vpp,
    id: SurfaceId,
}

impl Drop for Surface<'_> {
    fn drop(&mut self) {
        // SAFETY: the surface was created on this display and nothing refers to it after.
        unsafe { vaDestroySurfaces(self.vpp.display, &mut self.id, 1) };
    }
}

#[cfg(test)]
mod tests {
    use std::mem::offset_of;

    use super::*;

    #[test]
    fn structs_match_the_libva_2_24_headers() {
        assert_eq!(size_of::<PipelineParameters>(), 224);
        assert_eq!(offset_of!(PipelineParameters, output_region), 24);
        assert_eq!(offset_of!(PipelineParameters, blend_state), 96);
        assert_eq!(offset_of!(PipelineParameters, input_color_properties), 132);
        assert_eq!(offset_of!(PipelineParameters, output_color_properties), 140);
        assert_eq!(offset_of!(PipelineParameters, output_hdr_metadata), 152);
        assert_eq!(size_of::<PrimeDescriptor>(), 312);
        assert_eq!(size_of::<SurfaceAttrib>(), 24);
        assert_eq!(size_of::<Rectangle>(), 8);
    }

    #[test]
    fn packed_rgb_maps_to_va_fourccs() {
        let drm = |code: &[u8; 4]| u32::from_le_bytes(*code);
        assert_eq!(va_fourcc(drm(b"XR24")), Some(drm(b"BGRX")));
        assert_eq!(va_fourcc(drm(b"AB24")), Some(drm(b"RGBA")));
        assert_eq!(va_fourcc(drm(b"NV12")), None);
    }
}
