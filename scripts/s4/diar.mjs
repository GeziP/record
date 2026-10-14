// 说话人分离评测。结果写在 <S4 目录>\results\diar\<阶段>\<任务键>.json，已有的结果不重跑（加 --force 重跑）。
//   node scripts/s4/diar.mjs sweep --home <目录> [--workers N]
//       8 场会议的“系统声音”（“我”之外的人各自静音掉没说话的部分再相加）× 5 个声纹模型 ×（阈值 0.3～1.0 步长 0.1 + 已知人数）= 360 个任务，
//       每个任务单线程、后台优先级，同时跑 N 个（默认 CPU 核数减 6）。
//   node scripts/s4/diar.mjs status --home <目录>
//       扫描完成后：各声纹模型的最低 DER 排名、胜出模型的阈值区间与中点（按 decide.mjs 里写死的规则）。
//   node scripts/s4/diar.mjs confirm --home <目录>
//       中点不在已测的阈值上时，在中点把 8 场再测一遍（核对门槛用）。
//   node scripts/s4/diar.mjs far --home <目录>
//       远场：8 场阵列第 1 通道（含所有参会者），用胜出的模型与阈值。
//   node scripts/s4/diar.mjs rotations --home <目录>
//       补充（不是门槛）：“我”依次换成每位参会者，其余 17 种组合用胜出的模型与阈值再测。
//   node scripts/s4/diar.mjs timing --home <目录>
//       计时：三场（头、中、尾）的系统声音，用胜出的模型与阈值，8 线程、正常优先级、一个一个地跑（这时别的程序不要占 CPU）。
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { cpuSnapshot, othersLoad } from './lib/cpu.mjs';
import { log, parseArgs, pickThree, runPool } from './lib/cli.mjs';
import { pickEmbedding } from './lib/decide.mjs';
import { curves, diarJobs, EMBEDDINGS, runDiarJob, SEGMENTATION_FILE } from './lib/diar-eval.mjs';
import { loadSessions } from './lib/dataset.mjs';
import { homeFrom, readJson, s4Paths, writeJsonAtomic } from './lib/home.mjs';
import { measureAsync } from './lib/measure.mjs';
import { buildFarMix, buildMix } from './lib/prep.mjs';

const { positional, flags } = parseArgs(process.argv.slice(2));
const stage = positional[0];
const paths = s4Paths(homeFrom(flags.home));
const sessions = loadSessions(paths.dataset);
const run = (req) => measureAsync({ priority: 'Normal', ...req });
const force = flags.force === true;
const workers = Number(flags.workers ?? Math.max(1, os.cpus().length - 6));
const exe = paths.exe('sherpa-onnx-offline-speaker-diarization');
const segmentationModel = path.join(paths.models, SEGMENTATION_FILE);

const resultDir = (sub) => path.join(paths.results, 'diar', sub);
const resultFile = (sub, key) => path.join(resultDir(sub), `${key}.json`);

function readAll(sub) {
  const dir = resultDir(sub);
  return fs.existsSync(dir) ? fs.readdirSync(dir).filter((f) => f.endsWith('.json')).sort().map((f) => readJson(path.join(dir, f))) : [];
}

const primaryMixes = () => sessions.map((s) => buildMix({ work: paths.work, session: s, me: s.me }));

async function runJobs(jobs, sub, { threads = 1, priority = 'BelowNormal' } = {}) {
  const todo = jobs.filter((j) => force || !fs.existsSync(resultFile(sub, j.key)));
  log(`${sub}：共 ${jobs.length} 个任务，已有 ${jobs.length - todo.length} 个，要跑 ${todo.length} 个，同时 ${workers} 个`);
  let done = 0;
  await runPool(todo, workers, async (job) => {
    const r = await runDiarJob({ run, exe, segmentationModel, embeddingDir: paths.downloads, job, outDir: path.join(paths.work, 'diar-out', sub), threads, priority });
    writeJsonAtomic(resultFile(sub, job.key), r);
    done++;
    log(`  (${done}/${todo.length}) ${job.key}  DER ${(r.der * 100).toFixed(1)}%  人数 ${r.estimatedSpeakers}/${r.actualSpeakers}  ${(r.wallMs / 1000).toFixed(0)} 秒`);
  });
}

/** 扫描结果 → 各声纹模型的曲线与胜出者（任何一个设置缺了哪一场都会报错，不拿残缺的数字去比） */
function winner() {
  const results = readAll('sweep');
  if (results.length === 0) throw new Error('还没有扫描结果，先跑 sweep');
  const c = curves(results, { sessions: sessions.length });
  const models = Object.entries(c).map(([name, v]) => ({ name, points: v.points.map(({ threshold, der, countHits }) => ({ threshold, der, countHits })) }));
  return { pick: pickEmbedding(models), curves: c };
}

const embeddingOf = (pick) => EMBEDDINGS.find((e) => e.id === pick.model);

async function sweep() {
  await runJobs(diarJobs({ mixes: primaryMixes() }), 'sweep');
}

function status() {
  const { pick, curves: c } = winner();
  log('各声纹模型的最低 DER（越低越好）：');
  for (const r of pick.ranking) log(`  ${r.name.padEnd(20)} ${(r.minDer * 100).toFixed(2)}%   已知人数时 ${(c[r.name].knownCount.der * 100).toFixed(2)}%`);
  log(`胜出：${pick.model}，最低 DER ${(pick.minDer * 100).toFixed(2)}%，距最低点 1 个百分点内的阈值区间 ${pick.band.lo}～${pick.band.hi}，取中点 ${pick.threshold}`);
  log(pick.needsConfirm ? '中点不在已测的阈值上，要跑 confirm' : `中点就是已测的阈值：DER ${(pick.point.der * 100).toFixed(2)}%，人数合格 ${pick.point.countHits}/${sessions.length} 场`);
}

async function confirm() {
  const { pick } = winner();
  if (!pick.needsConfirm) return log('区间中点已经在测过的阈值上，不需要再测');
  await runJobs(diarJobs({ mixes: primaryMixes(), embeddings: [embeddingOf(pick)], thresholds: [pick.threshold], knownCount: false }), 'confirm');
}

async function far() {
  const { pick } = winner();
  const mixes = sessions.map((s) => buildFarMix({ work: paths.work, session: s }));
  await runJobs(diarJobs({ mixes, embeddings: [embeddingOf(pick)], thresholds: [pick.threshold], knownCount: false }), 'far');
}

async function rotations() {
  const { pick } = winner();
  const mixes = sessions.flatMap((s) => s.tracks.filter((t) => t.speaker !== s.me).map((t) => buildMix({ work: paths.work, session: s, me: t.speaker })));
  await runJobs(diarJobs({ mixes, embeddings: [embeddingOf(pick)], thresholds: [pick.threshold], knownCount: false }), 'rotations');
}

async function timing() {
  const { pick } = winner();
  const file = path.join(resultDir('timing'), 'timing.json');
  if (fs.existsSync(file) && !force) return log(`已有，跳过：${file}`);
  const embedding = embeddingOf(pick);
  const mixes = pickThree(sessions).map((s) => buildMix({ work: paths.work, session: s, me: s.me }));
  const jobs = diarJobs({ mixes, embeddings: [embedding], thresholds: [pick.threshold], knownCount: false });
  const runs = [];
  for (const job of jobs) {
    log(`计时：${job.key}，8 线程`);
    const before = cpuSnapshot();
    const r = await runDiarJob({ run, exe, segmentationModel, embeddingDir: paths.downloads, job: { ...job, key: `${job.key}__timing` }, outDir: path.join(paths.work, 'diar-out', 'timing'), threads: 8, priority: 'Normal' });
    const others = othersLoad(before, cpuSnapshot(), r.cpuMs);
    runs.push({ mixId: r.mixId, durationSec: r.durationSec, wallMs: r.wallMs, elapsedSec: r.elapsedSec, rtfElapsed: r.rtf, rtfWall: r.wallMs / 1000 / r.durationSec, peakWorkingSetMb: r.peakWorkingSetMb, cpuMs: r.cpuMs, othersLoad: others });
    log(`  实时倍数（含加载）${runs.at(-1).rtfWall.toFixed(4)}  峰值内存 ${r.peakWorkingSetMb} MB  别的程序占用 ${(others * 100).toFixed(0)}%`);
  }
  const sum = (k) => runs.reduce((a, r) => a + r[k], 0);
  writeJsonAtomic(file, { threads: 8, embedding: embedding.id, threshold: pick.threshold, runs, summary: { rtfWall: sum('wallMs') / 1000 / sum('durationSec'), peakWorkingSetMb: Math.max(...runs.map((r) => r.peakWorkingSetMb)), othersLoadMean: sum('othersLoad') / runs.length }, finishedAt: new Date().toISOString() });
}

const stages = { sweep, status, confirm, far, rotations, timing };
if (!stages[stage]) throw new Error('用法：node scripts/s4/diar.mjs <sweep|status|confirm|far|rotations|timing> --home <S4 目录> [--workers N] [--force]');
await stages[stage]();
log(`${stage} 完成`);
