// 两条录音的时间对齐检查：把各自的能量包络（每 hop 个点一格，取对数）做互相关，找出 b 相对 a 晚了多少。
// 用来核对 AliMeeting 的远场阵列录音与近场标注的时间轴有没有错位（错位超过评分容差时远场的 DER 就不可信）。
import { fft } from './fft.mjs';

/** 每 hop 个点一格的对数能量；不足一格的尾巴丢掉 */
export function envelope(samples, hop) {
  const n = Math.floor(samples.length / hop);
  const out = new Float64Array(n);
  for (let i = 0; i < n; i++) {
    let s = 0;
    for (let j = 0; j < hop; j++) s += samples[i * hop + j] ** 2;
    out[i] = Math.log10(1e-8 + s / hop);
  }
  return out;
}

function demean(a) {
  let m = 0;
  for (const x of a) m += x;
  m /= a.length;
  return Float64Array.from(a, (x) => x - m);
}

/** b 相对 a 的延迟（格数，b[n] ≈ a[n − lag]，范围 ±maxLag）与互相关峰值 */
export function lagBetween(a, b, maxLag) {
  if (a.length === 0 || b.length === 0) throw new Error('没有数据，没法求延迟');
  let size = 1;
  while (size < a.length + b.length) size <<= 1;
  const ar = new Float64Array(size);
  const ai = new Float64Array(size);
  const br = new Float64Array(size);
  const bi = new Float64Array(size);
  ar.set(demean(a));
  br.set(demean(b));
  fft(ar, ai);
  fft(br, bi);
  for (let k = 0; k < size; k++) {
    const re = br[k] * ar[k] + bi[k] * ai[k]; // b × conj(a)
    const im = bi[k] * ar[k] - br[k] * ai[k];
    ar[k] = re;
    ai[k] = im;
  }
  fft(ar, ai, true);
  let best = 0;
  let peak = -Infinity;
  for (let l = -maxLag; l <= maxLag; l++) {
    const v = ar[(l + size) % size];
    if (v > peak) {
      peak = v;
      best = l;
    }
  }
  return { lag: best, peak };
}
