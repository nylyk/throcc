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

    /// The picture `factor` times smaller each way, each pixel the mean of the block it
    /// covers. Sizes stay even.
    pub fn shrunk(&self, factor: usize) -> VideoFrame {
        let factor = factor.clamp(1, (self.width.min(self.height) / 2).max(1));
        let width = ((self.width / factor) & !1).max(2);
        let height = ((self.height / factor) & !1).max(2);
        let mut out = VideoFrame::blank(width, height);
        out.timestamp = self.timestamp;
        shrink_plane(&self.y, self.width, 1, factor, &mut out.y, width, height);
        shrink_plane(
            &self.uv,
            self.width,
            2,
            factor,
            &mut out.uv,
            width / 2,
            height / 2,
        );
        out
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

/// Averages `factor`×`factor` blocks of samples of `channels` interleaved bytes from rows
/// `stride` bytes apart into `columns`×`rows` samples, packed in rows as wide as `src`'s.
fn shrink_plane(
    src: &[u8],
    stride: usize,
    channels: usize,
    factor: usize,
    out: &mut [u8],
    columns: usize,
    rows: usize,
) {
    let out_stride = out.len() / rows.max(1);
    let mut sums = vec![0u32; columns * channels];
    for row in 0..rows {
        sums.fill(0);
        for line in 0..factor {
            let start = (row * factor + line) * stride;
            let Some(source) = src.get(start..start + columns * factor * channels) else {
                continue;
            };
            for (block, sum) in source
                .chunks_exact(factor * channels)
                .zip(sums.chunks_exact_mut(channels))
            {
                if let [total] = sum {
                    *total += block.iter().map(|v| u32::from(*v)).sum::<u32>();
                } else {
                    for sample in block.chunks_exact(channels) {
                        for (total, value) in sum.iter_mut().zip(sample) {
                            *total += u32::from(*value);
                        }
                    }
                }
            }
        }
        let area = (factor * factor) as u32;
        let target = &mut out[row * out_stride..row * out_stride + columns * channels];
        for (pixel, sum) in target.iter_mut().zip(&sums) {
            *pixel = (sum / area) as u8;
        }
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
    fn a_shrunk_frame_averages_each_block() {
        let mut frame = VideoFrame::blank(8, 4);
        frame.timestamp = 7;
        for (i, y) in frame.y.iter_mut().enumerate() {
            *y = if (i % 8) < 4 { 40 } else { 200 };
        }
        frame.y[0] = 44;
        for pair in frame.uv.as_chunks_mut::<2>().0 {
            *pair = [100, 150];
        }
        let small = frame.shrunk(2);
        assert_eq!((small.width, small.height, small.timestamp), (4, 2, 7));
        assert_eq!(small.y, [41, 40, 200, 200, 40, 40, 200, 200]);
        assert_eq!(small.uv, [100, 150, 100, 150]);
    }

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
