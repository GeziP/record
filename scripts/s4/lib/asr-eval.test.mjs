import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { decodeFiles, decodeWithVad, punctuationRate, scorePairs, summarizeBatches } from './asr-eval.mjs';
import { asrArgs } from './sherpa.mjs';
import { readWavInfo, writeWavMono } from './wav.mjs';

const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), 'record-s4-asreval-'));

function makeWavs(dir, seconds) {
  return seconds.map((s, i) => {
    const f = path.join(dir, `${String(i).padStart(4, '0')}.wav`);
    writeWavMono(f, new Float32Array(Math.round(s * 16000)), 16000);
    return f;
  });
}

// 假的识别程序：每个 wav 一行 JSON（文本取自 texts[文件名]），stderr 按 rtf 编出解码用时
function fakeOffline({ texts = {}, rtf = 0.1, load = 1.5, exitCode = 0, timedOut = false, dropLast = false } = {}) {
  const calls = [];
  const run = async (req) => {
    calls.push(req);
    const files = req.args.filter((a) => a.endsWith('.wav'));
    let audio = 0;
    const lines = files.map((f) => {
      const i = readWavInfo(f);
      audio += i.frames / i.sampleRate;
      return JSON.stringify({ lang: '', emotion: '', text: texts[path.basename(f)] ?? '' });
    });
    if (dropLast) lines.pop();
    fs.writeFileSync(req.stdoutFile, lines.join('\n') + '\n');
    fs.writeFileSync(req.stderrFile, `recognizer created in ${load} s\nStarted\nElapsed seconds: ${audio * rtf} s\nReal time factor (RTF): ${audio * rtf} / ${audio} = ${rtf}\n`);
    return { exitCode, wallMs: 1000, cpuMs: 800, peakWorkingSetMb: 500 + calls.length, peakPrivateMb: 400 + calls.length, timedOut };
  };
  return { run, calls };
}

const common = { exe: 'E:/t/sherpa-onnx-offline.exe', candId: 'paraformer', modelsRoot: 'E:/m', threads: 8 };

test('逐文件识别：按批调用，结果按输入顺序拼回，每批记下耗时、内存与别人的 CPU', async () => {
  const dir = tmp();
  const wavs = makeWavs(dir, [1, 2, 3, 4, 5]);
  const texts = Object.fromEntries(wavs.map((w, i) => [path.basename(w), `第${i}句`]));
  const { run, calls } = fakeOffline({ texts });
  const r = await decodeFiles({ run, ...common, files: wavs, outDir: path.join(dir, 'out'), chunk: { maxCount: 2 } });
  assert.deepEqual(r.texts, ['第0句', '第1句', '第2句', '第3句', '第4句']);
  assert.equal(calls.length, 3);
  assert.deepEqual(calls.map((c) => c.args.filter((a) => a.endsWith('.wav')).length), [2, 2, 1]);
  const base = asrArgs('paraformer', 'E:/m', { threads: 8 });
  assert.deepEqual(calls[0].args.slice(0, base.length), base);
  assert.equal(calls[1].stdoutFile, path.join(dir, 'out', 'batch-1.out.txt'));
  assert.deepEqual(r.batches.map((b) => b.files), [2, 2, 1]);
  assert.deepEqual(r.batches.map((b) => b.audioSec), [3, 7, 5]);
  assert.equal(r.batches[0].loadSec, 1.5);
  assert.equal(r.batches[0].wallMs, 1000);
  assert.equal(r.batches[2].peakWorkingSetMb, 503);
  assert.ok(r.batches.every((b) => b.othersLoad >= 0 && b.othersLoad <= 1));
  fs.rmSync(dir, { recursive: true, force: true });
});

test('别人的 CPU 占用：整机忙碌增量扣掉被测程序自己的 CPU，再除以总时间增量', async () => {
  const dir = tmp();
  const wavs = makeWavs(dir, [1]);
  const snaps = [{ busyMs: 1000, totalMs: 10000 }, { busyMs: 3000, totalMs: 20000 }, { busyMs: 3000, totalMs: 20000 }, { busyMs: 4000, totalMs: 30000 }];
  const snapshot = () => snaps.shift();
  const { run } = fakeOffline(); // cpuMs 固定 800
  const r = await decodeFiles({ run, ...common, files: wavs, outDir: path.join(dir, 'o'), snapshot });
  assert.equal(r.batches[0].othersLoad, (2000 - 800) / 10000);
  const vadRun = async (req) => {
    fs.writeFileSync(req.stdoutFile, '');
    fs.writeFileSync(req.stderrFile, 'Elapsed seconds: 1.0 s\nReal time factor (RTF): 1.0 / 10.000 = 0.100\n');
    return { exitCode: 0, wallMs: 1, cpuMs: 200, peakWorkingSetMb: 1, peakPrivateMb: 1, timedOut: false };
  };
  const v = await decodeWithVad({ run: vadRun, exe: 'e', candId: 'sensevoice', modelsRoot: 'E:/m', threads: 1, vadModel: 'v', wav: 'a.wav', outDir: path.join(dir, 'o2'), tag: 't', snapshot });
  assert.equal(v.batch.othersLoad, (1000 - 200) / 10000);
  fs.rmSync(dir, { recursive: true, force: true });
});

test('逐文件识别：退出码不是 0、超时、输出行数对不上都明确报错，不悄悄略过', async () => {
  const dir = tmp();
  const wavs = makeWavs(dir, [1, 1]);
  const go = (opts) => decodeFiles({ run: fakeOffline(opts).run, ...common, files: wavs, outDir: path.join(dir, 'o'), chunk: {} });
  await assert.rejects(go({ exitCode: 3 }), /paraformer 第 1\/1 批：退出码 3，见 .*batch-0\.err\.txt/);
  await assert.rejects(go({ timedOut: true }), /paraformer 第 1\/1 批：超时被杀掉/);
  await assert.rejects(go({ dropLast: true }), /结果行数不对：期望 2，得到 1/);
  fs.rmSync(dir, { recursive: true, force: true });
});

test('逐文件识别：没有文件时不调用程序', async () => {
  const dir = tmp();
  const { run, calls } = fakeOffline();
  const r = await decodeFiles({ run, ...common, files: [], outDir: path.join(dir, 'o') });
  assert.deepEqual(r, { texts: [], batches: [] });
  assert.equal(calls.length, 0);
  fs.rmSync(dir, { recursive: true, force: true });
});

test('命令行长度：程序路径与各参数都算在内，容量卡到差一个字符时少放一个文件', async () => {
  const dir = tmp();
  const wavs = makeWavs(dir, new Array(30).fill(0.1));
  const base = asrArgs(common.candId, common.modelsRoot, { threads: common.threads });
  const l0 = common.exe.length + 3 + base.reduce((n, a) => n + a.length + 3, 0); // 除了文件以外的长度
  const cost = wavs[0].length + 3; // 每个文件等长
  const sizes = async (maxChars) => {
    const { run, calls } = fakeOffline();
    await decodeFiles({ run, ...common, files: wavs, outDir: path.join(dir, 'o'), chunk: { maxChars } });
    assert.equal(calls.flatMap((c) => c.args.filter((a) => a.endsWith('.wav'))).length, 30); // 一个不丢
    return calls.map((c) => c.args.filter((a) => a.endsWith('.wav')).length);
  };
  assert.deepEqual((await sizes(l0 + 5 * cost)).slice(0, 2), [5, 5]); // 刚好放得下 5 个
  assert.deepEqual((await sizes(l0 + 5 * cost - 1)).slice(0, 2), [4, 4]); // 差一个字符就只放 4 个
  fs.rmSync(dir, { recursive: true, force: true });
});

test('汇总：RTF 用用时之和除以时长之和、加载用时取平均（缺的不算）、内存取最大、别人的占用按耗时加权', () => {
  const b = (o) => ({ files: 10, loadSec: 2, elapsedSec: 5, audioSec: 50, wallMs: 1000, cpuMs: 4000, peakWorkingSetMb: 300, peakPrivateMb: 250, othersLoad: 0.1, ...o });
  const s = summarizeBatches([b({}), b({ loadSec: null, elapsedSec: 15, audioSec: 50, wallMs: 3000, peakWorkingSetMb: 900, peakPrivateMb: 700, othersLoad: 0.3 }), b({ loadSec: 4, files: 5, audioSec: 100, othersLoad: 0.2 })]);
  assert.equal(s.batches, 3);
  assert.equal(s.files, 25);
  assert.equal(s.audioSec, 200);
  assert.equal(s.elapsedSec, 25);
  assert.equal(s.rtf, 25 / 200); // 各批 RTF 的平均是 0.1667，不是这个
  assert.equal(s.loadSecMean, 3);
  assert.equal(s.wallSec, 5);
  assert.equal(s.cpuSec, 12);
  assert.equal(s.peakWorkingSetMb, 900);
  assert.equal(s.peakPrivateMb, 700);
  assert.ok(Math.abs(s.othersLoadMean - (0.1 * 1000 + 0.3 * 3000 + 0.2 * 1000) / 5000) < 1e-12);
  assert.equal(s.othersLoadMax, 0.3);
  assert.equal(summarizeBatches([b({ loadSec: null })]).loadSecMean, null);
  assert.throws(() => summarizeBatches([]), /没有任何批次/);
  assert.equal(summarizeBatches([b({ audioSec: 0 })]).rtf, Infinity);
});

test('打分：CER 合计，识别结果里的阿拉伯数字与空结果单独计数', () => {
  const r = scorePairs([
    ['你好世界', '你好世界'],
    ['二零二四', '2024年'],
    ['再见', ''],
  ]);
  assert.equal(r.cer.refLen, 10);
  assert.equal(r.cer.errors, 3); // 年（插入）一个，再见（删除）两个
  assert.equal(r.cer.rate, 0.3);
  assert.equal(r.digitsInHyp, 4);
  assert.equal(r.emptyHyps, 1);
  assert.equal(scorePairs([['甲', '１２']]).digitsInHyp, 2); // 全角数字也算
  assert.equal(scorePairs([['甲', ' ']]).emptyHyps, 1);
});

test('VAD 端到端：取出各段与文本，记下这一次的耗时与内存', async () => {
  const dir = tmp();
  const calls = [];
  const run = async (req) => {
    calls.push(req);
    fs.writeFileSync(req.stdoutFile, '7.430 -- 12.844: 你好\n13.350 -- 20.620: 世界\n');
    fs.writeFileSync(req.stderrFile, 'Recognizer created!\nElapsed seconds: 15.271 s\nReal time factor (RTF): 15.271 / 1551.820 = 0.010\n');
    return { exitCode: 0, wallMs: 16330, cpuMs: 120000, peakWorkingSetMb: 518, peakPrivateMb: 533, timedOut: false };
  };
  const r = await decodeWithVad({ run, exe: 'E:/t/vad.exe', candId: 'sensevoice', modelsRoot: 'E:/m', threads: 8, vadModel: 'E:/v/silero_vad.onnx', wav: 'E:/d/a.wav', outDir: path.join(dir, 'o'), tag: 'R1_SPK1' });
  assert.deepEqual(r.segments, [{ start: 7.43, end: 12.844, text: '你好' }, { start: 13.35, end: 20.62, text: '世界' }]);
  assert.equal(r.batch.loadSec, null);
  assert.equal(r.batch.audioSec, 1551.82);
  assert.equal(r.batch.elapsedSec, 15.271);
  assert.equal(r.batch.peakWorkingSetMb, 518);
  assert.equal(calls[0].args.at(-1), 'E:/d/a.wav');
  assert.ok(calls[0].args.includes('--silero-vad-model=E:/v/silero_vad.onnx'));
  assert.equal(calls[0].stdoutFile, path.join(dir, 'o', 'R1_SPK1.out.txt'));
  fs.rmSync(dir, { recursive: true, force: true });
});

test('标点比例：标点个数除以非空白字符数（全角、半角标点都算）；没有字符时是 0', () => {
  assert.equal(punctuationRate(['你好，世界。']), 2 / 6);
  assert.equal(punctuationRate(['你好', '，']), 1 / 3); // 多条合在一起算
  assert.equal(punctuationRate(['a b, c']), 1 / 4); // 空白不计入分母
  assert.equal(punctuationRate(['你好 世界']), 0);
  assert.equal(punctuationRate(['！？']), 1);
  assert.equal(punctuationRate(['３，4.']), 2 / 4); // 全角与半角标点都算
  assert.equal(punctuationRate([]), 0);
  assert.equal(punctuationRate(['  ', '']), 0);
  assert.equal(punctuationRate(['1+1=2']), 0); // 加号与等号是符号，不是标点
});
