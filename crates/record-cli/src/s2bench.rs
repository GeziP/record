//! S2 压力与 CPU：真实编码器 + 独立进程里的动画测试窗口，分别计量采集进程与 ffmpeg 的 CPU。
//!
//! 为什么窗口要放在独立进程：动画窗口自己的绘制也吃 CPU，放在同一进程里会算进采集的账。
//! 编码输出写到 `NUL`：只要吞吐与 CPU，不在磁盘上留下任何画面内容（整块显示器都会被录进去）。

use crate::args::Args;
use crate::rec::parse_encoder;
use crate::s2::{parse_size, pick_from_args};
use anyhow::{Context, Result, bail};
use crossbeam_channel::unbounded;
use record_capture::cpu::{ProcCpu, logical_cpus, system_busy_total_ms};
use record_capture::monitor::{ensure_dpi_aware, list_monitors};
use record_capture::qpc::Qpc;
use record_capture::testwin::{TestWindow, TestWindowSpec};
use record_capture::video::{VideoConfig, VideoSource};
use record_core::ffmpeg_args::VideoSpec;
use record_core::geometry::output_size;
use record_mux::locate::find_ffmpeg;
use record_mux::probe::resolve_encoder;
use record_mux::writer::{Pushed, VideoWriter};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// 只放一个动画测试窗口，不采集。`bench` 把它当子进程拉起。
pub fn window_cmd(a: &Args) -> Result<()> {
    ensure_dpi_aware();
    let secs: u64 = a.num("secs", 30)?;
    let (w, h) = parse_size(a.get("size").unwrap_or("960x540"))?;
    let list = list_monitors()?;
    let mon = pick_from_args(a, &list)?;
    if w > mon.width || h > mon.height {
        bail!("测试窗口 {w}×{h} 比显示器 {}×{} 还大", mon.width, mon.height);
    }
    let window = TestWindow::open(TestWindowSpec { left: mon.left + ((mon.width - w) / 2) as i32, top: mon.top + ((mon.height - h) / 2) as i32, width: w, height: h, animate: true }, Qpc::new())
        .map_err(anyhow::Error::msg)
        .context("创建测试窗口")?;
    std::thread::sleep(Duration::from_secs(secs));
    window.close();
    Ok(())
}

/// 两次读数之间某个进程占用的 CPU 毫秒。
struct Meter<'a> {
    cpu: &'a ProcCpu,
    start_ms: u64,
}

impl<'a> Meter<'a> {
    fn start(cpu: &'a ProcCpu) -> Self {
        Self { cpu, start_ms: cpu.total_ms().unwrap_or(0) }
    }

    fn used_ms(&self) -> u64 {
        self.cpu.total_ms().unwrap_or(0).saturating_sub(self.start_ms)
    }
}

/// `used_ms` 毫秒 CPU 在 `wall` 内占一个核的百分之几。
fn pct_of_core(used_ms: u64, wall: Duration) -> f64 {
    used_ms as f64 / 10.0 / wall.as_secs_f64()
}

pub fn bench_cmd(a: &Args) -> Result<()> {
    ensure_dpi_aware();
    let secs: u64 = a.num("secs", 30)?;
    let fps: u32 = a.num("fps", 15)?;
    let report_every: u64 = a.num("report", 10)?;
    let limit_pct: f64 = a.num("limit", 5.0)?;
    let size_text = a.get("size").unwrap_or("960x540").to_string();
    let monitor_arg = a.get("monitor").unwrap_or("primary").to_string();
    let wanted = parse_encoder(a.get("encoder").unwrap_or("auto"))?;

    let list = list_monitors()?;
    let mon = pick_from_args(a, &list)?;
    let out = output_size(mon.width, mon.height, 1920, 1080);
    let ffmpeg = find_ffmpeg().context("找 ffmpeg")?;
    let encoder = resolve_encoder(&ffmpeg, wanted).context("选编码器")?;
    println!("显示器：{}", mon.describe());
    println!("输出：{}×{}，{} fps，编码器 {:?}，时长 {} 秒，{}", out.0, out.1, fps, encoder, secs, if a.flag("no-window") { "不放测试窗口（采集当前桌面）" } else { "测试窗口在独立进程里" });

    let mut window_proc = if a.flag("no-window") {
        None
    } else {
        let exe = std::env::current_exe().context("找自己的路径")?;
        let child = Command::new(exe)
            .args(["s2", "window", "--secs", &(secs + 8).to_string(), "--size", &size_text, "--monitor", &monitor_arg])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("拉起测试窗口进程")?;
        std::thread::sleep(Duration::from_millis(1500)); // 窗口出现、桌面稳定
        Some(child)
    };
    let window_cpu = window_proc.as_ref().and_then(|c| ProcCpu::open(c.id()));

    let writer = VideoWriter::spawn(&ffmpeg, &VideoSpec { width: out.0, height: out.1, fps, encoder }, Path::new("NUL"), fps as usize * 2).context("启动 ffmpeg")?;
    let ffmpeg_cpu = writer.process_id().and_then(ProcCpu::open).context("打开 ffmpeg 进程读 CPU 时间")?;
    let writer = Arc::new(writer);
    let me = ProcCpu::current();

    let (event_tx, event_rx) = unbounded();
    let started = Instant::now();
    let printer = std::thread::spawn(move || {
        for e in event_rx {
            println!("  [+{:.1}s] {e:?}", started.elapsed().as_secs_f64());
        }
    });
    let overflow = Arc::new(AtomicU64::new(0));
    let rejected = Arc::new(AtomicU64::new(0));
    let (w, ov, rj) = (Arc::clone(&writer), Arc::clone(&overflow), Arc::clone(&rejected));
    let qpc = Qpc::new();
    println!("\n── 运行（每 {report_every} 秒一行：已交付帧 / 新画面 / 采集进程与 ffmpeg 各占一个核的百分比）──");
    let source = VideoSource::start(
        VideoConfig { monitor: mon.clone(), fps, out_w: out.0, out_h: out.1, t0: qpc.now_hns(), rebuild_interval: Duration::from_secs(1) },
        qpc,
        event_tx,
        Box::new(move |index, data, _kind| match w.submit(index, data) {
            Pushed::Queued => {}
            Pushed::Overflow(n) => {
                ov.fetch_add(n, Ordering::Relaxed);
            }
            Pushed::WriterFailed => {
                rj.fetch_add(1, Ordering::Relaxed);
            }
        }),
    )
    .context("启动画面采集")?;

    // 从首帧稳定之后开始计量，把着色器编译、编码器启动这类一次性开销排除在外
    std::thread::sleep(Duration::from_secs(2));
    let (me_meter, ff_meter) = (Meter::start(&me), Meter::start(&ffmpeg_cpu));
    let win_meter = window_cpu.as_ref().map(Meter::start);
    let sys_start = system_busy_total_ms();
    let t_start = Instant::now();
    let live = Arc::clone(source.live());
    let frames_start = live.emitted_frames();
    let mut next_report = report_every;
    while t_start.elapsed().as_secs() < secs {
        std::thread::sleep(Duration::from_millis(250));
        let t = t_start.elapsed();
        if t.as_secs() >= next_report {
            next_report += report_every;
            println!(
                "  t={:>4}s  帧 {:>6}  新画面 {:>6}  采集进程 {:.1}%  ffmpeg {:.1}%  写入器 {:?}",
                t.as_secs(),
                live.emitted_frames() - frames_start,
                live.fresh_frames(),
                pct_of_core(me_meter.used_ms(), t),
                pct_of_core(ff_meter.used_ms(), t),
                writer.health()
            );
        }
    }
    let wall = t_start.elapsed();
    let (me_ms, ff_ms) = (me_meter.used_ms(), ff_meter.used_ms());
    let win_ms = win_meter.as_ref().map(Meter::used_ms);
    let sys_end = system_busy_total_ms();
    let frames_in_window = live.emitted_frames() - frames_start;

    let result = source.stop();
    printer.join().expect("事件打印线程");
    if let Some(c) = window_proc.as_mut() {
        let _ = c.kill();
        let _ = c.wait();
    }
    let report = Arc::try_unwrap(writer).map_err(|_| anyhow::anyhow!("写入器仍被引用")).context("收回写入器")?.finish(Duration::from_secs(20));

    let cpus = logical_cpus();
    let (me_core, ff_core) = (pct_of_core(me_ms, wall), pct_of_core(ff_ms, wall));
    let total_core = me_core + ff_core;
    let total_machine = total_core / cpus as f64;
    println!("\n── 帧 ──");
    let st = &result.stats;
    println!(
        "计量窗口 {:.1} 秒内交付 {frames_in_window} 帧（期望约 {:.0}）；全程新画面 {}，无变化重复 {}，安全桌面 {}，重建中 {}，落后补帧 {}",
        wall.as_secs_f64(),
        wall.as_secs_f64() * f64::from(fps),
        st.fresh,
        st.repeat_no_change,
        st.repeat_secure_desktop,
        st.repeat_duplication_rebuild,
        st.catch_up
    );
    println!(
        "桌面格式 {}；首帧 {} ms；失效 {} 次；重建 {} 次；断开累计 {} ms；取帧耗时 p50 {:.1} ms，p95 {:.1} ms，p99 {:.1} ms，最大 {:.1} ms",
        result.src_format.map_or("未知".into(), |f| f.to_string()),
        result.first_frame_ms.map_or("无".into(), |v| v.to_string()),
        result.lost,
        result.rebuilds,
        result.down_ms,
        f64::from(result.grab.p50_us) / 1000.0,
        f64::from(result.grab.p95_us) / 1000.0,
        f64::from(result.grab.p99_us) / 1000.0,
        f64::from(result.grab.max_us) / 1000.0
    );
    println!("\n── 写入器 ──");
    println!(
        "ffmpeg 退出码 {:?}；强杀 {}；失败 {:?}；写入 {:.1} MB（{} 帧）；队列溢出丢帧 {}；写入器拒收 {}；序号空缺重复 {}；迟到丢弃 {}",
        report.exit_code,
        report.killed,
        report.failed,
        report.bytes_written as f64 / 1e6,
        report.bytes_written / (out.0 as u64 * out.1 as u64 * 3 / 2),
        overflow.load(Ordering::Relaxed),
        rejected.load(Ordering::Relaxed),
        report.gap_repeats,
        report.late_discarded
    );
    if !report.stderr_tail.trim().is_empty() {
        println!("ffmpeg 输出的末尾：{}", report.stderr_tail.trim());
    }

    println!("\n── CPU（{} 个逻辑处理器，计量窗口 {:.1} 秒）──", cpus, wall.as_secs_f64());
    println!("  采集进程（捕获 + GPU 转换 + 回读 + 写管道）  {me_core:.2}% 个核");
    println!("  ffmpeg（{encoder:?} 编码）                    {ff_core:.2}% 个核");
    println!("  合计                                          {total_core:.2}% 个核 = 整机 {total_machine:.2}%");
    if let Some(ms) = win_ms {
        println!("  （测试窗口进程的绘制，不计入）                {:.2}% 个核", pct_of_core(ms, wall));
    }
    if let (Some((b0, t0)), Some((b1, t1))) = (sys_start, sys_end)
        && t1 > t0
    {
        println!("  （整机 CPU 使用率，含所有程序，仅供参考）      {:.1}%", (b1 - b0) as f64 * 100.0 / (t1 - t0) as f64);
    }

    let mut problems: Vec<String> = Vec::new();
    if total_machine >= limit_pct {
        problems.push(format!("整机占用 {total_machine:.2}% 超过上限 {limit_pct}%"));
    }
    if report.exit_code != Some(0) || report.killed || report.failed.is_some() {
        problems.push("ffmpeg 没有正常结束".into());
    }
    if overflow.load(Ordering::Relaxed) > 0 || rejected.load(Ordering::Relaxed) > 0 {
        problems.push("写入器有丢帧".into());
    }
    if (frames_in_window as f64) < wall.as_secs_f64() * f64::from(fps) * 0.95 {
        problems.push(format!("计量窗口内只交付了 {frames_in_window} 帧"));
    }
    if result.lost > 0 || result.down_ms > 0 {
        problems.push(format!("管线断开过：失效 {} 次，累计 {} ms", result.lost, result.down_ms));
    }
    if problems.is_empty() {
        println!("\n压力测试通过（整机占用 {total_machine:.2}% < {limit_pct}%）。");
        Ok(())
    } else {
        for p in &problems {
            println!("✘ {p}");
        }
        bail!("压力测试未通过（{} 项）", problems.len())
    }
}
