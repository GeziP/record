//! 统一时钟：QPC 计数与 100 ns 单位之间的换算（设计文档 5.1）。
//!
//! 全程序所有数据都用 100 ns 为单位的时间戳。WASAPI 的 `qpcPosition` 本身就是这个单位；
//! `QueryPerformanceCounter` 的原始计数要靠 [`QpcClock`] 换算。

/// 100 ns 为单位的时间戳或时长。
pub type Hns = i64;

/// 每秒的 100 ns 单位数。
pub const HNS_PER_SEC: i64 = 10_000_000;

/// QPC 计数换算器。`freq` 是 `QueryPerformanceFrequency` 返回的每秒计数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QpcClock {
    freq: i64,
}

impl QpcClock {
    /// # Panics
    /// `freq` 不是正数时 panic（系统不会给出这种值，出现说明调用方写错了）。
    pub fn new(freq: i64) -> Self {
        assert!(freq > 0, "QPC 频率必须为正数，实际为 {freq}");
        Self { freq }
    }

    pub fn freq(&self) -> i64 {
        self.freq
    }

    /// QPC 计数 → 100 ns 单位，四舍五入到最近值。中间用 128 位运算，不会溢出。
    pub fn ticks_to_hns(&self, ticks: i64) -> Hns {
        div_round(i128::from(ticks) * i128::from(HNS_PER_SEC), i128::from(self.freq)) as i64
    }

    /// 100 ns 单位 → QPC 计数，四舍五入到最近值。
    pub fn hns_to_ticks(&self, hns: Hns) -> i64 {
        div_round(i128::from(hns) * i128::from(self.freq), i128::from(HNS_PER_SEC)) as i64
    }
}

/// 把 100 ns 时长换算为采样帧数，向零截断（宁可少算，不多算）。
pub fn hns_to_frames(hns: Hns, rate: u32) -> i64 {
    (i128::from(hns) * i128::from(rate) / i128::from(HNS_PER_SEC)) as i64
}

/// 把采样帧数换算为 100 ns 时长，向零截断。
pub fn frames_to_hns(frames: i64, rate: u32) -> Hns {
    (i128::from(frames) * i128::from(HNS_PER_SEC) / i128::from(rate)) as i64
}

/// 毫秒换算为 100 ns 单位。
pub const fn ms_to_hns(ms: i64) -> Hns {
    ms * 10_000
}

fn div_round(num: i128, den: i128) -> i128 {
    let half = den / 2;
    if num >= 0 { (num + half) / den } else { (num - half) / den }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_when_freq_is_10mhz() {
        let c = QpcClock::new(10_000_000);
        assert_eq!(c.ticks_to_hns(123_456_789), 123_456_789);
        assert_eq!(c.hns_to_ticks(123_456_789), 123_456_789);
    }

    #[test]
    fn converts_common_frequencies() {
        // 24 MHz：24_000_000 计数正好 1 秒
        let c = QpcClock::new(24_000_000);
        assert_eq!(c.ticks_to_hns(24_000_000), HNS_PER_SEC);
        assert_eq!(c.hns_to_ticks(HNS_PER_SEC), 24_000_000);
        // 3.0 GHz 量级
        let c = QpcClock::new(3_000_000_000);
        assert_eq!(c.ticks_to_hns(3_000_000_000), HNS_PER_SEC);
    }

    #[test]
    fn rounds_to_nearest_and_is_symmetric() {
        let c = QpcClock::new(3); // 1 计数 = 3_333_333.33.. 个 100ns
        assert_eq!(c.ticks_to_hns(1), 3_333_333);
        assert_eq!(c.ticks_to_hns(2), 6_666_667);
        assert_eq!(c.ticks_to_hns(-2), -6_666_667);
    }

    #[test]
    fn does_not_overflow_on_huge_tick_counts() {
        // 运行 30 年的计数值：约 1e9 秒 × 1e7 Hz
        let c = QpcClock::new(10_000_000);
        let ticks = 9_500_000_000_000_000i64;
        assert_eq!(c.ticks_to_hns(ticks), ticks);
        let c = QpcClock::new(24_000_000);
        let ticks = 24_000_000i64 * 1_000_000_000;
        assert_eq!(c.ticks_to_hns(ticks), HNS_PER_SEC * 1_000_000_000);
    }

    #[test]
    #[should_panic(expected = "QPC 频率必须为正数")]
    fn rejects_non_positive_frequency() {
        let _ = QpcClock::new(0);
    }

    #[test]
    fn frame_and_time_conversions() {
        assert_eq!(hns_to_frames(ms_to_hns(10), 48_000), 480);
        assert_eq!(frames_to_hns(480, 48_000), ms_to_hns(10));
        assert_eq!(hns_to_frames(HNS_PER_SEC, 48_000), 48_000);
        // 向零截断
        assert_eq!(hns_to_frames(-ms_to_hns(1), 44_100), -44);
        assert_eq!(hns_to_frames(ms_to_hns(1), 44_100), 44);
    }
}
