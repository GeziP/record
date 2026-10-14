//! ffmpeg 命令行的构造（设计文档 5.6、5.7、5.8）。只拼参数，不启动进程。
//!
//! - 写入：每轨一个进程，从 stdin 读原始数据，写 Matroska；`-flush_packets 1` 让崩溃时丢得最少。
//! - 收尾：视频直接拷贝，两条音轨按起点偏移对齐（`adelay`，精确到采样），等增益相加并限幅，编码为 AAC。
//!   不用 `-itsoffset` 对齐音频：`amix` 按到达顺序混合，不看时间戳，起点晚的音轨会被提前混进去。
//! - 修复：拷贝式重封装，重新生成索引。

use crate::clock::HNS_PER_SEC;
use crate::config::Encoder;
use crate::session::{MEETING_FILE, SessionMeta, TrackKind, Tracks};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ArgsError {
    #[error("编码器还没有确定（\"auto\" 需要先探测）")]
    EncoderNotResolved,
    #[error("没有任何可用的轨道可以生成 {MEETING_FILE}")]
    NothingToMix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoSpec {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub encoder: Encoder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioSpec {
    pub rate: u32,
    pub channels: u16,
}

const LOG: [&str; 5] = ["-hide_banner", "-nostats", "-loglevel", "warning", "-y"];

fn os(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

fn encoder_args(enc: Encoder, fps: u32) -> Result<Vec<OsString>, ArgsError> {
    let gop = (fps * 2).to_string();
    let mut a = match enc {
        Encoder::Auto => return Err(ArgsError::EncoderNotResolved),
        Encoder::Nvenc => os(&["-c:v", "h264_nvenc", "-preset", "p4", "-rc", "vbr", "-cq", "28", "-b:v", "0", "-bf", "0"]),
        Encoder::Qsv => os(&["-c:v", "h264_qsv", "-preset", "medium", "-global_quality", "28", "-look_ahead", "0", "-bf", "0"]),
        Encoder::Amf => os(&["-c:v", "h264_amf", "-quality", "balanced", "-rc", "cqp", "-qp_i", "28", "-qp_p", "28", "-bf", "0"]),
        Encoder::Software => os(&["-c:v", "libopenh264", "-b:v", "2M", "-pix_fmt", "yuv420p"]),
    };
    a.push("-g".into());
    a.push(gop.into());
    Ok(a)
}

/// 画面写入：stdin 收 NV12 原始帧，恒定帧率，输出标注为 BT.709 有限范围（与 GPU 转换的色彩空间一致）。
pub fn video_writer_args(spec: &VideoSpec, out: &Path) -> Result<Vec<OsString>, ArgsError> {
    let mut a = os(&LOG);
    a.extend(os(&["-f", "rawvideo", "-pix_fmt", "nv12", "-s"]));
    a.push(format!("{}x{}", spec.width, spec.height).into());
    a.extend(os(&["-framerate"]));
    a.push(spec.fps.to_string().into());
    a.extend(os(&["-i", "pipe:0", "-an"]));
    a.extend(encoder_args(spec.encoder, spec.fps)?);
    a.extend(os(&["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv"]));
    a.extend(os(&["-fps_mode", "passthrough"]));
    a.extend(os(&MKV_CRASH_SAFE));
    a.push(out.as_os_str().to_owned());
    Ok(a)
}

/// Matroska 的 cluster 默认攒到 5 秒或 5 MB 才写盘，`-flush_packets` 冲不掉它；
/// 把 cluster 限制在 1 秒内，ffmpeg 被杀或断电时最多丢最后约 1 秒（实测见 record-mux 的集成测试）。
const MKV_CRASH_SAFE: [&str; 8] = ["-cluster_time_limit", "1000", "-cluster_size_limit", "1048576", "-flush_packets", "1", "-f", "matroska"];

/// 音轨写入：stdin 收交错 f32，FLAC 无损（24 位）。
pub fn audio_writer_args(spec: &AudioSpec, out: &Path) -> Vec<OsString> {
    let mut a = os(&LOG);
    a.extend(os(&["-f", "f32le", "-ar"]));
    a.push(spec.rate.to_string().into());
    a.push("-ac".into());
    a.push(spec.channels.to_string().into());
    a.extend(os(&["-i", "pipe:0", "-vn", "-c:a", "flac", "-sample_fmt", "s32", "-bits_per_raw_sample", "24"]));
    a.extend(os(&MKV_CRASH_SAFE));
    a.push(out.as_os_str().to_owned());
    a
}

/// 探测编码器是否真能用：编一帧纯黑画面丢进空输出。
pub fn probe_args(enc: Encoder) -> Result<Vec<OsString>, ArgsError> {
    let mut a = os(&["-hide_banner", "-nostats", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=black:s=256x256:r=15", "-frames:v", "1"]);
    a.extend(encoder_args(enc, 15)?);
    a.extend(os(&["-f", "null", "-"]));
    Ok(a)
}

/// 数一个画面文件里有多少帧：拷贝到空输出，`-progress` 在结束时给出 `frame=N`。
/// 不用 ffprobe：安装包里只带 ffmpeg 一个可执行文件。
pub fn count_frames_args(file: &Path) -> Vec<OsString> {
    let mut a = os(&["-hide_banner", "-nostats", "-nostdin", "-loglevel", "error", "-i"]);
    a.push(file.as_os_str().to_owned());
    a.extend(os(&["-map", "0:v:0", "-c", "copy", "-f", "null", "-", "-progress", "pipe:2"]));
    a
}

/// 从 `-progress` 的输出里取最后一个 `frame=N`。
pub fn parse_progress_frames(text: &str) -> Option<u64> {
    text.lines().rev().find_map(|l| l.trim().strip_prefix("frame=")?.trim().parse().ok())
}

/// 崩溃后的拷贝式修复：读到哪算哪，重新写出完整的容器与索引。
pub fn repair_args(src: &Path, dst: &Path) -> Vec<OsString> {
    let mut a = os(&LOG);
    a.push("-i".into());
    a.push(src.as_os_str().to_owned());
    a.extend(os(&["-c", "copy", "-f", "matroska"]));
    a.push(dst.as_os_str().to_owned());
    a
}

/// 编码器重启过的轨道里的一段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanSegment {
    pub file: String,
    /// 相对最早起点的偏移（100 ns）。
    pub offset_hns: i64,
    /// 画面轨：本段的帧数，拼接时用它给出精确时长；最后一段为空。
    pub frames: Option<u64>,
}

/// 画面轨分段拼接时用的清单文件名（写在会话目录里）。
pub const VIDEO_CONCAT_FILE: &str = "video.concat.txt";

/// 收尾时每轨的起点偏移（100 ns），`None` 表示没有这一轨（或文件不存在）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FinalizePlan {
    pub video: Option<i64>,
    pub mic: Option<i64>,
    pub system: Option<i64>,
    /// 成品时长上限（100 ns），`None` 不裁剪。回环音轨在停止之后还会拖出约一个设备延迟的尾巴
    /// （S1 实测 10–560 ms，设备时间戳是“播放时刻”），按停止时刻裁掉，各轨才一样长。
    pub duration_hns: Option<i64>,
    /// 编码器重启过的轨道，各段按先后顺序。没有这一项的轨道只有默认文件名那一个文件。
    pub segments: BTreeMap<TrackKind, Vec<PlanSegment>>,
}

impl FinalizePlan {
    /// 按 `session.json` 里的起点计算偏移；`present` 判断该轨文件是否存在。不裁剪时长。
    pub fn from_tracks(tracks: &Tracks, present: impl Fn(TrackKind) -> bool) -> Self {
        let off = tracks.offsets_hns();
        let pick = |k: TrackKind| if present(k) { Some(off.get(&k).copied().unwrap_or(0)) } else { None };
        Self { video: pick(TrackKind::Video), mic: pick(TrackKind::Mic), system: pick(TrackKind::System), ..Default::default() }
    }

    /// 在 [`FinalizePlan::from_tracks`] 之上：记录了停止时刻的会话再按“停止时刻 − 最早起点”裁剪时长（崩溃的会话没有停止时刻，不裁剪）；
    /// 编码器重启过的轨道带上各段及其起点。调用方要先去掉空文件的段。
    pub fn from_meta(meta: &SessionMeta, present: impl Fn(TrackKind) -> bool) -> Self {
        let mut plan = Self::from_tracks(&meta.tracks, &present);
        let earliest = TrackKind::ALL.iter().filter(|&&k| present(k)).filter_map(|&k| meta.tracks.get(k).and_then(|t| t.t0_hns)).min();
        plan.duration_hns = meta.stopped_hns.zip(earliest).map(|(stop, t0)| (stop - t0).max(0));
        for kind in TrackKind::ALL {
            let Some(track) = meta.tracks.get(kind).filter(|t| present(kind) && !t.segments.is_empty()) else { continue };
            let base = earliest.unwrap_or(0);
            let segs = track.segments.iter().map(|s| PlanSegment { file: s.file.clone(), offset_hns: s.t0_hns - base, frames: s.frames }).collect();
            plan.segments.insert(kind, segs);
        }
        plan
    }
}

/// 临时输出名：生成完成后由调用方改名为 `meeting.mp4`，半成品不会被当成成品。
pub fn meeting_part_path(dir: &Path) -> PathBuf {
    dir.join(format!("{MEETING_FILE}.part"))
}

fn seconds(hns: i64) -> String {
    format!("{}.{:07}", hns / HNS_PER_SEC, hns % HNS_PER_SEC)
}

fn delay_samples(offset_hns: i64, rate: u32) -> i64 {
    ((i128::from(offset_hns) * i128::from(rate) + i128::from(HNS_PER_SEC) / 2) / i128::from(HNS_PER_SEC)) as i64
}

/// 生成 `meeting.mp4` 的命令。麦克风是单声道，复制到左右两路；系统音是立体声。
pub fn finalize_args(dir: &Path, plan: &FinalizePlan, audio_rate: u32) -> Result<Vec<OsString>, ArgsError> {
    if plan.video.is_none() && plan.mic.is_none() && plan.system.is_none() {
        return Err(ArgsError::NothingToMix);
    }
    let mut a = os(&LOG);
    let mut next_input = 0usize;
    let mut video_input = None;
    if let Some(off) = plan.video {
        a.push("-itsoffset".into());
        a.push(seconds(off).into());
        match plan.segments.get(&TrackKind::Video) {
            // 编码器重启过：各段用 concat 解复用器首尾相接（拷贝，不重新编码）
            Some(segs) if segs.len() > 1 => {
                a.extend(os(&["-f", "concat", "-safe", "0", "-i"]));
                a.push(dir.join(VIDEO_CONCAT_FILE).into_os_string());
            }
            // 只剩一段有内容（另一段是空的）：直接读那一个文件，它不一定叫 video.mkv
            Some(segs) if !segs.is_empty() => {
                a.push("-i".into());
                a.push(dir.join(&segs[0].file).into_os_string());
            }
            _ => {
                a.push("-i".into());
                a.push(dir.join(TrackKind::Video.file_name()).into_os_string());
            }
        }
        video_input = Some(next_input);
        next_input += 1;
    }
    // (输入序号, 是否单声道, 延迟采样数)。重启过的音轨每一段各算一路输入，各自按起点延迟，之后一起混：段与段之间不重叠，混完就是接起来。
    let mut audio: Vec<(usize, bool, i64)> = Vec::new();
    for (kind, off, mono) in [(TrackKind::Mic, plan.mic, true), (TrackKind::System, plan.system, false)] {
        let Some(off) = off else { continue };
        let inputs: Vec<(PathBuf, i64)> = match plan.segments.get(&kind) {
            Some(segs) if !segs.is_empty() => segs.iter().map(|s| (dir.join(&s.file), s.offset_hns)).collect(),
            _ => vec![(dir.join(kind.file_name()), off)],
        };
        for (path, offset) in inputs {
            a.push("-i".into());
            a.push(path.into_os_string());
            audio.push((next_input, mono, delay_samples(offset, audio_rate)));
            next_input += 1;
        }
    }

    let mut graph = String::new();
    for (n, &(input, mono, delay)) in audio.iter().enumerate() {
        let mut chain = format!("[{input}:a]");
        let mut filters: Vec<String> = Vec::new();
        if mono {
            filters.push("pan=stereo|c0=c0|c1=c0".into());
        }
        if delay > 0 {
            filters.push(format!("adelay={delay}S:all=1"));
        }
        if filters.is_empty() {
            filters.push("anull".into());
        }
        chain.push_str(&filters.join(","));
        chain.push_str(&format!("[a{n}];"));
        graph.push_str(&chain);
    }
    match audio.len() {
        0 => {}
        1 => graph.push_str("[a0]anull[a]"),
        n => {
            let labels: String = (0..n).map(|i| format!("[a{i}]")).collect();
            graph.push_str(&format!("{labels}amix=inputs={n}:normalize=0,alimiter=limit=0.95[a]"));
        }
    }

    if !audio.is_empty() {
        a.push("-filter_complex".into());
        a.push(graph.into());
    }
    if let Some(v) = video_input {
        a.push("-map".into());
        a.push(format!("{v}:v").into());
    }
    if !audio.is_empty() {
        a.push("-map".into());
        a.push("[a]".into());
    }
    if video_input.is_some() {
        a.extend(os(&["-c:v", "copy"]));
    }
    if !audio.is_empty() {
        a.extend(os(&["-c:a", "aac", "-b:a", "128k"]));
    }
    if let Some(d) = plan.duration_hns {
        a.push("-t".into());
        a.push(seconds(d).into());
    }
    a.extend(os(&["-movflags", "+faststart", "-f", "mp4"]));
    a.push(meeting_part_path(dir).into_os_string());
    Ok(a)
}

/// 画面轨分段拼接的 ffconcat 清单：除最后一段外都写明精确时长（帧数 ÷ 帧率），
/// 后一段才会正好接在前一段的第 N 帧之后，不依赖被强杀的那一段容器里估出来的时长。
pub fn concat_list(segs: &[PlanSegment], fps: u32) -> String {
    let mut text = String::from("ffconcat version 1.0\n");
    for (i, seg) in segs.iter().enumerate() {
        text.push_str(&format!("file '{}'\n", seg.file));
        if let Some(frames) = seg.frames.filter(|_| i + 1 < segs.len()) {
            text.push_str(&format!("duration {:.7}\n", frames as f64 / f64::from(fps)));
        }
    }
    text
}

/// 日志里打印命令用。
pub fn display(args: &[OsString]) -> String {
    args.iter()
        .map(|a| {
            let s = a.to_string_lossy();
            if s.contains(' ') || s.contains('[') || s.contains('|') { format!("\"{s}\"") } else { s.into_owned() }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::TrackMeta;

    fn s(args: &[OsString]) -> Vec<String> {
        args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    fn has_seq(args: &[String], seq: &[&str]) -> bool {
        args.windows(seq.len()).any(|w| w.iter().zip(seq).all(|(a, b)| a == b))
    }

    fn vspec(enc: Encoder) -> VideoSpec {
        VideoSpec { width: 1920, height: 1080, fps: 15, encoder: enc }
    }

    #[test]
    fn nvenc_video_writer_reads_nv12_from_stdin_and_tags_bt709() {
        let a = s(&video_writer_args(&vspec(Encoder::Nvenc), Path::new("video.mkv")).unwrap());
        assert!(has_seq(&a, &["-f", "rawvideo", "-pix_fmt", "nv12", "-s", "1920x1080", "-framerate", "15", "-i", "pipe:0"]), "{a:?}");
        assert!(has_seq(&a, &["-c:v", "h264_nvenc", "-preset", "p4", "-rc", "vbr", "-cq", "28"]));
        assert!(has_seq(&a, &["-bf", "0", "-g", "30"]));
        assert!(has_seq(&a, &["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv"]));
        assert!(has_seq(&a, &["-fps_mode", "passthrough"]));
        assert!(has_seq(&a, &["-cluster_time_limit", "1000", "-cluster_size_limit", "1048576", "-flush_packets", "1", "-f", "matroska", "video.mkv"]));
        assert!(a.contains(&"-an".to_string()));
    }

    #[test]
    fn every_encoder_has_its_own_codec_and_gop() {
        for (enc, codec) in [(Encoder::Nvenc, "h264_nvenc"), (Encoder::Qsv, "h264_qsv"), (Encoder::Amf, "h264_amf"), (Encoder::Software, "libopenh264")] {
            let a = s(&video_writer_args(&VideoSpec { fps: 30, ..vspec(enc) }, Path::new("v.mkv")).unwrap());
            assert!(has_seq(&a, &["-c:v", codec]), "{enc:?}");
            assert!(has_seq(&a, &["-g", "60"]), "{enc:?} 的关键帧间隔应为 2 秒");
        }
        let sw = s(&video_writer_args(&vspec(Encoder::Software), Path::new("v.mkv")).unwrap());
        assert!(has_seq(&sw, &["-pix_fmt", "yuv420p"]), "软件编码需要 yuv420p");
    }

    #[test]
    fn auto_encoder_must_be_resolved_first() {
        assert_eq!(video_writer_args(&vspec(Encoder::Auto), Path::new("v.mkv")).unwrap_err(), ArgsError::EncoderNotResolved);
    }

    #[test]
    fn audio_writer_is_lossless_flac_in_matroska() {
        let a = s(&audio_writer_args(&AudioSpec { rate: 48_000, channels: 2 }, Path::new("system.mka")));
        assert!(has_seq(&a, &["-f", "f32le", "-ar", "48000", "-ac", "2", "-i", "pipe:0"]));
        assert!(has_seq(&a, &["-c:a", "flac", "-sample_fmt", "s32", "-bits_per_raw_sample", "24"]));
        assert!(has_seq(&a, &["-cluster_time_limit", "1000", "-cluster_size_limit", "1048576", "-flush_packets", "1", "-f", "matroska", "system.mka"]), "{a:?}");
    }

    #[test]
    fn counting_frames_copies_the_video_stream_to_a_null_output_and_reports_progress() {
        let a = s(&count_frames_args(Path::new("v.mkv")));
        assert!(has_seq(&a, &["-i", "v.mkv", "-map", "0:v:0", "-c", "copy", "-f", "null", "-", "-progress", "pipe:2"]), "{a:?}");
    }

    #[test]
    fn the_last_progress_block_wins() {
        let text = "frame=10\nfps=0.0\nprogress=continue\nframe=150\nfps=0.0\ntotal_size=N/A\nprogress=end\n";
        assert_eq!(parse_progress_frames(text), Some(150));
        assert_eq!(parse_progress_frames("frame=  7 \n"), Some(7));
        assert_eq!(parse_progress_frames("没有进度输出"), None);
        assert_eq!(parse_progress_frames("frame=N/A\n"), None);
    }

    #[test]
    fn repair_copies_streams_without_re_encoding() {
        let a = s(&repair_args(Path::new("a.mkv"), Path::new("a.mkv.repair")));
        assert!(has_seq(&a, &["-i", "a.mkv", "-c", "copy", "-f", "matroska", "a.mkv.repair"]));
    }

    fn meta(t0: i64) -> TrackMeta {
        TrackMeta::new("", Some(t0))
    }

    #[test]
    fn plan_uses_offsets_from_the_earliest_track_and_skips_missing_files() {
        let mut t = Tracks::default();
        t.set(TrackKind::Video, meta(10_000_500));
        t.set(TrackKind::Mic, meta(10_000_000));
        t.set(TrackKind::System, meta(10_020_000));
        let p = FinalizePlan::from_tracks(&t, |_| true);
        assert_eq!(p, FinalizePlan { video: Some(500), mic: Some(0), system: Some(20_000), ..Default::default() });
        let p = FinalizePlan::from_tracks(&t, |k| k != TrackKind::Mic);
        assert_eq!((p.video, p.mic, p.system), (Some(500), None, Some(20_000)));
    }

    #[test]
    fn full_mix_aligns_with_adelay_and_mixes_with_a_limiter() {
        let plan = FinalizePlan { video: Some(0), mic: Some(0), system: Some(25_000_000), ..Default::default() }; // 系统音晚 2.5 秒
        let a = s(&finalize_args(Path::new("D"), &plan, 48_000).unwrap());
        let graph = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert_eq!(graph, "[1:a]pan=stereo|c0=c0|c1=c0[a0];[2:a]adelay=120000S:all=1[a1];[a0][a1]amix=inputs=2:normalize=0,alimiter=limit=0.95[a]");
        assert!(
            has_seq(&a, &["-itsoffset", "0.0000000", "-i", "D\\video.mkv", "-i", "D\\mic.mka", "-i", "D\\system.mka"])
                || has_seq(&a, &["-itsoffset", "0.0000000", "-i", "D/video.mkv", "-i", "D/mic.mka", "-i", "D/system.mka"]),
            "{a:?}"
        );
        assert!(has_seq(&a, &["-map", "0:v", "-map", "[a]", "-c:v", "copy", "-c:a", "aac", "-b:a", "128k"]));
        assert!(has_seq(&a, &["-movflags", "+faststart", "-f", "mp4"]));
        assert!(a.last().unwrap().ends_with("meeting.mp4.part"));
    }

    #[test]
    fn video_offset_uses_seven_decimal_seconds() {
        let plan = FinalizePlan { video: Some(12_345_678), mic: Some(0), system: None, ..Default::default() };
        let a = s(&finalize_args(Path::new("D"), &plan, 48_000).unwrap());
        assert!(has_seq(&a, &["-itsoffset", "1.2345678"]));
    }

    #[test]
    fn a_single_audio_track_is_not_mixed() {
        let plan = FinalizePlan { video: Some(0), mic: None, system: Some(0), ..Default::default() };
        let a = s(&finalize_args(Path::new("D"), &plan, 48_000).unwrap());
        let graph = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert_eq!(graph, "[1:a]anull[a0];[a0]anull[a]");
        assert!(!graph.contains("amix"));
    }

    #[test]
    fn video_only_has_no_audio_codec_or_filter() {
        let plan = FinalizePlan { video: Some(0), mic: None, system: None, ..Default::default() };
        let a = s(&finalize_args(Path::new("D"), &plan, 48_000).unwrap());
        assert!(!a.contains(&"-filter_complex".to_string()) && !a.contains(&"-c:a".to_string()));
        assert!(has_seq(&a, &["-map", "0:v", "-c:v", "copy"]));
    }

    #[test]
    fn audio_only_still_produces_a_playable_file() {
        let plan = FinalizePlan { video: None, mic: Some(0), system: Some(0), ..Default::default() };
        let a = s(&finalize_args(Path::new("D"), &plan, 48_000).unwrap());
        assert!(!a.contains(&"-c:v".to_string()));
        let graph = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert!(graph.starts_with("[0:a]pan=stereo"), "{graph}");
    }

    #[test]
    fn the_duration_cap_goes_before_the_output_options() {
        let plan = FinalizePlan { video: Some(0), mic: Some(0), system: Some(0), duration_hns: Some(36_005_000_000), ..Default::default() };
        let a = s(&finalize_args(Path::new("D"), &plan, 48_000).unwrap());
        assert!(has_seq(&a, &["-t", "3600.5000000", "-movflags", "+faststart"]), "{a:?}");
        let uncapped = s(&finalize_args(Path::new("D"), &FinalizePlan { video: Some(0), ..Default::default() }, 48_000).unwrap());
        assert!(!uncapped.contains(&"-t".to_string()));
    }

    fn session_with(stopped: Option<i64>, starts: [(TrackKind, Option<i64>); 3]) -> SessionMeta {
        let mut m = SessionMeta::new("x", "0", chrono::Utc::now(), 10_000_000);
        for (k, t0) in starts {
            m.tracks.set(k, TrackMeta::new(k.file_name(), t0));
        }
        m.stopped_hns = stopped;
        m
    }

    #[test]
    fn duration_is_the_stop_time_minus_the_earliest_start() {
        let m = session_with(Some(500_000_000), [(TrackKind::Video, Some(100_000_000)), (TrackKind::Mic, Some(100_300_000)), (TrackKind::System, Some(100_500_000))]);
        let p = FinalizePlan::from_meta(&m, |_| true);
        assert_eq!(p.duration_hns, Some(400_000_000));
        assert_eq!((p.video, p.mic, p.system), (Some(0), Some(300_000), Some(500_000)));
    }

    #[test]
    fn a_missing_track_file_does_not_move_the_origin() {
        // 视频文件不在：起点以仍然存在的最早一轨为准
        let m = session_with(Some(500_000_000), [(TrackKind::Video, Some(100_000_000)), (TrackKind::Mic, Some(120_000_000)), (TrackKind::System, Some(130_000_000))]);
        let p = FinalizePlan::from_meta(&m, |k| k != TrackKind::Video);
        assert_eq!(p.duration_hns, Some(380_000_000));
    }

    #[test]
    fn a_crashed_session_has_no_stop_time_and_is_not_trimmed() {
        let m = session_with(None, [(TrackKind::Video, Some(1)), (TrackKind::Mic, Some(2)), (TrackKind::System, None)]);
        assert_eq!(FinalizePlan::from_meta(&m, |_| true).duration_hns, None);
    }

    #[test]
    fn a_stop_time_before_the_start_never_goes_negative() {
        let m = session_with(Some(5), [(TrackKind::Video, Some(100)), (TrackKind::Mic, None), (TrackKind::System, None)]);
        assert_eq!(FinalizePlan::from_meta(&m, |_| true).duration_hns, Some(0));
    }

    fn seg(file: &str, offset_hns: i64, frames: Option<u64>) -> PlanSegment {
        PlanSegment { file: file.into(), offset_hns, frames }
    }

    #[test]
    fn segment_file_names_count_from_one() {
        assert_eq!(TrackKind::Video.segment_file_name(1), "video.mkv");
        assert_eq!(TrackKind::Video.segment_file_name(2), "video.2.mkv");
        assert_eq!(TrackKind::Mic.segment_file_name(3), "mic.3.mka");
        assert_eq!(TrackKind::System.segment_file_name(0), "system.mka");
    }

    #[test]
    fn a_restarted_track_carries_its_segments_into_the_plan_relative_to_the_earliest_start() {
        let mut m = session_with(Some(900_000_000), [(TrackKind::Video, Some(100_000_000)), (TrackKind::Mic, Some(100_200_000)), (TrackKind::System, None)]);
        let mic = m.tracks.mic.as_mut().unwrap();
        mic.segments = vec![
            crate::session::SegmentMeta { file: "mic.mka".into(), t0_hns: 100_200_000, frames: None },
            crate::session::SegmentMeta { file: "mic.2.mka".into(), t0_hns: 400_000_000, frames: None },
        ];
        let p = FinalizePlan::from_meta(&m, |k| k != TrackKind::System);
        assert_eq!(p.segments.get(&TrackKind::Mic), Some(&vec![seg("mic.mka", 200_000, None), seg("mic.2.mka", 300_000_000, None)]));
        assert!(!p.segments.contains_key(&TrackKind::Video), "没有重启过的轨道不带分段");
    }

    #[test]
    fn video_segments_are_joined_with_the_concat_demuxer_and_keep_the_track_offset() {
        let mut plan = FinalizePlan { video: Some(5_000_000), mic: Some(0), ..Default::default() };
        plan.segments.insert(TrackKind::Video, vec![seg("video.mkv", 5_000_000, Some(150)), seg("video.2.mkv", 105_000_000, None)]);
        let a = s(&finalize_args(Path::new("D"), &plan, 48_000).unwrap());
        assert!(
            has_seq(&a, &["-itsoffset", "0.5000000", "-f", "concat", "-safe", "0", "-i", "D\\video.concat.txt"])
                || has_seq(&a, &["-itsoffset", "0.5000000", "-f", "concat", "-safe", "0", "-i", "D/video.concat.txt"]),
            "{a:?}"
        );
        assert!(has_seq(&a, &["-map", "0:v"]));
    }

    #[test]
    fn one_remaining_video_segment_is_read_directly_even_if_it_is_not_the_first_file() {
        let mut plan = FinalizePlan { video: Some(0), ..Default::default() };
        plan.segments.insert(TrackKind::Video, vec![seg("video.2.mkv", 0, None)]);
        let a = s(&finalize_args(Path::new("D"), &plan, 48_000).unwrap());
        assert!(a.iter().any(|x| x.ends_with("video.2.mkv")) && !a.contains(&"concat".to_string()), "{a:?}");
    }

    #[test]
    fn audio_segments_are_delayed_individually_and_mixed_with_the_other_track() {
        let mut plan = FinalizePlan { mic: Some(0), system: Some(0), ..Default::default() };
        plan.segments.insert(TrackKind::Mic, vec![seg("mic.mka", 0, None), seg("mic.2.mka", 20_000_000, None)]); // 第二段晚 2 秒
        let a = s(&finalize_args(Path::new("D"), &plan, 48_000).unwrap());
        let graph = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert_eq!(graph, "[0:a]pan=stereo|c0=c0|c1=c0[a0];[1:a]pan=stereo|c0=c0|c1=c0,adelay=96000S:all=1[a1];[2:a]anull[a2];[a0][a1][a2]amix=inputs=3:normalize=0,alimiter=limit=0.95[a]");
    }

    #[test]
    fn the_concat_list_gives_exact_durations_for_all_but_the_last_segment() {
        let text = concat_list(&[seg("video.mkv", 0, Some(150)), seg("video.2.mkv", 0, Some(30)), seg("video.3.mkv", 0, None)], 15);
        assert_eq!(text, "ffconcat version 1.0\nfile 'video.mkv'\nduration 10.0000000\nfile 'video.2.mkv'\nduration 2.0000000\nfile 'video.3.mkv'\n");
    }
    #[test]
    fn nothing_to_mix_is_an_error() {
        assert_eq!(finalize_args(Path::new("D"), &FinalizePlan::default(), 48_000).unwrap_err(), ArgsError::NothingToMix);
    }

    #[test]
    fn delay_rounds_to_the_nearest_sample() {
        assert_eq!(delay_samples(0, 48_000), 0);
        assert_eq!(delay_samples(10_000_000, 48_000), 48_000);
        assert_eq!(delay_samples(104, 48_000), 0); // 约 0.5 个采样以内
        assert_eq!(delay_samples(105, 48_000), 1);
    }

    #[test]
    fn display_quotes_arguments_with_spaces_or_filter_syntax() {
        let d = display(&os(&["-i", "C:\\a b\\x.mkv", "-map", "[a]"]));
        assert_eq!(d, "-i \"C:\\a b\\x.mkv\" -map \"[a]\"");
    }
}
