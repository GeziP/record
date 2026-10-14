// 前后处理的速度测量用到的小函数：找 ffmpeg、取中位数、实时倍数、由两种文本长度的用时拆出“加载”与“每千个单位”。
import fs from 'node:fs';
import path from 'node:path';

/**
 * 找 ffmpeg：--ffmpeg 参数，没有就用环境变量 RECORD_FFMPEG，再没有就用仓库里的 vendor/ffmpeg/bin/ffmpeg.exe（root 是仓库根目录）。
 * 只认第一个给出的；它不存在就报错，不悄悄换下一个，更不去 PATH 里找。
 */
export function findFfmpeg({ flag, env = process.env, root, exists = fs.existsSync }) {
  const vendor = root ? path.join(root, 'vendor', 'ffmpeg', 'bin', 'ffmpeg.exe') : undefined;
  const picked = [flag, env.RECORD_FFMPEG, vendor].find((x) => typeof x === 'string' && x !== '');
  if (!picked) throw new Error('没有 ffmpeg 的位置：用 --ffmpeg 或环境变量 RECORD_FFMPEG 指定（不会去 PATH 里找）');
  if (!exists(picked)) throw new Error(`没有找到 ffmpeg：${picked}（不会去 PATH 里找）`);
  return picked;
}

/** 中位数；个数为偶数时取中间两个的平均 */
export function median(xs) {
  if (xs.length === 0) throw new Error('没有数可取中位数');
  const s = [...xs].sort((a, b) => a - b);
  const m = s.length >> 1;
  return s.length % 2 === 1 ? s[m] : (s[m - 1] + s[m]) / 2;
}

/** 实时倍数：处理用时（毫秒）÷ 音频时长（秒）÷ 1000 */
export function rtfOf(wallMs, durationSec) {
  if (!(durationSec > 0)) throw new Error(`音频时长要大于 0：${durationSec}`);
  return wallMs / 1000 / durationSec;
}

/**
 * 标点模型：每次调用 = 加载 + 每千个单位的处理。用两种文本长度各测一次，解出两项。
 * points：[{ size, sec }, { size, sec }]（顺序不限；size 是文本里的单位数，单位由调用方定，同一次测量里要一致）。
 * 单位数相同或者长的反而更快（测量噪声盖过了差别）就报错，换更长的文本或重测。
 */
export function punctFit(points) {
  if (points.length !== 2) throw new Error('要恰好两个点才能解出加载用时与每千个单位的用时');
  const [a, b] = [...points].sort((x, y) => x.size - y.size);
  if (a.size === b.size) throw new Error('两种文本一样长，解不出每千个单位的用时');
  const secPer1000 = (b.sec - a.sec) / ((b.size - a.size) / 1000);
  if (!(secPer1000 > 0)) throw new Error(`长文本 ${b.size} 个单位用时 ${b.sec} 秒不比短文本 ${a.size} 个单位的 ${a.sec} 秒长：测量噪声盖过了差别，重测或换更长的文本`);
  const loadSec = a.sec - (secPer1000 * a.size) / 1000;
  return { loadSec: Math.max(0, loadSec), secPer1000 };
}
