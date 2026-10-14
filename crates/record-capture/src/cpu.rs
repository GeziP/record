//! 进程 CPU 时间计量（设计文档 10 的 S2：整体 CPU 占用 < 5%）。
//!
//! 持有进程句柄，进程退出之后仍可读到累计时间，所以可以先停止、再读数。

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes, GetSystemTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

pub struct ProcCpu {
    handle: HANDLE,
    owned: bool,
}

fn filetime_ms(t: FILETIME) -> u64 {
    // 单位是 100 ns
    (u64::from(t.dwHighDateTime) << 32 | u64::from(t.dwLowDateTime)) / 10_000
}

impl ProcCpu {
    /// 当前进程。
    pub fn current() -> Self {
        Self { handle: unsafe { GetCurrentProcess() }, owned: false }
    }

    pub fn open(pid: u32) -> Option<Self> {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        Some(Self { handle, owned: true })
    }

    /// 累计 `(内核态毫秒, 用户态毫秒)`。
    pub fn times_ms(&self) -> Option<(u64, u64)> {
        let (mut create, mut exit, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        unsafe { GetProcessTimes(self.handle, &mut create, &mut exit, &mut kernel, &mut user) }.ok()?;
        Some((filetime_ms(kernel), filetime_ms(user)))
    }

    /// 累计 CPU 毫秒（内核 + 用户）。
    pub fn total_ms(&self) -> Option<u64> {
        self.times_ms().map(|(k, u)| k + u)
    }
}

impl Drop for ProcCpu {
    fn drop(&mut self) {
        if self.owned {
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }
}

/// 逻辑处理器数，用来把“占几个核”换算成“占整机百分之几”。
pub fn logical_cpus() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

/// 整机累计 `(忙碌毫秒, 总毫秒)`，总毫秒 = 全部逻辑处理器的墙钟时间之和。仅作背景参考：包含所有程序。
pub fn system_busy_total_ms() -> Option<(u64, u64)> {
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
    // 内核时间里包含空闲时间
    let total = filetime_ms(kernel) + filetime_ms(user);
    Some((total.saturating_sub(filetime_ms(idle)), total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_work_shows_up_in_the_cpu_time_of_this_process() {
        let me = ProcCpu::current();
        let before = me.total_ms().expect("读取 CPU 时间");
        let start = std::time::Instant::now();
        let mut x = 0u64;
        while start.elapsed().as_millis() < 300 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        }
        std::hint::black_box(x);
        let used = me.total_ms().expect("读取 CPU 时间") - before;
        assert!(used >= 200, "忙等 300 ms，只算出 {used} ms");
    }

    #[test]
    fn the_whole_machine_has_busy_time_below_its_total() {
        let (busy, total) = system_busy_total_ms().expect("读取整机 CPU 时间");
        assert!(total > 0 && busy <= total, "busy {busy} total {total}");
    }

    #[test]
    fn another_process_can_be_opened_by_pid() {
        let mut child = std::process::Command::new("cmd").args(["/c", "ping -n 2 127.0.0.1 >nul"]).spawn().expect("启动子进程");
        let cpu = ProcCpu::open(child.id()).expect("按 pid 打开");
        let _ = child.wait();
        assert!(cpu.times_ms().is_some(), "进程退出后仍应能读到累计时间");
        assert!(logical_cpus() >= 1);
    }
}
