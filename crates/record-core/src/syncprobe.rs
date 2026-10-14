//! 画面与声音的同步测量（设计文档 10 的 S3）。
//!
//! 测试时放一个整块黑白闪烁的窗口，每次变白的同时播放一声 1 kHz 的短音。录完之后从成品里找：
//! 画面亮度序列的上升沿（闪屏），声音里测试音的起点；配对后得到“画面比声音晚多少秒”。
//!
//! 画面只有每秒 15 帧：闪屏出现在某两帧之间，成品里“第一个亮帧”的时刻总是晚于真实时刻，平均晚半帧。
//! 所以闪烁的间隔要和帧周期错开（相位均匀扫过），再把平均值减掉半帧；声音的起点精度是毫秒级，不用修正。

/// 取 `data` 的 `p` 分位数（0.0–1.0）。
fn percentile(data: &[u8], p: f64) -> f64 {
    let mut v = data.to_vec();
    v.sort_unstable();
    f64::from(v[((v.len() - 1) as f64 * p).round() as usize])
}

/// 画面亮度序列里“暗 → 亮”的上升沿：返回每次第一个亮帧的时刻（秒）。
/// `lum[i]` 是第 i 帧的平均亮度，`start` 是第 0 帧的时刻。亮暗差不到 40 级时认为没有闪屏，返回空。
pub fn flash_onsets(lum: &[u8], fps: f64, start: f64) -> Vec<f64> {
    if lum.is_empty() {
        return Vec::new();
    }
    let (lo, hi) = (percentile(lum, 0.05), percentile(lum, 0.95));
    if hi - lo < 40.0 {
        return Vec::new();
    }
    // 迟滞：要冲过上沿才算亮，掉到下沿以下才算暗，编码噪声不会造成假的上升沿
    let (mid, quarter) = ((lo + hi) / 2.0, (hi - lo) / 4.0);
    let mut bright = f64::from(lum[0]) > mid;
    let mut onsets = Vec::new();
    for (i, &v) in lum.iter().enumerate() {
        let v = f64::from(v);
        if !bright && v > mid + quarter {
            bright = true;
            onsets.push(start + i as f64 / fps);
        } else if bright && v < mid - quarter {
            bright = false;
        }
    }
    onsets
}

/// 汉宁窗加权的 Goertzel：`window` 里 `freq` 处的幅度估计（纯正弦时约等于振幅）。
fn tone_level(window: &[f32], freq: f64, rate: f64) -> f64 {
    let n = window.len();
    let (mut re, mut im, mut wsum) = (0.0f64, 0.0f64, 0.0f64);
    for (i, &s) in window.iter().enumerate() {
        let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos();
        let phase = 2.0 * std::f64::consts::PI * freq * i as f64 / rate;
        re += f64::from(s) * w * phase.cos();
        im -= f64::from(s) * w * phase.sin();
        wsum += w;
    }
    2.0 * (re * re + im * im).sqrt() / wsum
}

/// 低于这个幅度（满量程为 1）认为没有测试音。
const MIN_LEVEL: f64 = 0.01;

/// 单声道音频在测试音频率处的幅度序列：每 1 毫秒看一个 10 毫秒的窗口。
pub struct ToneLevels {
    levels: Vec<f64>,
    start: f64,
    hop: f64,
    half_window: f64,
}

impl ToneLevels {
    /// `start` 是第 0 个采样的时刻（秒）。
    pub fn new(samples: &[f32], rate: u32, freq: f64, start: f64) -> Self {
        let (win, hop) = ((rate / 100) as usize, (rate / 1000) as usize);
        let levels = if samples.len() < win + hop { Vec::new() } else { (0..=samples.len() - win).step_by(hop).map(|i| tone_level(&samples[i..i + win], freq, f64::from(rate))).collect() };
        Self { levels, start, hop: hop as f64 / f64::from(rate), half_window: win as f64 / 2.0 / f64::from(rate) }
    }

    /// 第 `k` 个窗口的中心时刻。窗口一半被测试音盖住时幅度正好是一半，所以幅度过半时的窗口中心就是起点。
    fn time_of(&self, k: usize) -> f64 {
        self.start + k as f64 * self.hop + self.half_window
    }

    /// 全部测试音的起点（秒）：幅度第一次超过峰值的一半。适合安静环境里的单一信号；有别的声音时用 [`onset_near`](Self::onset_near)。
    pub fn onsets(&self) -> Vec<f64> {
        let peak = self.levels.iter().copied().fold(0.0, f64::max);
        if peak < MIN_LEVEL {
            return Vec::new();
        }
        let (up, down) = (peak * 0.5, peak * 0.25);
        let mut sounding = self.levels[0] > up;
        let mut onsets = Vec::new();
        for (k, &l) in self.levels.iter().enumerate() {
            if !sounding && l > up {
                sounding = true;
                onsets.push(self.time_of(k));
            } else if sounding && l < down {
                sounding = false;
            }
        }
        onsets
    }

    /// `center` 前后各 `radius` 秒之内那一声的起点：范围内要有一个明显突起的声音（峰值不低于 [`MIN_LEVEL`]，
    /// 且至少是范围内中位数的 4 倍），起点取从“安静”冲过峰值一半的位置。一开始就在响的、持续不断的声音返回 `None`。
    pub fn onset_near(&self, center: f64, radius: f64) -> Option<f64> {
        let index = |t: f64| ((t - self.start - self.half_window) / self.hop).floor().max(0.0) as usize;
        let (lo, hi) = (index(center - radius).min(self.levels.len()), (index(center + radius) + 1).min(self.levels.len()));
        let part = self.levels.get(lo..hi).filter(|p| !p.is_empty())?;
        let peak = part.iter().copied().fold(0.0, f64::max);
        let mut sorted = part.to_vec();
        sorted.sort_by(f64::total_cmp);
        let median = sorted[sorted.len() / 2];
        if peak < MIN_LEVEL || peak < 4.0 * median {
            return None;
        }
        let (up, down) = (peak * 0.5, peak * 0.25);
        let mut quiet = false;
        for (i, &l) in part.iter().enumerate() {
            if l < down {
                quiet = true;
            } else if quiet && l > up {
                return Some(self.time_of(lo + i));
            }
        }
        None
    }
}

/// 单声道音频里 `freq` 测试音的起点（秒），见 [`ToneLevels::onsets`]。
pub fn tone_onsets(samples: &[f32], rate: u32, freq: f64, start: f64) -> Vec<f64> {
    ToneLevels::new(samples, rate, freq, start).onsets()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pair {
    pub video: f64,
    pub audio: f64,
}

impl Pair {
    /// 画面比声音晚的秒数；负数表示画面先到。
    pub fn delta(&self) -> f64 {
        self.video - self.audio
    }
}

/// 每个闪屏配上它前后 `radius` 秒之内的测试音；附近听不到测试音的闪屏跳过。`radius` 要小于刺激的间隔的一半以内，免得配到邻居。
pub fn pair_flashes(tone: &ToneLevels, flashes: &[f64], radius: f64) -> Vec<Pair> {
    flashes.iter().filter_map(|&video| tone.onset_near(video, radius).map(|audio| Pair { video, audio })).collect()
}

/// 去掉与中位数差得超过 `tolerance` 秒的配对（别的声音或误检造成），返回保留的配对和去掉的个数。
pub fn keep_consistent(pairs: &[Pair], tolerance: f64) -> (Vec<Pair>, usize) {
    if pairs.is_empty() {
        return (Vec::new(), 0);
    }
    let mut deltas: Vec<f64> = pairs.iter().map(Pair::delta).collect();
    deltas.sort_by(f64::total_cmp);
    let median = deltas[deltas.len() / 2];
    let kept: Vec<Pair> = pairs.iter().copied().filter(|p| (p.delta() - median).abs() <= tolerance).collect();
    let dropped = pairs.len() - kept.len();
    (kept, dropped)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Estimate {
    pub n: usize,
    /// 修正帧量化之后的偏移（秒）：画面比声音晚为正。
    pub offset: f64,
    /// 各对之间最大与最小差值之差。相位扫得均匀时应接近一个帧周期；明显更大说明有抖动。
    pub spread: f64,
    /// 修正前的平均差值。
    pub raw_mean: f64,
    /// 因“只能看到帧”带来的不确定度（一倍标准差，秒）。
    pub uncertainty: f64,
}

/// 由配好的对估计偏移。`fps` 是画面帧率。没有配对时返回 `None`。
pub fn estimate(pairs: &[Pair], fps: f64) -> Option<Estimate> {
    if pairs.is_empty() {
        return None;
    }
    let period = 1.0 / fps;
    let deltas: Vec<f64> = pairs.iter().map(Pair::delta).collect();
    let n = deltas.len();
    let raw_mean = deltas.iter().sum::<f64>() / n as f64;
    let (min, max) = deltas.iter().fold((f64::MAX, f64::MIN), |(lo, hi), &d| (lo.min(d), hi.max(d)));
    Some(Estimate { n, offset: raw_mean - period / 2.0, spread: max - min, raw_mean, uncertainty: period / 12f64.sqrt() / (n as f64).sqrt() })
}

/// 判定：偏移的绝对值不超过 `limit`（秒）。把不确定度算进去：偏移加减两倍标准差仍在限内才算通过。
pub fn within(e: &Estimate, limit: f64) -> bool {
    e.offset.abs() + 2.0 * e.uncertainty <= limit
}

#[cfg(test)]
mod tests {
    use super::*;

    const FPS: f64 = 15.0;
    const RATE: u32 = 48_000;

    /// 在 `fps` 下合成亮度序列：`flashes` 里每个时刻开始变白，持续 `on` 秒；第 i 帧看到的是 `t_i` 时刻的屏幕。
    fn frames(flashes: &[f64], on: f64, secs: f64, start: f64) -> Vec<u8> {
        (0..(secs * FPS) as usize)
            .map(|i| {
                let t = start + i as f64 / FPS;
                if flashes.iter().any(|&s| t >= s && t < s + on) { 235 } else { 16 }
            })
            .collect()
    }

    /// 合成单声道音频：`beeps` 里每个时刻起一声 `dur` 秒的 `freq` 测试音，2 毫秒淡入。
    fn audio(beeps: &[f64], freq: f64, dur: f64, secs: f64) -> Vec<f32> {
        (0..(secs * f64::from(RATE)) as usize)
            .map(|n| {
                let t = n as f64 / f64::from(RATE);
                beeps.iter().find(|&&b| t >= b && t < b + dur).map_or(0.0, |&b| ((t - b) / 0.002).min(1.0) as f32 * 0.3 * (2.0 * std::f64::consts::PI * freq * t).sin() as f32)
            })
            .collect()
    }

    #[test]
    fn a_flash_shows_up_as_the_first_bright_frame() {
        let lum = frames(&[1.0, 3.0], 0.3, 5.0, 0.0);
        let on = flash_onsets(&lum, FPS, 0.0);
        assert_eq!(on.len(), 2);
        // 1.0 秒正好是第 15 帧；3.0 秒是第 45 帧
        assert!((on[0] - 1.0).abs() < 1e-9 && (on[1] - 3.0).abs() < 1e-9, "{on:?}");
    }

    #[test]
    fn the_series_start_time_shifts_every_onset() {
        let lum = frames(&[1.0], 0.3, 3.0, 0.0);
        assert!((flash_onsets(&lum, FPS, 10.0)[0] - 11.0).abs() < 1e-9);
    }

    #[test]
    fn a_picture_with_no_flashes_gives_nothing() {
        assert!(flash_onsets(&[120u8; 100], FPS, 0.0).is_empty());
        assert!(flash_onsets(&[], FPS, 0.0).is_empty());
        let weak: Vec<u8> = (0..100).map(|i| if i % 10 < 5 { 100 } else { 120 }).collect();
        assert!(flash_onsets(&weak, FPS, 0.0).is_empty(), "亮暗只差 20 级不算闪屏");
    }

    #[test]
    fn noise_around_the_threshold_does_not_make_extra_edges() {
        let mut lum = frames(&[1.0], 0.5, 3.0, 0.0);
        for (i, v) in lum.iter_mut().enumerate() {
            *v = v.saturating_add((i % 3) as u8 * 4).saturating_sub(4);
        }
        assert_eq!(flash_onsets(&lum, FPS, 0.0).len(), 1);
    }

    #[test]
    fn the_onset_of_a_test_tone_is_found_within_three_milliseconds() {
        // 测试音有 2 毫秒淡入，幅度到一半要 1 毫秒，加上 1 毫秒的步进，所以偏晚 1–2 毫秒是正常的
        let beeps = [0.5, 2.5, 4.5];
        let on = tone_onsets(&audio(&beeps, 1000.0, 0.15, 6.0), RATE, 1000.0, 0.0);
        assert_eq!(on.len(), 3, "{on:?}");
        for (found, want) in on.iter().zip(beeps) {
            assert!((found - want).abs() < 0.003, "找到 {found}，真实 {want}");
        }
    }

    #[test]
    fn other_frequencies_and_silence_are_not_mistaken_for_the_tone() {
        assert!(tone_onsets(&audio(&[1.0], 3000.0, 0.15, 3.0), RATE, 1000.0, 0.0).is_empty());
        assert!(tone_onsets(&vec![0.0; 48_000], RATE, 1000.0, 0.0).is_empty());
        assert!(tone_onsets(&[0.0; 10], RATE, 1000.0, 0.0).is_empty());
    }

    #[test]
    fn a_beep_is_found_around_its_flash_even_when_something_louder_sounds_elsewhere() {
        // 3.0 秒处有一声很响的同频率声音（别的程序），0.5 秒和 5.5 秒是我们的测试音
        let mut samples = audio(&[0.5, 5.5], 1000.0, 0.15, 7.0);
        for (s, loud) in samples.iter_mut().zip(audio(&[3.0], 1000.0, 0.15, 7.0)) {
            *s += loud * 3.0;
        }
        let tone = ToneLevels::new(&samples, RATE, 1000.0, 0.0);
        assert!((tone.onset_near(0.55, 0.3).unwrap() - 0.5).abs() < 0.003);
        assert!((tone.onset_near(5.6, 0.3).unwrap() - 5.5).abs() < 0.003);
        assert_eq!(tone.onset_near(3.5, 0.2), None, "这个范围里没有突起的声音");
        // 全局找会被响的那声压住：阈值按峰值的一半算，我们自己的测试音（幅度只有 1/3）不到一半
        assert_eq!(tone_onsets(&samples, RATE, 1000.0, 0.0).len(), 1);
    }

    #[test]
    fn a_sustained_tone_or_a_window_that_starts_mid_sound_gives_no_onset() {
        let hum = audio(&[0.0], 1000.0, 4.0, 4.0);
        let tone = ToneLevels::new(&hum, RATE, 1000.0, 0.0);
        assert_eq!(tone.onset_near(2.0, 1.0), None, "持续不断的声音没有起点");
        assert_eq!(tone.onset_near(0.1, 0.05), None, "窗口一开始就在响");
        assert_eq!(tone.onset_near(100.0, 1.0), None, "窗口在录音之外");
        assert_eq!(ToneLevels::new(&[], RATE, 1000.0, 0.0).onset_near(1.0, 1.0), None);
    }

    #[test]
    fn each_flash_is_paired_with_the_beep_around_it() {
        let tone = ToneLevels::new(&audio(&[1.05, 3.62], 1000.0, 0.15, 6.0), RATE, 1000.0, 0.0);
        let pairs = pair_flashes(&tone, &[1.00, 3.00, 5.0], 1.0);
        assert_eq!(pairs.len(), 2, "5.0 秒的闪屏附近没有测试音");
        assert!((pairs[0].delta() + 0.05).abs() < 0.003 && (pairs[1].delta() + 0.62).abs() < 0.003, "{pairs:?}");
    }

    #[test]
    fn pairs_far_from_the_median_are_dropped() {
        let pair = |d: f64| Pair { video: 10.0 + d, audio: 10.0 };
        let (kept, dropped) = keep_consistent(&[pair(-0.03), pair(-0.05), pair(-0.02), pair(0.4), pair(-0.04)], 0.13);
        assert_eq!((kept.len(), dropped), (4, 1));
        assert!(kept.iter().all(|p| p.delta() < 0.0));
        assert_eq!(keep_consistent(&[], 0.1), (Vec::new(), 0));
    }

    /// 端到端的合成：画面比声音早 40 毫秒（负偏移），闪烁间隔与帧周期错开，估计值应落在真实值附近。
    fn simulate(true_offset: f64, interval: f64, count: usize) -> Estimate {
        let first = 1.0;
        let flashes: Vec<f64> = (0..count).map(|k| first + k as f64 * interval).collect();
        // 声音在闪屏之后 `-true_offset` 秒出现（真实偏移 = 画面可见时刻 − 声音可闻时刻）
        let beeps: Vec<f64> = flashes.iter().map(|s| s - true_offset).collect();
        let secs = first + count as f64 * interval + 1.0;
        let video = flash_onsets(&frames(&flashes, 0.3, secs, 0.0), FPS, 0.0);
        let tone = ToneLevels::new(&audio(&beeps, 1000.0, 0.15, secs), RATE, 1000.0, 0.0);
        let pairs = pair_flashes(&tone, &video, 1.0);
        assert_eq!(pairs.len(), count, "每次闪烁都应配上对");
        estimate(&pairs, FPS).unwrap()
    }

    #[test]
    fn the_estimate_recovers_the_true_offset_when_the_flash_phase_sweeps_the_frame_grid() {
        // 间隔 2 + 1/120 秒：相位每次错开 1/8 帧
        for true_offset in [-0.040, 0.0, 0.060] {
            let e = simulate(true_offset, 2.0 + 1.0 / 120.0, 16);
            assert!((e.offset - true_offset).abs() < 0.008, "真实 {true_offset}，估计 {e:?}");
            assert!(e.spread > 0.05 && e.spread < 0.075, "相位扫满一个帧周期，差值的范围应约等于 66.7 ms：{e:?}");
        }
    }

    #[test]
    fn a_fixed_phase_cannot_be_corrected_which_is_why_the_interval_must_not_be_a_whole_number_of_frames() {
        // 间隔恰好是整数帧：每次落在帧格上的同一位置，修正“半帧”会带来最多 ±33 ms 的系统误差，且 spread 为 0
        let e = simulate(0.0, 2.0, 8);
        assert!(e.spread < 1e-6, "{e:?}");
    }

    #[test]
    fn the_verdict_accounts_for_the_uncertainty() {
        let e = Estimate { n: 4, offset: 0.070, spread: 0.067, raw_mean: 0.1, uncertainty: 0.01 };
        assert!(!within(&e, 0.080), "70 ms 加两倍不确定度 20 ms 已经超过 80 ms");
        assert!(within(&Estimate { offset: -0.050, ..e }, 0.080));
        assert!(estimate(&[], FPS).is_none());
    }
}
