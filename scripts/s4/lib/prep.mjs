// 评测输入的准备：近场录音按标注切段、把“对方”的近场录音相加成“系统声音”、取远场阵列的第 1 通道，以及远场与标注的时间对齐检查。
// 产物都放在 S4 目录的 work\ 下，可以随时删掉重做；已有的产物直接沿用（写到一半被打断不会留下半截文件）。
import fs from 'node:fs';
import path from 'node:path';
import { envelope, lagBetween } from './align.mjs';
import { cutSegments, extractChannel, mixTracks, RATE } from './dataset.mjs';
import { maskFromSegments } from './echo.mjs';
import { readWavChannel, readWavInfo, writeWavMono } from './wav.mjs';

const pad4 = (i) => String(i).padStart(4, '0');

// 起点相同按终点；起止都相同时保持原来的先后（排序是稳定的，参会者本来就按编号排好）
const byStart = (a, b) => a.start - b.start || a.end - b.end;

/** 近场段文件所在目录：<work>\segs\<会议>\<参会者>\，里面是 0000.wav 起的文件，序号对应 segments 的下标 */
export function segDir(work, sessionId, speaker) {
  return path.join(work, 'segs', sessionId, speaker);
}

/** 一条音轨的各段与它们的文件：[{ index, file, start, end, text }]；整段在音轨之外、没有切出文件的段不在其中 */
export function segmentItems(dir, segments) {
  return segments
    .map((s, index) => ({ index, file: path.join(dir, `${pad4(index)}.wav`), start: s.start, end: s.end, text: s.text }))
    .filter((x) => fs.existsSync(x.file));
}

/** 切好一条音轨的段。切完才写 .done（记着个数），有 .done 就跳过。返回切出的个数。 */
export function prepareTrackSegments({ work, sessionId, track }) {
  const dir = segDir(work, sessionId, track.speaker);
  const done = path.join(dir, '.done');
  if (fs.existsSync(done)) return Number(fs.readFileSync(done, 'utf8'));
  const n = cutSegments(track.wav, track.segments, dir);
  fs.writeFileSync(done, String(n));
  return n;
}

export const mixId = (sessionId, me) => `${sessionId}__me-${me}`;

function distinctSpeakers(ref) {
  return new Set(ref.map((r) => r.speaker)).size;
}

/**
 * 把一条近场录音里“这位参会者自己没在说话”的部分静音（标注的发言段前后各放宽 marginSec 秒）。
 * 线下会议室里近场耳机会收进旁边人的说话声与房间噪声；不静音就相加的话，被排除的“我”的声音会混进“系统声音”，
 * 这些在参考里没有人说话的时间全被算成误检（冒烟实测：只有一位对方的会议里，误检占 46%）。
 */
export function gateToSpeech(samples, segments, marginSec = 0.2) {
  const mask = maskFromSegments(
    segments.map((s) => ({ start: s.start - marginSec, end: s.end + marginSec })),
    RATE,
    samples.length,
  );
  const out = new Float32Array(samples.length);
  for (let i = 0; i < out.length; i++) if (mask[i]) out[i] = samples[i];
  return out;
}

/**
 * 把 me 当作“我”时的“系统声音”：其余参会者的近场录音各自静音掉没在说话的部分（见 gateToSpeech）后相加
 * （峰值超过 0.99 时整体缩小，时间轴不变），参考是他们的发言段。
 * 返回 { id, wav, ref: [{start,end,speaker}], durationSec, actualSpeakers, me }。
 */
export function buildMix({ work, session, me, marginSec = 0.2 }) {
  const others = session.tracks.filter((t) => t.speaker !== me);
  if (others.length === session.tracks.length) throw new Error(`会议 ${session.id} 里没有参会者 ${me}`);
  if (others.length === 0) throw new Error(`会议 ${session.id} 只有 ${me} 一位参会者，没有“对方”可混`);
  const id = mixId(session.id, me);
  const wav = path.join(work, 'mix-gated', `${id}.wav`);
  const ref = others.flatMap((t) => t.segments.map((s) => ({ start: s.start, end: s.end, speaker: t.speaker }))).sort(byStart);
  if (!fs.existsSync(wav)) {
    const { samples } = mixTracks(others.map((t) => gateToSpeech(readWavChannel(t.wav, 0).samples, t.segments, marginSec)));
    fs.mkdirSync(path.dirname(wav), { recursive: true });
    writeWavMono(`${wav}.tmp`, samples, RATE);
    fs.renameSync(`${wav}.tmp`, wav);
  }
  const info = readWavInfo(wav);
  return { id, wav, ref, durationSec: info.frames / info.sampleRate, actualSpeakers: distinctSpeakers(ref), me };
}

/** 远场“混合”：阵列的第 1 通道，参考是所有参会者的发言段（含“我”）。 */
export function buildFarMix({ work, session }) {
  const wav = path.join(work, 'far', `${session.id}.ch1.wav`);
  if (!fs.existsSync(wav)) {
    extractChannel(session.far.wav, 0, `${wav}.tmp`);
    fs.renameSync(`${wav}.tmp`, wav);
  }
  const ref = Object.entries(session.far.bySpeaker)
    .flatMap(([speaker, segs]) => segs.map((s) => ({ start: s.start, end: s.end, speaker })))
    .sort(byStart);
  return { id: `${session.id}__far`, wav, ref, durationSec: session.far.durationSec, actualSpeakers: distinctSpeakers(ref), me: null };
}

/**
 * 远场第 1 通道相对于“所有近场录音相加”的延迟（秒，正数表示远场更晚）：10 毫秒一格的能量包络做互相关，范围 ±60 秒。
 * 近场录音与标注同一条时间轴，所以这个数就是远场相对标注的错位。
 */
export function farLagSec({ nearWavs, farWav, hop = 160, maxLagSec = 60 }) {
  let near = new Float32Array(0);
  for (const w of nearWavs) {
    const x = readWavChannel(w, 0).samples;
    if (x.length > near.length) {
      const grown = new Float32Array(x.length);
      grown.set(near);
      near = grown;
    }
    for (let i = 0; i < x.length; i++) near[i] += x[i];
  }
  const far = readWavChannel(farWav, 0).samples;
  const ea = envelope(near, hop);
  const eb = envelope(far, hop);
  const n = Math.min(ea.length, eb.length);
  const { lag, peak } = lagBetween(ea.subarray(0, n), eb.subarray(0, n), Math.round((maxLagSec * RATE) / hop));
  return { lagSec: (lag * hop) / RATE, peak, nearSec: near.length / RATE, farSec: far.length / RATE };
}
