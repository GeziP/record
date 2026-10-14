//! 采集健康监测（设计文档 5.10）：不静默失败。
//!
//! 三个探测器都是纯状态机，由调用方每秒喂一次数据，返回要不要告警。
//! 睡眠、锁屏、显示器关闭期间，调用方传 `active = false`，所有基于超时的判定停用，
//! 恢复后重新布防，避免唤醒后误报（夸克的看门狗也是这样处理的）。

use crate::clock::{HNS_PER_SEC, Hns};

// ---------------------------------------------------------------------------
// 静默失效监测：流是通的，写进去的却全是静音
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WatchdogConfig {
    /// 播放设备电平表峰值高于它，记为“系统里有声”。
    pub meter_threshold: f32,
    /// 本轨这一秒输出的峰值高于它，记为“采到了声音”（约 −60 dBFS）。
    pub captured_threshold: f32,
    /// 连续多少秒“有声而没采到”才告警。
    pub confirm_ticks: u32,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self { meter_threshold: 0.01, captured_threshold: 32.0 / 32768.0, confirm_ticks: 5 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogEvent {
    /// 播放设备有声，本轨却连续静音。
    Alert,
    /// 告警之后，本轨又采到了声音。
    Recovered,
}

#[derive(Debug, Clone)]
pub struct SilenceWatchdog {
    cfg: WatchdogConfig,
    mismatch: u32,
    alerted: bool,
}

impl SilenceWatchdog {
    pub fn new(cfg: WatchdogConfig) -> Self {
        Self { cfg, mismatch: 0, alerted: false }
    }

    pub fn is_alerting(&self) -> bool {
        self.alerted
    }

    /// 每秒调用一次。
    /// - `meter_peak`：所有活动播放设备电平表的最大峰值；取不到电平表时为 `None`，此时不报告也不猜。
    /// - `captured_peak`：本轨这一秒内输出的峰值。
    /// - `active`：睡眠、锁屏、显示器关闭期间为 `false`。
    ///
    /// 只要有一秒采到声音，计数就清零。不能因为“已经采到过一次声音”就永久解除：
    /// OpenWhispr 的实现一度栽在这里，开场的一声通知音让整场的检查失效。
    pub fn tick(&mut self, meter_peak: Option<f32>, captured_peak: f32, active: bool) -> Option<WatchdogEvent> {
        if !active {
            self.mismatch = 0;
            return None;
        }
        if captured_peak > self.cfg.captured_threshold {
            self.mismatch = 0;
            if self.alerted {
                self.alerted = false;
                return Some(WatchdogEvent::Recovered);
            }
            return None;
        }
        let Some(meter) = meter_peak else {
            self.mismatch = 0;
            return None;
        };
        if meter > self.cfg.meter_threshold {
            self.mismatch += 1;
            if self.mismatch >= self.cfg.confirm_ticks && !self.alerted {
                self.alerted = true;
                return Some(WatchdogEvent::Alert);
            }
        } else {
            self.mismatch = 0;
        }
        None
    }
}

// ---------------------------------------------------------------------------
// 设备挂起：连续 N 秒没有任何包或帧
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HungEvent {
    Hung { idle_secs: u32 },
    Recovered,
}

/// 只用于“正常情况下一直有数据”的流：麦克风、视频。回环设备在没有声音播放时本来就不出包，
/// 不能用它判挂起；回环靠补静音与 `large_gap` 发现线程被饿死。
#[derive(Debug, Clone)]
pub struct HungDetector {
    limit: Hns,
    last_progress: Hns,
    alerted: bool,
}

impl HungDetector {
    pub fn new(limit_secs: u32, now: Hns) -> Self {
        Self { limit: i64::from(limit_secs) * HNS_PER_SEC, last_progress: now, alerted: false }
    }

    /// 收到包或帧时调用。
    pub fn progress(&mut self, now: Hns) -> Option<HungEvent> {
        self.last_progress = now;
        if self.alerted {
            self.alerted = false;
            return Some(HungEvent::Recovered);
        }
        None
    }

    /// 睡眠、锁屏恢复后调用，重新计时。
    pub fn rearm(&mut self, now: Hns) {
        self.last_progress = now;
    }

    /// 每秒调用一次。
    pub fn tick(&mut self, now: Hns, active: bool) -> Option<HungEvent> {
        if !active {
            self.last_progress = now;
            return None;
        }
        let idle = now - self.last_progress;
        if idle >= self.limit && !self.alerted {
            self.alerted = true;
            return Some(HungEvent::Hung { idle_secs: (idle / HNS_PER_SEC) as u32 });
        }
        None
    }
}

// ---------------------------------------------------------------------------
// 持续低帧率
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LowRateEvent {
    Low { ratio: f32 },
    Recovered,
}

#[derive(Debug, Clone)]
pub struct LowRateDetector {
    min_ratio: f32,
    window: u32,
    below: u32,
    alerted: bool,
}

impl LowRateDetector {
    /// 默认：连续 10 秒低于设定帧率的 80%。
    pub fn new(min_ratio: f32, window_secs: u32) -> Self {
        Self { min_ratio, window: window_secs, below: 0, alerted: false }
    }

    /// 每秒调用一次。`achieved` 是这一秒按时完成的帧数（不含追帧补出来的重复帧），`expected` 是设定帧率。
    pub fn tick(&mut self, achieved: u32, expected: u32, active: bool) -> Option<LowRateEvent> {
        if !active || expected == 0 {
            self.below = 0;
            return None;
        }
        let ratio = achieved as f32 / expected as f32;
        if ratio < self.min_ratio {
            self.below += 1;
            if self.below >= self.window && !self.alerted {
                self.alerted = true;
                return Some(LowRateEvent::Low { ratio });
            }
            return None;
        }
        self.below = 0;
        if self.alerted {
            self.alerted = false;
            return Some(LowRateEvent::Recovered);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wd() -> SilenceWatchdog {
        SilenceWatchdog::new(WatchdogConfig::default())
    }

    #[test]
    fn alerts_after_five_consecutive_blind_seconds_and_only_once() {
        // Teams 通话：播放设备电平 0.4，进程级回环采到 0
        let mut w = wd();
        for i in 1..=4 {
            assert_eq!(w.tick(Some(0.4), 0.0, true), None, "第 {i} 秒");
        }
        assert_eq!(w.tick(Some(0.4), 0.0, true), Some(WatchdogEvent::Alert));
        for _ in 0..30 {
            assert_eq!(w.tick(Some(0.4), 0.0, true), None, "同一次异常只告警一次");
        }
        assert!(w.is_alerting());
    }

    #[test]
    fn an_opening_notification_sound_does_not_disarm_the_watchdog() {
        // OpenWhispr 的缺陷：开场采到一声通知音，之后整场静默都不再报告
        let mut w = wd();
        assert_eq!(w.tick(Some(0.6), 0.5, true), None); // 通知音，采到了
        let mut alert_at = None;
        for i in 1..=20 {
            if w.tick(Some(0.4), 0.0, true) == Some(WatchdogEvent::Alert) {
                alert_at = Some(i);
                break;
            }
        }
        assert_eq!(alert_at, Some(5));
    }

    #[test]
    fn one_audible_second_resets_the_counter() {
        let mut w = wd();
        for _ in 0..4 {
            assert_eq!(w.tick(Some(0.4), 0.0, true), None);
        }
        assert_eq!(w.tick(Some(0.4), 0.2, true), None); // 采到了
        for _ in 0..4 {
            assert_eq!(w.tick(Some(0.4), 0.0, true), None);
        }
        assert!(!w.is_alerting());
    }

    #[test]
    fn recovers_and_rearms_after_sound_returns() {
        let mut w = wd();
        for _ in 0..5 {
            w.tick(Some(0.4), 0.0, true);
        }
        assert!(w.is_alerting());
        assert_eq!(w.tick(Some(0.4), 0.3, true), Some(WatchdogEvent::Recovered));
        assert!(!w.is_alerting());
        let mut again = false;
        for _ in 0..5 {
            again |= w.tick(Some(0.4), 0.0, true) == Some(WatchdogEvent::Alert);
        }
        assert!(again, "恢复后应重新布防");
    }

    #[test]
    fn no_meter_means_no_report() {
        let mut w = wd();
        for _ in 0..60 {
            assert_eq!(w.tick(None, 0.0, true), None);
        }
    }

    #[test]
    fn quiet_room_is_not_an_alert() {
        // 系统里没声音，采集也没声音：正常
        let mut w = wd();
        for _ in 0..60 {
            assert_eq!(w.tick(Some(0.0), 0.0, true), None);
        }
    }

    #[test]
    fn suspend_resets_the_counter() {
        let mut w = wd();
        for _ in 0..4 {
            w.tick(Some(0.4), 0.0, true);
        }
        assert_eq!(w.tick(Some(0.4), 0.0, false), None); // 锁屏 / 睡眠 / 显示器关闭
        for _ in 0..4 {
            assert_eq!(w.tick(Some(0.4), 0.0, true), None);
        }
        assert_eq!(w.tick(Some(0.4), 0.0, true), Some(WatchdogEvent::Alert));
    }

    #[test]
    fn hung_detector_fires_once_and_recovers() {
        let mut d = HungDetector::new(10, 0);
        for s in 1..10 {
            assert_eq!(d.tick(s * HNS_PER_SEC, true), None);
        }
        assert_eq!(d.tick(10 * HNS_PER_SEC, true), Some(HungEvent::Hung { idle_secs: 10 }));
        assert_eq!(d.tick(11 * HNS_PER_SEC, true), None);
        assert_eq!(d.progress(12 * HNS_PER_SEC), Some(HungEvent::Recovered));
        assert_eq!(d.progress(13 * HNS_PER_SEC), None);
    }

    #[test]
    fn hung_detector_is_disabled_while_suspended() {
        let mut d = HungDetector::new(10, 0);
        // 睡眠 1 小时，期间 active = false
        for s in 1..=3_600 {
            assert_eq!(d.tick(s * HNS_PER_SEC, false), None);
        }
        // 唤醒后从最后一次“不活动”的时刻重新计时，不会立刻误报
        assert_eq!(d.tick(3_601 * HNS_PER_SEC, true), None);
        assert_eq!(d.tick(3_609 * HNS_PER_SEC, true), None);
        assert!(matches!(d.tick(3_610 * HNS_PER_SEC, true), Some(HungEvent::Hung { .. })));
    }

    #[test]
    fn low_rate_detector_needs_a_sustained_drop() {
        let mut d = LowRateDetector::new(0.8, 10);
        for _ in 0..9 {
            assert_eq!(d.tick(7, 15, true), None);
        }
        assert_eq!(d.tick(15, 15, true), None); // 恢复一秒，计数清零
        for _ in 0..9 {
            assert_eq!(d.tick(7, 15, true), None);
        }
        assert!(matches!(d.tick(7, 15, true), Some(LowRateEvent::Low { .. })));
        assert_eq!(d.tick(7, 15, true), None);
        assert_eq!(d.tick(15, 15, true), Some(LowRateEvent::Recovered));
    }
}
