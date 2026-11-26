use std::os::fd::OwnedFd;

/// The most planes a DMA-BUF picture has.
pub const MAX_PLANES: usize = 4;

/// A captured picture still in GPU memory, as DRM PRIME planes.
#[derive(Debug)]
pub struct DmaBuf {
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub modifier: u64,
    pub planes: [Option<DmaBufPlane>; MAX_PLANES],
    /// The part of the picture that holds what is shared, where its producer marked one.
    pub crop: Option<Region>,
    /// Media timestamp, 90 kHz.
    pub timestamp: u32,
}

impl DmaBuf {
    pub fn planes(&self) -> impl Iterator<Item = &DmaBufPlane> {
        self.planes.iter().flatten()
    }

    /// The part of the picture to show: its crop, or all of it.
    pub fn shown(&self) -> Region {
        self.crop.unwrap_or(Region {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        })
    }
}

/// A rectangle of a picture, in pixels from its top-left corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug)]
pub struct DmaBufPlane {
    pub fd: OwnedFd,
    pub offset: u32,
    pub stride: u32,
}
