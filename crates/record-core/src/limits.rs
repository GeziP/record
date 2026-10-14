//! 自动保护（设计文档 6.5）：单场时长上限、磁盘剩余空间下限。纯判定，采样由外壳每几秒做一次。

use crate::config::Limits;
use crate::session::EndReason;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// 返回需要自动结束的原因。磁盘空间优先于时长：磁盘写满会损坏正在写的文件。
pub fn check(elapsed_secs: u64, free_bytes: u64, limits: &Limits) -> Option<EndReason> {
    if (free_bytes as f64) < limits.min_free_gb * GIB {
        return Some(EndReason::DiskLow);
    }
    if elapsed_secs as f64 >= limits.max_hours * 3600.0 {
        return Some(EndReason::MaxDuration);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn healthy_sessions_are_left_alone() {
        assert_eq!(check(3_600, 100 * GB, &Limits::default()), None);
    }

    #[test]
    fn stops_exactly_at_the_time_limit() {
        let l = Limits::default();
        assert_eq!(check(6 * 3600 - 1, 100 * GB, &l), None);
        assert_eq!(check(6 * 3600, 100 * GB, &l), Some(EndReason::MaxDuration));
    }

    #[test]
    fn stops_below_the_free_space_floor() {
        let l = Limits::default();
        assert_eq!(check(10, 5 * GB, &l), None);
        assert_eq!(check(10, 5 * GB - 1, &l), Some(EndReason::DiskLow));
    }

    #[test]
    fn disk_beats_duration_when_both_apply() {
        assert_eq!(check(7 * 3600, GB, &Limits::default()), Some(EndReason::DiskLow));
    }

    #[test]
    fn fractional_limits_work() {
        let l = Limits { max_hours: 0.5, min_free_gb: 1.5 };
        assert_eq!(check(1_799, 2 * GB, &l), None);
        assert_eq!(check(1_800, 2 * GB, &l), Some(EndReason::MaxDuration));
        assert_eq!(check(0, GB, &l), Some(EndReason::DiskLow));
    }
}
