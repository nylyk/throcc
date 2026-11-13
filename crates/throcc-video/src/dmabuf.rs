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
    /// Media timestamp, 90 kHz.
    pub timestamp: u32,
}

impl DmaBuf {
    pub fn planes(&self) -> impl Iterator<Item = &DmaBufPlane> {
        self.planes.iter().flatten()
    }
}

#[derive(Debug)]
pub struct DmaBufPlane {
    pub fd: OwnedFd,
    pub offset: u32,
    pub stride: u32,
}
