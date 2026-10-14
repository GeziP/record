import test from 'node:test';
import assert from 'node:assert/strict';
import { fft } from './fft.mjs';

function lcg(seed) {
  let s = seed >>> 0;
  return () => ((s = (Math.imul(s, 1664525) + 1013904223) >>> 0) / 4294967296) * 2 - 1;
}

function naiveDft(re, im) {
  const n = re.length;
  const or = new Float64Array(n);
  const oi = new Float64Array(n);
  for (let k = 0; k < n; k++) {
    for (let t = 0; t < n; t++) {
      const a = (-2 * Math.PI * k * t) / n;
      or[k] += re[t] * Math.cos(a) - im[t] * Math.sin(a);
      oi[k] += re[t] * Math.sin(a) + im[t] * Math.cos(a);
    }
  }
  return { re: or, im: oi };
}

test('与朴素 DFT 一致（长度 16，随机复数输入）', () => {
  const r = lcg(7);
  const re = Float64Array.from({ length: 16 }, r);
  const im = Float64Array.from({ length: 16 }, r);
  const want = naiveDft(re, im);
  fft(re, im);
  for (let i = 0; i < 16; i++) {
    assert.ok(Math.abs(re[i] - want.re[i]) < 1e-9, `实部 ${i}`);
    assert.ok(Math.abs(im[i] - want.im[i]) < 1e-9, `虚部 ${i}`);
  }
});

test('正变换再逆变换得到原信号', () => {
  const r = lcg(11);
  const re0 = Float64Array.from({ length: 1024 }, r);
  const im0 = Float64Array.from({ length: 1024 }, r);
  const re = Float64Array.from(re0);
  const im = Float64Array.from(im0);
  fft(re, im);
  fft(re, im, true);
  for (let i = 0; i < 1024; i++) {
    assert.ok(Math.abs(re[i] - re0[i]) < 1e-12);
    assert.ok(Math.abs(im[i] - im0[i]) < 1e-12);
  }
});

test('单位脉冲的频谱是平的；长度 1 原样返回', () => {
  const re = new Float64Array(8);
  const im = new Float64Array(8);
  re[0] = 1;
  fft(re, im);
  assert.deepEqual(Array.from(re), Array(8).fill(1));
  assert.deepEqual(Array.from(im), Array(8).fill(0));
  const one = Float64Array.from([3]);
  fft(one, new Float64Array(1));
  assert.equal(one[0], 3);
});

test('长度不是 2 的幂、实虚部长度不同都报错', () => {
  assert.throws(() => fft(new Float64Array(12), new Float64Array(12)), /2 的幂/);
  assert.throws(() => fft(new Float64Array(0), new Float64Array(0)), /2 的幂/);
  assert.throws(() => fft(new Float64Array(8), new Float64Array(4)), /长度必须相同/);
});
