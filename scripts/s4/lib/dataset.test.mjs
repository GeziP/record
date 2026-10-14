import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { cutPlan, cutSegments, extractChannel, farSegmentsBySpeaker, loadSessions, mixTracks, nearSegments, pickMe, RATE, refText, scanDataset, segmentsOfTier, speechFraction } from './dataset.mjs';
import { readWavChannel, readWavInfo, writeWavMono } from './wav.mjs';

// 短格式 TextGrid：tiers = [{ name, intervals: [[起, 止, 文本], ...] }]
function tg(tiers, xmax = 100) {
  const lines = ['File type = "ooTextFile"', 'Object class = "TextGrid"', '', '0', String(xmax), '<exists>', String(tiers.length)];
  for (const t of tiers) {
    lines.push('"IntervalTier"', `"${t.name}"`, '0', String(xmax), String(t.intervals.length));
    for (const [a, b, text] of t.intervals) lines.push(String(a), String(b), `"${text}"`);
  }
  return lines.join('\n') + '\n';
}

function tmp() {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'record-s4-dataset-'));
}

function touch(file) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, '');
}

function fixture(root, { skip = [], extra = [] } = {}) {
  const files = [
    'Eval_Ali_near/audio_dir/R8002_M8002_N_SPK8010.wav',
    'Eval_Ali_near/textgrid_dir/R8002_M8002_N_SPK8010.TextGrid',
    'Eval_Ali_near/audio_dir/R8001_M8001_N_SPK8002.wav',
    'Eval_Ali_near/textgrid_dir/R8001_M8001_N_SPK8002.TextGrid',
    'Eval_Ali_near/audio_dir/R8001_M8001_N_SPK8001.wav',
    'Eval_Ali_near/textgrid_dir/R8001_M8001_N_SPK8001.TextGrid',
    'Eval_Ali_far/audio_dir/R8001_M8001_MS801.wav',
    'Eval_Ali_far/textgrid_dir/R8001_M8001.TextGrid',
    'Eval_Ali_far/audio_dir/R8002_M8002_MS802.wav',
    'Eval_Ali_far/textgrid_dir/R8002_M8002.TextGrid',
    ...extra,
  ].filter((f) => !skip.includes(f));
  for (const f of files) touch(path.join(root, f));
}

test('扫描：会议与参会者都按编号排序，近场、远场与标注都对上', () => {
  const root = tmp();
  fixture(root);
  const { sessions } = scanDataset(root);
  assert.deepEqual(sessions.map((s) => s.id), ['R8001_M8001', 'R8002_M8002']);
  assert.deepEqual(sessions[0].tracks.map((t) => t.speaker), ['SPK8001', 'SPK8002']);
  assert.equal(sessions[0].tracks[0].wav, path.join(root, 'Eval_Ali_near', 'audio_dir', 'R8001_M8001_N_SPK8001.wav'));
  assert.equal(sessions[0].tracks[0].textgrid, path.join(root, 'Eval_Ali_near', 'textgrid_dir', 'R8001_M8001_N_SPK8001.TextGrid'));
  assert.equal(sessions[0].far.wav, path.join(root, 'Eval_Ali_far', 'audio_dir', 'R8001_M8001_MS801.wav'));
  assert.equal(sessions[0].far.textgrid, path.join(root, 'Eval_Ali_far', 'textgrid_dir', 'R8001_M8001.TextGrid'));
  assert.equal(sessions[1].tracks.length, 1);
  fs.rmSync(root, { recursive: true, force: true });
});

test('扫描：缺标注、缺远场、缺近场、文件名不认识、一份远场都没有，都明确报错', () => {
  const cases = [
    [{ skip: ['Eval_Ali_near/textgrid_dir/R8001_M8001_N_SPK8002.TextGrid'] }, /缺文件.*R8001_M8001_N_SPK8002\.TextGrid/],
    [{ skip: ['Eval_Ali_far/textgrid_dir/R8002_M8002.TextGrid'] }, /缺文件.*R8002_M8002\.TextGrid/],
    [{ skip: ['Eval_Ali_far/audio_dir/R8002_M8002_MS802.wav', 'Eval_Ali_far/textgrid_dir/R8002_M8002.TextGrid'] }, /R8002_M8002 没有远场录音/],
    [{ skip: ['Eval_Ali_near/audio_dir/R8002_M8002_N_SPK8010.wav'] }, /R8002_M8002 没有近场录音/],
    [{ extra: ['Eval_Ali_near/audio_dir/oops.wav'] }, /近场录音的文件名不认识：oops\.wav/],
    [{ extra: ['Eval_Ali_far/audio_dir/oops.wav'] }, /远场录音的文件名不认识：oops\.wav/],
    [{ extra: ['Eval_Ali_far/audio_dir/xR8003_M8003_MS803.wav'] }, /远场录音的文件名不认识：xR8003_M8003_MS803\.wav/],
    [{ extra: ['Eval_Ali_near/audio_dir/R8001_M8001_N_SPK8003.WAV'] }, /近场录音的文件名不认识：R8001_M8001_N_SPK8003\.WAV/], // 大写后缀不悄悄略过
    [{ extra: ['Eval_Ali_far/audio_dir/R8001_M8001_MS899.wav'] }, /R8001_M8001 有不止一份远场录音/],
  ];
  for (const [opts, re] of cases) {
    const root = tmp();
    fixture(root, opts);
    assert.throws(() => scanDataset(root), re, JSON.stringify(opts));
    fs.rmSync(root, { recursive: true, force: true });
  }
  const empty = tmp();
  fs.mkdirSync(path.join(empty, 'Eval_Ali_near', 'audio_dir'), { recursive: true });
  fs.mkdirSync(path.join(empty, 'Eval_Ali_far', 'audio_dir'), { recursive: true });
  assert.throws(() => scanDataset(empty), /没有找到任何会议/);
  assert.throws(() => scanDataset(path.join(empty, 'nope')), /缺文件/);
  fs.rmSync(empty, { recursive: true, force: true });
});

test('扫描：不是 wav 的文件忽略，大写后缀的 wav 也认', () => {
  const root = tmp();
  fixture(root, { extra: ['Eval_Ali_near/audio_dir/readme.txt'] });
  assert.equal(scanDataset(root).sessions.length, 2);
  fs.rmSync(root, { recursive: true, force: true });
});

test('近场标注：跳过空文本与零长度的段，按起点排序；不是一层就报错', () => {
  const text = tg([{ name: 'c1', intervals: [[5, 6, '后面'], [0, 1, ''], [1, 1, '零长'], [2, 3, '  '], [1, 2, '前面'], [1, 2.5, '同起点更长']] }]);
  assert.deepEqual(nearSegments(text), [
    { start: 1, end: 2, text: '前面' },
    { start: 1, end: 2.5, text: '同起点更长' },
    { start: 5, end: 6, text: '后面' },
  ]);
  assert.throws(() => nearSegments(tg([{ name: 'a', intervals: [] }, { name: 'b', intervals: [] }])), /应当只有 1 层，得到 2 层/);
  assert.throws(() => nearSegments(tg([])), /应当只有 1 层，得到 0 层/);
});

test('远场标注：层名去掉 N_ 前缀，每位参会者一层；重名报错', () => {
  const text = tg([
    { name: 'N_SPK8001', intervals: [[0, 1, '你好']] },
    { name: 'N_SPK8002', intervals: [[1, 2, '再见']] },
  ]);
  assert.deepEqual(farSegmentsBySpeaker(text), { SPK8001: [{ start: 0, end: 1, text: '你好' }], SPK8002: [{ start: 1, end: 2, text: '再见' }] });
  assert.deepEqual(farSegmentsBySpeaker(tg([{ name: 'SPK8003', intervals: [] }])), { SPK8003: [] });
  assert.deepEqual(farSegmentsBySpeaker(tg([{ name: 'XN_SPK8004', intervals: [] }])), { XN_SPK8004: [] }); // 只去开头的 N_
  assert.throws(() => farSegmentsBySpeaker(tg([{ name: 'N_SPK1', intervals: [] }, { name: 'SPK1', intervals: [] }])), /SPK1 出现了两层/);
  assert.deepEqual(segmentsOfTier({ name: 'x', intervals: [{ xmin: 0, xmax: 1, text: 'a' }] }), [{ start: 0, end: 1, text: 'a' }]);
});

test('说话占比：重叠的段只算一次；参考文本按顺序接起来', () => {
  const segs = [{ start: 0, end: 4, text: '甲' }, { start: 2, end: 6, text: '乙' }, { start: 8, end: 9, text: '丙' }];
  assert.equal(speechFraction(segs, 20), 7 / 20);
  assert.equal(speechFraction([], 20), 0);
  assert.equal(refText(segs), '甲乙丙');
  assert.equal(refText([]), '');
});

test('选“我”：说话最多的；一样多取编号小的；没有人就报错', () => {
  assert.equal(pickMe([{ speaker: 'SPK8001', speechSec: 10 }, { speaker: 'SPK8002', speechSec: 30 }, { speaker: 'SPK8003', speechSec: 20 }]), 'SPK8002');
  assert.equal(pickMe([{ speaker: 'SPK8003', speechSec: 30 }, { speaker: 'SPK8002', speechSec: 30 }, { speaker: 'SPK8004', speechSec: 30 }]), 'SPK8002');
  assert.equal(pickMe([{ speaker: 'SPK8005', speechSec: 1 }]), 'SPK8005');
  assert.throws(() => pickMe([]), /没有参会者/);
});

test('混合：逐点相加，短的补 0；峰值超过 0.99 时整体缩小；不超就不动', () => {
  const a = Float32Array.from([0.1, 0.2, 0.3]);
  const b = Float32Array.from([0.1, 0.1]);
  const m = mixTracks([a, b]);
  assert.equal(m.gain, 1);
  assert.deepEqual(Array.from(m.samples).map((x) => +x.toFixed(6)), [0.2, 0.3, 0.3]);
  const loud = mixTracks([Float32Array.from([0.9, -0.2]), Float32Array.from([0.9, 0.1])]);
  assert.ok(Math.abs(loud.gain - 0.99 / 1.8) < 1e-6);
  assert.ok(Math.abs(loud.samples[0] - 0.99) < 1e-6);
  assert.ok(Math.abs(loud.samples[1] - (-0.1 * 0.99) / 1.8) < 1e-6);
  const neg = mixTracks([Float32Array.from([-1.2, 0.5])]);
  assert.ok(Math.abs(neg.samples[0] + 0.99) < 1e-6); // 负峰值也算
  assert.equal(mixTracks([]).samples.length, 0);
  assert.equal(mixTracks([Float32Array.from([0.98, -0.5])]).gain, 1); // 没到 0.99 不动
});

test('切段计划：四舍五入到帧，超出文件末尾的截断，整段在文件之外的去掉但保留原序号', () => {
  const segs = [{ start: 0.5, end: 1.0 }, { start: 1.0, end: 9.0 }, { start: 12, end: 13 }, { start: 3, end: 3.00001 }];
  assert.deepEqual(cutPlan(segs, 10, 100), [
    { index: 0, startFrame: 5, frames: 5 },
    { index: 1, startFrame: 10, frames: 80 },
  ]);
  assert.deepEqual(cutPlan([{ start: 9.5, end: 11 }], 10, 100), [{ index: 0, startFrame: 95, frames: 5 }]);
  assert.deepEqual(cutPlan([{ start: 0.57, end: 1.0 }], 10, 100), [{ index: 0, startFrame: 6, frames: 4 }]); // 5.7 帧：四舍五入到 6，不是向下取整
  assert.deepEqual(cutPlan([{ start: 10, end: 11 }], 10, 100), []);
});

test('切段与取通道：写出的 wav 是 16 位单声道、长度与内容对得上', () => {
  const dir = tmp();
  const rate = 1000;
  const x = Float32Array.from({ length: 5000 }, (_, i) => Math.sin(i / 20) * 0.5);
  const wav = path.join(dir, 'a.wav');
  writeWavMono(wav, x, rate);
  const n = cutSegments(wav, [{ start: 0.5, end: 1.5 }, { start: 9, end: 10 }, { start: 2, end: 2.25 }], path.join(dir, 'cut'));
  assert.equal(n, 2);
  assert.deepEqual(fs.readdirSync(path.join(dir, 'cut')).sort(), ['0000.wav', '0002.wav']); // 序号对应原来的下标
  const info = readWavInfo(path.join(dir, 'cut', '0002.wav'));
  assert.equal(info.channels, 1);
  assert.equal(info.bits, 16);
  assert.equal(info.sampleRate, rate);
  assert.equal(info.frames, 250);
  const got = readWavChannel(path.join(dir, 'cut', '0000.wav'), 0).samples;
  assert.equal(got.length, 1000);
  for (let i = 0; i < 1000; i += 97) assert.ok(Math.abs(got[i] - x[500 + i]) < 1e-3);

  // 取通道：先写一个 2 通道的 16 位 wav，再取第 2 个通道
  const stereo = Buffer.alloc(44 + 4 * 6);
  stereo.write('RIFF', 0, 'latin1');
  stereo.writeUInt32LE(36 + 24, 4);
  stereo.write('WAVEfmt ', 8, 'latin1');
  stereo.writeUInt32LE(16, 16);
  stereo.writeUInt16LE(1, 20);
  stereo.writeUInt16LE(2, 22);
  stereo.writeUInt32LE(8000, 24);
  stereo.writeUInt32LE(8000 * 4, 28);
  stereo.writeUInt16LE(4, 32);
  stereo.writeUInt16LE(16, 34);
  stereo.write('data', 36, 'latin1');
  stereo.writeUInt32LE(24, 40);
  for (let i = 0; i < 6; i++) {
    stereo.writeInt16LE(1000 * (i + 1), 44 + i * 4);
    stereo.writeInt16LE(-2000 * (i + 1), 46 + i * 4);
  }
  fs.writeFileSync(path.join(dir, 'st.wav'), stereo);
  const r = extractChannel(path.join(dir, 'st.wav'), 1, path.join(dir, 'sub', 'ch2.wav'));
  assert.deepEqual(r, { sampleRate: 8000, frames: 6 });
  const ch2 = readWavChannel(path.join(dir, 'sub', 'ch2.wav'), 0).samples;
  assert.deepEqual(Array.from(ch2).map((v) => Math.round(v * 32768)), [-2000, -4000, -6000, -8000, -10000, -12000]);
  assert.throws(() => extractChannel(path.join(dir, 'st.wav'), 2, path.join(dir, 'x.wav')), /只有 2 个通道/);
  fs.rmSync(dir, { recursive: true, force: true });
});

// 一个很小的完整评测集：一场会议两位参会者，SPK8002 说得更多；每条音轨 8 秒
function buildDataset(root, { nearRate = RATE, farRate = RATE, nearFloat = false, farTiers = ['N_SPK8001', 'N_SPK8002'], stereoNear = false, spk1 = [[0, 1, '你好'], [3, 4, '再见']] } = {}) {
  const put = (rel, buf) => {
    const f = path.join(root, rel);
    fs.mkdirSync(path.dirname(f), { recursive: true });
    fs.writeFileSync(f, buf);
  };
  const wav = (file, rate, { float = false } = {}) => {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    writeWavMono(file, new Float32Array(rate * 8), rate, { float });
    return file;
  };
  const near = (spk, intervals) => {
    const rel = `Eval_Ali_near/audio_dir/R8001_M8001_N_${spk}.wav`;
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    if (stereoNear) {
      const b = Buffer.alloc(44 + 4);
      b.write('RIFF', 0, 'latin1');
      b.writeUInt32LE(40, 4);
      b.write('WAVEfmt ', 8, 'latin1');
      b.writeUInt32LE(16, 16);
      b.writeUInt16LE(1, 20);
      b.writeUInt16LE(2, 22);
      b.writeUInt32LE(RATE, 24);
      b.writeUInt32LE(RATE * 4, 28);
      b.writeUInt16LE(4, 32);
      b.writeUInt16LE(16, 34);
      b.write('data', 36, 'latin1');
      b.writeUInt32LE(4, 40);
      put(rel, b);
    } else {
      wav(path.join(root, rel), nearRate, { float: nearFloat });
    }
    put(`Eval_Ali_near/textgrid_dir/R8001_M8001_N_${spk}.TextGrid`, tg([{ name: 'c1', intervals }], 8));
  };
  near('SPK8001', spk1);
  near('SPK8002', [[1, 3, '中间的话'], [5, 7, '后面的话']]);
  wav(path.join(root, 'Eval_Ali_far/audio_dir/R8001_M8001_MS801.wav'), farRate);
  put('Eval_Ali_far/textgrid_dir/R8001_M8001.TextGrid', tg(farTiers.map((name) => ({ name, intervals: [[0, 1, 'x']] })), 8));
}

test('读入评测集：每位参会者的时长、发言段、说话时长，选出“我”，远场的时长与每人的发言段', () => {
  const root = tmp();
  buildDataset(root);
  const [s, ...rest] = loadSessions(root);
  assert.equal(rest.length, 0);
  assert.equal(s.id, 'R8001_M8001');
  assert.equal(s.me, 'SPK8002');
  assert.deepEqual(s.tracks.map((t) => [t.speaker, t.durationSec, t.speechSec, t.segments.length]), [['SPK8001', 8, 2, 2], ['SPK8002', 8, 4, 2]]);
  assert.deepEqual(s.tracks[1].segments[1], { start: 5, end: 7, text: '后面的话' });
  assert.equal(s.tracks[0].wav, path.join(root, 'Eval_Ali_near', 'audio_dir', 'R8001_M8001_N_SPK8001.wav'));
  assert.equal(s.far.durationSec, 8);
  assert.equal(s.far.channels, 1);
  assert.deepEqual(Object.keys(s.far.bySpeaker), ['SPK8001', 'SPK8002']);
  assert.deepEqual(s.far.bySpeaker.SPK8002, [{ start: 0, end: 1, text: 'x' }]);
  fs.rmSync(root, { recursive: true, force: true });

  const reversed = tmp(); // 远场里各层的先后顺序不要紧
  buildDataset(reversed, { farTiers: ['N_SPK8002', 'N_SPK8001'] });
  assert.equal(loadSessions(reversed)[0].me, 'SPK8002');
  fs.rmSync(reversed, { recursive: true, force: true });

  const overlap = tmp(); // 同一个人的标注段有重叠时，说话时长按合并后算（0～2 与 1～3 合起来是 3 秒）
  buildDataset(overlap, { spk1: [[0, 2, '甲'], [1, 3, '乙']] });
  assert.equal(loadSessions(overlap)[0].tracks[0].speechSec, 3);
  fs.rmSync(overlap, { recursive: true, force: true });
});

test('读入评测集：近场不是 16 kHz 单声道 16 位、远场不是 16 kHz、远场与近场的人对不上，都明确报错', () => {
  const cases = [
    [{ nearRate: 8000 }, /近场录音应当是 16 kHz 单声道 16 位，得到 8000 Hz 1 通道 16 位/],
    [{ nearFloat: true }, /得到 16000 Hz 1 通道 32 位/],
    [{ stereoNear: true }, /得到 16000 Hz 2 通道 16 位/],
    [{ farRate: 8000 }, /远场录音应当是 16 kHz，得到 8000 Hz/],
    [{ farTiers: ['N_SPK8001'] }, /远场标注与近场录音对不上：近场 SPK8001,SPK8002，远场 SPK8001/],
    [{ farTiers: ['N_SPK8001', 'N_SPK8009'] }, /远场标注与近场录音对不上/],
  ];
  for (const [opts, re] of cases) {
    const root = tmp();
    buildDataset(root, opts);
    assert.throws(() => loadSessions(root), re, JSON.stringify(opts));
    fs.rmSync(root, { recursive: true, force: true });
  }
});
