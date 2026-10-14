import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { loadSessions, RATE } from './dataset.mjs';
import { mulberry32 } from './echo.mjs';
import { buildFarMix, buildMix, farLagSec, gateToSpeech, mixId, prepareTrackSegments, segDir, segmentItems } from './prep.mjs';
import { readWavChannel, readWavInfo, writeWavMono } from './wav.mjs';

function tmp() {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'record-s4-prep-'));
}

function tg(tiers, xmax = 3) {
  const lines = ['File type = "ooTextFile"', 'Object class = "TextGrid"', '', '0', String(xmax), '<exists>', String(tiers.length)];
  for (const t of tiers) {
    lines.push('"IntervalTier"', `"${t.name}"`, '0', String(xmax), String(t.intervals.length));
    for (const [a, b, text] of t.intervals) lines.push(String(a), String(b), `"${text}"`);
  }
  return lines.join('\n') + '\n';
}

// 16 位双通道 wav：通道 0 与通道 1 的采样值
function stereoWav(file, ch0, ch1, rate) {
  const n = ch0.length;
  const b = Buffer.alloc(44 + n * 4);
  b.write('RIFF', 0, 'latin1');
  b.writeUInt32LE(36 + n * 4, 4);
  b.write('WAVEfmt ', 8, 'latin1');
  b.writeUInt32LE(16, 16);
  b.writeUInt16LE(1, 20);
  b.writeUInt16LE(2, 22);
  b.writeUInt32LE(rate, 24);
  b.writeUInt32LE(rate * 4, 28);
  b.writeUInt16LE(4, 32);
  b.writeUInt16LE(16, 34);
  b.write('data', 36, 'latin1');
  b.writeUInt32LE(n * 4, 40);
  for (let i = 0; i < n; i++) {
    b.writeInt16LE(Math.round(ch0[i] * 32768), 44 + i * 4);
    b.writeInt16LE(Math.round(ch1[i] * 32768), 46 + i * 4);
  }
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, b);
}

// 一场四人会议，每条近场录音 2 秒、是一个恒定的值；远场录音 2.5 秒。
// 发言段故意交错：SPK8001 的第二段在最后；SPK8001 与 SPK8003 有起止都相同的段；SPK8003 与 SPK8002 有起点相同、终点不同的段；
// SPK8004 的第二段整个落在录音之外（2.5～3 秒）。
const LEVELS = { SPK8001: 0.3, SPK8002: 0.5, SPK8003: 0.2, SPK8004: 0.45 };
const SEGS = {
  SPK8001: [[0, 1, '甲一'], [1.95, 2, '甲二']],
  SPK8002: [[0.5, 1.5, '乙一'], [1.6, 1.9, '乙二']],
  SPK8003: [[0, 1, '丙零'], [0.5, 1.2, '丙短'], [1, 2, '丙一']],
  SPK8004: [[0.6, 0.8, '丁一'], [2.5, 3, '丁在外']],
};

function build(root) {
  for (const [spk, level] of Object.entries(LEVELS)) {
    const w = path.join(root, `Eval_Ali_near/audio_dir/R8001_M8001_N_${spk}.wav`);
    fs.mkdirSync(path.dirname(w), { recursive: true });
    writeWavMono(w, new Float32Array(RATE * 2).fill(level), RATE);
    const f = path.join(root, `Eval_Ali_near/textgrid_dir/R8001_M8001_N_${spk}.TextGrid`);
    fs.mkdirSync(path.dirname(f), { recursive: true });
    fs.writeFileSync(f, tg([{ name: 'c1', intervals: SEGS[spk] }]));
  }
  stereoWav(path.join(root, 'Eval_Ali_far/audio_dir/R8001_M8001_MS801.wav'), new Float32Array(RATE * 2.5).fill(0.1), new Float32Array(RATE * 2.5).fill(-0.2), RATE);
  const ftg = path.join(root, 'Eval_Ali_far/textgrid_dir/R8001_M8001.TextGrid');
  fs.mkdirSync(path.dirname(ftg), { recursive: true });
  fs.writeFileSync(ftg, tg(Object.keys(LEVELS).map((spk) => ({ name: `N_${spk}`, intervals: SEGS[spk] }))));
  return loadSessions(root)[0];
}

test('段文件：切出的文件按下标命名，整段在录音之外的段没有文件但不影响别的段的序号', () => {
  const root = tmp();
  const work = path.join(root, 'work');
  const s = build(root);
  const t = s.tracks.find((x) => x.speaker === 'SPK8002');
  assert.equal(prepareTrackSegments({ work, sessionId: s.id, track: t }), 2);
  const dir = segDir(work, s.id, 'SPK8002');
  assert.equal(dir, path.join(work, 'segs', 'R8001_M8001', 'SPK8002'));
  const items = segmentItems(dir, t.segments);
  assert.deepEqual(items.map((x) => [x.index, path.basename(x.file), x.start, x.end, x.text]), [[0, '0000.wav', 0.5, 1.5, '乙一'], [1, '0001.wav', 1.6, 1.9, '乙二']]);
  assert.equal(readWavInfo(items[1].file).frames, Math.round(1.9 * RATE) - Math.round(1.6 * RATE));
  const d = s.tracks.find((x) => x.speaker === 'SPK8004');
  assert.equal(prepareTrackSegments({ work, sessionId: s.id, track: d }), 1); // 第二段在录音之外，没有文件
  assert.deepEqual(segmentItems(segDir(work, s.id, 'SPK8004'), d.segments).map((x) => [x.index, path.basename(x.file)]), [[0, '0000.wav']]);
  const outside = { speaker: 'SPKO', wav: t.wav, segments: [{ start: 5, end: 6, text: '外' }] };
  assert.equal(prepareTrackSegments({ work, sessionId: s.id, track: outside }), 0); // 唯一的一段在录音之外：一个文件也没有
  assert.deepEqual(segmentItems(segDir(work, s.id, 'SPKO'), outside.segments), []);
  // 前面的段在录音之外、后面的段在录音之内：序号仍然是原来的下标
  const mixed = { speaker: 'SPKX', wav: t.wav, segments: [{ start: 5, end: 6, text: '外' }, { start: 0.1, end: 0.2, text: '内' }] };
  assert.equal(prepareTrackSegments({ work, sessionId: s.id, track: mixed }), 1);
  assert.deepEqual(segmentItems(segDir(work, s.id, 'SPKX'), mixed.segments).map((x) => [x.index, path.basename(x.file), x.text]), [[1, '0001.wav', '内']]);
  fs.rmSync(root, { recursive: true, force: true });
});

test('段文件：有 .done 就跳过不再切；没有 .done（上次被打断）就重新切', () => {
  const root = tmp();
  const work = path.join(root, 'work');
  const s = build(root);
  const t = s.tracks.find((x) => x.speaker === 'SPK8001');
  const dir = segDir(work, s.id, 'SPK8001');
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, '.done'), '99');
  assert.equal(prepareTrackSegments({ work, sessionId: s.id, track: t }), 99);
  assert.equal(fs.existsSync(path.join(dir, '0000.wav')), false, '有 .done 时不重新切');
  fs.rmSync(path.join(dir, '.done'));
  assert.equal(prepareTrackSegments({ work, sessionId: s.id, track: t }), 2);
  assert.equal(fs.readFileSync(path.join(dir, '.done'), 'utf8'), '2');
  assert.equal(fs.existsSync(path.join(dir, '0001.wav')), true);
  fs.rmSync(root, { recursive: true, force: true });
});

test('系统声音混合：其余参会者各自静音掉没在说话的部分后逐点相加（峰值超过 0.99 时整体缩小），人数不含“我”', () => {
  const root = tmp();
  const work = path.join(root, 'work');
  const s = build(root);
  const m = buildMix({ work, session: s, me: 'SPK8003' });
  assert.equal(m.id, 'R8001_M8001__me-SPK8003');
  assert.equal(mixId('R8001_M8001', 'SPK8003'), m.id);
  assert.equal(m.wav, path.join(work, 'mix-gated', 'R8001_M8001__me-SPK8003.wav'));
  assert.equal(m.me, 'SPK8003');
  assert.equal(m.durationSec, 2);
  assert.equal(m.actualSpeakers, 3);
  const x = readWavChannel(m.wav, 0).samples;
  const at = (sec) => x[Math.round(sec * RATE)];
  const gain = 0.99 / 1.25; // 0.7 秒时 SPK8001、SPK8002、SPK8004 都在说话：0.3 + 0.5 + 0.45 = 1.25 超过 0.99，整体缩小到峰值 0.99
  assert.ok(Math.abs(at(0.7) - 0.99) < 1e-3, `得到 ${at(0.7)}`);
  assert.ok(Math.abs(at(0.05) - 0.3 * gain) < 1e-3, `0.05 秒时只有 SPK8001（放宽 0.2 秒的范围内）在说话；得到 ${at(0.05)}`);
  assert.ok(Math.abs(at(1.99) - 0.8 * gain) < 1e-3, `1.99 秒时 SPK8001 与 SPK8002 在说话，SPK8004 早已安静；得到 ${at(1.99)}`);
  assert.ok(Math.abs(at(1.25) - 0.5 * gain) < 1e-3, `1.25 秒时 SPK8001 已过放宽范围（1.2 秒），只有 SPK8002 在说话；得到 ${at(1.25)}`);
  const wide = buildMix({ work: path.join(root, 'work2'), session: s, me: 'SPK8003', marginSec: 0.5 }); // 放宽 0.5 秒：1.4 秒时 SPK8001（到 1.5 秒）也还在
  assert.ok(Math.abs(readWavChannel(wide.wav, 0).samples[Math.round(1.4 * RATE)] - (0.3 + 0.5) * gain) < 1e-3);
  const only = buildMix({ work, session: s, me: 'SPK8002' }); // 0.7 秒：0.3 + 0.2 + 0.45 = 0.95，不缩放
  assert.ok(Math.abs(readWavChannel(only.wav, 0).samples[Math.round(0.7 * RATE)] - 0.95) < 1e-3);
  fs.rmSync(root, { recursive: true, force: true });
});

test('系统声音混合的参考：带说话人，按起点排序，起点相同按终点，起止都相同保持参会者编号的先后', () => {
  const root = tmp();
  const work = path.join(root, 'work');
  const s = build(root);
  const m = buildMix({ work, session: s, me: 'SPK8004' });
  assert.deepEqual(
    m.ref.map((r) => [r.speaker, r.start, r.end]),
    [
      ['SPK8001', 0, 1],
      ['SPK8003', 0, 1], // 起止都相同：保持编号的先后
      ['SPK8003', 0.5, 1.2], // 起点相同，终点短的在前
      ['SPK8002', 0.5, 1.5],
      ['SPK8003', 1, 2],
      ['SPK8002', 1.6, 1.9],
      ['SPK8001', 1.95, 2],
    ],
  );
  assert.equal(m.actualSpeakers, 3);
  fs.rmSync(root, { recursive: true, force: true });
});

test('系统声音混合：已有就沿用；“我”不在会议里、只有“我”一个人都报错', () => {
  const root = tmp();
  const work = path.join(root, 'work');
  const s = build(root);
  const m = buildMix({ work, session: s, me: 'SPK8001' });
  writeWavMono(m.wav, new Float32Array(RATE * 5), RATE); // 换成一个 5 秒的文件，再调一次应当沿用它
  assert.equal(buildMix({ work, session: s, me: 'SPK8001' }).durationSec, 5);
  assert.equal(fs.existsSync(`${m.wav}.tmp`), false);
  assert.throws(() => buildMix({ work, session: s, me: 'SPK9999' }), /没有参会者 SPK9999/);
  const solo = { id: 'R9', tracks: [s.tracks[0]] };
  assert.throws(() => buildMix({ work, session: solo, me: s.tracks[0].speaker }), /只有 SPK8001 一位参会者/);
  fs.rmSync(root, { recursive: true, force: true });
});

test('远场混合：取第 1 通道，参考是所有参会者（含“我”）的发言段，时长是远场自己的', () => {
  const root = tmp();
  const work = path.join(root, 'work');
  const s = build(root);
  const f = buildFarMix({ work, session: s });
  assert.equal(f.id, 'R8001_M8001__far');
  assert.equal(f.wav, path.join(work, 'far', 'R8001_M8001.ch1.wav'));
  assert.equal(f.me, null);
  assert.equal(f.durationSec, 2.5);
  assert.equal(f.actualSpeakers, 4);
  assert.deepEqual(
    f.ref.map((r) => [r.speaker, r.start, r.end]),
    [
      ['SPK8001', 0, 1],
      ['SPK8003', 0, 1],
      ['SPK8003', 0.5, 1.2],
      ['SPK8002', 0.5, 1.5],
      ['SPK8004', 0.6, 0.8],
      ['SPK8003', 1, 2],
      ['SPK8002', 1.6, 1.9],
      ['SPK8001', 1.95, 2],
      ['SPK8004', 2.5, 3],
    ],
  );
  assert.ok(Math.abs(readWavChannel(f.wav, 0).samples[10] - 0.1) < 1e-3, '取的是通道 0（0.1），不是通道 1（−0.2）');
  fs.rmSync(f.wav);
  fs.mkdirSync(path.dirname(f.wav), { recursive: true });
  writeWavMono(f.wav, new Float32Array(10), RATE); // 已有就沿用
  assert.equal(readWavInfo(buildFarMix({ work, session: s }).wav).frames, 10);
  assert.equal(fs.existsSync(`${f.wav}.tmp`), false);
  fs.rmSync(root, { recursive: true, force: true });
});

// 每 0.5 秒随机开关一次的噪声
function bursts(seed, seconds = 30) {
  const rnd = mulberry32(seed);
  const x = new Float32Array(RATE * seconds);
  for (let k = 0; k < seconds * 2; k++) {
    if (rnd() < 0.5) for (let i = k * RATE * 0.5; i < (k + 1) * RATE * 0.5; i++) x[i] = (rnd() - 0.5) * 0.4;
  }
  return x;
}

function shifted(x, sec) {
  const d = Math.round(sec * RATE);
  const y = new Float32Array(x.length);
  for (let i = 0; i < y.length; i++) y[i] = x[i - d] ?? 0;
  return y;
}

test('远场延迟：晚 0.30 秒得 +0.3、早 0.20 秒得 −0.2、对齐得 0、晚 3 秒得 +3（搜索范围是秒，不是格数）', () => {
  const dir = tmp();
  const a = bursts(11);
  const b = bursts(22, 25); // 比 a 短，近场相加时要补长
  const wa = path.join(dir, 'a.wav');
  const wb = path.join(dir, 'b.wav');
  writeWavMono(wa, a, RATE);
  writeWavMono(wb, b, RATE);
  const sum = new Float32Array(a.length);
  for (let i = 0; i < a.length; i++) sum[i] = a[i] + (i < b.length ? b[i] : 0);
  for (const shift of [0.3, -0.2, 0, 3]) {
    const far = path.join(dir, `far${shift}.wav`);
    writeWavMono(far, shifted(sum, shift), RATE);
    const r = farLagSec({ nearWavs: [wa, wb], farWav: far });
    assert.ok(Math.abs(r.lagSec - shift) <= 0.011, `移 ${shift} 秒，测得 ${r.lagSec}`);
    assert.ok(r.peak > 0);
    assert.equal(r.nearSec, 30);
    assert.equal(r.farSec, 30);
  }
  const trimmed = path.join(dir, 'trimmed.wav'); // 远场比近场短 2 秒
  writeWavMono(trimmed, shifted(sum, 0.3).subarray(0, 28 * RATE), RATE);
  const t = farLagSec({ nearWavs: [wa, wb], farWav: trimmed });
  assert.equal(t.farSec, 28);
  assert.equal(t.nearSec, 30);
  assert.ok(Math.abs(t.lagSec - 0.3) <= 0.011);
  fs.rmSync(dir, { recursive: true, force: true });
});

test('远场延迟：近场相加是真相加（远场只含较短的那条时，先给短的、再给长的也能对上）；搜索范围可以收窄', () => {
  const dir = tmp();
  const a = bursts(11); // 30 秒
  const b = bursts(22, 25); // 25 秒
  const wa = path.join(dir, 'a.wav');
  const wb = path.join(dir, 'b.wav');
  writeWavMono(wa, a, RATE);
  writeWavMono(wb, b, RATE);
  const far = path.join(dir, 'far.wav'); // 远场只有 b，晚 0.3 秒；a 是干扰。近场相加漏了 b（覆盖或补长时丢掉已有的）就对不上
  writeWavMono(far, shifted(b, 0.3), RATE);
  for (const order of [[wb, wa], [wa, wb]]) {
    const r = farLagSec({ nearWavs: order, farWav: far });
    assert.ok(Math.abs(r.lagSec - 0.3) <= 0.011, `顺序 ${order.map((p) => path.basename(p))}：测得 ${r.lagSec}`);
  }
  const wide = path.join(dir, 'far3.wav');
  writeWavMono(wide, shifted(b, 3), RATE);
  const narrow = farLagSec({ nearWavs: [wb], farWav: wide, maxLagSec: 0.2 });
  assert.ok(Math.abs(narrow.lagSec) <= 0.2 + 1e-9, `范围外的 3 秒找不到，得到 ${narrow.lagSec}`);
  fs.rmSync(dir, { recursive: true, force: true });
});

test('静音：发言段前后各放宽 marginSec 秒保留原样，其余置 0；放宽到文件之外的部分不越界', () => {
  const x = new Float32Array(RATE * 4).fill(0.5);
  const g = gateToSpeech(x, [{ start: 1, end: 2 }], 0.5);
  const at = (sec) => g[Math.round(sec * RATE)];
  assert.equal(at(0.49), 0);
  assert.equal(at(0.51), 0.5);
  assert.equal(at(1.5), 0.5);
  assert.equal(at(2.49), 0.5);
  assert.equal(at(2.51), 0);
  assert.equal(at(3.9), 0);
  assert.equal(g.length, x.length);
  assert.equal(x[0], 0.5, '输入不被改动');
  const edge = gateToSpeech(x, [{ start: 0.1, end: 3.9 }], 0.5); // 前后各放宽 0.5 秒都超出了信号：整段保留
  assert.equal(edge.every((v) => v === 0.5), true);
  const def = gateToSpeech(x, [{ start: 1, end: 2 }]); // 默认放宽 0.2 秒
  assert.equal(def[Math.round(0.79 * RATE)], 0);
  assert.equal(def[Math.round(0.81 * RATE)], 0.5);
  assert.equal(gateToSpeech(x, []).every((v) => v === 0), true);
});
