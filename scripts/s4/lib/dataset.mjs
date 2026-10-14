// AliMeeting 评测集（OpenSLR 119 的 Eval_Ali）的布局与派生量：会议与参会者、标注段、“我”的选取、混合、切段。
// 布局（解压后实测）：Eval_Ali_near/{audio_dir,textgrid_dir}：<会议>_N_<参会者>.wav 与同名 .TextGrid（一层）；
// Eval_Ali_far/{audio_dir,textgrid_dir}：<会议>_MS<阵列号>.wav（8 通道）与 <会议>.TextGrid（每位参会者一层，层名 N_<参会者>）。
import fs from 'node:fs';
import path from 'node:path';
import { unionDuration } from './estimate.mjs';
import { parseTextGrid } from './textgrid.mjs';
import { readWavChannel, readWavInfo, writeWavMono } from './wav.mjs';

const NEAR_RE = /^(R\d+_M\d+)_N_(SPK\d+)\.wav$/;
const FAR_RE = /^(R\d+_M\d+)_MS\d+\.wav$/;

function need(file) {
  if (!fs.existsSync(file)) throw new Error(`评测集缺文件：${file}`);
  return file;
}

function wavNames(dir) {
  need(dir);
  return fs.readdirSync(dir).filter((f) => f.toLowerCase().endsWith('.wav')).sort();
}

/**
 * 扫描解压后的 Eval_Ali 目录。会议与参会者都按编号从小到大。
 * 返回 { sessions: [{ id, far: { wav, textgrid }, tracks: [{ speaker, wav, textgrid }] }] }。文件名不认识或缺文件都报错。
 */
export function scanDataset(root) {
  const sessions = new Map();
  const get = (id) => {
    if (!sessions.has(id)) sessions.set(id, { id, far: null, tracks: [] });
    return sessions.get(id);
  };
  const nearDir = path.join(root, 'Eval_Ali_near');
  for (const f of wavNames(path.join(nearDir, 'audio_dir'))) {
    const m = NEAR_RE.exec(f);
    if (!m) throw new Error(`近场录音的文件名不认识：${f}`);
    get(m[1]).tracks.push({ speaker: m[2], wav: path.join(nearDir, 'audio_dir', f), textgrid: need(path.join(nearDir, 'textgrid_dir', f.replace(/\.wav$/i, '.TextGrid'))) });
  }
  const farDir = path.join(root, 'Eval_Ali_far');
  for (const f of wavNames(path.join(farDir, 'audio_dir'))) {
    const m = FAR_RE.exec(f);
    if (!m) throw new Error(`远场录音的文件名不认识：${f}`);
    const s = get(m[1]);
    if (s.far) throw new Error(`会议 ${m[1]} 有不止一份远场录音`);
    s.far = { wav: path.join(farDir, 'audio_dir', f), textgrid: need(path.join(farDir, 'textgrid_dir', `${m[1]}.TextGrid`)) };
  }
  // 目录列表已按文件名排序，会议与参会者的先后就是编号的先后（编号位数固定）
  const list = [...sessions.values()];
  for (const s of list) {
    if (!s.far) throw new Error(`会议 ${s.id} 没有远场录音`);
    if (s.tracks.length === 0) throw new Error(`会议 ${s.id} 没有近场录音`);
  }
  if (list.length === 0) throw new Error(`${root} 里没有找到任何会议`);
  return { sessions: list };
}

/** 评测与产品的流水线都按 16 kHz 处理；数据集不是这个采样率就停下，不在脚本里悄悄重采样 */
export const RATE = 16000;

/** 一层标注里的发言段：跳过空文本（静音）与零长度的段，按起点排序。 */
export function segmentsOfTier(tier) {
  return tier.intervals
    .filter((iv) => iv.text.trim() !== '' && iv.xmax > iv.xmin)
    .map((iv) => ({ start: iv.xmin, end: iv.xmax, text: iv.text }))
    .sort((a, b) => a.start - b.start || a.end - b.end);
}

/** 近场 TextGrid：只有一层，就是这位参会者自己的发言段。 */
export function nearSegments(textGridText) {
  const tg = parseTextGrid(textGridText);
  if (tg.tiers.length !== 1) throw new Error(`近场 TextGrid 应当只有 1 层，得到 ${tg.tiers.length} 层`);
  return segmentsOfTier(tg.tiers[0]);
}

/** 远场 TextGrid：每位参会者一层，层名 N_SPKxxxx；返回 { SPKxxxx: 发言段[] }。 */
export function farSegmentsBySpeaker(textGridText) {
  const out = {};
  for (const tier of parseTextGrid(textGridText).tiers) {
    const name = tier.name.replace(/^N_/, '');
    if (Object.hasOwn(out, name)) throw new Error(`远场 TextGrid 里 ${name} 出现了两层`);
    out[name] = segmentsOfTier(tier);
  }
  return out;
}

/** 说话占比：发言段合并重叠后的总时长 ÷ 音轨总时长 */
export function speechFraction(segments, durationSec) {
  return unionDuration(segments) / durationSec;
}

/** “我”：说话最多的那位（会议主持人的常见情形）；一样多时取编号小的。输入 [{ speaker, speechSec }]。 */
export function pickMe(tracks) {
  if (tracks.length === 0) throw new Error('没有参会者可选');
  return tracks.reduce((a, b) => (b.speechSec > a.speechSec || (b.speechSec === a.speechSec && b.speaker < a.speaker) ? b : a)).speaker;
}

/** 逐点相加成一条（长度取最长的，短的补 0）；峰值超过 0.99 时整体缩小到 0.99，不让 16 位写出时削波。返回 { samples, gain }。 */
export function mixTracks(arrays) {
  let n = 0;
  for (const a of arrays) n = Math.max(n, a.length);
  const out = new Float32Array(n);
  for (const a of arrays) for (let i = 0; i < a.length; i++) out[i] += a[i];
  let peak = 0;
  for (let i = 0; i < n; i++) peak = Math.max(peak, Math.abs(out[i]));
  const gain = peak > 0.99 ? 0.99 / peak : 1;
  if (gain !== 1) for (let i = 0; i < n; i++) out[i] *= gain;
  return { samples: out, gain };
}

/** 参考文本：按时间顺序把各段文字接起来 */
export function refText(segments) {
  return segments.map((s) => s.text).join('');
}

/** 切段计划：把秒换成帧，起点按四舍五入、终点不超过文件末尾，没有长度的段（整段在文件之外）去掉但保留原序号 */
export function cutPlan(segments, rate, totalFrames) {
  const plan = [];
  segments.forEach((s, index) => {
    const start = Math.round(s.start * rate);
    const end = Math.min(totalFrames, Math.round(s.end * rate));
    if (end > start) plan.push({ index, startFrame: start, frames: end - start });
  });
  return plan;
}

/** 把一条音轨按标注切成一个个 wav（16 位单声道，文件名 0000.wav 起，序号对应 segments 的下标）。返回写出的文件数。 */
export function cutSegments(wavPath, segments, outDir) {
  const info = readWavInfo(wavPath);
  fs.mkdirSync(outDir, { recursive: true });
  const plan = cutPlan(segments, info.sampleRate, info.frames);
  for (const p of plan) {
    const { samples } = readWavChannel(wavPath, 0, p.startFrame, p.frames);
    writeWavMono(path.join(outDir, `${String(p.index).padStart(4, '0')}.wav`), samples, info.sampleRate);
  }
  return plan.length;
}

/** 把多通道录音里的一个通道存成 16 位单声道 wav（远场阵列取第 1 通道用） */
export function extractChannel(wavPath, channel, outPath) {
  const { sampleRate, samples } = readWavChannel(wavPath, channel);
  fs.mkdirSync(path.dirname(outPath), { recursive: true });
  writeWavMono(outPath, samples, sampleRate);
  return { sampleRate, frames: samples.length };
}

/**
 * 读入整个评测集：每场会议的参会者（时长、发言段、说话时长）、远场（时长、通道数、每人的发言段）、被选作“我”的那一位。
 * 近场不是 16 kHz 单声道 16 位、远场不是 16 kHz、远场标注里的人与近场对不上，都报错。
 */
export function loadSessions(root) {
  return scanDataset(root).sessions.map((s) => {
    const tracks = s.tracks.map((t) => {
      const info = readWavInfo(t.wav);
      if (info.sampleRate !== RATE || info.channels !== 1 || info.bits !== 16) {
        throw new Error(`${t.wav}：近场录音应当是 16 kHz 单声道 16 位，得到 ${info.sampleRate} Hz ${info.channels} 通道 ${info.bits} 位`);
      }
      const segments = nearSegments(fs.readFileSync(t.textgrid, 'utf8'));
      return { speaker: t.speaker, wav: t.wav, durationSec: info.frames / info.sampleRate, segments, speechSec: unionDuration(segments) };
    });
    const farInfo = readWavInfo(s.far.wav);
    if (farInfo.sampleRate !== RATE) throw new Error(`${s.far.wav}：远场录音应当是 16 kHz，得到 ${farInfo.sampleRate} Hz`);
    const bySpeaker = farSegmentsBySpeaker(fs.readFileSync(s.far.textgrid, 'utf8'));
    const nearIds = tracks.map((t) => t.speaker); // scanDataset 已按编号排好
    const farIds = Object.keys(bySpeaker).sort();
    if (nearIds.join() !== farIds.join()) throw new Error(`会议 ${s.id} 的远场标注与近场录音对不上：近场 ${nearIds.join(',')}，远场 ${farIds.join(',')}`);
    return { id: s.id, tracks, me: pickMe(tracks), far: { wav: s.far.wav, channels: farInfo.channels, durationSec: farInfo.frames / farInfo.sampleRate, bySpeaker } };
  });
}
