// 回声消除评测：合成回声的条件网格、近端段与远端单讲段的挑选、一个条件下各处理方式的评估、结果汇总。
// 处理（回声消除程序）与识别都由调用方注入（deps.aec、deps.decode），所以这里可以用假的测。
import fs from 'node:fs';
import path from 'node:path';
import { cerTotal, normalizeText } from './cer.mjs';
import { cutPlan } from './dataset.mjs';
import { erleDb, makeRir, maskFromSegments, mixEcho } from './echo.mjs';
import { writeWavMono } from './wav.mjs';

/** 三种回声通路：直达延迟与混响时间（设计文档 3.5：延迟 20～300 ms，混响时间 0.2～0.6 秒） */
export const SCENARIOS = Object.freeze([
  { id: 'd020r02', delayMs: 20, rt60: 0.2 },
  { id: 'd100r04', delayMs: 100, rt60: 0.4 },
  { id: 'd300r06', delayMs: 300, rt60: 0.6 },
]);

/** 回声与近端语音的能量比（dB） */
export const RATIOS_DB = Object.freeze([-10, 0, 5]);

/**
 * 对齐时少平移的毫秒数。第一次冒烟时零余量对齐让 aec3 彻底失效：延迟估得再准，直达声也恰好落在参考信号的第 0 点上，
 * 回声一比参考信号早到就消不掉。探查（2 场会议各 600 秒 × 3 个条件）：零余量时 aec3 的回声插入 70%～92%、消减量为负；
 * 余量 5、10、20 ms 时 0.6%～4.4%；corti 不受余量影响。取 10 ms：盖得住延迟估计的误差，又小于最短的 20 ms 延迟。
 */
export const ALIGN_MARGIN_MS = 10;

/** 处理方式：不处理、corti-aec、aec3，后两个各有“先估计整体延迟并对齐（留 ALIGN_MARGIN_MS 余量）”与“不对齐”；corti 的滤波器长 16384 点（16 kHz 下约 1 秒，盖得住最长的 300 ms 延迟加 0.6 秒混响） */
export const VARIANTS = Object.freeze([
  { id: 'none', algo: 'none', opts: [] },
  { id: 'corti', algo: 'corti', opts: ['filter_len=16384'] },
  { id: 'corti-align', algo: 'corti', opts: ['filter_len=16384', 'align=1', `margin_ms=${ALIGN_MARGIN_MS}`] },
  { id: 'aec3', algo: 'aec3', opts: [] },
  { id: 'aec3-align', algo: 'aec3', opts: ['align=1', `margin_ms=${ALIGN_MARGIN_MS}`] },
]);

/** 九个条件：三种通路 × 三档能量比 */
export function conditions() {
  const out = [];
  for (const scenario of SCENARIOS) for (const ratioDb of RATIOS_DB) out.push({ id: `${scenario.id}_r${ratioDb}`, scenario, ratioDb });
  return out;
}

/** 只留终点不超过 endSec 的段（跨过截取终点的整段不要） */
export function excerptSegments(segments, endSec) {
  return segments.filter((s) => s.end <= endSec);
}

/**
 * 远端单讲段：对方的发言段里，与“我”的任何发言段（前后各放宽 guard 秒）都不重叠的那些。
 * 回声插入率只在这些段里量：这里“我”没有说话，识别出来的字都是回声残留。
 */
export function farOnlySegments(meSegments, otherSegments, guard = 0.25) {
  return otherSegments.filter((o) => !meSegments.some((m) => o.start < m.end + guard && m.start - guard < o.end));
}

/** 按条件合成麦克风信号：近端 + 远端经脉冲响应得到的回声（按能量比）+ 底噪。返回 { mic, echo }。 */
export function makeMic({ near, far, meSegments, otherSegments, scenario, ratioDb, rate }) {
  const rir = makeRir({ rate, delayMs: scenario.delayMs, rt60: scenario.rt60, seed: 1 });
  const nearMask = maskFromSegments(meSegments, rate, near.length);
  const farMask = maskFromSegments(otherSegments, rate, near.length);
  const { mic, echo } = mixEcho({ near, far, rir, nearMask, farMask, ratioDb, noiseDb: -40, seed: 2 });
  return { mic, echo };
}

/** 把一条信号按段切成 wav，文件名 <前缀><序号>.wav；有的段落在信号之外就报错（评测用的段都应当在截取范围内） */
export function cutSignal(samples, segments, rate, dir, prefix) {
  fs.mkdirSync(dir, { recursive: true });
  const plan = cutPlan(segments, rate, samples.length);
  if (plan.length !== segments.length) throw new Error(`${prefix}：${segments.length - plan.length} 个段落在信号之外`);
  return plan.map((p) => {
    const file = path.join(dir, `${prefix}${String(p.index).padStart(4, '0')}.wav`);
    writeWavMono(file, samples.subarray(p.startFrame, p.startFrame + p.frames), rate);
    return file;
  });
}

/** 只留掩码为 1 的点，其余置零（数字静音）；返回新数组，不改原信号 */
export function keepOnly(samples, mask) {
  const out = new Float32Array(samples.length);
  for (let i = 0; i < samples.length; i++) if (mask[i]) out[i] = samples[i];
  return out;
}

/**
 * 一个条件下各处理方式：处理麦克风信号 → 远端单讲段的回声消减量 → 切出近端段并识别（所有近端文件一次交给 decode）；
 * 回声插入不能把远端单讲段逐段直接丢给识别器：没有回声、只有底噪的段，识别器自己也会吐出“好的”“嗯”之类的字（冒烟探查：没有回声时的“插入”9%～23%），
 * 量不出 5% 的门槛。所以按产品里的做法，先过 VAD 再识别：整条处理后的信号只保留远端单讲段、其余置零，交给 deps.decodeFar（VAD + 识别）。
 * deps.aec(variant, far, mic) → { samples, info }；deps.decode(files) → 与 files 同序的文本；deps.decodeFar(wav 文件) → 整条信号识别出的全部文字。
 * 返回 [{ variant, erleDb, info, nearHyps, farText }]。
 */
export async function evalVariants({ rate, far, mic, nearSegs, farOnlySegs, variants = VARIANTS, deps, dir }) {
  const farOnlyMask = maskFromSegments(farOnlySegs, rate, mic.length);
  const staged = [];
  const files = [];
  for (const v of variants) {
    const { samples, info } = v.algo === 'none' ? { samples: mic, info: null } : await deps.aec(v, far, mic);
    if (samples.length !== mic.length) throw new Error(`${v.id}：处理后长度 ${samples.length} 与输入 ${mic.length} 不同`);
    const nearFiles = cutSignal(samples, nearSegs, rate, dir, `${v.id}-near-`);
    const farFile = path.join(dir, `${v.id}-far.wav`);
    writeWavMono(farFile, keepOnly(samples, farOnlyMask), rate);
    staged.push({ variant: v.id, erleDb: v.algo === 'none' ? 0 : erleDb(mic, samples, farOnlyMask), info, nearCount: nearFiles.length, farFile });
    files.push(...nearFiles);
  }
  const texts = await deps.decode(files);
  if (texts.length !== files.length) throw new Error(`识别返回了 ${texts.length} 个结果，应当是 ${files.length} 个`);
  const out = [];
  let at = 0;
  for (const s of staged) {
    out.push({ variant: s.variant, erleDb: s.erleDb, info: s.info, nearHyps: texts.slice(at, at + s.nearCount), farText: await deps.decodeFar(s.farFile) });
    at += s.nearCount;
  }
  return out;
}

const chars = (s) => Array.from(normalizeText(s)).length;

/** 近端 CER 的计数（错误数与参考字数）与回声插入的计数（远端单讲段里识别出的字数与参考字数） */
export function scoreOutput({ nearRefs, farRefs }, { nearHyps, farText }) {
  if (nearRefs.length !== nearHyps.length) throw new Error('参考与识别结果的个数对不上');
  const near = cerTotal(nearRefs.map((r, i) => [r, nearHyps[i]]));
  return { nearErrors: near.errors, nearRefLen: near.refLen, insHyp: chars(farText), insRef: farRefs.reduce((n, r) => n + chars(r), 0) };
}

/** 干净近端（没有回声、不处理）的 CER 计数，作为“近端 CER 增加了多少”的基线 */
export function baselineCounts(nearRefs, nearHyps) {
  if (nearRefs.length !== nearHyps.length) throw new Error('参考与识别结果的个数对不上');
  const near = cerTotal(nearRefs.map((r, i) => [r, nearHyps[i]]));
  return { errors: near.errors, refLen: near.refLen };
}

/**
 * 汇总各场会议：基线 CER、没有回声时的底线，与每个（处理方式 × 条件）的 { insertion, nearCer, nearCerIncrease, erleDb }，都按总量合计。
 * 底线 = 麦克风里只有近端和底噪、没有回声时（不处理）：同一套 VAD + 识别在远端单讲段里识别出的字数 ÷ 参考字数（insertion），与近端 CER（nearCer）。
 * sessions：[{ baseline: { errors, refLen }, floor: { nearErrors, nearRefLen, insHyp, insRef }, conditions: { [条件编号]: { [处理方式]: { nearErrors, nearRefLen, insHyp, insRef, erleDb } } } }]
 * 返回 { baselineCer, floor: { insertion, nearCer }, variants: { [处理方式]: { conditions: [{ id, insertion, nearCer, nearCerIncrease, erleDb }] } } }，variants 可直接交给 decide.pickAec（去掉 none 之后）。
 */
export function aggregateEcho(sessions) {
  if (sessions.length === 0) throw new Error('没有会议可汇总');
  const base = sessions.reduce((a, s) => ({ errors: a.errors + s.baseline.errors, refLen: a.refLen + s.baseline.refLen }), { errors: 0, refLen: 0 });
  const baselineCer = base.errors / base.refLen;
  for (const s of sessions) if (!s.floor) throw new Error('有的会议没有底线结果（没有回声时的识别）');
  const floorSum = (k) => sessions.reduce((a, s) => a + s.floor[k], 0);
  if (floorSum('insRef') === 0) throw new Error('底线：没有远端单讲段的参考字，量不出底线');
  const floor = { insertion: floorSum('insHyp') / floorSum('insRef'), nearCer: floorSum('nearErrors') / floorSum('nearRefLen') };
  const ids = Object.keys(sessions[0].conditions);
  const variants = {};
  for (const id of ids) {
    for (const s of sessions) if (!s.conditions[id]) throw new Error(`有的会议缺条件 ${id}`);
    const names = Object.keys(sessions[0].conditions[id]);
    for (const name of names) {
      const cells = sessions.map((s) => {
        if (!s.conditions[id][name]) throw new Error(`有的会议缺 ${name} 在条件 ${id} 下的结果`);
        return s.conditions[id][name];
      });
      const sum = (k) => cells.reduce((a, c) => a + c[k], 0);
      if (sum('insRef') === 0) throw new Error(`${name} 在条件 ${id} 下没有远端单讲段的参考字，量不出回声插入率`);
      const nearCer = sum('nearErrors') / sum('nearRefLen');
      (variants[name] ??= { conditions: [] }).conditions.push({ id, insertion: sum('insHyp') / sum('insRef'), nearCer, nearCerIncrease: nearCer - baselineCer, erleDb: sum('erleDb') / cells.length });
    }
  }
  return { baselineCer, floor, variants };
}
