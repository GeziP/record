//! 测量用的小型信号处理：Goertzel 单频检测、脉冲串检测、间隔与漂移估计。
//! S1、S3 的分析工具与自动化测试共用，不用于录制路径。

use std::f64::consts::PI;

/// 加汉宁窗的 Goertzel：返回 `freq` 处的幅度估计，纯正弦时约等于其振幅。
pub fn tone_level(window: &[f32], freq: f64, rate: u32) -> f64 {
    let n = window.len();
    if n < 2 {
        return 0.0;
    }
    let (mut re, mut im, mut wsum) = (0.0f64, 0.0f64, 0.0f64);
    for (i, &s) in window.iter().enumerate() {
        let w = 0.5 - 0.5 * (2.0 * PI * i as f64 / (n - 1) as f64).cos();
        let phase = 2.0 * PI * freq * i as f64 / f64::from(rate);
        re += f64::from(s) * w * phase.cos();
        im -= f64::from(s) * w * phase.sin();
        wsum += w;
    }
    2.0 * (re * re + im * im).sqrt() / wsum
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Burst {
    pub start_s: f64,
    pub end_s: f64,
}

/// 找出 `freq` 的脉冲串：10 ms 窗、5 ms 步长，音量达到 `threshold`（建议取脉冲振幅的一半）算有声。
/// 起止时刻取窗口中心，精度约 5 ms。
pub fn detect_bursts(mono: &[f32], rate: u32, freq: f64, threshold: f64) -> Vec<Burst> {
    let win = (rate / 100) as usize;
    let hop = (win / 2).max(1);
    let mut out = Vec::new();
    let mut open: Option<f64> = None;
    let mut last_active = 0.0;
    let mut i = 0;
    while i + win <= mono.len() {
        let t = (i + win / 2) as f64 / f64::from(rate);
        if tone_level(&mono[i..i + win], freq, rate) >= threshold {
            open.get_or_insert(t);
            last_active = t;
        } else if let Some(start) = open.take() {
            out.push(Burst { start_s: start, end_s: last_active });
        }
        i += hop;
    }
    if let Some(start) = open {
        out.push(Burst { start_s: start, end_s: last_active });
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spacing {
    pub count: usize,
    pub mean_interval_s: f64,
    /// 各脉冲相对“起点 + k × 期望间隔”的最大偏差（毫秒）。
    pub max_dev_ms: f64,
    /// 间隔相对期望值的漂移（百万分率）：正数表示实际间隔更长。
    pub drift_ppm: f64,
}

/// 用最小二乘拟合 `onset_k = a + b × k`，得到平均间隔、漂移与最大偏差。
pub fn spacing(bursts: &[Burst], expected_interval_s: f64) -> Option<Spacing> {
    let n = bursts.len();
    if n < 3 {
        return None;
    }
    let nf = n as f64;
    let (sx, sy): (f64, f64) = bursts.iter().enumerate().fold((0.0, 0.0), |(sx, sy), (k, b)| (sx + k as f64, sy + b.start_s));
    let (mx, my) = (sx / nf, sy / nf);
    let (mut sxx, mut sxy) = (0.0, 0.0);
    for (k, b) in bursts.iter().enumerate() {
        sxx += (k as f64 - mx) * (k as f64 - mx);
        sxy += (k as f64 - mx) * (b.start_s - my);
    }
    let slope = sxy / sxx;
    let a = my - slope * mx;
    let max_dev = bursts.iter().enumerate().map(|(k, b)| (b.start_s - (a + expected_interval_s * k as f64)).abs()).fold(0.0, f64::max);
    // 最大偏差相对“期望间隔”的理想网格；允许整体平移，所以以拟合截距为基准
    Some(Spacing { count: n, mean_interval_s: slope, max_dev_ms: max_dev * 1000.0, drift_ppm: (slope / expected_interval_s - 1.0) * 1e6 })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn sine(freq: f64, secs: f64, amp: f32) -> Vec<f32> {
        (0..(secs * f64::from(RATE)) as usize).map(|n| (2.0 * PI * freq * n as f64 / f64::from(RATE)).sin() as f32 * amp).collect()
    }

    /// 总长 `total` 秒，每 `interval` 秒一个 150 ms 的 1 kHz 脉冲，第一个在 `first` 秒。
    fn beep_train(first: f64, interval: f64, total: f64, amp: f32) -> Vec<f32> {
        let mut v = vec![0.0f32; (total * f64::from(RATE)) as usize];
        let tone = sine(1000.0, 0.15, amp);
        let mut t = first;
        while t + 0.15 <= total {
            let s = (t * f64::from(RATE)) as usize;
            v[s..s + tone.len()].copy_from_slice(&tone);
            t += interval;
        }
        v
    }

    #[test]
    fn tone_level_reads_the_amplitude_of_the_matching_frequency_only() {
        let w = sine(1000.0, 0.02, 0.4);
        assert!((tone_level(&w, 1000.0, RATE) - 0.4).abs() < 0.02);
        assert!(tone_level(&w, 2000.0, RATE) < 0.02);
        assert_eq!(tone_level(&[], 1000.0, RATE), 0.0);
    }

    #[test]
    fn bursts_are_found_with_millisecond_accuracy() {
        let v = beep_train(0.5, 1.0, 10.0, 0.4);
        let b = detect_bursts(&v, RATE, 1000.0, 0.2);
        assert_eq!(b.len(), 10, "0.5、1.5 … 9.5 秒各一个");
        for (k, burst) in b.iter().enumerate() {
            let expect = 0.5 + k as f64;
            assert!((burst.start_s - expect).abs() < 0.012, "第 {k} 个：{} 期望 {expect}", burst.start_s);
            assert!((burst.end_s - burst.start_s - 0.15).abs() < 0.02);
        }
    }

    #[test]
    fn silence_and_other_tones_are_not_bursts() {
        assert!(detect_bursts(&vec![0.0; 48_000], RATE, 1000.0, 0.2).is_empty());
        assert!(detect_bursts(&sine(440.0, 1.0, 0.5), RATE, 1000.0, 0.2).is_empty());
    }

    #[test]
    fn a_burst_cut_by_the_end_of_the_buffer_is_still_reported() {
        let mut v = vec![0.0f32; 48_000];
        let tone = sine(1000.0, 0.3, 0.4);
        v[40_000..].copy_from_slice(&tone[..8_000]);
        let b = detect_bursts(&v, RATE, 1000.0, 0.2);
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn spacing_of_a_perfect_train_has_no_drift() {
        let v = beep_train(0.5, 1.0, 20.0, 0.4);
        let s = spacing(&detect_bursts(&v, RATE, 1000.0, 0.2), 1.0).unwrap();
        assert_eq!(s.count, 20);
        assert!(s.drift_ppm.abs() < 500.0, "{s:?}");
        assert!(s.max_dev_ms < 8.0, "{s:?}");
    }

    #[test]
    fn spacing_reports_drift_in_ppm() {
        // 间隔 1.001 s：快慢 1000 ppm
        let bursts: Vec<Burst> = (0..30).map(|k| Burst { start_s: 0.2 + 1.001 * k as f64, end_s: 0.35 + 1.001 * k as f64 }).collect();
        let s = spacing(&bursts, 1.0).unwrap();
        assert!((s.drift_ppm - 1000.0).abs() < 1.0, "{s:?}");
        assert!((s.mean_interval_s - 1.001).abs() < 1e-9);
    }

    #[test]
    fn spacing_flags_a_missing_burst_as_a_large_deviation() {
        let bursts: Vec<Burst> = [0.0, 1.0, 2.0, 4.0, 5.0, 6.0].iter().map(|&t| Burst { start_s: t, end_s: t + 0.15 }).collect();
        let s = spacing(&bursts, 1.0).unwrap();
        assert!(s.max_dev_ms > 200.0, "{s:?}");
    }

    #[test]
    fn spacing_needs_at_least_three_bursts() {
        assert!(spacing(&[Burst { start_s: 0.0, end_s: 0.1 }; 2], 1.0).is_none());
    }
}
