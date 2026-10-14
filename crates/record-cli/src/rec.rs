//! `record`、`recover`：用录制引擎做一场真实的录制，验证整条链路（S3，以及外壳接入之前的联调）。

use crate::args::Args;
use anyhow::{Context, Result, bail};
use crossbeam_channel::{RecvTimeoutError, unbounded};
use record_capture::monitor::{Pick, ensure_dpi_aware};
use record_core::config::{Config, Encoder, MonitorSel};
use record_core::session::{EndReason, TrackKind};
use record_engine::recorder::{StartParams, TrackSelect, pick_from_config, start};
use record_engine::{EngineEvent, Fault, recovery};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) fn parse_encoder(s: &str) -> Result<Encoder> {
    Ok(match s {
        "auto" => Encoder::Auto,
        "nvenc" => Encoder::Nvenc,
        "qsv" => Encoder::Qsv,
        "amf" => Encoder::Amf,
        "software" => Encoder::Software,
        other => bail!("--encoder 的值“{other}”应为 auto、nvenc、qsv、amf 或 software"),
    })
}

fn parse_track(s: &str) -> Result<TrackKind> {
    Ok(match s {
        "video" => TrackKind::Video,
        "mic" => TrackKind::Mic,
        "system" => TrackKind::System,
        other => bail!("轨道“{other}”应为 video、mic 或 system"),
    })
}

/// `stall:mic:10@5`（5 秒时让麦克风采集线程停 10 秒）、`kill:video@20`（20 秒时杀掉画面编码器），逗号分隔可写多条。
pub(crate) fn parse_inject(text: &str) -> Result<Vec<(Duration, Fault)>> {
    let mut out = Vec::new();
    for item in text.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (what, at) = item.split_once('@').with_context(|| format!("注入“{item}”缺少 @ 时刻"))?;
        let at = Duration::from_secs_f64(at.parse::<f64>().with_context(|| format!("注入“{item}”的时刻不是数字"))?);
        let parts: Vec<&str> = what.split(':').collect();
        let fault = match parts.as_slice() {
            ["stall", track, secs] => Fault::StallCapture { track: parse_track(track)?, secs: secs.parse().with_context(|| format!("注入“{item}”的秒数"))? },
            ["kill", track] => Fault::KillEncoder { track: parse_track(track)? },
            _ => bail!("注入“{item}”的格式应为 stall:轨道:秒@时刻 或 kill:轨道@时刻"),
        };
        out.push((at, fault));
    }
    out.sort_by_key(|(at, _)| *at);
    Ok(out)
}

fn parse_monitor(s: &str) -> Result<Pick> {
    let sel = match s.parse::<u32>() {
        Ok(n) => MonitorSel::Index(n),
        Err(_) => MonitorSel::Named(s.to_string()),
    };
    pick_from_config(&sel).map_err(anyhow::Error::msg)
}

fn stamp(since: Duration) -> String {
    format!("[{:02}:{:04.1}]", since.as_secs() / 60, since.as_secs_f64() % 60.0)
}

fn print_event(since: Duration, e: &EngineEvent) {
    match e {
        EngineEvent::Info(t) => println!("{} {t}", stamp(since)),
        EngineEvent::Alert(a) => println!("{} {} {}", stamp(since), if a.is_warning() { "!! 告警" } else { "-- 提示" }, a.describe()),
        EngineEvent::RequestStop(r) => println!("{} >> 引擎要求结束本场：{}", stamp(since), r.describe()),
    }
}

fn default_dir() -> PathBuf {
    std::env::temp_dir().join("record-sessions")
}

pub fn record_cmd(a: &Args) -> Result<()> {
    ensure_dpi_aware();
    let secs: f64 = a.num("secs", 30.0)?;
    let out_dir = a.get("out").map(PathBuf::from).unwrap_or_else(default_dir);
    let ffmpeg = record_mux::locate::find_ffmpeg()?;
    let mut config = match a.get("config") {
        Some(p) => Config::load(Path::new(p))?,
        None => Config::default(),
    };
    config.video.fps = a.num("fps", config.video.fps)?;
    let tracks = TrackSelect { video: !a.flag("no-video"), mic: !a.flag("no-mic"), system: !a.flag("no-system") };
    let wanted = parse_encoder(a.get("encoder").unwrap_or("auto"))?;
    let encoder = if tracks.video { record_mux::probe::resolve_encoder(&ffmpeg, wanted)? } else { wanted };
    let monitor = a.get("monitor").map(parse_monitor).transpose()?;
    let mut pending = a.get("inject").map(parse_inject).transpose()?.unwrap_or_default();

    let (tx, rx) = unbounded();
    let params = StartParams { config, ffmpeg, encoder, output_dir: out_dir.clone(), app_version: env!("CARGO_PKG_VERSION").into(), tracks, monitor };
    let t_call = Instant::now();
    let rec = start(params, tx)?;
    let start_ms = t_call.elapsed().as_millis();
    println!("已开始：{}（start 用时 {start_ms} ms，编码器 {encoder:?}，目录 {}）", rec.id(), rec.dir().display());

    let t0 = Instant::now();
    let mut reason = EndReason::User;
    let mut last_status = Instant::now();
    loop {
        let since = t0.elapsed();
        if since.as_secs_f64() >= secs {
            break;
        }
        while pending.first().is_some_and(|(at, _)| since >= *at) {
            let (_, fault) = pending.remove(0);
            match rec.inject(fault) {
                Ok(()) => println!("{} ** 注入：{fault:?}", stamp(since)),
                Err(e) => println!("{} ** 注入失败：{e}", stamp(since)),
            }
        }
        if last_status.elapsed() >= Duration::from_secs(10) {
            last_status = Instant::now();
            let st = rec.status();
            println!("{} 录制中：{:.0} 秒，已写入 {:.1} MB", stamp(since), st.elapsed.as_secs_f64(), st.bytes as f64 / 1e6);
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(EngineEvent::RequestStop(r)) => {
                print_event(since, &EngineEvent::RequestStop(r));
                reason = r;
                break;
            }
            Ok(e) => print_event(since, &e),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {}
        }
    }

    let t_stop = Instant::now();
    let outcome = rec.stop(reason);
    let stop_ms = t_stop.elapsed().as_millis();
    for e in rx.try_iter() {
        print_event(t0.elapsed(), &e);
    }
    println!("已保存：{}（{}）", outcome.dir.display(), outcome.reason.describe());
    println!(
        "录制时长 {:.2} 秒；stop 用时 {stop_ms} ms（采集源 {} ms，写入器 {} ms，成品 {} ms）",
        outcome.duration_secs, outcome.timings.sources_ms, outcome.timings.writers_ms, outcome.timings.finalize_ms
    );
    for kind in TrackKind::ALL {
        let Some(t) = outcome.meta.tracks.get(kind) else { continue };
        // 编码器重启过的轨道有好几段文件，每段单独列出
        for file in t.files() {
            if let Ok(m) = std::fs::metadata(outcome.dir.join(&file)) {
                println!("  {:<11} {:>8.2} MB  状态 {:?}  起点偏移 {:?} ms", file, m.len() as f64 / 1e6, t.status, outcome.meta.tracks.offsets_hns().get(&kind).map(|v| v / 10_000));
            }
        }
    }
    match &outcome.meeting {
        Some(p) => println!("  meeting.mp4 {:>8.2} MB  {}", std::fs::metadata(p)?.len() as f64 / 1e6, p.display()),
        None => println!("  没有生成 meeting.mp4"),
    }
    for w in &outcome.warnings {
        println!("  警告：{w}");
    }
    println!("{}", serde_json::to_string_pretty(&outcome.meta.diagnostics)?);
    Ok(())
}

pub fn recover_cmd(a: &Args) -> Result<()> {
    let ffmpeg = record_mux::locate::find_ffmpeg()?;
    let reports = match a.get("session") {
        Some(dir) => recovery::recover(&ffmpeg, Path::new(dir))?.into_iter().collect(),
        None => recovery::recover_all(&ffmpeg, &a.get("out").map(PathBuf::from).unwrap_or_else(default_dir)),
    };
    if reports.is_empty() {
        println!("没有需要恢复的会话");
    }
    for r in reports {
        println!("{}：修复 {:?}，失败 {:?}，成品 {:?}，其他错误 {:?}", r.dir.display(), r.repaired, r.repair_errors, r.meeting, r.errors);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injections_parse_in_time_order() {
        let v = parse_inject("kill:video@20, stall:mic:10@5").unwrap();
        assert_eq!(v, vec![(Duration::from_secs(5), Fault::StallCapture { track: TrackKind::Mic, secs: 10 }), (Duration::from_secs(20), Fault::KillEncoder { track: TrackKind::Video }),]);
    }

    #[test]
    fn bad_injections_are_rejected_with_a_reason() {
        for bad in ["kill:video", "stall:mic@5", "kill:speaker@1", "boom:video@1", "kill:video@x"] {
            assert!(parse_inject(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn monitor_arguments_are_one_based_like_the_config() {
        assert_eq!(parse_monitor("1").unwrap(), Pick::Index(0));
        assert_eq!(parse_monitor("primary").unwrap(), Pick::Primary);
        assert!(parse_monitor("0").is_err());
    }

    #[test]
    fn encoder_names_cover_the_documented_choices() {
        assert_eq!(parse_encoder("nvenc").unwrap(), Encoder::Nvenc);
        assert!(parse_encoder("x264").is_err());
    }
}
