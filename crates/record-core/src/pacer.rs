//! 画面的恒定帧率节拍与按原因分类的帧统计（设计文档 5.2、5.9）。
//!
//! 帧序号 = floor((now − t0) × fps)。采集循环落后时，一次会有多帧同时到期，
//! 调用方用最近一帧补齐，保证输出文件的时间线与 QPC 一致。

use crate::clock::{HNS_PER_SEC, Hns};
use serde::{Deserialize, Serialize};
use std::ops::Range;

#[derive(Debug, Clone)]
pub struct FramePacer {
    fps: u32,
    t0: Hns,
    next: u64,
}

impl FramePacer {
    pub fn new(fps: u32, t0: Hns) -> Self {
        assert!(fps > 0, "帧率必须为正数");
        Self { fps, t0, next: 0 }
    }

    pub fn fps(&self) -> u32 {
        self.fps
    }

    /// 下一个还没有交付的帧序号。
    pub fn next_index(&self) -> u64 {
        self.next
    }

    /// 序号为 `index` 的帧最早到期的时刻（向上取整，与 [`FramePacer::due`] 互逆）。
    pub fn time_of(&self, index: u64) -> Hns {
        let fps = i128::from(self.fps);
        self.t0 + ((i128::from(index) * i128::from(HNS_PER_SEC) + fps - 1) / fps) as i64
    }

    /// 到 `now` 为止应当已交付、但还没交付的帧序号。调用后这些帧视为已交付。
    pub fn due(&mut self, now: Hns) -> Range<u64> {
        if now < self.t0 {
            return self.next..self.next;
        }
        let elapsed = i128::from(now - self.t0);
        let last = (elapsed * i128::from(self.fps) / i128::from(HNS_PER_SEC)) as u64;
        let end = last + 1;
        let start = self.next.min(end);
        self.next = end.max(self.next);
        start..end
    }
}

/// 重复上一帧的原因。分原因计数，是为了出问题时能直接看出是哪一环（参考 Loom 的做法）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatReason {
    /// 屏幕没有变化（`AcquireNextFrame` 超时），正常情况。
    NoChange,
    /// UAC、锁屏等安全桌面，系统不允许抓取。
    SecureDesktop,
    /// 复制对象失效，正在重建。
    DuplicationRebuild,
    /// 复制对象出错，没有重建成功。
    DuplicationFailed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameStats {
    pub fresh: u64,
    pub repeat_no_change: u64,
    pub repeat_secure_desktop: u64,
    pub repeat_duplication_rebuild: u64,
    pub repeat_duplication_failed: u64,
    /// 采集循环落后，用最近一帧补出来的帧。
    pub catch_up: u64,
    pub dropped_encoder: u64,
    pub dropped_error: u64,
    pub dropped_queue: u64,
    pub dropped_resync: u64,
}

impl FrameStats {
    pub fn record_repeat(&mut self, reason: RepeatReason) {
        match reason {
            RepeatReason::NoChange => self.repeat_no_change += 1,
            RepeatReason::SecureDesktop => self.repeat_secure_desktop += 1,
            RepeatReason::DuplicationRebuild => self.repeat_duplication_rebuild += 1,
            RepeatReason::DuplicationFailed => self.repeat_duplication_failed += 1,
        }
    }

    /// 所有帧的总数：每个到期的帧恰好属于其中一类。
    pub fn total_emitted(&self) -> u64 {
        self.fresh + self.repeat_no_change + self.repeat_secure_desktop + self.repeat_duplication_rebuild + self.repeat_duplication_failed + self.catch_up
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ms_to_hns;

    #[test]
    fn first_frame_is_due_immediately() {
        let mut p = FramePacer::new(15, 1_000);
        assert_eq!(p.due(1_000), 0..1);
        assert_eq!(p.due(1_000), 1..1, "同一时刻不会重复交付");
    }

    #[test]
    fn frames_are_due_at_their_scheduled_times() {
        let mut p = FramePacer::new(15, 0);
        assert_eq!(p.due(0), 0..1);
        assert_eq!(p.due(ms_to_hns(60)), 1..1); // 第 1 帧在 66.67 ms
        assert_eq!(p.due(ms_to_hns(67)), 1..2);
        assert_eq!(p.due(ms_to_hns(133)), 2..2); // 第 2 帧在 133.33 ms
        assert_eq!(p.due(ms_to_hns(134)), 2..3);
    }

    #[test]
    fn a_late_caller_gets_all_missed_frames_at_once() {
        let mut p = FramePacer::new(15, 0);
        assert_eq!(p.due(0), 0..1);
        // 采集循环卡了 1 秒
        let r = p.due(ms_to_hns(1_000));
        assert_eq!(r, 1..16);
        assert_eq!(r.end - r.start, 15);
    }

    #[test]
    fn exactly_fps_frames_per_second_over_a_long_run() {
        let mut p = FramePacer::new(15, 0);
        let mut count = 0u64;
        // 每 7 ms 调用一次，跑 60 秒
        let mut now = 0;
        while now <= ms_to_hns(60_000) {
            count += p.due(now).count() as u64;
            now += ms_to_hns(7);
        }
        // 最后一次调用在 59.997 秒，此时第 899 帧已到期，第 900 帧（60.0 秒）还没有
        assert_eq!(count, 900);
    }

    #[test]
    fn time_of_is_the_exact_inverse_of_due() {
        for fps in [10, 15, 30] {
            let p = FramePacer::new(fps, 5_000);
            assert_eq!(p.time_of(0), 5_000);
            assert_eq!(p.time_of(u64::from(fps)), 5_000 + 10_000_000);
            for k in 1..200u64 {
                let mut before = FramePacer::new(fps, 5_000);
                assert_eq!(before.due(p.time_of(k) - 1).end, k, "fps {fps} 第 {k} 帧到期前一刻");
                let mut at = FramePacer::new(fps, 5_000);
                assert_eq!(at.due(p.time_of(k)).end, k + 1, "fps {fps} 第 {k} 帧到期时");
            }
        }
    }

    #[test]
    fn a_clock_before_t0_yields_nothing() {
        let mut p = FramePacer::new(30, 10_000);
        assert_eq!(p.due(5_000), 0..0);
    }

    #[test]
    fn frame_stats_count_every_frame_exactly_once() {
        let mut s = FrameStats::default();
        s.fresh += 3;
        s.record_repeat(RepeatReason::NoChange);
        s.record_repeat(RepeatReason::SecureDesktop);
        s.record_repeat(RepeatReason::DuplicationRebuild);
        s.record_repeat(RepeatReason::DuplicationFailed);
        s.catch_up += 2;
        assert_eq!(s.total_emitted(), 9);
        assert_eq!(s.repeat_secure_desktop, 1);
    }
}
