import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { aggregate, curves, diarJobs, EMBEDDINGS, jobKey, runDiarJob, SEGMENTATION_FILE, speakerCounts, THRESHOLDS } from './diar-eval.mjs';

const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), 'record-s4-diareval-'));

// 参考：两个人各 10 秒，轮流说；音轨共 30 秒
const REF = [
  { start: 0, end: 10, speaker: 'SPK1' },
  { start: 10, end: 20, speaker: 'SPK2' },
  { start: 20, end: 30, speaker: 'SPK1' },
];
const mix = (id = 'R1_ME1') => ({ id, wav: `E:/work/${id}.wav`, ref: REF, durationSec: 30, actualSpeakers: 2 });

function fakeDiar({ stdout, exitCode = 0, timedOut = false, duration = 30, elapsed = 6 }) {
  const calls = [];
  const run = async (req) => {
    calls.push(req);
    fs.writeFileSync(req.stdoutFile, `Started\n${stdout}\n`);
    fs.writeFileSync(req.stderrFile, `progress 100%\nDuration : ${duration} s\nElapsed seconds: ${elapsed} s\nReal time factor (RTF): ${elapsed} / ${duration} = ${elapsed / duration}\n`);
    return { exitCode, timedOut, wallMs: 7000, cpuMs: 6500, peakWorkingSetMb: 210, peakPrivateMb: 190 };
  };
  return { run, calls };
}

const base = { exe: 'E:/t/diar.exe', segmentationModel: 'E:/m/seg.onnx', embeddingDir: 'E:/d' };

test('候选与范围：五个声纹模型、阈值 0.3 到 1.0 共八档；分割模型文件', () => {
  assert.deepEqual(EMBEDDINGS.map((e) => e.id), ['campplus', 'eres2net-base', 'eres2net-large', 'eres2netv2', 'wespeaker-resnet34']);
  assert.ok(EMBEDDINGS.every((e) => e.file.endsWith('.onnx')));
  assert.equal(new Set(EMBEDDINGS.map((e) => e.file)).size, 5);
  assert.deepEqual(THRESHOLDS, [0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0]);
  assert.equal(SEGMENTATION_FILE, 'sherpa-onnx-pyannote-segmentation-3-0/model.onnx');
});

test('任务键：阈值保留一位小数，已知人数用 k', () => {
  assert.equal(jobKey('R1_ME1', 'campplus', { threshold: 0.5 }), 'R1_ME1__campplus__t0.50');
  assert.equal(jobKey('R1_ME1', 'campplus', { threshold: 0.30000000000000004 }), 'R1_ME1__campplus__t0.30');
  assert.equal(jobKey('R1_ME1', 'campplus', { threshold: 0.55 }), 'R1_ME1__campplus__t0.55'); // 区间中点不在 0.1 的格子上，键不能和 0.6 混
  assert.equal(jobKey('R1_ME1', 'eres2netv2', { clusters: 3 }), 'R1_ME1__eres2netv2__k3');
});

test('任务清单：每个音轨 × 每个模型 × （每个阈值 + 已知人数）；已知人数用参考里的人数；键都不重复', () => {
  const mixes = [mix('A'), { ...mix('B'), actualSpeakers: 3 }];
  const jobs = diarJobs({ mixes, embeddings: EMBEDDINGS.slice(0, 2), thresholds: [0.4, 0.6, 0.8] });
  assert.equal(jobs.length, 2 * 2 * (3 + 1));
  assert.equal(new Set(jobs.map((j) => j.key)).size, jobs.length);
  const known = jobs.filter((j) => j.clusters !== undefined);
  assert.equal(known.length, 4);
  assert.deepEqual(known.filter((j) => j.mix.id === 'B').map((j) => j.clusters), [3, 3]);
  assert.ok(jobs.filter((j) => j.clusters === undefined).every((j) => typeof j.threshold === 'number' && j.key.includes('__t')));
  assert.equal(diarJobs({ mixes: [mix()], embeddings: EMBEDDINGS.slice(0, 1), thresholds: [0.5], knownCount: false }).length, 1);
  assert.equal(diarJobs({ mixes: [mix()] }).length, 5 * (8 + 1)); // 默认：五个模型、八档阈值
});

test('说话人数：出现过的人数；3 秒以下的碎片另外给一个数', () => {
  const hyp = [{ start: 0, end: 10, speaker: 'speaker_00' }, { start: 10, end: 20, speaker: 'speaker_01' }, { start: 20, end: 21, speaker: 'speaker_02' }];
  assert.deepEqual(speakerCounts(REF, hyp), { actual: 2, estimated: 3, estimated3s: 2 });
});

test('跑一个任务：命令行、优先级、解析输出、按参考打分（说话人编号对不上也能映射）', async () => {
  const dir = tmp();
  const { run, calls } = fakeDiar({ stdout: '0.000 -- 10.000 speaker_01\n10.000 -- 20.000 speaker_00\n20.000 -- 30.000 speaker_01' });
  const job = diarJobs({ mixes: [mix('R1_ME1')], embeddings: [EMBEDDINGS[0]], thresholds: [0.5], knownCount: false })[0];
  const r = await runDiarJob({ run, ...base, job, outDir: dir });
  assert.deepEqual(calls[0].args, [
    '--segmentation.pyannote-model=E:/m/seg.onnx',
    `--embedding.model=${path.join('E:/d', EMBEDDINGS[0].file)}`,
    '--segmentation.num-threads=1',
    '--embedding.num-threads=1',
    '--clustering.cluster-threshold=0.5',
    'E:/work/R1_ME1.wav',
  ]);
  assert.equal(calls[0].priority, 'BelowNormal');
  assert.equal(calls[0].stdoutFile, path.join(dir, 'R1_ME1__campplus__t0.50.out.txt'));
  assert.equal(r.der, 0);
  assert.ok(Math.abs(r.total - 28.5) < 1e-6, `参与评分的说话量 ${r.total}`); // 30 秒扣掉各边界前后 0.25 秒的容差区共 1.5 秒
  assert.equal(r.actualSpeakers, 2);
  assert.equal(r.estimatedSpeakers, 2);
  assert.equal(r.threshold, 0.5);
  assert.equal(r.clusters, null);
  assert.equal(r.embedding, 'campplus');
  assert.equal(r.mixId, 'R1_ME1');
  assert.equal(r.key, 'R1_ME1__campplus__t0.50');
  assert.equal(r.durationSec, 30);
  assert.equal(r.elapsedSec, 6);
  assert.equal(r.rtf, 0.2);
  assert.equal(r.wallMs, 7000);
  assert.equal(r.cpuMs, 6500);
  assert.equal(r.peakWorkingSetMb, 210);
  assert.deepEqual(r.hyp, [[0, 10, 'speaker_01'], [10, 20, 'speaker_00'], [20, 30, 'speaker_01']]);
  fs.rmSync(dir, { recursive: true, force: true });
});

test('跑一个任务：已知人数的命令行；漏掉一个人时 DER 里是漏检；人数多出的碎片计入估计人数', async () => {
  const dir = tmp();
  const { run, calls } = fakeDiar({ stdout: '0.000 -- 10.000 speaker_00\n20.000 -- 30.000 speaker_00\n12.000 -- 13.000 speaker_01' });
  const job = diarJobs({ mixes: [mix()], embeddings: [EMBEDDINGS[1]], thresholds: [], knownCount: true })[0];
  const r = await runDiarJob({ run, ...base, job, outDir: dir, threads: 4, priority: 'Idle' });
  assert.ok(calls[0].args.includes('--clustering.num-clusters=2'));
  assert.ok(!calls[0].args.some((a) => a.startsWith('--clustering.cluster-threshold')));
  assert.ok(calls[0].args.includes('--segmentation.num-threads=4'));
  assert.equal(calls[0].priority, 'Idle');
  assert.equal(r.clusters, 2);
  assert.equal(r.threshold, null);
  assert.equal(r.estimatedSpeakers, 2);
  assert.equal(r.estimatedSpeakers3s, 1);
  // SPK2 的 10～20 秒里只有 12～13 秒被碎片对上；其余扣掉两头的容差区后是 1.75 + 6.75 = 8.5 秒漏检，没有误检与混淆
  assert.ok(Math.abs(r.miss - 8.5) < 1e-6, `漏检 ${r.miss} 秒`);
  assert.ok(r.fa < 1e-9 && r.conf < 1e-9);
  assert.ok(Math.abs(r.der - 8.5 / 28.5) < 1e-9, `DER ${r.der}`);
  fs.rmSync(dir, { recursive: true, force: true });
});

test('跑一个任务：多出一个碎片说话人时估计人数比实际多一；评分只算到音轨末尾，之后的输出不计误检', async () => {
  const dir = tmp();
  const { run } = fakeDiar({ stdout: '0.000 -- 10.000 speaker_00\n10.000 -- 20.000 speaker_01\n20.000 -- 30.000 speaker_00\n25.000 -- 26.000 speaker_02\n30.500 -- 33.000 speaker_00' });
  const job = diarJobs({ mixes: [mix()], embeddings: [EMBEDDINGS[0]], thresholds: [0.5], knownCount: false })[0];
  const r = await runDiarJob({ run, ...base, job, outDir: dir });
  assert.equal(r.actualSpeakers, 2);
  assert.equal(r.estimatedSpeakers, 3);
  assert.equal(r.estimatedSpeakers3s, 2);
  assert.ok(Math.abs(r.fa - 1) < 1e-6, `误检 ${r.fa} 秒：只有 25～26 秒的碎片，30 秒之后的不计`);
  fs.rmSync(dir, { recursive: true, force: true });
});

test('跑一个任务：退出码不是 0 或超时都明确报错', async () => {
  const dir = tmp();
  const job = diarJobs({ mixes: [mix()], embeddings: [EMBEDDINGS[0]], thresholds: [0.5], knownCount: false })[0];
  await assert.rejects(runDiarJob({ run: fakeDiar({ stdout: '', exitCode: 2 }).run, ...base, job, outDir: dir }), /R1_ME1__campplus__t0\.50：退出码 2，见 .*\.err\.txt/);
  await assert.rejects(runDiarJob({ run: fakeDiar({ stdout: '', timedOut: true }).run, ...base, job, outDir: dir }), /超时被杀掉/);
  fs.rmSync(dir, { recursive: true, force: true });
});

const res = (o) => ({ total: 100, miss: 5, fa: 3, conf: 2, actualSpeakers: 3, estimatedSpeakers: 3, ...o });

test('汇总：DER 按总量合计（不是各场比例的平均），人数相差不超过 1 的场次数', () => {
  const a = aggregate([res({ total: 100, miss: 10, fa: 0, conf: 0 }), res({ total: 300, miss: 0, fa: 30, conf: 0, estimatedSpeakers: 5 }), res({ total: 100, miss: 0, fa: 0, conf: 20, estimatedSpeakers: 2 })]);
  assert.equal(a.sessions, 3);
  assert.equal(a.total, 500);
  assert.equal(a.der, 60 / 500); // 各场比例的平均是 0.1，不是这个
  assert.equal(a.miss, 10 / 500);
  assert.equal(a.fa, 30 / 500);
  assert.equal(a.conf, 20 / 500);
  assert.equal(a.countHits, 2); // 差 0、2、1：差 2 的那场不算
  assert.ok(Math.abs(a.countMeanAbsError - 1) < 1e-12);
  assert.throws(() => aggregate([]), /没有结果/);
});

test('曲线：按模型分组、阈值升序、已知人数单独放；设置缺了某场的结果就报错', () => {
  const r = (embedding, threshold, extra = {}) => ({ ...res(extra), embedding, threshold, clusters: null });
  const known = (embedding) => ({ ...res({}), embedding, threshold: null, clusters: 3 });
  const all = [];
  for (const s of [1, 2]) {
    all.push(r('a', 0.7, { miss: 7 }), r('a', 0.3, { miss: 3 }), known('a'), r('b', 0.5, { miss: 1 }));
  }
  const c = curves(all, { sessions: 2 });
  assert.deepEqual(Object.keys(c), ['a', 'b']);
  assert.deepEqual(c.a.points.map((p) => p.threshold), [0.3, 0.7]);
  assert.equal(c.a.points[0].der, (3 + 3 + 2) / 100);
  assert.equal(c.a.points[1].sessions, 2);
  assert.equal(c.a.knownCount.der, 0.1);
  assert.equal(c.b.knownCount, null);
  assert.deepEqual(c.b.points.map((p) => p.threshold), [0.5]);
  assert.throws(() => curves(all.slice(1), { sessions: 2 }), /只有 1\/2 场/);
  assert.throws(() => curves([...all, r('a', 0.3, {})], { sessions: 2 }), /只有 3\/2 场/);
});
