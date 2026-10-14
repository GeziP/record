//! NV12 帧的取样与分析：区域平均色、亮条定位。纯计算，S2/S3 的分析器共用。

use record_core::geometry::Rect;

/// 一帧 NV12：`w*h` 字节的 Y 平面，紧接着 `w*h/2` 字节的交错 UV 平面。
#[derive(Clone, Copy)]
pub struct Nv12<'a> {
    pub data: &'a [u8],
    pub w: usize,
    pub h: usize,
}

impl<'a> Nv12<'a> {
    pub fn new(data: &'a [u8], w: u32, h: u32) -> Self {
        let (w, h) = (w as usize, h as usize);
        assert_eq!(data.len(), w * h * 3 / 2, "NV12 帧大小与尺寸不符");
        Self { data, w, h }
    }

    pub fn y(&self, x: usize, y: usize) -> u8 {
        self.data[y * self.w + x]
    }

    pub fn uv(&self, x: usize, y: usize) -> (u8, u8) {
        let off = self.w * self.h + (y / 2) * self.w + (x / 2) * 2;
        (self.data[off], self.data[off + 1])
    }

    /// 矩形内的平均 Y、U、V。Y 逐像素取平均，U/V 按 2×2 采样的色度取平均。
    pub fn mean_yuv(&self, r: Rect) -> [f64; 3] {
        let (x1, y1) = ((r.right() as usize).min(self.w), (r.bottom() as usize).min(self.h));
        let (x0, y0) = ((r.x as usize).min(x1), (r.y as usize).min(y1));
        let (mut sy, mut ny) = (0u64, 0u64);
        for y in y0..y1 {
            for x in x0..x1 {
                sy += u64::from(self.y(x, y));
                ny += 1;
            }
        }
        let (mut su, mut sv, mut nc) = (0u64, 0u64, 0u64);
        for y in (y0 & !1..y1).step_by(2) {
            for x in (x0 & !1..x1).step_by(2) {
                let (u, v) = self.uv(x, y);
                su += u64::from(u);
                sv += u64::from(v);
                nc += 1;
            }
        }
        let avg = |s: u64, n: u64| if n == 0 { f64::NAN } else { s as f64 / n as f64 };
        [avg(sy, ny), avg(su, nc), avg(sv, nc)]
    }

    /// 裁出一块区域（位置和大小都取偶数），仍是紧凑的 NV12。
    pub fn crop(&self, r: Rect) -> (Vec<u8>, u32, u32) {
        let (x, y) = ((r.x as usize & !1).min(self.w), (r.y as usize & !1).min(self.h));
        let (w, h) = ((r.w as usize).min(self.w - x) & !1, (r.h as usize).min(self.h - y) & !1);
        let mut out = Vec::with_capacity(w * h * 3 / 2);
        for row in y..y + h {
            out.extend_from_slice(&self.data[row * self.w + x..row * self.w + x + w]);
        }
        for row in y / 2..(y + h) / 2 {
            let base = self.w * self.h + row * self.w + x;
            out.extend_from_slice(&self.data[base..base + w]);
        }
        (out, w as u32, h as u32)
    }

    /// 在条带的中间一行里找亮条：Y 超过 `threshold` 的最长连续段，返回 `(左边缘, 右边缘)`（右边缘不含）。
    pub fn find_bright_run(&self, strip: Rect, threshold: u8) -> Option<(usize, usize)> {
        let row = (strip.y + strip.h / 2) as usize;
        if row >= self.h {
            return None;
        }
        let (x0, x1) = (strip.x as usize, (strip.right() as usize).min(self.w));
        let (mut best, mut start): (Option<(usize, usize)>, Option<usize>) = (None, None);
        for x in x0..=x1 {
            let bright = x < x1 && self.y(x, row) > threshold;
            match (bright, start) {
                (true, None) => start = Some(x),
                (false, Some(s)) => {
                    if best.is_none_or(|(a, b)| x - s > b - a) {
                        best = Some((s, x));
                    }
                    start = None;
                }
                _ => {}
            }
        }
        best.filter(|(a, b)| b - a >= 2)
    }
}

/// 把矩形向内收缩 `fraction`（0..0.5）的边距，取样时避开边缘的缩放过渡。
pub fn shrink(r: Rect, fraction: f64) -> Rect {
    let (dx, dy) = ((f64::from(r.w) * fraction) as u32, (f64::from(r.h) * fraction) as u32);
    Rect { x: r.x + dx, y: r.y + dy, w: r.w.saturating_sub(2 * dx).max(1), h: r.h.saturating_sub(2 * dy).max(1) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一帧 NV12：整帧一个颜色，再在指定矩形里涂另一个颜色。
    fn frame(w: usize, h: usize, base: [u8; 3], patches: &[(Rect, [u8; 3])]) -> Vec<u8> {
        let mut d = vec![0u8; w * h * 3 / 2];
        let mut paint = |r: Rect, c: [u8; 3]| {
            for y in r.y as usize..r.bottom() as usize {
                for x in r.x as usize..r.right() as usize {
                    d[y * w + x] = c[0];
                    let off = w * h + (y / 2) * w + (x / 2) * 2;
                    d[off] = c[1];
                    d[off + 1] = c[2];
                }
            }
        };
        paint(Rect { x: 0, y: 0, w: w as u32, h: h as u32 }, base);
        for &(r, c) in patches {
            paint(r, c);
        }
        d
    }

    #[test]
    fn mean_of_a_flat_region_is_that_color() {
        let d = frame(64, 32, [16, 128, 128], &[(Rect { x: 8, y: 8, w: 16, h: 8 }, [180, 60, 200])]);
        let f = Nv12::new(&d, 64, 32);
        let m = f.mean_yuv(Rect { x: 8, y: 8, w: 16, h: 8 });
        assert_eq!(m, [180.0, 60.0, 200.0]);
        let outside = f.mean_yuv(Rect { x: 32, y: 16, w: 16, h: 8 });
        assert_eq!(outside, [16.0, 128.0, 128.0]);
    }

    #[test]
    fn mean_mixes_colors_in_proportion_to_area() {
        let d = frame(64, 32, [0, 100, 100], &[(Rect { x: 0, y: 0, w: 32, h: 32 }, [200, 100, 100])]);
        let f = Nv12::new(&d, 64, 32);
        let m = f.mean_yuv(Rect { x: 0, y: 0, w: 64, h: 32 });
        assert_eq!(m[0], 100.0);
    }

    #[test]
    fn the_uv_plane_is_read_at_half_resolution_after_the_y_plane() {
        // 只改 UV 平面里 (x=10, y=6) 对应的那一对：它在 UV 平面的第 3 行第 5 对
        let mut d = frame(64, 32, [100, 128, 128], &[]);
        let off = 64 * 32 + 3 * 64 + 5 * 2;
        d[off] = 77;
        d[off + 1] = 99;
        let f = Nv12::new(&d, 64, 32);
        assert_eq!(f.uv(10, 6), (77, 99));
        assert_eq!(f.uv(11, 7), (77, 99));
        assert_eq!(f.uv(12, 6), (128, 128));
    }

    #[test]
    fn the_bar_is_found_in_the_middle_row_of_the_strip() {
        let strip = Rect { x: 0, y: 24, w: 64, h: 8 };
        let d = frame(64, 32, [36, 128, 128], &[(Rect { x: 20, y: 24, w: 4, h: 8 }, [235, 128, 128])]);
        let f = Nv12::new(&d, 64, 32);
        assert_eq!(f.find_bright_run(strip, 128), Some((20, 24)));
    }

    #[test]
    fn the_longest_bright_run_wins_and_a_lone_pixel_is_ignored() {
        let strip = Rect { x: 0, y: 24, w: 64, h: 8 };
        let d = frame(64, 32, [36, 128, 128], &[(Rect { x: 5, y: 24, w: 1, h: 8 }, [235, 128, 128]), (Rect { x: 30, y: 24, w: 6, h: 8 }, [235, 128, 128])]);
        assert_eq!(Nv12::new(&d, 64, 32).find_bright_run(strip, 128), Some((30, 36)));
        let lone = frame(64, 32, [36, 128, 128], &[(Rect { x: 5, y: 24, w: 1, h: 8 }, [235, 128, 128])]);
        assert_eq!(Nv12::new(&lone, 64, 32).find_bright_run(strip, 128), None);
        let none = frame(64, 32, [36, 128, 128], &[]);
        assert_eq!(Nv12::new(&none, 64, 32).find_bright_run(strip, 128), None);
    }

    #[test]
    fn a_crop_keeps_both_planes_aligned() {
        let d = frame(64, 32, [16, 128, 128], &[(Rect { x: 10, y: 6, w: 8, h: 4 }, [180, 60, 200])]);
        let f = Nv12::new(&d, 64, 32);
        let (c, w, h) = f.crop(Rect { x: 8, y: 4, w: 16, h: 8 });
        assert_eq!((w, h, c.len()), (16, 8, 16 * 8 * 3 / 2));
        let cf = Nv12::new(&c, w, h);
        assert_eq!(cf.y(2, 2), 180); // 原图 (10, 6)
        assert_eq!(cf.uv(2, 2), (60, 200));
        assert_eq!(cf.y(0, 0), 16);
        // 奇数位置和大小向下取偶
        let (_, w2, h2) = f.crop(Rect { x: 9, y: 5, w: 15, h: 7 });
        assert_eq!((w2, h2), (14, 6));
    }

    #[test]
    fn shrinking_keeps_the_center_and_never_collapses() {
        let r = shrink(Rect { x: 100, y: 50, w: 200, h: 100 }, 0.25);
        assert_eq!(r, Rect { x: 150, y: 75, w: 100, h: 50 });
        let tiny = shrink(Rect { x: 0, y: 0, w: 2, h: 2 }, 0.45);
        assert!(tiny.w >= 1 && tiny.h >= 1);
    }

    #[test]
    #[should_panic(expected = "NV12 帧大小")]
    fn a_frame_of_the_wrong_size_is_rejected() {
        let d = vec![0u8; 10];
        let _ = Nv12::new(&d, 64, 32);
    }
}
