//! QPC 时钟：所有轨道共用的时间基准，换算为 100 ns（设计文档 5.1）。

use record_core::clock::{Hns, QpcClock};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

#[derive(Debug, Clone, Copy)]
pub struct Qpc {
    clock: QpcClock,
}

impl Qpc {
    pub fn new() -> Self {
        let mut freq = 0i64;
        // 在 Windows XP 之后不会失败
        unsafe { QueryPerformanceFrequency(&mut freq).expect("QueryPerformanceFrequency") };
        Self { clock: QpcClock::new(freq) }
    }

    pub fn frequency(&self) -> i64 {
        self.clock.freq()
    }

    pub fn now_ticks(&self) -> i64 {
        let mut t = 0i64;
        unsafe { QueryPerformanceCounter(&mut t).expect("QueryPerformanceCounter") };
        t
    }

    pub fn now_hns(&self) -> Hns {
        self.clock.ticks_to_hns(self.now_ticks())
    }

    pub fn ticks_to_hns(&self, ticks: i64) -> Hns {
        self.clock.ticks_to_hns(ticks)
    }
}

impl Default for Qpc {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_is_monotonic_and_has_a_sane_frequency() {
        let q = Qpc::new();
        assert!(q.frequency() >= 1_000_000, "QPC 频率 {}", q.frequency());
        let a = q.now_hns();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let b = q.now_hns();
        let ms = (b - a) / 10_000;
        assert!((45..=200).contains(&ms), "睡 50 ms 实测 {ms} ms");
    }
}
