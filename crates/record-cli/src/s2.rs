//! S2 画面 spike 的命令：显示器信息、GPU 路径自检。
//!
//! 自检不去看用户的屏幕：在目标显示器中央放一个自建的测试窗口（已知颜色分区加一条移动的亮条），
//! 只在采集到的帧里对这个窗口的区域取样。颜色验证 GPU 转换路径，亮条位置反推画面内容的陈旧程度。

use crate::args::Args;
use crate::frames::{Nv12, shrink};
use anyhow::{Context, Result, bail};
use crossbeam_channel::unbounded;
use record_capture::monitor::{MonitorInfo, Pick, ensure_dpi_aware, list_monitors, pick_monitor};
use record_capture::qpc::Qpc;
use record_capture::testwin::{TestWindow, TestWindowSpec};
use record_capture::video::{FrameKind, VideoConfig, VideoSource};
use record_core::clock::Hns;
use record_core::geometry::{Rect, bars, letterbox, map_to_output, output_size};
use record_core::pacer::FramePacer;
use record_core::testpattern::{QUADRANT_RGB, bar_width, expected_yuv, phase_of_bar, quadrants, strip, time_of_phase};
use std::time::Duration;

pub fn monitors_cmd() -> Result<()> {
    ensure_dpi_aware();
    let list = list_monitors()?;
    if list.is_empty() {
        bail!("没有找到接在桌面上的显示器");
    }
    for (i, m) in list.iter().enumerate() {
        println!("[{i}] {}", m.describe());
    }
    Ok(())
}

pub(crate) fn pick_from_args(a: &Args, list: &[MonitorInfo]) -> Result<MonitorInfo> {
    let pick = match a.get("monitor").unwrap_or("primary") {
        "primary" => Pick::Primary,
        "foreground" => Pick::Foreground,
        n => Pick::Index(n.parse().with_context(|| format!("--monitor 的值“{n}”应为 primary、foreground 或序号"))?),
    };
    pick_monitor(list, pick).cloned().with_context(|| format!("没有匹配的显示器（{pick:?}），现有 {} 块", list.len()))
}

pub(crate) fn parse_size(text: &str) -> Result<(u32, u32)> {
    let (w, h) = text.split_once(['x', 'X']).with_context(|| format!("--size 的值“{text}”应形如 960x540"))?;
    Ok((w.trim().parse().context("--size 的宽")?, h.trim().parse().context("--size 的高")?))
}

/// 测试窗口在采集帧里的位置，以及各取样区域。
struct Probe {
    win: Rect,
    quads_out: [Rect; 4],
    strip_out: Rect,
    /// 输出画面里没被源画面盖住的黑边（取样区，已向内收缩）。
    bars_out: Vec<Rect>,
    pacer: FramePacer,
    t_ref: Hns,
    animate: bool,
}

#[derive(Debug, Clone)]
struct Measurement {
    yuv: [[f64; 3]; 4],
    /// 各黑边取样区的平均 Y、U、V。
    bars: Vec<[f64; 3]>,
    /// 亮条左边缘（窗口坐标），没找到为 `None`。
    bar_left: Option<f64>,
    /// 名义时刻减内容绘制时刻：正数表示画面内容比帧时刻旧。
    stale_ms: Option<f64>,
}

impl Probe {
    fn new(mon: &MonitorInfo, win: Rect, out: (u32, u32), t_ref: Hns, t0: Hns, fps: u32, animate: bool) -> Self {
        let layout = letterbox(mon.width, mon.height, out.0, out.1);
        let to_out = |r: Rect| map_to_output(r, mon.width, mon.height, layout);
        let at_window = |r: Rect| Rect { x: win.x + r.x, y: win.y + r.y, w: r.w, h: r.h };
        Self {
            win,
            quads_out: quadrants(win.w, win.h).map(|q| shrink(to_out(at_window(q)), 0.25)),
            strip_out: to_out(at_window(strip(win.w, win.h))),
            bars_out: bars(layout, out.0, out.1).into_iter().map(|r| shrink(r, 0.2)).collect(),
            pacer: FramePacer::new(fps, t0),
            t_ref,
            animate,
        }
    }

    fn measure(&self, f: &Nv12<'_>, index: u64) -> Measurement {
        let yuv = self.quads_out.map(|r| f.mean_yuv(r));
        let bars = self.bars_out.iter().map(|&r| f.mean_yuv(r)).collect();
        let mut m = Measurement { yuv, bars, bar_left: None, stale_ms: None };
        if self.animate
            && let Some((l, r)) = f.find_bright_run(self.strip_out, 128)
        {
            // 输出坐标 → 窗口坐标
            let scale = f64::from(self.strip_out.w) / f64::from(self.win.w);
            let center = ((l + r) as f64 / 2.0 - f64::from(self.strip_out.x)) / scale;
            let left = center - f64::from(bar_width(self.win.w)) / 2.0;
            let near = self.pacer.time_of(index);
            let drawn = time_of_phase(phase_of_bar(left, self.win.w), self.t_ref, near);
            m.bar_left = Some(left);
            m.stale_ms = Some((near - drawn) as f64 / 10_000.0);
        }
        m
    }
}

fn format_name(f: i32) -> &'static str {
    match f {
        87 => "B8G8R8A8_UNORM",
        88 => "B8G8R8X8_UNORM",
        28 => "R8G8B8A8_UNORM",
        24 => "R10G10B10A2_UNORM",
        10 => "R16G16B16A16_FLOAT",
        _ => "其它格式",
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[(((sorted.len() - 1) as f64) * p).round() as usize]
}

fn default_dir(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(name)
}

/// 排查用：对照“GDI 读到的屏幕实际颜色”和“桌面复制采到的颜色”，并把只含测试窗口的裁剪区域导出成 PNG。
/// 只有 GDI 确认测试窗口确实在预期位置时才导出，避免把用户屏幕上的其它内容写出来。
pub fn probe_cmd(a: &Args) -> Result<()> {
    ensure_dpi_aware();
    let fps: u32 = a.num("fps", 15)?;
    let (win_w, win_h) = parse_size(a.get("size").unwrap_or("960x540"))?;
    let list = list_monitors()?;
    let mon = pick_from_args(a, &list)?;
    let out = output_size(mon.width, mon.height, 1920, 1080);
    let dir = a.get("out").map(std::path::PathBuf::from).unwrap_or_else(|| default_dir("record-s2-probe"));
    std::fs::create_dir_all(&dir)?;

    let qpc = Qpc::new();
    let win = Rect { x: (mon.width - win_w) / 2, y: (mon.height - win_h) / 2, w: win_w, h: win_h };
    let window = TestWindow::open(TestWindowSpec { left: mon.left + win.x as i32, top: mon.top + win.y as i32, width: win_w, height: win_h, animate: false }, qpc).map_err(anyhow::Error::msg)?;
    std::thread::sleep(Duration::from_millis(500));

    let (actual, dpi) = window.actual_rect();
    println!("窗口：期望 ({}, {}) {}×{}；实际 ({}, {}) {}×{}；窗口 DPI {}（96 = 100%）", mon.left + win.x as i32, mon.top + win.y as i32, win_w, win_h, actual.0, actual.1, actual.2, actual.3, dpi);

    // GDI 参照：各分区中心的实际颜色
    let mut gdi_ok = true;
    for (q, rect) in quadrants(win_w, win_h).into_iter().enumerate() {
        let (cx, cy) = rect.center();
        let got = record_capture::testwin::screen_pixel(mon.left + (win.x + cx) as i32, mon.top + (win.y + cy) as i32);
        let want = QUADRANT_RGB[q];
        let ok = got.iter().zip(want).all(|(g, w)| (i32::from(*g) - i32::from(w)).abs() <= 2);
        gdi_ok &= ok;
        println!("  GDI 分区 {q}：期望 RGB{want:?}，读到 RGB{got:?} {}", if ok { "✔" } else { "✘" });
    }

    // 采几帧
    let t0 = qpc.now_hns();
    let (event_tx, event_rx) = unbounded();
    let last = std::sync::Arc::new(std::sync::Mutex::new(None::<std::sync::Arc<[u8]>>));
    let sink_last = std::sync::Arc::clone(&last);
    let source = VideoSource::start(
        VideoConfig { monitor: mon.clone(), fps, out_w: out.0, out_h: out.1, t0, rebuild_interval: Duration::from_secs(1) },
        qpc,
        event_tx,
        Box::new(move |_, data, kind| {
            if kind == FrameKind::Fresh {
                *sink_last.lock().unwrap() = Some(data);
            }
        }),
    )?;
    std::thread::sleep(Duration::from_millis(1500));
    let result = source.stop();
    window.close();
    for e in event_rx.try_iter() {
        println!("  事件：{e:?}");
    }
    println!("桌面复制纹理格式：{}", result.src_format.map_or("未取到".to_string(), |f| format!("{}（{f}）", format_name(f))));
    let frame = last.lock().unwrap().take().context("没有采到新画面")?;

    let probe = Probe::new(&mon, win, out, 0, t0, fps, false);
    let nv = Nv12::new(&frame, out.0, out.1);
    let m = probe.measure(&nv, 0);
    for (q, rgb) in QUADRANT_RGB.iter().enumerate() {
        let want = expected_yuv(*rgb);
        println!("  采集 分区 {q}：期望 Y{:.0} U{:.0} V{:.0}；实测 Y{:.1} U{:.1} V{:.1}", want[0], want[1], want[2], m.yuv[q][0], m.yuv[q][1], m.yuv[q][2]);
    }

    if !gdi_ok {
        println!("GDI 没有在预期位置读到测试窗口的颜色，不导出裁剪图（避免写出用户屏幕上的其它内容）。");
        return Ok(());
    }
    let layout = letterbox(mon.width, mon.height, out.0, out.1);
    let region = map_to_output(win, mon.width, mon.height, layout);
    let (crop, cw, ch) = nv.crop(region);
    let raw = dir.join("crop.nv12");
    let png = dir.join("crop.png");
    std::fs::write(&raw, &crop)?;
    let ffmpeg = record_mux::locate::find_ffmpeg()?;
    let status = std::process::Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "nv12", "-s", &format!("{cw}x{ch}"), "-i"])
        .arg(&raw)
        .args(["-frames:v", "1"])
        .arg(&png)
        .status()?;
    let _ = std::fs::remove_file(&raw);
    if !status.success() {
        bail!("ffmpeg 转 PNG 失败：{status}");
    }
    println!("裁剪图（仅测试窗口区域 {cw}×{ch}）：{}", png.display());
    Ok(())
}

pub fn selftest_cmd(a: &Args) -> Result<()> {
    ensure_dpi_aware();
    let secs: u64 = a.num("secs", 8)?;
    let fps: u32 = a.num("fps", 15)?;
    let (win_w, win_h) = parse_size(a.get("size").unwrap_or("960x540"))?;
    let animate = !a.flag("static");
    let tolerance: f64 = a.num("tolerance", 6.0)?;

    let list = list_monitors()?;
    let mon = pick_from_args(a, &list)?;
    // 默认按显示器比例算输出尺寸；`--out WxH` 指定一个别的比例，用来验证黑边布局
    let out = match a.get("out") {
        Some(text) => {
            let (w, h) = parse_size(text)?;
            if w < 2 || h < 2 || w % 2 != 0 || h % 2 != 0 {
                bail!("--out 的宽高必须是不小于 2 的偶数，得到 {w}x{h}");
            }
            (w, h)
        }
        None => output_size(mon.width, mon.height, 1920, 1080),
    };
    println!("显示器：{}", mon.describe());
    println!("输出：{}×{}，{} fps，测试窗口 {}×{}（{}），时长 {} 秒", out.0, out.1, fps, win_w, win_h, if animate { "亮条移动" } else { "静止" }, secs);
    if win_w > mon.width || win_h > mon.height {
        bail!("测试窗口 {win_w}×{win_h} 比显示器 {}×{} 还大", mon.width, mon.height);
    }

    let qpc = Qpc::new();
    let win = Rect { x: (mon.width - win_w) / 2, y: (mon.height - win_h) / 2, w: win_w, h: win_h };
    let window = TestWindow::open(TestWindowSpec { left: mon.left + win.x as i32, top: mon.top + win.y as i32, width: win_w, height: win_h, animate }, qpc)
        .map_err(anyhow::Error::msg)
        .context("创建测试窗口")?;
    std::thread::sleep(Duration::from_millis(400)); // 让窗口先画出来、桌面稳定下来

    let t0 = qpc.now_hns();
    let probe = Probe::new(&mon, win, out, window.t_ref, t0, fps, animate);
    let (frame_tx, frame_rx) = unbounded::<(u64, FrameKind, Option<std::sync::Arc<[u8]>>)>();
    let (event_tx, event_rx) = unbounded();
    let (out_w, out_h) = out;
    println!("\n── 事件（时刻相对采集启动）──");
    let started = std::time::Instant::now();
    let printer = std::thread::spawn(move || {
        for e in event_rx {
            println!("  [+{:.2}s] {e:?}", started.elapsed().as_secs_f64());
        }
    });
    let analyzer = std::thread::spawn(move || {
        let (mut seen, mut measures) = (Vec::new(), Vec::new());
        for (index, kind, data) in frame_rx {
            seen.push((index, kind));
            if let Some(data) = data {
                measures.push(probe.measure(&Nv12::new(&data, out_w, out_h), index));
            }
        }
        (seen, measures)
    });
    let source = VideoSource::start(
        VideoConfig { monitor: mon.clone(), fps, out_w: out.0, out_h: out.1, t0, rebuild_interval: Duration::from_secs(1) },
        qpc,
        event_tx,
        Box::new(move |index, data, kind| {
            let _ = frame_tx.send((index, kind, (kind == FrameKind::Fresh).then_some(data)));
        }),
    )
    .context("启动画面采集")?;

    std::thread::sleep(Duration::from_secs(secs));
    let result = source.stop();
    window.close();
    let (seen, measures) = analyzer.join().expect("分析线程");
    printer.join().expect("事件打印线程");

    let mut problems: Vec<String> = Vec::new();
    let st = &result.stats;
    println!("\n── 帧统计 ──");
    println!("桌面复制给出的纹理格式：{}", result.src_format.map_or("未取到".to_string(), |f| format!("{}（{f}）", format_name(f))));
    println!(
        "共交付 {} 帧：新画面 {}；无变化重复 {}；安全桌面 {}；重建中 {}；重建失败 {}；落后补帧 {}",
        seen.len(),
        st.fresh,
        st.repeat_no_change,
        st.repeat_secure_desktop,
        st.repeat_duplication_rebuild,
        st.repeat_duplication_failed,
        st.catch_up
    );
    println!(
        "首帧 {} ms；失效 {} 次；重建 {} 次；断开累计 {} ms；取帧耗时（含 GPU 转换与回读）p50 {:.1} ms，p95 {:.1} ms，p99 {:.1} ms，最大 {:.1} ms（{} 次）",
        result.first_frame_ms.map_or("无".to_string(), |v| v.to_string()),
        result.lost,
        result.rebuilds,
        result.down_ms,
        f64::from(result.grab.p50_us) / 1000.0,
        f64::from(result.grab.p95_us) / 1000.0,
        f64::from(result.grab.p99_us) / 1000.0,
        f64::from(result.grab.max_us) / 1000.0,
        result.grab.count
    );
    if seen.is_empty() {
        bail!("没有采集到任何帧");
    }

    // 帧序号必须连续，每个到期的帧恰好交付一次
    let (first, last) = (seen.first().unwrap().0, seen.last().unwrap().0);
    let ordered = seen.windows(2).all(|w| w[1].0 == w[0].0 + 1);
    println!("帧序号 {first}..={last}，{}", if ordered { "连续，无空缺无重复" } else { "不连续" });
    if !ordered {
        problems.push("帧序号不连续".into());
    }
    let expected = (secs * u64::from(fps)) as f64;
    if (seen.len() as f64) < expected * 0.9 {
        problems.push(format!("交付 {} 帧，少于期望的 {expected} 帧的 90%", seen.len()));
    }
    if animate && (st.fresh as f64) < seen.len() as f64 * 0.8 {
        problems.push(format!("亮条一直在动，新画面却只有 {}/{} 帧", st.fresh, seen.len()));
    }
    if result.first_frame_ms.is_none_or(|v| v > 1000) {
        problems.push(format!("首帧过慢或没有：{:?} ms", result.first_frame_ms));
    }
    if result.lost > 0 || result.down_ms > 0 {
        problems.push(format!("录制中管线断开过：失效 {} 次，累计 {} ms", result.lost, result.down_ms));
    }

    println!("\n── 颜色（BT.709 有限范围，容差 ±{tolerance}）──");
    if measures.is_empty() {
        problems.push("没有可分析的新画面".into());
    } else {
        let names = ["左上 红", "右上 绿", "左下 蓝", "右下 白"];
        let mut worst_all = 0.0f64;
        for q in 0..4 {
            let want = expected_yuv(QUADRANT_RGB[q]);
            let mut worst = [0.0f64; 3];
            for m in &measures {
                for c in 0..3 {
                    worst[c] = worst[c].max((m.yuv[q][c] - want[c]).abs());
                }
            }
            let got = &measures[measures.len() / 2].yuv[q];
            println!(
                "  {}：期望 Y{:.0} U{:.0} V{:.0}；实测 Y{:.1} U{:.1} V{:.1}；全部 {} 帧最大偏差 Y{:.1} U{:.1} V{:.1}",
                names[q],
                want[0],
                want[1],
                want[2],
                got[0],
                got[1],
                got[2],
                measures.len(),
                worst[0],
                worst[1],
                worst[2]
            );
            worst_all = worst_all.max(worst.into_iter().fold(0.0, f64::max));
        }
        if worst_all > tolerance {
            problems.push(format!("颜色最大偏差 {worst_all:.1} 超过容差 {tolerance}"));
        }
    }

    if let Some(first) = measures.first().filter(|m| !m.bars.is_empty()) {
        let black = expected_yuv([0, 0, 0]);
        println!("\n── 黑边（{} 块，期望纯黑 Y{:.0} U{:.0} V{:.0}）──", first.bars.len(), black[0], black[1], black[2]);
        let mut worst = 0.0f64;
        for m in &measures {
            for b in &m.bars {
                for c in 0..3 {
                    worst = worst.max((b[c] - black[c]).abs());
                }
            }
        }
        println!("  全部 {} 帧最大偏差 {worst:.1}", measures.len());
        if worst > tolerance {
            problems.push(format!("黑边不是纯黑，最大偏差 {worst:.1} 超过容差 {tolerance}"));
        }
    }

    if animate {
        println!("\n── 画面陈旧程度（帧名义时刻 − 内容绘制时刻）──");
        let mut stale: Vec<f64> = measures.iter().filter_map(|m| m.stale_ms).collect();
        let found = stale.len();
        if found == 0 {
            problems.push("没有在任何一帧里找到亮条".into());
        } else {
            stale.sort_by(f64::total_cmp);
            println!(
                "  {found}/{} 帧找到亮条：最小 {:.0} ms，p50 {:.0} ms，p95 {:.0} ms，最大 {:.0} ms",
                measures.len(),
                stale[0],
                percentile(&stale, 0.5),
                percentile(&stale, 0.95),
                stale[found - 1]
            );
            if (found as f64) < measures.len() as f64 * 0.9 {
                problems.push(format!("只有 {found}/{} 帧找到亮条", measures.len()));
            }
        }
    }

    if problems.is_empty() {
        println!("\n画面自检通过。");
        Ok(())
    } else {
        for p in &problems {
            println!("✘ {p}");
        }
        bail!("画面自检未通过（{} 项）", problems.len())
    }
}
