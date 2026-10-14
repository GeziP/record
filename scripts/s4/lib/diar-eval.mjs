// 说话人分离评测：声纹模型 × 聚类阈值（或已知人数）的任务清单、跑一次官方分离程序并打分、把各场结果汇总成 decide.mjs 要的曲线。
// 真正起进程的是调用方给的 run（生产里是 measureAsync 的包装），所以这里可以用假进程测。
import fs from 'node:fs';
import path from 'node:path';
import { countSpeakers, scoreDer } from './der.mjs';
import { diarArgs, parseDiarStderr, parseDiarStdout } from './sherpa.mjs';

export const SEGMENTATION_FILE = 'sherpa-onnx-pyannote-segmentation-3-0/model.onnx';

/** 声纹模型候选：编号与文件名（在 downloads 目录里） */
export const EMBEDDINGS = Object.freeze([
  { id: 'campplus', file: '3dspeaker_speech_campplus_sv_zh-cn_16k-common.onnx' },
  { id: 'eres2net-base', file: '3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx' },
  { id: 'eres2net-large', file: '3dspeaker_speech_eres2net_large_sv_zh-cn_3dspeaker_16k.onnx' },
  { id: 'eres2netv2', file: '3dspeaker_speech_eres2netv2_sv_zh-cn_16k-common.onnx' },
  { id: 'wespeaker-resnet34', file: 'wespeaker_zh_cnceleb_resnet34.onnx' },
]);

/**
 * 聚类阈值扫描范围：0.3～1.0，步长 0.1。设计文档写的是 0.3～0.9；真数据上冒烟时（前 400 秒，一场会议）
 * 多数声纹模型的 DER 最低点落在 0.9 这个边上，所以把 1.0 也扫了，免得最低点被范围卡住。
 */
export const THRESHOLDS = Object.freeze([0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0]);

/** 任务键：同一场、同一模型、同一设置得到同一个键，也用作结果文件名 */
export function jobKey(mixId, embeddingId, { threshold, clusters }) {
  return `${mixId}__${embeddingId}__${clusters !== undefined ? `k${clusters}` : `t${threshold.toFixed(2)}`}`;
}

/**
 * 任务清单：每个会议音轨 × 每个声纹模型 × （每个阈值 + 已知人数）。
 * mix：{ id, wav, ref, durationSec, actualSpeakers }；已知人数取参考里的说话人数。
 */
export function diarJobs({ mixes, embeddings = EMBEDDINGS, thresholds = THRESHOLDS, knownCount = true }) {
  const jobs = [];
  for (const mix of mixes) {
    for (const embedding of embeddings) {
      for (const threshold of thresholds) jobs.push({ key: jobKey(mix.id, embedding.id, { threshold }), mix, embedding, threshold });
      if (knownCount) jobs.push({ key: jobKey(mix.id, embedding.id, { clusters: mix.actualSpeakers }), mix, embedding, clusters: mix.actualSpeakers });
    }
  }
  return jobs;
}

/** 假设里估计的人数与参考的人数：都按“出现过的说话人”数 */
export function speakerCounts(ref, hyp) {
  return { actual: countSpeakers(ref), estimated: countSpeakers(hyp), estimated3s: countSpeakers(hyp, 3) };
}

/**
 * 跑一个任务：调分离程序（单线程，后台优先级），解析输出，按参考打分（容差 0.25 秒，计重叠）。
 * run：async ({ exe, args, stdoutFile, stderrFile, priority }) => 测量结果（见 measure.mjs）。
 */
export async function runDiarJob({ run, exe, segmentationModel, embeddingDir, job, outDir, threads = 1, priority = 'BelowNormal' }) {
  const { mix, embedding } = job;
  const args = [...diarArgs({ segmentationModel, embeddingModel: path.join(embeddingDir, embedding.file), threshold: job.threshold, clusters: job.clusters, threads }), mix.wav];
  fs.mkdirSync(outDir, { recursive: true });
  const stdoutFile = path.join(outDir, `${job.key}.out.txt`);
  const stderrFile = path.join(outDir, `${job.key}.err.txt`);
  const m = await run({ exe, args, stdoutFile, stderrFile, priority });
  if (m.timedOut) throw new Error(`${job.key}：超时被杀掉`);
  if (m.exitCode !== 0) throw new Error(`${job.key}：退出码 ${m.exitCode}，见 ${stderrFile}`);
  const hyp = parseDiarStdout(fs.readFileSync(stdoutFile, 'utf8'));
  const e = parseDiarStderr(fs.readFileSync(stderrFile, 'utf8'));
  const score = scoreDer(mix.ref, hyp, { collar: 0.25, duration: mix.durationSec });
  const counts = speakerCounts(mix.ref, hyp);
  return {
    key: job.key,
    mixId: mix.id,
    embedding: embedding.id,
    threshold: job.threshold ?? null,
    clusters: job.clusters ?? null,
    durationSec: e.durationSec,
    elapsedSec: e.elapsedSec,
    rtf: e.rtf,
    wallMs: m.wallMs,
    cpuMs: m.cpuMs,
    peakWorkingSetMb: m.peakWorkingSetMb,
    total: score.total,
    miss: score.miss,
    fa: score.fa,
    conf: score.conf,
    der: score.der,
    actualSpeakers: counts.actual,
    estimatedSpeakers: counts.estimated,
    estimatedSpeakers3s: counts.estimated3s,
    hyp: hyp.map((s) => [s.start, s.end, s.speaker]),
  };
}

/**
 * 把同一个“声纹模型 + 设置”在各场上的结果合成一个点：DER 按总量合计（总误差 ÷ 总参考说话量，不是各场比例的平均），
 * 人数相差不超过 1 的场次数。
 */
export function aggregate(results) {
  if (results.length === 0) throw new Error('没有结果可汇总');
  const sum = (k) => results.reduce((a, r) => a + r[k], 0);
  const total = sum('total');
  const errors = sum('miss') + sum('fa') + sum('conf');
  return {
    sessions: results.length,
    total,
    miss: sum('miss') / total,
    fa: sum('fa') / total,
    conf: sum('conf') / total,
    der: errors / total,
    countHits: results.filter((r) => Math.abs(r.estimatedSpeakers - r.actualSpeakers) <= 1).length,
    countMeanAbsError: results.reduce((a, r) => a + Math.abs(r.estimatedSpeakers - r.actualSpeakers), 0) / results.length,
  };
}

/**
 * 全部结果 → { 声纹模型: { points: [{ threshold, ...aggregate }], knownCount: aggregate|null } }，points 按阈值升序。
 * 每个设置必须在所有场次上都有结果（缺了就报错，不拿残缺的数字去比）。
 */
export function curves(results, { sessions }) {
  const byModel = new Map();
  for (const r of results) {
    if (!byModel.has(r.embedding)) byModel.set(r.embedding, new Map());
    const cfg = r.clusters !== null ? 'k' : `t${r.threshold.toFixed(2)}`;
    const m = byModel.get(r.embedding);
    if (!m.has(cfg)) m.set(cfg, []);
    m.get(cfg).push(r);
  }
  const out = {};
  for (const [emb, cfgs] of byModel) {
    const points = [];
    let knownCount = null;
    for (const [cfg, list] of cfgs) {
      if (list.length !== sessions) throw new Error(`${emb} 的设置 ${cfg} 只有 ${list.length}/${sessions} 场的结果`);
      const a = aggregate(list);
      if (cfg === 'k') knownCount = a;
      else points.push({ threshold: list[0].threshold, ...a });
    }
    points.sort((a, b) => a.threshold - b.threshold);
    out[emb] = { points, knownCount };
  }
  return out;
}
