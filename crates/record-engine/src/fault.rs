//! 故障注入（设计文档 10.1 S3：暂停采集线程、杀编码器，检查 5.10 的各探测器）。
//!
//! 录制路径上只读原子变量；没有注入时没有任何额外开销。

use record_core::session::TrackKind;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// 让某一轨的采集线程在回调里睡 `secs` 秒，模拟线程被饿死或被挂起。
    StallCapture { track: TrackKind, secs: u64 },
    /// 杀掉某一轨的 ffmpeg 进程，模拟编码器崩溃（例如显卡驱动重置）。
    KillEncoder { track: TrackKind },
}

/// 三轨在固定长度数组里的位置。
pub(crate) fn track_index(kind: TrackKind) -> usize {
    match kind {
        TrackKind::Video => 0,
        TrackKind::Mic => 1,
        TrackKind::System => 2,
    }
}

#[derive(Debug, Default)]
pub struct FaultSwitch {
    stall_until: [AtomicI64; 3],
}

impl FaultSwitch {
    pub fn stall_until(&self, kind: TrackKind, until_hns: i64) {
        self.stall_until[track_index(kind)].store(until_hns, Ordering::Release);
    }

    /// 采集回调里调用：还该睡多久。
    pub fn remaining_stall(&self, kind: TrackKind, now_hns: i64) -> Option<Duration> {
        let until = self.stall_until[track_index(kind)].load(Ordering::Acquire);
        (until > now_hns).then(|| Duration::from_nanos(((until - now_hns) as u64).saturating_mul(100)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_stalled_by_default() {
        let f = FaultSwitch::default();
        for k in TrackKind::ALL {
            assert_eq!(f.remaining_stall(k, 123), None);
        }
    }

    #[test]
    fn a_stall_lasts_until_its_deadline_and_only_for_its_track() {
        let f = FaultSwitch::default();
        f.stall_until(TrackKind::Mic, 1_000 + 50_000_000);
        assert_eq!(f.remaining_stall(TrackKind::Mic, 1_000), Some(Duration::from_secs(5)));
        assert_eq!(f.remaining_stall(TrackKind::Mic, 1_000 + 40_000_000), Some(Duration::from_secs(1)));
        assert_eq!(f.remaining_stall(TrackKind::Mic, 1_000 + 50_000_000), None);
        assert_eq!(f.remaining_stall(TrackKind::System, 1_000), None);
        assert_eq!(f.remaining_stall(TrackKind::Video, 1_000), None);
    }
}
