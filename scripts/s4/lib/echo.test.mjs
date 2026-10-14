import test from 'node:test';
import assert from 'node:assert/strict';
import { convolve, directDelay, erleDb, makeRir, maskFromSegments, mixEcho, mulberry32, rmsOver, shiftMask } from './echo.mjs';

const noise = (n, seed, amp = 1) => {
  const r = mulberry32(seed);
  return Float32Array.from({ length: n }, () => (r() * 2 - 1) * amp);
};
const near = (a, b, tol, msg) => assert.ok(Math.abs(a - b) <= tol, `${msg ?? ''} 期望 ${b}，得到 ${a}`);

test('伪随机数：同一个种子给出同一串数，不同种子不同，范围在 [0,1)', () => {
  const a = mulberry32(5);
  const b = mulberry32(5);
  const xs = Array.from({ length: 50 }, () => a());
  assert.deepEqual(xs, Array.from({ length: 50 }, () => b()));
  assert.ok(xs.every((v) => v >= 0 && v < 1));
  assert.notDeepEqual(xs.slice(0, 5), Array.from({ length: 5 }, mulberry32(6)));
});

test('脉冲响应：直达声在延迟处且为 1，之前全是 0，尾巴能量等于 tailRatio，长度 = 延迟 + 1 + rt60 秒', () => {
  const rate = 16000;
  const h = makeRir({ rate, delayMs: 100, rt60: 0.4, tailRatio: 0.5 });
  const d = 1600;
  assert.equal(h.length, d + 1 + 6400);
  assert.equal(h[d], 1);
  assert.ok(h.subarray(0, d).every((v) => v === 0));
  let e = 0;
  for (let i = d + 1; i < h.length; i++) e += h[i] * h[i];
  near(e, 0.5, 1e-4, '尾巴能量');
  assert.equal(directDelay(h), d);
});

test('脉冲响应：尾巴按指数衰减（头四分之一的能量远大于末四分之一），同种子可复现、不同种子不同', () => {
  const rate = 16000;
  const h = makeRir({ rate, delayMs: 0, rt60: 0.6, seed: 3 });
  const tail = h.subarray(1);
  const q = Math.floor(tail.length / 4);
  const energy = (a, b) => tail.subarray(a, b).reduce((s, v) => s + v * v, 0);
  assert.ok(energy(0, q) > 1000 * energy(tail.length - q, tail.length));
  assert.deepEqual(Array.from(makeRir({ rate, delayMs: 0, rt60: 0.6, seed: 3 })), Array.from(h));
  assert.notDeepEqual(Array.from(makeRir({ rate, delayMs: 0, rt60: 0.6, seed: 4 }).subarray(0, 50)), Array.from(h.subarray(0, 50)));
});

function direct(x, h) {
  const y = new Float64Array(x.length);
  for (let i = 0; i < x.length; i++) for (let k = 0; k < h.length && k <= i; k++) y[i] += h[k] * x[i - k];
  return y;
}

test('卷积与直接卷积一致（跨多个 FFT 块），并且只取前 x.length 个点', () => {
  const x = noise(5000, 1);
  const h = noise(700, 2);
  const y = convolve(x, h);
  const want = direct(x, h);
  assert.equal(y.length, 5000);
  let worst = 0;
  for (let i = 0; i < x.length; i++) worst = Math.max(worst, Math.abs(y[i] - want[i]));
  assert.ok(worst < 1e-3, `最大误差 ${worst}`);
});

test('卷积：单位脉冲在 d 处就是整体右移 d 个点；空输入给空输出', () => {
  const x = noise(300, 9);
  const h = new Float32Array(50);
  h[20] = 1;
  const y = convolve(x, h);
  for (let i = 0; i < 300; i++) near(y[i], i >= 20 ? x[i - 20] : 0, 1e-6, `第 ${i} 点`);
  assert.equal(convolve(new Float32Array(0), h).length, 0);
  assert.deepEqual(Array.from(convolve(x.subarray(0, 4), new Float32Array(0))), [0, 0, 0, 0]);
});

test('掩码：由秒换算成点，越界截断，空段忽略；平移后右边出界的丢掉', () => {
  const m = maskFromSegments([{ start: 0.5, end: 1.0 }, { start: 1.9, end: 5 }, { start: 1.2, end: 1.2 }], 10, 20);
  assert.deepEqual(Array.from(m), [0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
  const s = shiftMask(m, 3);
  assert.deepEqual(Array.from(s), [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0]);
});

test('掩码内均方根：正弦 0.5 的均方根是 0.3536；没有掩码点时是 0', () => {
  const x = Float32Array.from({ length: 16000 }, (_, i) => 0.5 * Math.sin((2 * Math.PI * 440 * i) / 16000));
  const all = new Uint8Array(16000).fill(1);
  near(rmsOver(x, all), 0.5 / Math.SQRT2, 1e-4);
  const half = new Uint8Array(16000);
  half.fill(1, 0, 8000);
  near(rmsOver(x, half), 0.5 / Math.SQRT2, 1e-3);
  assert.equal(rmsOver(x, new Uint8Array(16000)), 0);
});

function scene(delayMs = 100) {
  const rate = 16000;
  const len = 6 * rate;
  const nearSig = new Float32Array(len);
  for (let i = 0; i < rate; i++) nearSig[i] = 0.5 * Math.sin((2 * Math.PI * 300 * i) / rate); // 0～1 秒近端说话
  const far = new Float32Array(len);
  far.set(noise(rate, 7, 0.3), 2 * rate); // 2～3 秒远端说话
  const rir = new Float32Array(Math.round((delayMs * rate) / 1000) + 1);
  rir[rir.length - 1] = 1; // 只有直达声，回声就是远端延迟 delayMs
  return {
    rate,
    near: nearSig,
    far,
    rir,
    nearMask: maskFromSegments([{ start: 0, end: 1 }], rate, len),
    farMask: maskFromSegments([{ start: 2, end: 3 }], rate, len),
  };
}

test('混回声：回声均方根按 ratioDb 相对近端（0 dB 相等，−10 dB 为 0.316 倍，+5 dB 为 1.778 倍）', () => {
  const s = scene();
  for (const [db, factor] of [[0, 1], [-10, 10 ** -0.5], [5, 10 ** 0.25]]) {
    const r = mixEcho({ ...s, ratioDb: db });
    const echoMask = shiftMask(s.farMask, 1600);
    near(rmsOver(r.echo, echoMask) / r.rmsNear, factor, 1e-3, `${db} dB`);
  }
});

test('混回声：回声是远端整体延迟后的样子，近端有声段内没有回声，mic = 近端 + 回声 + 底噪', () => {
  const s = scene(100);
  const r = mixEcho({ ...s, ratioDb: 0, noiseDb: -40 });
  const i0 = 2 * s.rate + 500;
  near(r.echo[i0 + 1600] / r.gain, s.far[i0], 1e-6, '延迟 100 ms 后重现');
  assert.ok(rmsOver(r.echo, s.nearMask) < 1e-9, '近端说话时没有回声');
  const quiet = maskFromSegments([{ start: 4, end: 6 }], s.rate, s.near.length); // 近端、远端、回声都没有的区间，只剩底噪
  near(rmsOver(r.mic, quiet), r.rmsNear * 0.01, r.rmsNear * 0.001, '底噪均方根（−40 dB）');
  const resid = r.mic.map((v, i) => v - s.near[i] - r.echo[i]);
  near(rmsOver(resid, new Uint8Array(resid.length).fill(1)), r.rmsNear * 0.01, r.rmsNear * 0.001, '残差就是底噪');
});

test('混回声：同种子可复现；近端或远端没有有声段时明确报错', () => {
  const s = scene();
  const a = mixEcho({ ...s, ratioDb: 0 });
  const b = mixEcho({ ...s, ratioDb: 0 });
  assert.deepEqual(Array.from(a.mic.subarray(0, 100)), Array.from(b.mic.subarray(0, 100)));
  assert.throws(() => mixEcho({ ...s, ratioDb: 0, nearMask: new Uint8Array(s.near.length) }), /近端没有有声段/);
  assert.throws(() => mixEcho({ ...s, ratioDb: 0, farMask: new Uint8Array(s.near.length) }), /远端没有有声段/);
});

test('ERLE：只在掩码内算；十倍幅度是 20 dB，不变是 0 dB，处理后全零是无穷大', () => {
  const before = Float32Array.from([1, 1, 1, 1, 100, 100]);
  const after = Float32Array.from([0.1, 0.1, 0.1, 0.1, 1, 1]);
  const mask = Uint8Array.from([1, 1, 1, 1, 0, 0]);
  near(erleDb(before, after, mask), 20, 1e-5); // 0.1 在单精度里不精确，所以不能要求到 1e-9
  near(erleDb(before, before, mask), 0, 1e-9);
  assert.equal(erleDb(before, new Float32Array(6), mask), Infinity);
});
