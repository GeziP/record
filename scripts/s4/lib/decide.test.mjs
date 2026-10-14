import test from 'node:test';
import assert from 'node:assert/strict';
import { GATES, checkDiarGate, checkFarFieldGate, pickAec, pickAsr, pickEmbedding, thresholdBand } from './decide.mjs';

test('门槛与设计文档 3.6 一致，并且被冻结（不能在运行时改）', () => {
  assert.deepEqual(GATES.asr, { cerMax: 0.15, estMinutesMax: 30, peakMemGbMax: 4 });
  assert.deepEqual(GATES.diar, { derMax: 0.25, countToleranceMax: 1, countSessionsMin: 6, farFieldDerMax: 0.3 });
  assert.deepEqual(GATES.aec, { insertionMax: 0.05, nearCerIncreaseMax: 0.02 });
  assert.equal(GATES.asrTieBand, 0.005);
  assert.equal(GATES.thresholdBand, 0.01);
  for (const o of [GATES, GATES.asr, GATES.diar, GATES.aec]) assert.ok(Object.isFrozen(o));
  assert.throws(() => {
    'use strict';
    GATES.asr.cerMax = 0.5;
  }, TypeError);
});

const asr = (name, cer, estMinutes, peakMemGb = 1) => ({ name, cer, estMinutes, peakMemGb });

test('识别：只在速度与内存都达标的候选里挑 CER 最低的', () => {
  const r = pickAsr([asr('慢但准', 0.05, 45), asr('快', 0.1, 10), asr('占内存', 0.04, 10, 6), asr('中', 0.12, 20)]);
  assert.equal(r.status, 'ok');
  assert.equal(r.chosen.name, '快');
  assert.deepEqual(r.eligible.map((c) => c.name).sort(), ['中', '快']);
});

test('识别：CER 与最低者相差在 0.5 个百分点内的取更快的；超过就不取', () => {
  const within = pickAsr([asr('A', 0.1, 20), asr('B', 0.1049, 10)]);
  assert.equal(within.chosen.name, 'B');
  const beyond = pickAsr([asr('A', 0.1, 20), asr('B', 0.1051, 10)]);
  assert.equal(beyond.chosen.name, 'A');
  // 比较的是与“最低者”的差，不是链式的：C 比 B 只差 0.4 点，但比 A 差 0.8 点，不能因 B 入选而把 C 也算进来
  const chain = pickAsr([asr('A', 0.1, 30), asr('B', 0.104, 25), asr('C', 0.108, 5)]);
  assert.equal(chain.chosen.name, 'B');
});

test('识别：门槛上的数字（15%、30 分钟、4 GB）恰好等于门槛算达标，略超算不达标', () => {
  assert.equal(pickAsr([asr('A', 0.15, 30, 4)]).status, 'ok');
  assert.equal(pickAsr([asr('A', 0.1501, 30, 4)]).status, 'cer-gate-failed');
  assert.equal(pickAsr([asr('A', 0.1, 30.01, 4)]).status, 'none-eligible');
  assert.equal(pickAsr([asr('A', 0.1, 30, 4.01)]).status, 'none-eligible');
});

test('识别：浮点误差不改变结论（0.1 + 0.05 在计算机里是 0.15000000000000002，仍然算 15%；0.009 与 0.014 相差恰好 0.5 个百分点）', () => {
  assert.equal(pickAsr([asr('A', 0.1 + 0.05, 30, 4)]).status, 'ok');
  assert.equal(pickAsr([asr('A', 0.009, 20), asr('B', 0.014, 10)]).chosen.name, 'B');
});

test('识别：没有候选达标时不给默认，并把原因说清楚', () => {
  const none = pickAsr([asr('A', 0.1, 50)]);
  assert.equal(none.status, 'none-eligible');
  assert.equal(none.chosen, undefined);
  const bad = pickAsr([asr('A', 0.2, 10), asr('B', 0.3, 5)]);
  assert.equal(bad.status, 'cer-gate-failed');
  assert.equal(bad.chosen.name, 'A');
  assert.match(bad.reason, /20\.00%/);
});

test('阈值区间：从最低点向两侧延伸到 DER 超出 1 个百分点为止，取中点', () => {
  const pts = [[0.3, 0.3], [0.4, 0.2], [0.5, 0.151], [0.6, 0.15], [0.7, 0.155], [0.8, 0.17], [0.9, 0.3]].map(([threshold, der]) => ({ threshold, der }));
  const b = thresholdBand(pts);
  assert.equal(b.best.threshold, 0.6);
  assert.equal(b.lo, 0.5);
  assert.equal(b.hi, 0.7);
  assert.equal(b.mid, 0.6);
});

test('阈值区间：区间不连续时不能跨过超标的点；输入乱序也行；最低点在边界上', () => {
  const pts = [[0.9, 0.14], [0.3, 0.14], [0.5, 0.3], [0.4, 0.145], [0.8, 0.5]].map(([threshold, der]) => ({ threshold, der }));
  const b = thresholdBand(pts);
  assert.equal(b.lo, 0.3);
  assert.equal(b.hi, 0.4);
  assert.equal(b.mid, 0.35);
  assert.equal(thresholdBand([{ threshold: 0.5, der: 0.2 }]).mid, 0.5);
});

const model = (name, ders, hits = 8) => ({ name, points: ders.map((der, i) => ({ threshold: Math.round((0.3 + i * 0.1) * 10) / 10, der, countHits: hits })) });

test('阈值区间：浮点误差不改变结论（0.06 加 1 个百分点在计算机里是 0.06999999999999999，0.07 仍在区间内）', () => {
  const b = thresholdBand([[0.3, 0.2], [0.4, 0.06], [0.5, 0.07], [0.6, 0.2]].map(([threshold, der]) => ({ threshold, der })));
  assert.equal(b.lo, 0.4);
  assert.equal(b.hi, 0.5);
  assert.equal(b.mid, 0.45);
});

test('声纹模型：取各自最低 DER 里最低的那个，并按它的曲线取阈值', () => {
  const r = pickEmbedding([
    model('甲', [0.3, 0.25, 0.2, 0.19, 0.2, 0.25, 0.3]),
    model('乙', [0.3, 0.2, 0.15, 0.14, 0.145, 0.2, 0.3]),
  ]);
  assert.equal(r.model, '乙');
  assert.equal(r.minDer, 0.14);
  assert.equal(r.band.lo, 0.5);
  assert.equal(r.band.hi, 0.7);
  assert.equal(r.threshold, 0.6);
  assert.equal(r.needsConfirm, false);
  assert.equal(r.point.threshold, 0.6);
  assert.deepEqual(r.ranking.map((x) => x.name), ['乙', '甲']);
});

test('声纹模型：中点落在两个已测阈值之间时要求补测', () => {
  const r = pickEmbedding([model('甲', [0.3, 0.2, 0.15, 0.151, 0.3, 0.4, 0.5])]);
  assert.equal(r.threshold, 0.55);
  assert.equal(r.needsConfirm, true);
  assert.equal(r.point, null);
});

test('分离门槛：DER ≤ 25% 且人数相差不超过 1 的场次 ≥ 6；远场 DER ≤ 30%', () => {
  assert.deepEqual(checkDiarGate({ der: 0.25, countHits: 6 }), { derOk: true, countOk: true, pass: true });
  assert.equal(checkDiarGate({ der: 0.2501, countHits: 8 }).pass, false);
  assert.equal(checkDiarGate({ der: 0.1, countHits: 5 }).pass, false);
  assert.equal(checkFarFieldGate(0.3).pass, true);
  assert.equal(checkFarFieldGate(0.3001).pass, false);
});

const cond = (id, insertion, nearCerIncrease) => ({ id, insertion, nearCerIncrease });

test('回声：所有条件都达标才算达标；达标的取平均近端 CER 增加最小者', () => {
  const r = pickAec([
    { name: '甲', conditions: [cond('a', 0.01, 0.01), cond('b', 0.02, 0.015)] },
    { name: '乙', conditions: [cond('a', 0.01, 0.005), cond('b', 0.02, 0.01)] },
    { name: '丙', conditions: [cond('a', 0.01, 0.0), cond('b', 0.06, 0.0)] },
  ]);
  assert.equal(r.status, 'ok');
  assert.equal(r.chosen, '乙');
  assert.deepEqual(r.report.find((x) => x.name === '丙').failed, ['b']);
  assert.ok(Math.abs(r.report.find((x) => x.name === '乙').meanIncrease - 0.0075) < 1e-12, '报告里的平均值');
});

test('回声：门槛上的数字算达标，略超不算；全都不达标就是不做', () => {
  assert.equal(pickAec([{ name: '甲', conditions: [cond('a', 0.05, 0.02)] }]).status, 'ok');
  assert.equal(pickAec([{ name: '甲', conditions: [cond('a', 0.0501, 0.0)] }]).status, 'none');
  assert.equal(pickAec([{ name: '甲', conditions: [cond('a', 0.0, 0.0201)] }]).status, 'none');
  const none = pickAec([{ name: '甲', conditions: [cond('a', 0.5, 0.5)] }]);
  assert.match(none.reason, /戴耳机/);
  assert.equal(none.chosen, undefined);
});
