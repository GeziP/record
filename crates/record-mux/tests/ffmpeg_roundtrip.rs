//! 用真实 ffmpeg 验证写入、收尾对齐、崩溃修复。每个测试自己建临时目录，互不影响。

mod common;

use common::*;
use record_core::config::Encoder;
use record_core::ffmpeg_args::{AudioSpec, FinalizePlan, PlanSegment, VIDEO_CONCAT_FILE, VideoSpec, concat_list};
use record_core::session::TrackKind;
use record_mux::finalize::{finalize_meeting, repair_track};
use record_mux::probe::{count_video_frames, resolve_encoder};
use record_mux::writer::{AudioWriter, Health, Pushed, VideoWriter};
use std::sync::Arc;
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(20);

fn push_audio(w: &AudioWriter, samples: &[f32], channels: usize) {
    for chunk in samples.chunks(480 * channels) {
        assert_eq!(w.push(chunk), Pushed::Queued);
    }
}

fn write_video(path: &std::path::Path, frames: &[(u64, Arc<[u8]>)], w: u32, h: u32) -> record_mux::writer::WriterReport {
    let spec = VideoSpec { width: w, height: h, fps: 15, encoder: encoder() };
    // 测试一次性灌入所有帧，队列要装得下；溢出行为另有专门的测试
    let vw = VideoWriter::spawn(&ffmpeg(), &spec, path, frames.len() + 1).expect("启动视频写入");
    for (i, d) in frames {
        assert_eq!(vw.submit(*i, Arc::clone(d)), Pushed::Queued);
    }
    vw.finish(T)
}

#[test]
fn audio_writer_produces_exact_lossless_flac() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("system.mka");
    let w = AudioWriter::spawn(&ffmpeg(), &AudioSpec { rate: RATE, channels: 2 }, &out, 10).unwrap();
    let tone = sine(440.0, 2, 3.0, 0.25);
    push_audio(&w, &tone, 2);
    let r = w.finish(T);
    assert_eq!((r.exit_code, r.killed, r.failed.clone()), (Some(0), false, None), "{r:?}");
    assert_eq!(r.bytes_written, (tone.len() * 4) as u64);
    assert!((probe_duration(&out) - 3.0).abs() < 0.05, "{}", probe_duration(&out));
    let pcm = decode_stereo(&out);
    assert_eq!(pcm.len(), tone.len(), "无损：采样数一致");
    let max_err = pcm.iter().zip(&tone).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(max_err < 1e-6, "24 位 FLAC 的量化误差应小于 1e-6，实际 {max_err}");
}

#[test]
fn video_writer_writes_every_frame_at_constant_rate() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("video.mkv");
    let frames: Vec<_> = (0..30).map(|i| (i as u64, Arc::from(nv12_frame(640, 360, i)))).collect();
    let r = write_video(&out, &frames, 640, 360);
    assert_eq!((r.exit_code, r.failed.clone()), (Some(0), None), "{r:?}");
    assert_eq!((r.gap_repeats, r.late_discarded, r.dropped_queue), (0, 0, 0));
    assert_eq!(probe_frames(&out), 30);
    assert!((probe_duration(&out) - 2.0).abs() < 0.1, "{}", probe_duration(&out));
}

#[test]
fn index_gaps_are_filled_by_repeating_the_previous_frame() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("video.mkv");
    let f = |i: usize| -> Arc<[u8]> { Arc::from(nv12_frame(640, 360, i)) };
    let frames = vec![(0, f(0)), (1, f(1)), (2, f(2)), (5, f(5)), (6, f(6))];
    let r = write_video(&out, &frames, 640, 360);
    assert_eq!((r.gap_repeats, r.exit_code), (2, Some(0)), "{r:?}");
    assert_eq!(probe_frames(&out), 7, "序号 0..=6 各占一帧，时间线不缩短");
}

#[test]
fn late_frames_are_discarded_and_counted() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("video.mkv");
    let f = |i: usize| -> Arc<[u8]> { Arc::from(nv12_frame(640, 360, i)) };
    let frames = vec![(0, f(0)), (1, f(1)), (2, f(2)), (3, f(3)), (2, f(2))];
    let r = write_video(&out, &frames, 640, 360);
    assert_eq!(r.late_discarded, 1, "{r:?}");
    assert_eq!(probe_frames(&out), 4);
}

#[test]
fn killing_ffmpeg_marks_the_writer_failed_and_later_data_is_counted_not_written() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("mic.mka");
    let w = AudioWriter::spawn(&ffmpeg(), &AudioSpec { rate: RATE, channels: 1 }, &out, 10).unwrap();
    let tone = sine(440.0, 1, 1.0, 0.25);
    push_audio(&w, &tone, 1);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(w.health(), Health::Running);
    w.kill_process();
    // 继续送数据：写入线程发现管道断了，之后的数据只计数
    let t = Instant::now();
    while w.health() == Health::Running {
        assert!(t.elapsed() < Duration::from_secs(5), "5 秒内应发现 ffmpeg 已死");
        let _ = w.push(&tone[..480]);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(matches!(w.health(), Health::Failed(_)));
    assert_eq!(w.push(&tone[..480]), Pushed::WriterFailed);
    let t = Instant::now();
    let r = w.finish(T);
    assert!(t.elapsed() < Duration::from_secs(3), "出错后收尾不应卡住");
    assert!(r.failed.is_some() && r.dropped_failed > 0, "{r:?}");
}

#[test]
fn audio_queue_overflow_drops_new_data_and_never_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("mic.mka");
    // 队列预算只有 1 秒；一次塞 3 秒，必然溢出
    let w = AudioWriter::spawn(&ffmpeg(), &AudioSpec { rate: RATE, channels: 1 }, &out, 1).unwrap();
    let big = sine(440.0, 1, 3.0, 0.1);
    let t = Instant::now();
    let first = w.push(&big[..RATE as usize]);
    let second = w.push(&big[RATE as usize..2 * RATE as usize]);
    assert!(t.elapsed() < Duration::from_millis(200), "push 不应阻塞");
    assert_eq!(first, Pushed::Queued);
    // 第一块可能已经被写入线程取走，也可能还在队列；第二块要么入队要么溢出，二者之一
    assert!(matches!(second, Pushed::Queued | Pushed::Overflow(_)));
    let r = w.finish(T);
    assert_eq!(r.exit_code, Some(0));
}

#[test]
fn finalize_aligns_tracks_by_their_start_offsets() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // 麦克风从 0 开始，系统音晚 1.0 秒，画面晚 0.5 秒；各录 5 秒
    let mic = AudioWriter::spawn(&ffmpeg(), &AudioSpec { rate: RATE, channels: 1 }, &d.join("mic.mka"), 10).unwrap();
    push_audio(&mic, &sine(440.0, 1, 5.0, 0.25), 1);
    assert_eq!(mic.finish(T).exit_code, Some(0));
    let sys = AudioWriter::spawn(&ffmpeg(), &AudioSpec { rate: RATE, channels: 2 }, &d.join("system.mka"), 10).unwrap();
    push_audio(&sys, &sine(880.0, 2, 5.0, 0.25), 2);
    assert_eq!(sys.finish(T).exit_code, Some(0));
    let frames: Vec<_> = (0..75).map(|i| (i as u64, Arc::from(nv12_frame(640, 360, i)))).collect();
    assert_eq!(write_video(&d.join("video.mkv"), &frames, 640, 360).exit_code, Some(0));

    let plan = FinalizePlan { video: Some(5_000_000), mic: Some(0), system: Some(10_000_000), ..Default::default() };
    let out = finalize_meeting(&ffmpeg(), d, &plan, RATE, T).expect("收尾");
    assert_eq!(out, d.join("meeting.mp4"));
    assert!(!d.join("meeting.mp4.part").exists(), "半成品应已改名");

    let pcm = decode_stereo(&out);
    let seconds = pcm.len() as f64 / 2.0 / f64::from(RATE);
    assert!((seconds - 6.0).abs() < 0.15, "总长应为 1.0 + 5.0 = 6.0 秒，实际 {seconds}");
    let onset_880 = tone_onset(&pcm, 880.0, 0.08).expect("应当出现 880 Hz");
    assert!((onset_880 - 1.0).abs() < 0.04, "系统音应从 1.0 秒开始，实际 {onset_880}");
    let onset_440 = tone_onset(&pcm, 440.0, 0.08).expect("应当出现 440 Hz");
    assert!(onset_440 < 0.04, "麦克风应从 0 开始，实际 {onset_440}");

    let video_start = probe_start_time(&out, "v:0");
    assert!((video_start - 0.5).abs() < 0.05, "画面应从 0.5 秒开始，实际 {video_start}");
    assert_eq!(probe_frames(&out), 75, "视频只拷贝，不重新编码");
}

#[test]
fn a_killed_ffmpeg_leaves_a_playable_file_and_repair_makes_it_complete() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("system.mka");
    let w = AudioWriter::spawn(&ffmpeg(), &AudioSpec { rate: RATE, channels: 2 }, &out, 30).unwrap();
    push_audio(&w, &sine(440.0, 2, 4.0, 0.25), 2);
    std::thread::sleep(Duration::from_millis(800));
    w.kill_process(); // 模拟 ffmpeg 被杀或断电：没有机会写收尾
    let _ = w.finish(T);

    let before = decode_stereo(&out);
    let before_s = before.len() as f64 / 2.0 / f64::from(RATE);
    // 共写入 4 秒；cluster 限制为 1 秒，被杀时最多丢最后约 1 秒（多留一点余量给 FLAC 帧与调度）
    assert!((2.5..=4.0).contains(&before_s), "被杀之后应留下 2.5–4 秒可播放内容，实际 {before_s}");

    repair_track(&ffmpeg(), &out, T).expect("修复");
    assert!(!dir.path().join("system.mka.repair").exists());
    let after = decode_stereo(&out);
    assert!(after.len() >= before.len(), "修复不应丢数据");
    let dur = probe_duration(&out);
    assert!((dur - after.len() as f64 / 2.0 / f64::from(RATE)).abs() < 0.05, "修复后容器里应有正确的时长：{dur}");
}

#[test]
fn repairing_a_missing_file_fails_and_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.mka");
    assert!(repair_track(&ffmpeg(), &missing, T).is_err());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn the_frame_counter_matches_what_was_written() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("video.mkv");
    let frames: Vec<_> = (0..30).map(|i| (i as u64, Arc::from(nv12_frame(640, 360, i)))).collect();
    assert_eq!(write_video(&out, &frames, 640, 360).exit_code, Some(0));
    assert_eq!(count_video_frames(&ffmpeg(), &out, T).unwrap(), 30);
    assert!(count_video_frames(&ffmpeg(), &dir.path().join("nope.mkv"), T).is_err());
}

#[test]
fn a_continuation_segment_starts_at_the_given_global_index_and_pads_the_lost_interval() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("video.2.mkv");
    let spec = VideoSpec { width: 640, height: 360, fps: 15, encoder: encoder() };
    let w = VideoWriter::spawn_from(&ffmpeg(), &spec, &out, 32, 100).unwrap();
    for i in 103..=105u64 {
        assert_eq!(w.submit(i, Arc::from(nv12_frame(640, 360, i as usize))), Pushed::Queued);
    }
    let r = w.finish(T);
    assert_eq!((r.gap_repeats, r.late_discarded, r.exit_code), (3, 0, Some(0)), "{r:?}");
    assert_eq!(count_video_frames(&ffmpeg(), &out, T).unwrap(), 6, "序号 100..=105 各占一帧，开头 3 帧用第一帧补齐");
}

#[test]
fn a_video_encoder_killed_mid_recording_continues_in_a_second_segment_joined_without_a_seam() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let spec = VideoSpec { width: 640, height: 360, fps: 15, encoder: encoder() };
    let frame = |i: u64| -> Arc<[u8]> { Arc::from(nv12_frame(640, 360, i as usize)) };

    // 第 1 段：写 40 帧后编码器被杀
    let first = VideoWriter::spawn(&ffmpeg(), &spec, &d.join("video.mkv"), 64).unwrap();
    for i in 0..40u64 {
        first.submit(i, frame(i));
    }
    std::thread::sleep(Duration::from_millis(600));
    first.kill_process();
    let _ = first.finish(T);
    let n1 = count_video_frames(&ffmpeg(), &d.join("video.mkv"), T).unwrap();
    assert!(n1 > 0 && n1 <= 40, "第 1 段应留下一部分帧，实际 {n1}");

    // 第 2 段：从第 n1 帧接着写，中间“丢失”的那一截由新段的第一帧补齐；整场共 75 帧（序号 0..75）
    let second = VideoWriter::spawn_from(&ffmpeg(), &spec, &d.join("video.2.mkv"), 64, n1).unwrap();
    for i in 45..75u64 {
        second.submit(i, frame(i));
    }
    let r = second.finish(T);
    assert_eq!((r.exit_code, r.failed.clone()), (Some(0), None), "{r:?}");

    let segs = vec![PlanSegment { file: "video.mkv".into(), offset_hns: 0, frames: Some(n1) }, PlanSegment { file: "video.2.mkv".into(), offset_hns: 0, frames: None }];
    std::fs::write(d.join(VIDEO_CONCAT_FILE), concat_list(&segs, 15)).unwrap();
    let mut plan = FinalizePlan { video: Some(0), ..Default::default() };
    plan.segments.insert(TrackKind::Video, segs);
    let out = finalize_meeting(&ffmpeg(), d, &plan, RATE, T).expect("拼接收尾");

    assert_eq!(probe_frames(&out), 75, "两段首尾相接，序号 0..75 一帧不少");
    assert!((probe_duration(&out) - 5.0).abs() < 0.1, "{}", probe_duration(&out));
    let decode = std::process::Command::new(ffmpeg()).args(["-v", "error", "-i"]).arg(&out).args(["-f", "null", "-"]).output().unwrap();
    assert!(decode.status.success() && decode.stderr.is_empty(), "拼接处不应有解码错误：{}", String::from_utf8_lossy(&decode.stderr));
}

#[test]
fn audio_segments_of_one_track_are_placed_by_their_offsets_with_silence_in_between() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let spec = AudioSpec { rate: RATE, channels: 1 };
    // 第 1 段 0–2 秒（440 Hz），编码器重启，第 2 段从 3.0 秒开始（880 Hz），共 2 秒；中间 1 秒没有录到
    let a = AudioWriter::spawn(&ffmpeg(), &spec, &d.join("mic.mka"), 10).unwrap();
    push_audio(&a, &sine(440.0, 1, 2.0, 0.25), 1);
    assert_eq!(a.finish(T).exit_code, Some(0));
    let b = AudioWriter::spawn(&ffmpeg(), &spec, &d.join("mic.2.mka"), 10).unwrap();
    push_audio(&b, &sine(880.0, 1, 2.0, 0.25), 1);
    assert_eq!(b.finish(T).exit_code, Some(0));

    let mut plan = FinalizePlan { mic: Some(0), ..Default::default() };
    plan.segments.insert(TrackKind::Mic, vec![PlanSegment { file: "mic.mka".into(), offset_hns: 0, frames: None }, PlanSegment { file: "mic.2.mka".into(), offset_hns: 30_000_000, frames: None }]);
    let out = finalize_meeting(&ffmpeg(), d, &plan, RATE, T).expect("收尾");

    let pcm = decode_stereo(&out);
    let seconds = pcm.len() as f64 / 2.0 / f64::from(RATE);
    assert!((seconds - 5.0).abs() < 0.15, "总长应为 3.0 + 2.0 = 5.0 秒，实际 {seconds}");
    assert!(tone_onset(&pcm, 440.0, 0.08).expect("应有 440 Hz") < 0.04);
    let onset_880 = tone_onset(&pcm, 880.0, 0.08).expect("应有 880 Hz");
    assert!((onset_880 - 3.0).abs() < 0.04, "第 2 段应从 3.0 秒开始，实际 {onset_880}");
    // 2.1–2.9 秒之间应当是静音
    let rate = RATE as usize;
    let quiet = &pcm[2 * (21 * rate / 10)..2 * (29 * rate / 10)];
    assert!(quiet.iter().all(|s| s.abs() < 1e-3), "缺口应是静音");
}

#[test]
fn auto_encoder_resolves_on_the_dev_machine_and_software_is_never_chosen() {
    let enc = resolve_encoder(&ffmpeg(), Encoder::Auto).expect("开发机应有硬件编码器");
    assert!(matches!(enc, Encoder::Nvenc | Encoder::Qsv | Encoder::Amf), "{enc:?}");
    assert_eq!(resolve_encoder(&ffmpeg(), Encoder::Software).unwrap(), Encoder::Software);
}
