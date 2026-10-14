//! 画面输出尺寸与黑边布局（设计文档 5.2）。纯计算，不依赖任何系统接口。
//!
//! NV12 的色度按 2×2 采样，所以输出的宽、高以及画面区域的位置和大小都必须是偶数。

/// 像素矩形。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn right(&self) -> u32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> u32 {
        self.y + self.h
    }

    pub fn center(&self) -> (u32, u32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

fn even(v: u32) -> u32 {
    v & !1
}

/// 录制开始时确定输出尺寸：保持宽高比，长边 ≤ `max_long`，短边 ≤ `max_short`，不放大；宽高取偶数且不小于 2。
pub fn output_size(src_w: u32, src_h: u32, max_long: u32, max_short: u32) -> (u32, u32) {
    assert!(src_w > 0 && src_h > 0, "源画面尺寸必须为正");
    assert!(max_long >= 2 && max_short >= 2, "输出上限至少为 2");
    let (long, short) = (src_w.max(src_h), src_w.min(src_h));
    // 边长不超过 16384，f64 的误差远小于 1 像素
    let scale = (f64::from(max_long) / f64::from(long)).min(f64::from(max_short) / f64::from(short)).min(1.0);
    let w = even((f64::from(src_w) * scale).round() as u32).max(2);
    let h = even((f64::from(src_h) * scale).round() as u32).max(2);
    (w, h)
}

/// 把 `src` 等比缩放后居中放进 `dst`（输出尺寸固定，显示模式中途变化时靠它加黑边），返回画面区域。
/// 源比输出小时会放大。`dst_w`、`dst_h` 必须是偶数。
pub fn letterbox(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Rect {
    assert!(src_w > 0 && src_h > 0, "源画面尺寸必须为正");
    assert!(dst_w >= 2 && dst_h >= 2 && dst_w.is_multiple_of(2) && dst_h.is_multiple_of(2), "输出尺寸必须是不小于 2 的偶数");
    let scale = (f64::from(dst_w) / f64::from(src_w)).min(f64::from(dst_h) / f64::from(src_h));
    let w = even((f64::from(src_w) * scale).round() as u32).clamp(2, dst_w);
    let h = even((f64::from(src_h) * scale).round() as u32).clamp(2, dst_h);
    Rect { x: even((dst_w - w) / 2), y: even((dst_h - h) / 2), w, h }
}

/// 源画面里的矩形 `r`，在输出画面中的位置。`layout` 是 [`letterbox`] 的结果。
pub fn map_to_output(r: Rect, src_w: u32, src_h: u32, layout: Rect) -> Rect {
    let sx = f64::from(layout.w) / f64::from(src_w);
    let sy = f64::from(layout.h) / f64::from(src_h);
    let x0 = f64::from(layout.x) + f64::from(r.x) * sx;
    let y0 = f64::from(layout.y) + f64::from(r.y) * sy;
    let x1 = f64::from(layout.x) + f64::from(r.right()) * sx;
    let y1 = f64::from(layout.y) + f64::from(r.bottom()) * sy;
    Rect { x: x0.round() as u32, y: y0.round() as u32, w: (x1.round() - x0.round()) as u32, h: (y1.round() - y0.round()) as u32 }
}

/// 一帧 NV12 的字节数：Y 平面 `w*h`，UV 平面 `w*h/2`。
pub fn nv12_frame_bytes(w: u32, h: u32) -> usize {
    w as usize * h as usize * 3 / 2
}

/// 输出画面里没被源画面盖住的黑边（上、下、左、右；宽或高为 0 的不返回）。
pub fn bars(layout: Rect, out_w: u32, out_h: u32) -> Vec<Rect> {
    [
        Rect { x: 0, y: 0, w: out_w, h: layout.y },
        Rect { x: 0, y: layout.bottom(), w: out_w, h: out_h.saturating_sub(layout.bottom()) },
        Rect { x: 0, y: layout.y, w: layout.x, h: layout.h },
        Rect { x: layout.right(), y: layout.y, w: out_w.saturating_sub(layout.right()), h: layout.h },
    ]
    .into_iter()
    .filter(|r| r.w > 0 && r.h > 0)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_size_halves_4k_and_scales_1440p_to_1080p() {
        assert_eq!(output_size(3840, 2160, 1920, 1080), (1920, 1080));
        assert_eq!(output_size(2560, 1440, 1920, 1080), (1920, 1080));
    }

    #[test]
    fn output_size_never_enlarges() {
        assert_eq!(output_size(1920, 1080, 1920, 1080), (1920, 1080));
        assert_eq!(output_size(1366, 768, 1920, 1080), (1366, 768));
    }

    #[test]
    fn output_size_keeps_the_aspect_ratio_for_ultrawide_and_16_by_10() {
        assert_eq!(output_size(3440, 1440, 1920, 1080), (1920, 804));
        assert_eq!(output_size(1920, 1200, 1920, 1080), (1728, 1080));
    }

    #[test]
    fn output_size_handles_portrait_screens() {
        assert_eq!(output_size(1080, 1920, 1920, 1080), (1080, 1920));
        assert_eq!(output_size(2160, 3840, 1920, 1080), (1080, 1920));
    }

    #[test]
    fn output_size_rounds_odd_sizes_down_to_even() {
        assert_eq!(output_size(1365, 767, 1920, 1080), (1364, 766));
        assert_eq!(output_size(2, 2, 1920, 1080), (2, 2));
        assert_eq!(output_size(1, 1, 1920, 1080), (2, 2));
    }

    #[test]
    fn letterbox_fills_the_output_when_the_aspect_matches() {
        assert_eq!(letterbox(3840, 2160, 1920, 1080), Rect { x: 0, y: 0, w: 1920, h: 1080 });
        assert_eq!(letterbox(1920, 1080, 1920, 1080), Rect { x: 0, y: 0, w: 1920, h: 1080 });
    }

    #[test]
    fn letterbox_adds_bars_on_the_sides_for_a_narrower_source() {
        // 4:3 进 16:9：左右黑边
        let r = letterbox(1024, 768, 1920, 1080);
        assert_eq!((r.y, r.h), (0, 1080));
        assert_eq!(r.w, 1440);
        assert_eq!(r.x, 240);
    }

    #[test]
    fn letterbox_adds_bars_on_top_and_bottom_for_a_wider_source() {
        let r = letterbox(3440, 1440, 1920, 1080);
        assert_eq!((r.x, r.w), (0, 1920));
        assert_eq!(r.h, 804);
        assert_eq!(r.y, 138);
    }

    #[test]
    fn letterbox_enlarges_a_small_source_to_fit() {
        // 显示模式从 4K 中途降到 1080p：输出尺寸不变，画面放大
        assert_eq!(letterbox(1280, 720, 1920, 1080), Rect { x: 0, y: 0, w: 1920, h: 1080 });
    }

    #[test]
    fn layouts_are_even_and_inside_the_output_for_many_sizes() {
        let mut seed = 0x9E37_79B9_u32;
        let mut next = |lo: u32, hi: u32| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            lo + (seed >> 8) % (hi - lo + 1)
        };
        for _ in 0..2_000 {
            let (sw, sh) = (next(1, 8_000), next(1, 8_000));
            let (ow, oh) = output_size(sw, sh, 1920, 1080);
            assert!(ow.is_multiple_of(2) && oh.is_multiple_of(2) && ow >= 2 && oh >= 2, "{sw}x{sh} -> {ow}x{oh}");
            assert!(ow.max(oh) <= 1920 && ow.min(oh) <= 1080, "{sw}x{sh} -> {ow}x{oh}");
            // 显示模式变化后的另一个源尺寸，放进同一个输出
            let (mw, mh) = (next(1, 8_000), next(1, 8_000));
            let r = letterbox(mw, mh, ow, oh);
            assert!(r.x.is_multiple_of(2) && r.y.is_multiple_of(2) && r.w.is_multiple_of(2) && r.h.is_multiple_of(2), "{r:?}");
            assert!(r.right() <= ow && r.bottom() <= oh, "{mw}x{mh} 进 {ow}x{oh}：{r:?}");
        }
    }

    #[test]
    fn mapping_a_source_rect_follows_scale_and_offset() {
        let layout = letterbox(1024, 768, 1920, 1080); // x=240, w=1440, 缩放 1.40625
        let r = map_to_output(Rect { x: 512, y: 384, w: 256, h: 192 }, 1024, 768, layout);
        assert_eq!(r, Rect { x: 960, y: 540, w: 360, h: 270 });
        // 整屏映射回画面区域本身
        assert_eq!(map_to_output(Rect { x: 0, y: 0, w: 1024, h: 768 }, 1024, 768, layout), layout);
    }

    #[test]
    fn nv12_frame_size_is_one_and_a_half_bytes_per_pixel() {
        assert_eq!(nv12_frame_bytes(1920, 1080), 3_110_400);
        assert_eq!(nv12_frame_bytes(2, 2), 6);
    }

    #[test]
    fn a_full_frame_has_no_bars() {
        let layout = letterbox(3840, 2160, 1920, 1080);
        assert!(bars(layout, 1920, 1080).is_empty());
    }

    #[test]
    fn side_bars_are_left_and_right_of_the_picture() {
        let layout = letterbox(1024, 768, 1920, 1080); // x=240, w=1440
        assert_eq!(bars(layout, 1920, 1080), vec![Rect { x: 0, y: 0, w: 240, h: 1080 }, Rect { x: 1680, y: 0, w: 240, h: 1080 }]);
    }

    #[test]
    fn top_and_bottom_bars_leave_the_picture_rows_alone() {
        let layout = letterbox(3440, 1440, 1920, 1080); // y=138, h=804
        assert_eq!(bars(layout, 1920, 1080), vec![Rect { x: 0, y: 0, w: 1920, h: 138 }, Rect { x: 0, y: 942, w: 1920, h: 138 }]);
    }

    #[test]
    fn bars_and_picture_tile_the_output_exactly() {
        let mut seed = 0xC0FF_EE11_u32;
        let mut next = |lo: u32, hi: u32| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            lo + (seed >> 8) % (hi - lo + 1)
        };
        for _ in 0..500 {
            let (ow, oh) = (next(1, 1000) * 2, next(1, 1000) * 2);
            let layout = letterbox(next(1, 8_000), next(1, 8_000), ow, oh);
            let area: u64 = bars(layout, ow, oh).iter().map(|r| u64::from(r.w) * u64::from(r.h)).sum::<u64>() + u64::from(layout.w) * u64::from(layout.h);
            assert_eq!(area, u64::from(ow) * u64::from(oh), "{layout:?} 在 {ow}x{oh}");
        }
    }
}
