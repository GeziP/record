// 合成回声：把“远端”（会议软件送来的对方声音）经过一条合成的房间脉冲响应，按指定的能量比混进“近端”（我的麦克风），再加一点底噪。
// 这只是一条线性的、固定的声学通路：没有喇叭失真、没有房间与人的移动，所以结果不等于真实外放回声。
import { fft } from './fft.mjs';

/** 可复现的伪随机数（0 到 1 之间） */
export function mulberry32(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function gaussian(rand) {
  return () => {
    let u = 0;
    while (u === 0) u = rand();
    return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * rand());
  };
}

/**
 * 合成脉冲响应：在 delayMs 处一个单位幅度的直达声，后面接指数衰减的高斯噪声作为混响尾巴（衰减 60 dB 用时 rt60 秒），
 * 尾巴总能量是直达声的 tailRatio 倍。
 */
export function makeRir({ rate, delayMs, rt60, seed = 1, tailRatio = 0.5 }) {
  const d = Math.round((delayMs * rate) / 1000);
  const tailLen = Math.ceil(rt60 * rate);
  const h = new Float32Array(d + 1 + tailLen);
  h[d] = 1;
  const rnd = gaussian(mulberry32(seed));
  const tail = new Float64Array(tailLen);
  let energy = 0;
  for (let n = 0; n < tailLen; n++) {
    tail[n] = rnd() * Math.exp((-6.907755 * n) / tailLen); // ln(1e-3)：振幅降到千分之一就是 60 dB
    energy += tail[n] * tail[n];
  }
  const k = Math.sqrt(tailRatio / energy);
  for (let n = 0; n < tailLen; n++) h[d + 1 + n] = tail[n] * k;
  return h;
}

/** 直达声的位置：脉冲响应里绝对值最大的那个点 */
export function directDelay(h) {
  let best = 0;
  for (let i = 1; i < h.length; i++) if (Math.abs(h[i]) > Math.abs(h[best])) best = i;
  return best;
}

/** 线性卷积，只取前 x.length 个点（因果、截掉尾巴）；用 FFT 重叠相加，长信号也快 */
export function convolve(x, h) {
  const n = x.length;
  const m = h.length;
  const out = new Float32Array(n);
  if (n === 0 || m === 0) return out;
  let size = 1;
  while (size < 2 * m) size <<= 1;
  const block = size - m + 1;
  const hr = new Float64Array(size);
  const hi = new Float64Array(size);
  for (let i = 0; i < m; i++) hr[i] = h[i];
  fft(hr, hi);
  const xr = new Float64Array(size);
  const xi = new Float64Array(size);
  for (let start = 0; start < n; start += block) {
    xr.fill(0);
    xi.fill(0);
    const len = Math.min(block, n - start);
    for (let i = 0; i < len; i++) xr[i] = x[start + i];
    fft(xr, xi);
    for (let k = 0; k < size; k++) {
      const a = xr[k] * hr[k] - xi[k] * hi[k];
      xi[k] = xr[k] * hi[k] + xi[k] * hr[k];
      xr[k] = a;
    }
    fft(xr, xi, true);
    const end = Math.min(len + m - 1, n - start);
    for (let i = 0; i < end; i++) out[start + i] += xr[i];
  }
  return out;
}

/** 由若干 {start, end}（秒）生成逐点的 0/1 掩码 */
export function maskFromSegments(segments, rate, length) {
  const mask = new Uint8Array(length);
  for (const s of segments) {
    const a = Math.max(0, Math.round(s.start * rate));
    const b = Math.min(length, Math.round(s.end * rate));
    for (let i = a; i < b; i++) mask[i] = 1;
  }
  return mask;
}

/** 掩码向后平移 d 个点 */
export function shiftMask(mask, d) {
  const out = new Uint8Array(mask.length);
  for (let i = 0; i + d < mask.length; i++) out[i + d] = mask[i];
  return out;
}

/** 掩码为 1 的那些点上的均方根；没有这样的点时是 0 */
export function rmsOver(samples, mask) {
  let sum = 0;
  let n = 0;
  for (let i = 0; i < samples.length; i++) {
    if (mask[i]) {
      sum += samples[i] * samples[i];
      n++;
    }
  }
  return n === 0 ? 0 : Math.sqrt(sum / n);
}

/**
 * 把远端经脉冲响应变成回声，按 ratioDb 混进近端，再加底噪。
 * ratioDb：回声的均方根（在“远端有声段平移一个直达声延迟”的范围内量）减去近端的均方根（在近端有声段内量），单位 dB；
 * noiseDb：白噪声的均方根相对近端有声段均方根，单位 dB。
 */
export function mixEcho({ near, far, rir, nearMask, farMask, ratioDb, noiseDb = -40, seed = 2 }) {
  const n = near.length;
  const farAligned = new Float32Array(n);
  farAligned.set(far.subarray(0, Math.min(n, far.length)));
  const echoRaw = convolve(farAligned, rir);
  const rmsNear = rmsOver(near, nearMask);
  const rmsEcho = rmsOver(echoRaw, shiftMask(farMask, directDelay(rir)));
  if (rmsNear === 0) throw new Error('近端没有有声段，没法按比例配回声');
  if (rmsEcho === 0) throw new Error('远端没有有声段，没法按比例配回声');
  const gain = (10 ** (ratioDb / 20) * rmsNear) / rmsEcho;
  const noiseRms = rmsNear * 10 ** (noiseDb / 20);
  const rnd = gaussian(mulberry32(seed));
  const mic = new Float32Array(n);
  const echo = new Float32Array(n);
  for (let i = 0; i < n; i++) {
    echo[i] = gain * echoRaw[i];
    mic[i] = near[i] + echo[i] + noiseRms * rnd();
  }
  return { mic, echo, gain, rmsNear, rmsEcho };
}

/** 回声消减量 ERLE：掩码范围内，处理前的能量除以处理后的能量，单位 dB（处理后全为 0 时是无穷大） */
export function erleDb(before, after, mask) {
  let e0 = 0;
  let e1 = 0;
  for (let i = 0; i < before.length; i++) {
    if (mask[i]) {
      e0 += before[i] * before[i];
      e1 += after[i] * after[i];
    }
  }
  return 10 * Math.log10(e0 / e1);
}
