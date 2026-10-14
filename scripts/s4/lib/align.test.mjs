import test from 'node:test';
import assert from 'node:assert/strict';
import { envelope, lagBetween } from './align.mjs';
import { mulberry32 } from './echo.mjs';

test('包络：每格 hop 个点的对数均方能量，不足一格的尾巴丢掉，全零不出负无穷', () => {
  const x = Float32Array.from([0.5, 0.5, 0.5, 0.5, 1, 1, 1]);
  const e = envelope(x, 2);
  assert.equal(e.length, 3);
  assert.ok(Math.abs(e[0] - Math.log10(0.25 + 1e-8)) < 1e-12);
  assert.ok(Math.abs(e[2] - Math.log10(1 + 1e-8)) < 1e-12);
  assert.equal(envelope(new Float32Array(10), 5)[0], -8);
  assert.equal(envelope(new Float32Array(3), 5).length, 0);
});

// 一串忽强忽弱的“说话段”：用固定种子的随机数决定每格是否有声
function bursts(n, seed) {
  const r = mulberry32(seed);
  const out = new Float64Array(n);
  let on = false;
  for (let i = 0; i < n; i++) {
    if (r() < 0.05) on = !on;
    out[i] = (on ? 1 : -3) + 0.2 * (r() - 0.5);
  }
  return out;
}

test('延迟：b 比 a 晚 37 格得 +37，早 37 格得 −37，没错位得 0', () => {
  const a = bursts(3000, 7);
  const shift = (d) => {
    const b = new Float64Array(a.length);
    for (let i = 0; i < a.length; i++) b[i] = a[i - d] ?? -3;
    return b;
  };
  assert.equal(lagBetween(a, shift(37), 100).lag, 37);
  assert.equal(lagBetween(a, shift(-37), 100).lag, -37);
  assert.equal(lagBetween(a, shift(0), 100).lag, 0);
  assert.equal(lagBetween(a, shift(100), 100).lag, 100); // 恰好在范围边缘也找得到
  assert.equal(lagBetween(a, shift(-100), 100).lag, -100);
  const lift = (x) => x.map((v) => v + 100);
  assert.equal(lagBetween(lift(a), lift(shift(37)), 100).lag, 37, '整体抬高一个很大的常数不影响结果（先去均值）');
  assert.ok(lagBetween(a, shift(5), 100).peak > 3 * lagBetween(a, bursts(3000, 99), 100).peak, '对得上的峰值应远高于不相干的信号');
});

test('延迟：只在 ±maxLag 内找（真实延迟更大时不会冒出范围外的值）；长度不同也行；没数据报错', () => {
  const a = bursts(2000, 9);
  const b = new Float64Array(1500);
  for (let i = 0; i < b.length; i++) b[i] = a[i - 60] ?? -3;
  const { lag } = lagBetween(a, b, 100);
  assert.equal(lag, 60);
  const narrow = lagBetween(a, b, 10).lag;
  assert.ok(Math.abs(narrow) <= 10);
  assert.throws(() => lagBetween(new Float64Array(0), b, 10), /没有数据/);
  assert.throws(() => lagBetween(a, new Float64Array(0), 10), /没有数据/);
});
