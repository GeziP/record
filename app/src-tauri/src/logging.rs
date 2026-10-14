//! 日志（设计文档 4.4）：按天滚动写到 `%LOCALAPPDATA%\record\logs\record.log.<日期>`。
//! 程序没有控制台，所以日志只进文件；用环境变量 `RECORD_LOG` 调级别（默认 info）。

use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;

/// 日志里的时间用本机本地时间（默认是 UTC，对着自己的时钟看日志会差几个小时）。
struct LocalClock;

impl FormatTime for LocalClock {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"))
    }
}

/// 返回的守卫要一直拿着：它掉了，缓冲里还没写完的日志就丢了。目录建不出来时返回 `None`，程序照常运行，只是没有日志。
pub fn init(dir: &Path) -> Option<WorkerGuard> {
    std::fs::create_dir_all(dir).ok()?;
    let (writer, guard) = tracing_appender::non_blocking(tracing_appender::rolling::daily(dir, "record.log"));
    let filter = EnvFilter::try_from_env("RECORD_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_writer(writer).with_ansi(false).with_timer(LocalClock).with_env_filter(filter).try_init().ok()?;
    Some(guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_logged_after_init_end_up_in_the_daily_file() {
        let dir = tempfile::tempdir().unwrap();
        let guard = init(dir.path()).expect("初始化日志");
        tracing::info!("日志自检 {}", 42);
        drop(guard); // 冲刷缓冲
        let text: String = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| std::fs::read_to_string(e.path()).unwrap()).collect();
        assert!(text.contains("日志自检 42"), "{text}");
        assert!(!text.contains('\u{1b}'), "文件里不应该有颜色转义");
        let stamp = text.lines().next().and_then(|line| line.get(..23)).unwrap_or_default();
        assert!(chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S%.3f").is_ok(), "行首应是本地时间：{text}");
    }
}
