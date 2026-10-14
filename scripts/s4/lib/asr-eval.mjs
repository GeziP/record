// 识别评测：把一批 wav 分批喂给官方的识别程序，记下每批的耗时、内存与别的程序的 CPU 占用，再按参考文本算 CER。
// 真正起进程的是调用方给的 run（生产里是 measureAsync 的包装），所以这里可以用假进程测。
import fs from 'node:fs';
import path from 'node:path';
import { cerTotal } from './cer.mjs';
import { cpuSnapshot, othersLoad } from './cpu.mjs';
import { asrArgs, chunkFiles, parseAsrStderr, parseAsrStdout, parseVadAsrStdout, vadAsrArgs } from './sherpa.mjs';

/** 命令行里除了文件以外的长度：程序路径加各参数，每项按 长度 + 3（空格与一对引号）算 */
function baseLength(exe, args) {
  return exe.length + 3 + args.reduce((n, a) => n + a.length + 3, 0);
}

async function runChecked(run, req, what, snapshot) {
  const before = snapshot();
  const m = await run(req);
  const after = snapshot();
  if (m.timedOut) throw new Error(`${what}：超时被杀掉`);
  if (m.exitCode !== 0) throw new Error(`${what}：退出码 ${m.exitCode}，见 ${req.stderrFile}`);
  return { m, others: othersLoad(before, after, m.cpuMs) };
}

function batchRecord(m, others, parsed, files) {
  return { files, loadSec: parsed.loadSec, elapsedSec: parsed.elapsedSec, audioSec: parsed.audioSec, wallMs: m.wallMs, cpuMs: m.cpuMs, peakWorkingSetMb: m.peakWorkingSetMb, peakPrivateMb: m.peakPrivateMb, othersLoad: others };
}

/**
 * 逐文件识别：按命令行长度分批，每批一次调用；返回 { texts（与 files 同序）, batches }。
 * run：async ({ exe, args, stdoutFile, stderrFile }) => 测量结果（见 measure.mjs）；snapshot：取整机 CPU 快照的函数（测试时换成假的）。
 */
export async function decodeFiles({ run, exe, candId, modelsRoot, threads, files, outDir, chunk = {}, snapshot = cpuSnapshot }) {
  const base = asrArgs(candId, modelsRoot, { threads });
  fs.mkdirSync(outDir, { recursive: true });
  const texts = [];
  const batches = [];
  const parts = chunkFiles(files, { baseLen: baseLength(exe, base), ...chunk });
  for (const [i, part] of parts.entries()) {
    const stdoutFile = path.join(outDir, `batch-${i}.out.txt`);
    const stderrFile = path.join(outDir, `batch-${i}.err.txt`);
    const { m, others } = await runChecked(run, { exe, args: [...base, ...part], stdoutFile, stderrFile }, `${candId} 第 ${i + 1}/${parts.length} 批`, snapshot);
    texts.push(...parseAsrStdout(fs.readFileSync(stdoutFile, 'utf8'), part.length));
    batches.push(batchRecord(m, others, parseAsrStderr(fs.readFileSync(stderrFile, 'utf8')), part.length));
  }
  return { texts, batches };
}

/** VAD + 识别一体：对一整条录音，返回 { segments: [{start,end,text}], batch } */
export async function decodeWithVad({ run, exe, candId, modelsRoot, threads, vadModel, wav, outDir, tag, snapshot = cpuSnapshot }) {
  const args = [...vadAsrArgs(candId, modelsRoot, { threads, vadModel }), wav];
  fs.mkdirSync(outDir, { recursive: true });
  const stdoutFile = path.join(outDir, `${tag}.out.txt`);
  const stderrFile = path.join(outDir, `${tag}.err.txt`);
  const { m, others } = await runChecked(run, { exe, args, stdoutFile, stderrFile }, `${candId} VAD 端到端 ${tag}`, snapshot);
  const segments = parseVadAsrStdout(fs.readFileSync(stdoutFile, 'utf8'));
  return { segments, batch: batchRecord(m, others, parseAsrStderr(fs.readFileSync(stderrFile, 'utf8')), 1) };
}

const sum = (xs) => xs.reduce((a, b) => a + b, 0);

/**
 * 汇总各批：RTF 用“解码用时之和 ÷ 音频时长之和”（程序打印的解码用时不含模型加载）；加载用时取平均；内存取最大；
 * 别的程序的 CPU 占用按各批耗时加权平均，并给出最大值。
 */
export function summarizeBatches(batches) {
  if (batches.length === 0) throw new Error('没有任何批次可汇总');
  const elapsedSec = sum(batches.map((b) => b.elapsedSec));
  const audioSec = sum(batches.map((b) => b.audioSec));
  const loads = batches.map((b) => b.loadSec).filter((x) => x !== null);
  const wall = sum(batches.map((b) => b.wallMs));
  return {
    batches: batches.length,
    files: sum(batches.map((b) => b.files)),
    audioSec,
    elapsedSec,
    rtf: audioSec > 0 ? elapsedSec / audioSec : Infinity,
    loadSecMean: loads.length ? sum(loads) / loads.length : null,
    wallSec: wall / 1000,
    cpuSec: sum(batches.map((b) => b.cpuMs)) / 1000,
    peakWorkingSetMb: Math.max(...batches.map((b) => b.peakWorkingSetMb)),
    peakPrivateMb: Math.max(...batches.map((b) => b.peakPrivateMb)),
    othersLoadMean: wall > 0 ? sum(batches.map((b) => b.othersLoad * b.wallMs)) / wall : 0,
    othersLoadMax: Math.max(...batches.map((b) => b.othersLoad)),
  };
}

/** 对一批 [参考, 识别结果] 打分：CER（合计）与识别结果里阿拉伯数字的个数（参考里一个都没有，用来看逆文本规范化的影响） */
export function scorePairs(pairs) {
  const digits = (s) => (s.normalize('NFKC').match(/[0-9]/g) ?? []).length;
  return { cer: cerTotal(pairs), digitsInHyp: pairs.reduce((n, [, h]) => n + digits(h), 0), emptyHyps: pairs.filter(([, h]) => h.trim() === '').length };
}

/** 识别结果里标点占的比例（标点个数 ÷ 非空白字符数）；没有任何字符时是 0。用来判断这个识别器自己会不会输出标点。 */
export function punctuationRate(texts) {
  let punct = 0;
  let all = 0;
  for (const t of texts) {
    for (const ch of t) {
      if (/\s/u.test(ch)) continue;
      all++;
      if (/\p{P}/u.test(ch)) punct++;
    }
  }
  return all === 0 ? 0 : punct / all;
}
