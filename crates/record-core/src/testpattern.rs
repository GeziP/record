//! 画面自检用的测试图案：四个已知颜色的分区，加一条随时间匀速移动的亮条。
//!
//! 测试窗口按它绘制，分析器按它在采集到的 NV12 帧里取样：颜色验证 GPU 转换路径（色彩矩阵、范围、
//! UV 平面布局），亮条的位置反推出这一帧内容的绘制时刻，用来测“画面采集的陈旧程度”。
//! 纯计算，不依赖任何系统接口。

use crate::clock::Hns;
use crate::geometry::Rect;

/// 亮条走完一个来回（从最左到最右再瞬间回到最左）的时间：4 秒。
pub const PERIOD_HNS: Hns = 40_000_000;

/// 左上、右上、左下、右下四个分区的颜色（sRGB）。选的是饱和度高、彼此远离的颜色，
/// 色彩矩阵用错（BT.601 / BT.709）或范围用错（全范围 / 有限范围）都会明显偏离。
pub const QUADRANT_RGB: [[u8; 3]; 4] = [[230, 30, 30], [30, 200, 60], [30, 60, 230], [240, 240, 240]];
pub const STRIP_RGB: [u8; 3] = [24, 24, 24];
pub const BAR_RGB: [u8; 3] = [255, 255, 255];

/// 底部亮条区域的高度：窗口高度的 1/8，至少 8 像素。
pub fn strip_height(h: u32) -> u32 {
    (h / 8).max(8).min(h / 2)
}

/// 四个颜色分区，顺序同 [`QUADRANT_RGB`]，坐标相对窗口左上角。
pub fn quadrants(w: u32, h: u32) -> [Rect; 4] {
    let area_h = h - strip_height(h);
    let (hw, hh) = (w / 2, area_h / 2);
    [Rect { x: 0, y: 0, w: hw, h: hh }, Rect { x: hw, y: 0, w: w - hw, h: hh }, Rect { x: 0, y: hh, w: hw, h: area_h - hh }, Rect { x: hw, y: hh, w: w - hw, h: area_h - hh }]
}

pub fn strip(w: u32, h: u32) -> Rect {
    let sh = strip_height(h);
    Rect { x: 0, y: h - sh, w, h: sh }
}

/// 亮条宽度：窗口宽度的 1/50，至少 8 像素。
pub fn bar_width(w: u32) -> u32 {
    (w / 50).max(8)
}

/// 时刻 `t` 的相位（0..1）：自 `t_ref` 起每 [`PERIOD_HNS`] 一圈。
pub fn phase_at(t: Hns, t_ref: Hns) -> f64 {
    (t - t_ref).rem_euclid(PERIOD_HNS) as f64 / PERIOD_HNS as f64
}

/// 相位对应的亮条左边缘（窗口坐标）。
pub fn bar_left(phase: f64, w: u32) -> u32 {
    let travel = w.saturating_sub(bar_width(w));
    ((phase * f64::from(travel)).floor() as u32).min(travel)
}

/// 由亮条左边缘反推相位。
pub fn phase_of_bar(left: f64, w: u32) -> f64 {
    let travel = f64::from(w.saturating_sub(bar_width(w))).max(1.0);
    (left / travel).clamp(0.0, 1.0)
}

/// 由相位还原时刻：在 `t_ref + (k + phase) × PERIOD` 里取离 `near` 最近的一个。
/// `near` 是这一帧的名义时刻，内容的实际绘制时刻总在它附近（相差远小于半个周期）。
pub fn time_of_phase(phase: f64, t_ref: Hns, near: Hns) -> Hns {
    let k = ((near - t_ref) as f64 / PERIOD_HNS as f64 - phase).round();
    t_ref + ((k + phase) * PERIOD_HNS as f64).round() as Hns
}

/// sRGB（0..255）对应的 BT.709 有限范围 YUV（Y 16..235，U/V 16..240）。
pub fn expected_yuv(rgb: [u8; 3]) -> [f64; 3] {
    let (r, g, b) = (f64::from(rgb[0]) / 255.0, f64::from(rgb[1]) / 255.0, f64::from(rgb[2]) / 255.0);
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let u = (b - y) / 1.8556;
    let v = (r - y) / 1.5748;
    [16.0 + 219.0 * y, 128.0 + 224.0 * u, 128.0 + 224.0 * v]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ms_to_hns;

    #[test]
    fn quadrants_and_strip_tile_the_window_without_overlap() {
        for (w, h) in [(960u32, 540u32), (801, 451), (64, 64), (1920, 1080)] {
            let mut covered = vec![0u8; (w * h) as usize];
            for r in quadrants(w, h).into_iter().chain([strip(w, h)]) {
                for y in r.y..r.bottom() {
                    for x in r.x..r.right() {
                        covered[(y * w + x) as usize] += 1;
                    }
                }
            }
            assert!(covered.iter().all(|&c| c == 1), "{w}×{h} 有像素没覆盖或重复覆盖");
        }
    }

    #[test]
    fn the_bar_sweeps_from_left_to_right_within_the_window() {
        let w = 960;
        assert_eq!(bar_left(0.0, w), 0);
        assert_eq!(bar_left(1.0, w), w - bar_width(w));
        let mut prev = 0;
        for i in 0..=100 {
            let x = bar_left(f64::from(i) / 100.0, w);
            assert!(x >= prev && x + bar_width(w) <= w);
            prev = x;
        }
    }

    #[test]
    fn phase_wraps_every_period_and_handles_times_before_the_reference() {
        let t_ref = 5_000_000;
        assert_eq!(phase_at(t_ref, t_ref), 0.0);
        assert!((phase_at(t_ref + PERIOD_HNS / 4, t_ref) - 0.25).abs() < 1e-12);
        assert!((phase_at(t_ref + PERIOD_HNS * 3 + PERIOD_HNS / 2, t_ref) - 0.5).abs() < 1e-12);
        assert!((phase_at(t_ref - PERIOD_HNS / 4, t_ref) - 0.75).abs() < 1e-12);
    }

    #[test]
    fn the_time_is_recovered_from_the_bar_position_within_a_few_pixels() {
        let (w, t_ref) = (960, 123_456_789);
        for ms in (0..20_000).step_by(37) {
            let drawn = t_ref + ms_to_hns(ms);
            let left = f64::from(bar_left(phase_at(drawn, t_ref), w));
            // 这一帧的名义时刻比绘制时刻晚 30 ms
            let near = drawn + ms_to_hns(30);
            let back = time_of_phase(phase_of_bar(left, w), t_ref, near);
            let err_ms = (back - drawn) / 10_000;
            // 一个像素约 4 ms，取整再加余量
            assert!(err_ms.abs() <= 6, "第 {ms} ms：误差 {err_ms} ms");
        }
    }

    #[test]
    fn the_recovered_time_picks_the_period_closest_to_the_frame_time() {
        let t_ref = 0;
        let near = PERIOD_HNS * 7 + ms_to_hns(100);
        let t = time_of_phase(phase_at(near - ms_to_hns(50), t_ref), t_ref, near);
        assert_eq!((near - t) / 10_000, 50);
    }

    #[test]
    fn expected_colors_match_known_bt709_limited_range_values() {
        let near = |a: [f64; 3], b: [f64; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.6);
        assert!(near(expected_yuv([0, 0, 0]), [16.0, 128.0, 128.0]));
        assert!(near(expected_yuv([255, 255, 255]), [235.0, 128.0, 128.0]));
        // 纯红 / 纯绿 / 纯蓝的 BT.709 有限范围值（标准表）
        assert!(near(expected_yuv([255, 0, 0]), [63.0, 102.0, 240.0]));
        assert!(near(expected_yuv([0, 255, 0]), [173.0, 42.0, 26.0]));
        assert!(near(expected_yuv([0, 0, 255]), [32.0, 240.0, 118.0]));
    }

    #[test]
    fn the_four_quadrant_colors_are_far_apart_in_yuv() {
        let yuv: Vec<[f64; 3]> = QUADRANT_RGB.iter().map(|&c| expected_yuv(c)).collect();
        for i in 0..4 {
            for j in i + 1..4 {
                let d = yuv[i].iter().zip(yuv[j]).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
                assert!(d > 30.0, "分区 {i} 与 {j} 在 YUV 里只差 {d}");
            }
        }
    }
}
