//! S3 同步 spike 的命令：闪屏加测试音的刺激，以及从录好的成品里测量画面与声音的偏移。
//!
//! 用法：先 `s3 stimulus` 在屏幕中央闪白（同时在默认播放设备上响一声 1 kHz 的短音），同时用录制引擎录；
//! 再对录出来的 `meeting.mp4` 跑 `s3 analyze`。刺激只在屏幕中央占一块区域，分析只看这块区域的平均亮度，
//! 不会去读用户屏幕上的其它内容。
//!
//! 声音写进播放设备后要过一段延迟才会出声（本机实测约 0.6 秒），回环采集给它打的是出声时刻；
//! 所以闪屏要等到测试音“出声”的那一刻才亮（时刻由播放流的设备位置推算），否则刺激自己就带着这段延迟。

use crate::args::Args;
use crate::s2::pick_from_args;
use anyhow::{Context, Result, bail};
use record_capture::devices::Role;
use record_capture::flashwin::FlashWindow;
use record_capture::monitor::{ensure_dpi_aware, list_monitors};
use record_capture::qpc::Qpc;
use record_capture::render::{PlayClock, RenderPlayer};
use record_core::syncprobe::{ToneLevels, estimate, flash_onsets, keep_consistent, pair_flashes, within};
use record_mux::locate::{ffprobe_for, find_ffmpeg};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const BEEP_RATE: u32 = 48_000;
/// 测试音长度与闪屏亮着的时间。
const BEEP_SECS: f64 = 0.15;
const FLASH_SECS: f64 = 0.3;
const NOT_YET: u64 = u64::MAX;

/// 睡到 QPC 时刻 `target`（100 ns）：大部分时间睡眠，最后 2 毫秒自旋，减少 Windows 默认定时器的抖动。
fn sleep_until_hns(qpc: &Qpc, target: i64) {
    loop {
        let left = target - qpc.now_hns();
        if left <= 0 {
            return;
        }
        if left > 20_000 {
            std::thread::sleep(Duration::from_nanos((left - 20_000) as u64 * 100));
        } else {
            std::hint::spin_loop();
        }
    }
}

/// 等渲染线程把测试音写进缓冲区，返回测试音第一帧的序号。
fn wait_for_beep(frame: &AtomicU64) -> Option<u64> {
    let until = Instant::now() + Duration::from_secs(1);
    while Instant::now() < until {
        let f = frame.load(Ordering::Acquire);
        if f != NOT_YET {
            return Some(f);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    None
}

pub fn stimulus_cmd(a: &Args) -> Result<()> {
    ensure_dpi_aware();
    let secs: f64 = a.num("secs", 60.0)?;
    // 默认间隔 2 + 1/120 秒：与 15 帧/秒的帧周期错开，让闪屏的相位均匀扫过帧格（分析时才能修正半帧的量化误差）
    let interval: f64 = a.num("interval", 2.0 + 1.0 / 120.0)?;
    let lead: f64 = a.num("lead", 3.0)?;
    let freq: f64 = a.num("freq", 1000.0)?;
    // 诊断用：测试音一写进缓冲区就闪屏（不等它出声），此时测出来的偏移就是播放设备的延迟
    let naive = a.flag("naive");
    if interval < FLASH_SECS + 0.5 + 0.7 {
        bail!("--interval 太短：至少要比闪屏时间 {FLASH_SECS} 秒多 1.2 秒（出声延迟可达 0.6 秒以上）");
    }
    let list = list_monitors()?;
    let mon = pick_from_args(a, &list)?;
    // 占显示器宽高的一半，居中：分析取中央 30%，确保落在窗口内
    let (w, h) = ((mon.width / 2) & !1, (mon.height / 2) & !1);
    let window = FlashWindow::open(mon.left + ((mon.width - w) / 2) as i32, mon.top + ((mon.height - h) / 2) as i32, w, h).map_err(anyhow::Error::msg).context("创建闪屏窗口")?;

    // 测试音：主线程要求播放时置位，渲染线程在下一次填充缓冲区时从头播一声，并记下它第一帧的序号
    let request = Arc::new(AtomicU32::new(0));
    let beep_frame = Arc::new(AtomicU64::new(NOT_YET));
    let clock = Arc::new(PlayClock::new(BEEP_RATE));
    let beep_frames = (BEEP_SECS * f64::from(BEEP_RATE)) as u32;
    let fade = (0.002 * f64::from(BEEP_RATE)) as u32;
    let (req, first, gen_clock) = (Arc::clone(&request), Arc::clone(&beep_frame), Arc::clone(&clock));
    let (mut left, mut done) = (0u32, 0u32);
    let player = RenderPlayer::start_clocked(a.get("device").map(String::from), Role::Console, BEEP_RATE, 2, Arc::clone(&clock), move |buf| {
        let r = req.swap(0, Ordering::AcqRel);
        if r > 0 {
            (left, done) = (r, 0);
            first.store(gen_clock.written(), Ordering::Release);
        }
        for frame in buf.chunks_exact_mut(2) {
            if left == 0 {
                break;
            }
            let ramp = (done as f32 / fade as f32).min(1.0);
            let v = ramp * 0.15 * (2.0 * std::f64::consts::PI * freq * f64::from(done) / f64::from(BEEP_RATE)).sin() as f32;
            frame.fill(v);
            left -= 1;
            done += 1;
        }
    })
    .map_err(anyhow::Error::msg)
    .context("在播放设备上播放测试音")?;

    let qpc = Qpc::new();
    println!("刺激开始：{secs} 秒，每 {interval:.4} 秒响一声 {freq} Hz 的短音，并在它出声的那一刻闪白（第一次约在 {lead} 秒后）{}", if naive { "【--naive：写入即闪】" } else { "" });
    let begin = Instant::now();
    let mut count = 0;
    loop {
        let at = lead + f64::from(count) * interval;
        if at + FLASH_SECS + 1.2 > secs {
            break;
        }
        let ask_at = begin + Duration::from_secs_f64(at);
        while Instant::now() < ask_at {
            std::thread::sleep(Duration::from_millis(1));
        }
        beep_frame.store(NOT_YET, Ordering::Release);
        let asked = qpc.now_hns();
        request.store(beep_frames, Ordering::Release);
        let frame = wait_for_beep(&beep_frame).context("渲染线程没有取走测试音")?;
        let heard = clock.heard_at(frame).context("还没有读到播放设备的位置")?;
        let flash_at = if naive { qpc.now_hns() } else { heard };
        sleep_until_hns(&qpc, flash_at);
        window.set(true);
        println!("[{:7.3}] 第 {} 次：写入后 {:.0} ms 出声，闪白", begin.elapsed().as_secs_f64(), count + 1, (heard - asked) as f64 / 10_000.0);
        sleep_until_hns(&qpc, flash_at + (FLASH_SECS * 1e7) as i64);
        window.set(false);
        count += 1;
    }
    let end = begin + Duration::from_secs_f64(secs);
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    window.close();
    player.stop();
    println!("刺激结束，共 {count} 次");
    Ok(())
}

/// ffprobe 取视频流的帧率与起点，以及音频流的起点。
fn probe_streams(ffprobe: &Path, file: &Path) -> Result<(f64, f64, f64)> {
    let run = |stream: &str, entries: &str| -> Result<String> {
        let out = Command::new(ffprobe).args(["-v", "error", "-select_streams", stream, "-show_entries", entries, "-of", "default=nw=1"]).arg(file).output().context("启动 ffprobe")?;
        if !out.status.success() {
            bail!("ffprobe 失败：{}", String::from_utf8_lossy(&out.stderr));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let field = |text: &str, key: &str| -> Option<String> { text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')).map(|v| v.trim().to_string()) };
    let v = run("v:0", "stream=r_frame_rate,start_time")?;
    let fps = {
        let r = field(&v, "r_frame_rate").context("没有 r_frame_rate")?;
        let (n, d) = r.split_once('/').context("r_frame_rate 格式不对")?;
        n.parse::<f64>()? / d.parse::<f64>()?
    };
    let vstart = field(&v, "start_time").context("画面流没有 start_time")?.parse()?;
    let astart = field(&run("a:0", "stream=start_time")?, "start_time").context("音频流没有 start_time")?.parse()?;
    Ok((fps, vstart, astart))
}

/// 感兴趣区域：占整幅画面的比例 `x,y,w,h`。
fn parse_roi(text: &str) -> Result<[f64; 4]> {
    let v: Vec<f64> = text.split(',').map(|p| p.trim().parse::<f64>()).collect::<Result<_, _>>().with_context(|| format!("--roi 的值“{text}”应形如 0.35,0.35,0.3,0.3"))?;
    let [x, y, w, h]: [f64; 4] = v.try_into().map_err(|_| anyhow::anyhow!("--roi 要 4 个数：x,y,w,h"))?;
    if !(w > 0.0 && h > 0.0 && x >= 0.0 && y >= 0.0 && x + w <= 1.0 && y + h <= 1.0) {
        bail!("--roi 必须落在画面内：x,y 不小于 0，x+w、y+h 不大于 1");
    }
    Ok([x, y, w, h])
}

pub fn analyze_cmd(a: &Args) -> Result<()> {
    let file = a.pos.get(2).map(Path::new).context("用法：s3 analyze <meeting.mp4> [--roi 0.35,0.35,0.3,0.3] [--freq 1000] [--limit-ms 80] [--search 1.0] [--verbose]")?;
    let [rx, ry, rw, rh] = parse_roi(a.get("roi").unwrap_or("0.35,0.35,0.3,0.3"))?;
    let freq: f64 = a.num("freq", 1000.0)?;
    let limit_ms: f64 = a.num("limit-ms", 80.0)?;
    // 在每个闪屏前后各这么多秒里找测试音：要小于刺激间隔的一半，免得配到邻居
    let search: f64 = a.num("search", 1.0)?;
    let ffmpeg = find_ffmpeg()?;
    let (fps, vstart, astart) = probe_streams(&ffprobe_for(&ffmpeg), file)?;

    // 画面：只取中央区域的平均亮度，一帧一个字节
    let crop = format!("crop=iw*{rw}:ih*{rh}:iw*{rx}:ih*{ry},scale=1:1:flags=area,format=gray");
    let vout = Command::new(&ffmpeg).args(["-v", "error", "-i"]).arg(file).args(["-an", "-vf", &crop, "-f", "rawvideo", "-"]).output().context("启动 ffmpeg 取画面亮度")?;
    if !vout.status.success() {
        bail!("取画面亮度失败：{}", String::from_utf8_lossy(&vout.stderr));
    }
    // 声音：单声道 48 kHz 浮点
    let aout = Command::new(&ffmpeg).args(["-v", "error", "-i"]).arg(file).args(["-vn", "-ac", "1", "-ar", "48000", "-f", "f32le", "-"]).output().context("启动 ffmpeg 解码声音")?;
    if !aout.status.success() {
        bail!("解码声音失败：{}", String::from_utf8_lossy(&aout.stderr));
    }
    let samples: Vec<f32> = aout.stdout.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();

    let video = flash_onsets(&vout.stdout, fps, vstart);
    let tone = ToneLevels::new(&samples, 48_000, freq, astart);
    println!("画面：{} 帧，{fps} 帧/秒，起点 {vstart:.3} 秒；找到 {} 次闪屏", vout.stdout.len(), video.len());
    println!("声音：{:.1} 秒，起点 {astart:.3} 秒；在 {freq} Hz 处全程共 {} 次起音", samples.len() as f64 / 48_000.0, tone.onsets().len());
    let found = pair_flashes(&tone, &video, search);
    let (pairs, dropped) = keep_consistent(&found, 2.0 / fps);
    if a.flag("verbose") {
        println!("闪屏时刻（秒）：{}", video.iter().map(|t| format!("{t:.3}")).collect::<Vec<_>>().join(" "));
        println!("配上的对（画面−声音，ms）：{}", found.iter().map(|p| format!("{:.3}→{:.3}({:+.0})", p.video, p.audio, p.delta() * 1000.0)).collect::<Vec<_>>().join("  "));
    }
    if pairs.len() < 6 {
        bail!("只配上 {} 对（至少需要 6 对才能估计；配上 {} 对，去掉不一致的 {dropped} 对）。检查刺激是否在录制期间运行、系统音是否录到了测试音、闪屏是否在画面中央", pairs.len(), found.len());
    }
    println!("  序号  闪屏(秒)  测试音(秒)  画面−声音(ms)");
    for (i, p) in pairs.iter().enumerate() {
        println!("  {:>3}  {:>8.3}  {:>9.3}  {:>+10.1}", i + 1, p.video, p.audio, p.delta() * 1000.0);
    }
    let e = estimate(&pairs, fps).context("没有配对")?;
    println!(
        "配对 {} 对（另有 {dropped} 对与中位数相差超过两帧，已去掉）；平均差 {:+.1} ms；修正半帧（{:.1} ms）后的偏移 {:+.1} ms（±{:.1} ms）；差值范围 {:.1} ms（相位扫满时应接近一帧 {:.1} ms）",
        e.n,
        e.raw_mean * 1000.0,
        500.0 / fps,
        e.offset * 1000.0,
        e.uncertainty * 1000.0,
        e.spread * 1000.0,
        1000.0 / fps
    );
    println!("说明：负数表示画面比声音先到。声音按出声时刻放置，画面按屏幕上出现的时刻放置；闪屏窗口自己从绘制到被桌面复制抓到还有几毫秒。");
    if dropped * 4 > found.len() {
        bail!("结论：不可信（{} 对里有 {dropped} 对与其它的不一致，声音环境里有别的 {freq} Hz 声音，请在安静时重测）", found.len());
    }
    if within(&e, limit_ms / 1000.0) {
        println!("结论：通过（|偏移| + 两倍不确定度 ≤ {limit_ms} ms）");
        Ok(())
    } else {
        bail!("结论：未通过（偏移 {:+.1} ms，限值 {limit_ms} ms）", e.offset * 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_region_must_be_four_fractions_inside_the_picture() {
        assert_eq!(parse_roi("0.35,0.35,0.3,0.3").unwrap(), [0.35, 0.35, 0.3, 0.3]);
        assert!(parse_roi("0.35,0.35,0.3").is_err());
        assert!(parse_roi("a,b,c,d").is_err());
        assert!(parse_roi("0.8,0.8,0.3,0.3").is_err(), "超出画面");
        assert!(parse_roi("0.1,0.1,0,0.3").is_err(), "宽度为 0");
    }
}
