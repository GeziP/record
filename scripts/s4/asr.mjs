// 识别评测。结果写在 <S4 目录>\results\asr\<候选>\ 下，已有的结果不重跑（加 --force 重跑）。
//   node scripts/s4/asr.mjs prescreen --home <目录> [--cands a,b] [--threads 8,4]
//       三场会议（头、中、尾）、每人前 5 分钟的标注段，每个候选分别用 8 与 4 线程各跑一遍：量实时倍数、内存与 CER。
//       跑的时候别的程序不要占 CPU（这是计时）；别的程序的 CPU 占用会记在结果里。
// 批识别的做法：官方程序把一次调用里的所有文件当作同一个批次、按最长的那个补齐来解码，批里长短差得多时又慢又占内存
// （实测 paraformer：278 段一次调用，实时倍数 0.037、2.9 GB；按时长排序后每批 32 段，0.0076、0.5 GB）。
// 所以所有批识别都先按段长从短到长排序，再每批 --batch 段（默认 32）。
//   node scripts/s4/asr.mjs full --home <目录> [--cands a,b]
//       全部 25 条近场录音的全部标注段，8 线程。预筛里“只算识别的估计用时”超过门槛 2 倍的候选不跑全量（记录在案）。
//   node scripts/s4/asr.mjs vad --home <目录> [--cands a,b]
//       决赛圈（全量里速度与内存达标、CER 最低的前 3 个；也可以用 --cands 指定）对 25 条整条近场录音做“VAD + 识别”端到端。
import fs from 'node:fs';
import path from 'node:path';
import { decodeFiles, decodeWithVad, punctuationRate, scorePairs, summarizeBatches } from './lib/asr-eval.mjs';
import { cerTotal } from './lib/cer.mjs';
import { log, parseArgs, pickThree } from './lib/cli.mjs';
import { GATES } from './lib/decide.mjs';
import { loadSessions, refText } from './lib/dataset.mjs';
import { homeFrom, readJson, s4Paths, writeJsonAtomic } from './lib/home.mjs';
import { measureAsync } from './lib/measure.mjs';
import { segDir, segmentItems } from './lib/prep.mjs';
import { asrOnlyMinutes, finalists, speechFractions } from './lib/report.mjs';
import { ASR_CANDIDATES } from './lib/sherpa.mjs';

const { positional, flags } = parseArgs(process.argv.slice(2));
const stage = positional[0];
const paths = s4Paths(homeFrom(flags.home));
const sessions = loadSessions(paths.dataset);
const speechFrac = speechFractions(sessions);
const run = (req) => measureAsync({ priority: 'Normal', ...req });
const force = flags.force === true;
const batchSize = Number(flags.batch ?? 32);
if (!Number.isInteger(batchSize) || batchSize < 1) throw new Error(`--batch 要是正整数：${flags.batch}`);
const known = ASR_CANDIDATES.map((c) => c.id);
const cands = flags.cands ? String(flags.cands).split(',') : known;
for (const c of cands) if (!known.includes(c)) throw new Error(`不认识的识别候选：${c}（可选：${known.join('、')}）`);

const resultFile = (cand, name) => path.join(paths.results, 'asr', cand, name);
const stamp = () => new Date().toISOString();

/** 某些会议的近场标注段（可只取终点不超过 maxEndSec 的），每段带上它的 wav 与参考文本 */
function itemsFor(subset, maxEndSec = Infinity) {
  return subset.flatMap((s) =>
    s.tracks.flatMap((t) =>
      segmentItems(segDir(paths.work, s.id, t.speaker), t.segments)
        .filter((x) => x.end <= maxEndSec)
        .map((x) => ({ session: s.id, speaker: t.speaker, index: x.index, file: x.file, ref: x.text, dur: x.end - x.start })),
    ),
  );
}

function scoreItems(items, texts) {
  const pairs = items.map((it, i) => [it.ref, texts[i]]);
  const bySession = {};
  for (const id of new Set(items.map((i) => i.session))) bySession[id] = cerTotal(pairs.filter((_, i) => items[i].session === id)).rate;
  return { score: scorePairs(pairs), bySession, punctRate: punctuationRate(texts) };
}

async function decodeStage({ cand, name, items: unsorted, threads, extra }) {
  const items = [...unsorted].sort((a, b) => a.dur - b.dur); // 稳定排序：段长一样的保持原来的先后
  const file = resultFile(cand, name);
  if (fs.existsSync(file) && !force) {
    log(`已有，跳过：${file}`);
    return readJson(file);
  }
  log(`${cand}  ${name}：${items.length} 段，${threads} 线程`);
  const { texts, batches } = await decodeFiles({
    run,
    exe: paths.exe('sherpa-onnx-offline'),
    candId: cand,
    modelsRoot: paths.models,
    threads,
    files: items.map((i) => i.file),
    outDir: path.join(paths.work, 'asr-out', cand, name.replace(/\.json$/, '')),
    chunk: { maxCount: batchSize },
  });
  const summary = summarizeBatches(batches);
  const result = { cand, name, threads, batchSize, sortedByDuration: true, files: items.length, summary, batches, ...scoreItems(items, texts), ...extra, finishedAt: stamp() };
  writeJsonAtomic(file, result);
  writeJsonAtomic(file.replace(/\.json$/, '.texts.json'), items.map((it, i) => ({ session: it.session, speaker: it.speaker, index: it.index, ref: it.ref, hyp: texts[i] })));
  log(`  CER ${(result.score.cer.rate * 100).toFixed(2)}%  实时倍数 ${summary.rtf.toFixed(4)}  加载 ${summary.loadSecMean?.toFixed(1) ?? '-'} 秒  峰值内存 ${summary.peakWorkingSetMb} MB  别的程序占用均值 ${(summary.othersLoadMean * 100).toFixed(0)}%`);
  return result;
}

async function prescreen() {
  const picked = pickThree(sessions);
  const items = itemsFor(picked, 300);
  const threadList = String(flags.threads ?? '8,4').split(',').map(Number);
  for (const cand of cands) {
    for (const threads of threadList) {
      await decodeStage({ cand, name: `prescreen-t${threads}.json`, items, threads, extra: { stage: 'prescreen', sessions: picked.map((s) => s.id), excerptSec: 300 } });
    }
  }
}

async function full() {
  const items = itemsFor(sessions);
  for (const cand of cands) {
    const pre = resultFile(cand, 'prescreen-t8.json');
    if (!fs.existsSync(pre)) throw new Error(`${cand} 还没有预筛结果（${pre}），先跑 prescreen`);
    const est = asrOnlyMinutes(speechFrac, readJson(pre).summary.rtf);
    const budget = GATES.asr.estMinutesMax;
    const file = resultFile(cand, 'full-t8.json');
    if (est > 2 * budget) {
      writeJsonAtomic(file, { cand, name: 'full-t8.json', skipped: true, reason: `预筛得到只算识别的估计用时 ${est.toFixed(1)} 分钟，超过门槛 ${budget} 分钟的 2 倍，不跑全量`, finishedAt: stamp() });
      log(`${cand}：预筛估计 ${est.toFixed(1)} 分钟，超过 ${2 * budget} 分钟，不跑全量（已记录）`);
      continue;
    }
    await decodeStage({ cand, name: 'full-t8.json', items, threads: 8, extra: { stage: 'full', asrOnlyEstimateMinFromPrescreen: est } });
  }
}

async function vad() {
  let chosen = cands;
  if (!flags.cands) {
    const loaded = known.map((id) => ({ id, full: fs.existsSync(resultFile(id, 'full-t8.json')) ? readJson(resultFile(id, 'full-t8.json')) : null }));
    const missing = loaded.filter((c) => !c.full).map((c) => c.id);
    if (missing.length) throw new Error(`这些候选还没有全量结果：${missing.join('、')}。先跑 full，或者用 --cands 指定要做端到端的候选`);
    chosen = finalists({ cands: loaded, speechFrac }).map((c) => c.id);
    log(`决赛圈：${chosen.join('、')}`);
    if (chosen.length === 0) throw new Error('没有候选在速度与内存上达标，没有决赛圈');
  }
  for (const cand of chosen) {
    const file = resultFile(cand, 'vad-t8.json');
    if (fs.existsSync(file) && !force) {
      log(`已有，跳过：${file}`);
      continue;
    }
    const tracks = [];
    const batches = [];
    const texts = [];
    for (const s of sessions) {
      for (const t of s.tracks) {
        const tag = `${s.id}_${t.speaker}`;
        log(`${cand}  VAD 端到端 ${tag}`);
        const r = await decodeWithVad({ run, exe: paths.exe('sherpa-onnx-vad-with-offline-asr'), candId: cand, modelsRoot: paths.models, threads: 8, vadModel: path.join(paths.downloads, 'silero_vad.onnx'), wav: t.wav, outDir: path.join(paths.work, 'asr-out', cand, 'vad-t8'), tag });
        const hyp = r.segments.map((x) => x.text).join('');
        tracks.push({ session: s.id, speaker: t.speaker, ref: refText(t.segments), hyp, segments: r.segments.length, audioSec: t.durationSec });
        texts.push({ session: s.id, speaker: t.speaker, segments: r.segments });
        batches.push(r.batch);
      }
    }
    const pairs = tracks.map((t) => [t.ref, t.hyp]);
    const summary = summarizeBatches(batches);
    const result = { cand, name: 'vad-t8.json', stage: 'vad', threads: 8, tracks: tracks.map(({ ref, hyp, ...rest }) => ({ ...rest, cer: cerTotal([[ref, hyp]]).rate })), summary, batches, score: scorePairs(pairs), punctRate: punctuationRate(tracks.map((t) => t.hyp)), finishedAt: stamp() };
    writeJsonAtomic(file, result);
    writeJsonAtomic(file.replace(/\.json$/, '.texts.json'), texts);
    log(`  端到端 CER ${(result.score.cer.rate * 100).toFixed(2)}%  实时倍数（按整条录音）${summary.rtf.toFixed(4)}  峰值内存 ${summary.peakWorkingSetMb} MB`);
  }
}

const stages = { prescreen, full, vad };
if (!stages[stage]) throw new Error('用法：node scripts/s4/asr.mjs <prescreen|full|vad> --home <S4 目录> [--cands a,b] [--threads 8,4] [--force]');
await stages[stage]();
log(`${stage} 完成`);
