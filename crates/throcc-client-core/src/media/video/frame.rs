/// A picture in NV12: a full-resolution Y plane, then interleaved half-resolution U and V.
/// Full range, BT.709.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoFrame {
    pub width: usize,
    pub height: usize,
    pub y: Vec<u8>,
    pub uv: Vec<u8>,
    /// Media timestamp, 90 kHz.
    pub timestamp: u32,
}

impl VideoFrame {
    pub fn blank(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            y: vec![16; width * height],
            uv: vec![128; width * height.div_ceil(2)],
            timestamp: 0,
        }
    }

    /// A moving test picture whose frame number is drawn as 16 black-or-white blocks
    /// across the top, so a receiver can read back which frame it shows.
    pub fn test_pattern(width: usize, height: usize, number: u16, timestamp: u32) -> Self {
        let mut frame = Self::blank(width, height);
        frame.timestamp = timestamp;
        let shift = usize::from(number) * 4;
        for row in 0..height {
            for col in 0..width {
                frame.y[row * width + col] = (((col + shift) ^ row) & 0xff) as u8 / 2 + 64;
            }
        }
        frame.stamp_number(number);
        frame
    }

    /// Draws `number` as 16 black-or-white blocks across the top.
    pub fn stamp_number(&mut self, number: u16) {
        let block = self.width / 16;
        for bit in 0..16 {
            let value = if number >> (15 - bit) & 1 == 1 {
                235
            } else {
                16
            };
            for row in 0..block.min(self.height) {
                let start = row * self.width + bit * block;
                self.y[start..start + block].fill(value);
            }
        }
    }

    /// Reads back the number drawn by [`VideoFrame::stamp_number`].
    pub fn read_test_number(&self) -> u16 {
        let block = self.width / 16;
        (0..16).fold(0u16, |acc, bit| {
            let center = (block / 2) * self.width + bit * block + block / 2;
            let on = self.y.get(center).is_some_and(|v| *v > 128);
            acc << 1 | u16::from(on)
        })
    }

    /// The same picture as BGRA, for renderers that take RGB images.
    pub fn to_bgra(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.width * self.height * 4];
        for row in 0..self.height {
            for col in 0..self.width {
                let y = f32::from(self.y[row * self.width + col]);
                let uv_at = (row / 2) * self.width + (col / 2) * 2;
                let u = f32::from(self.uv[uv_at]) - 128.0;
                let v = f32::from(self.uv[uv_at + 1]) - 128.0;
                let r = y + 1.5748 * v;
                let g = y - 0.1873 * u - 0.4681 * v;
                let b = y + 1.8556 * u;
                let px = (row * self.width + col) * 4;
                out[px] = b.clamp(0.0, 255.0) as u8;
                out[px + 1] = g.clamp(0.0, 255.0) as u8;
                out[px + 2] = r.clamp(0.0, 255.0) as u8;
                out[px + 3] = 255;
            }
        }
        out
    }
}

/// Peak signal-to-noise ratio of the luma planes, in dB.
pub fn luma_psnr(a: &VideoFrame, b: &VideoFrame) -> f64 {
    let mse: f64 =
        a.y.iter()
            .zip(&b.y)
            .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
            .sum::<f64>()
            / a.y.len().max(1) as f64;
    10.0 * (255.0f64.powi(2) / mse.max(1e-9)).log10()
}

/// The largest even size with the aspect ratio of `source` that fits in `bound`.
pub fn fit_within(source: (usize, usize), bound: (usize, usize)) -> (usize, usize) {
    let (sw, sh) = (source.0.max(1), source.1.max(1));
    let (bw, bh) = bound;
    let (width, height) = if sw * bh > bw * sh {
        (bw, sh * bw / sw)
    } else {
        (sw * bh / sh, bh)
    };
    let even = |v: usize| (v & !1).max(2);
    (even(width).min(bw & !1), even(height).min(bh & !1))
}

/// The size a share of a `source`-sized screen is encoded at: its own aspect ratio, fitted
/// in `bound` and never larger than the source.
pub fn share_size(source: (usize, usize), bound: (usize, usize)) -> (usize, usize) {
    let smaller = (bound.0.min(source.0).max(2), bound.1.min(source.1).max(2));
    fit_within(source, smaller)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_share_keeps_its_screens_shape() {
        assert_eq!(share_size((3024, 1964), (2560, 1440)), (2216, 1440));
        assert_eq!(share_size((2560, 1600), (2560, 1440)), (2304, 1440));
        assert_eq!(share_size((3440, 1440), (2560, 1440)), (2560, 1070));
        assert_eq!(share_size((1920, 1080), (2560, 1440)), (1920, 1080));
        assert_eq!(share_size((1365, 767), (2560, 1440)), (1364, 766));
        assert_eq!(share_size((0, 0), (1280, 720)), (2, 2));
    }

    #[test]
    fn test_pattern_number_reads_back() {
        for number in [0, 1, 0x1234, u16::MAX] {
            let frame = VideoFrame::test_pattern(320, 180, number, 0);
            assert_eq!(frame.read_test_number(), number);
        }
    }

    #[test]
    fn bgra_has_four_bytes_a_pixel() {
        let frame = VideoFrame::test_pattern(64, 36, 7, 0);
        assert_eq!(frame.to_bgra().len(), 64 * 36 * 4);
    }
}
