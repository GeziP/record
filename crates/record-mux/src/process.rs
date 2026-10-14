//! 对 `std::process::Child` 的薄封装：不弹控制台窗口、stderr 只留最后几十行、带超时的等待与强杀。

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

const TAIL_LINES: usize = 40;

pub struct FfmpegProc {
    child: Child,
    tail: Arc<Mutex<VecDeque<String>>>,
    stderr_thread: Option<JoinHandle<()>>,
    stderr_done: Arc<AtomicBool>,
}

impl FfmpegProc {
    /// `stdin_piped` 为真时可用 [`FfmpegProc::take_stdin`] 取走标准输入。
    /// `low_priority` 用于收尾与修复这类后台任务，不去抢正在进行的录制的 CPU。
    pub fn spawn(program: &Path, args: &[OsString], stdin_piped: bool, low_priority: bool) -> io::Result<Self> {
        let mut cmd = Command::new(program);
        cmd.args(args).stdin(if stdin_piped { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::null()).stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW | if low_priority { BELOW_NORMAL_PRIORITY_CLASS } else { 0 });
        }
        #[cfg(not(windows))]
        let _ = low_priority;
        let mut child = cmd.spawn()?;
        let tail = Arc::new(Mutex::new(VecDeque::with_capacity(TAIL_LINES)));
        let stderr = child.stderr.take().expect("stderr 已设置为管道");
        let t = Arc::clone(&tail);
        let stderr_done = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&stderr_done);
        let stderr_thread = thread::Builder::new().name("ffmpeg-stderr".into()).spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        // 错误信息里的路径可能是系统代码页编码，按有损方式解码，不因此丢日志
                        let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                        if line.is_empty() {
                            continue;
                        }
                        let mut q = t.lock().unwrap();
                        if q.len() == TAIL_LINES {
                            q.pop_front();
                        }
                        q.push_back(line);
                    }
                }
            }
            done.store(true, Ordering::Release);
        })?;
        Ok(Self { child, tail, stderr_thread: Some(stderr_thread), stderr_done })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    /// stderr 最后若干行，用于错误提示与日志。
    pub fn stderr_tail(&self) -> String {
        self.tail.lock().unwrap().iter().cloned().collect::<Vec<_>>().join("\n")
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// 在 `timeout` 内等待退出；超时返回 `Ok(None)`，进程仍在运行。
    pub fn wait_timeout(&mut self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                self.join_stderr();
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.join_stderr();
    }

    /// 进程已结束，等 stderr 读完最后几行；最多等 1 秒。
    /// 管道的写端可能被孙进程继承而一直不关闭，不能无限等下去，超时就放手，线程在管道关闭时自行退出。
    fn join_stderr(&mut self) {
        let Some(h) = self.stderr_thread.take() else { return };
        let deadline = Instant::now() + Duration::from_secs(1);
        while !self.stderr_done.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        if self.stderr_done.load(Ordering::Acquire) {
            let _ = h.join();
        }
    }
}

impl Drop for FfmpegProc {
    fn drop(&mut self) {
        // 正常路径由 finish / wait 收尾；走到这里仍在运行，说明调用方提前丢弃了它，强杀以免留下孤儿进程
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn cmd_exe() -> std::path::PathBuf {
        std::path::PathBuf::from(std::env::var_os("ComSpec").expect("ComSpec"))
    }

    #[test]
    fn captures_the_tail_of_stderr_and_the_exit_code() {
        let args: Vec<OsString> = ["/c", "echo first 1>&2 & echo second 1>&2 & exit 3"].iter().map(OsString::from).collect();
        let mut p = FfmpegProc::spawn(&cmd_exe(), &args, false, false).unwrap();
        let status = p.wait_timeout(Duration::from_secs(10)).unwrap().expect("应在 10 秒内结束");
        assert_eq!(status.code(), Some(3));
        let tail = p.stderr_tail();
        assert!(tail.contains("first") && tail.contains("second"), "{tail:?}");
    }

    #[test]
    fn stderr_keeps_only_the_last_lines() {
        let script = "for /l %i in (1,1,100) do @echo line%i 1>&2";
        let args: Vec<OsString> = ["/c", script].iter().map(OsString::from).collect();
        let mut p = FfmpegProc::spawn(&cmd_exe(), &args, false, false).unwrap();
        p.wait_timeout(Duration::from_secs(10)).unwrap().unwrap();
        let tail = p.stderr_tail();
        assert!(tail.contains("line100") && !tail.contains("line1\n") && !tail.contains("line10\n"), "{tail}");
        assert_eq!(tail.lines().count(), TAIL_LINES);
    }

    #[test]
    fn a_long_running_process_times_out_and_can_be_killed() {
        let args: Vec<OsString> = ["/c", "ping -n 30 127.0.0.1 > nul"].iter().map(OsString::from).collect();
        let mut p = FfmpegProc::spawn(&cmd_exe(), &args, false, false).unwrap();
        let t = Instant::now();
        assert!(p.wait_timeout(Duration::from_millis(300)).unwrap().is_none());
        assert!(t.elapsed() < Duration::from_secs(3));
        p.kill();
        assert!(p.try_wait().unwrap().is_some());
    }

    #[test]
    fn dropping_a_running_process_kills_it() {
        let args: Vec<OsString> = ["/c", "ping -n 30 127.0.0.1 > nul"].iter().map(OsString::from).collect();
        let p = FfmpegProc::spawn(&cmd_exe(), &args, false, false).unwrap();
        let pid = p.id();
        drop(p);
        let out = Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH"]).output().unwrap();
        assert!(!String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()), "进程 {pid} 仍在运行");
    }
}
