import test from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import { findFfmpeg, median, punctFit, rtfOf } from './speed.mjs';

const has = (...files) => (p) => files.includes(p);

test('找 ffmpeg：参数优先，其次环境变量，最后仓库里的 vendor；不去 PATH 里找', () => {
  const vendor = path.join('R', 'vendor', 'ffmpeg', 'bin', 'ffmpeg.exe');
  assert.equal(findFfmpeg({ flag: 'A.exe', env: { RECORD_FFMPEG: 'B.exe' }, root: 'R', exists: has('A.exe', 'B.exe', vendor) }), 'A.exe');
  assert.equal(findFfmpeg({ env: { RECORD_FFMPEG: 'B.exe' }, root: 'R', exists: has('B.exe', vendor) }), 'B.exe');
  assert.equal(findFfmpeg({ env: {}, root: 'R', exists: has(vendor) }), vendor);
  assert.equal(findFfmpeg({ flag: '', env: { RECORD_FFMPEG: '' }, root: 'R', exists: has(vendor) }), vendor); // 空字符串当作没给
});

test('找 ffmpeg：给了的那个不存在就报错，不换下一个；什么都没给也报错', () => {
  const vendor = path.join('R', 'vendor', 'ffmpeg', 'bin', 'ffmpeg.exe');
  assert.throws(() => findFfmpeg({ flag: 'A.exe', env: { RECORD_FFMPEG: 'B.exe' }, root: 'R', exists: has('B.exe', vendor) }), /没有找到 ffmpeg：A\.exe（不会去 PATH 里找）/);
  assert.throws(() => findFfmpeg({ env: { RECORD_FFMPEG: 'B.exe' }, root: 'R', exists: has(vendor) }), /没有找到 ffmpeg：B\.exe/);
  assert.throws(() => findFfmpeg({ env: {}, root: 'R', exists: has() }), /没有找到 ffmpeg：.*ffmpeg\.exe/);
  assert.throws(() => findFfmpeg({ env: {}, exists: has() }), /没有 ffmpeg 的位置/);
});

test('中位数：奇数个取中间，偶数个取中间两个的平均，不要求有序，不改原数组；空的报错', () => {
  assert.equal(median([3, 1, 2]), 2);
  assert.equal(median([10, 9, 100]), 10); // 按数值排，不是按字符串排
  assert.equal(median([4, 1, 3, 2]), 2.5);
  assert.equal(median([7]), 7);
  const xs = [9, 1, 5];
  median(xs);
  assert.deepEqual(xs, [9, 1, 5]);
  assert.throws(() => median([]), /没有数/);
});

test('实时倍数：毫秒数除以音频秒数再除以 1000；时长不是正数就报错', () => {
  assert.equal(rtfOf(3000, 1500), 0.002);
  assert.equal(rtfOf(0, 10), 0);
  assert.throws(() => rtfOf(1, 0), /音频时长要大于 0/);
  assert.throws(() => rtfOf(1, -3), /音频时长要大于 0/);
});

test('标点模型：由两种文本长度的用时解出加载用时与每千个单位的用时，与点的顺序无关', () => {
  const fit = punctFit([{ size: 1000, sec: 2 }, { size: 10000, sec: 11 }]);
  assert.ok(Math.abs(fit.secPer1000 - 1) < 1e-12);
  assert.ok(Math.abs(fit.loadSec - 1) < 1e-12);
  assert.deepEqual(punctFit([{ size: 10000, sec: 11 }, { size: 1000, sec: 2 }]), fit);
  // 加载项解出来是负的（测量噪声）：取 0，不返回负数
  const neg = punctFit([{ size: 1000, sec: 1 }, { size: 10000, sec: 20 }]);
  assert.equal(neg.loadSec, 0);
  assert.ok(Math.abs(neg.secPer1000 - 19 / 9) < 1e-12);
});

test('标点模型：点数不对、单位数相同、长文本反而更快，都报错', () => {
  assert.throws(() => punctFit([{ size: 1, sec: 1 }]), /恰好两个点/);
  assert.throws(() => punctFit([{ size: 1, sec: 1 }, { size: 2, sec: 1 }, { size: 3, sec: 1 }]), /恰好两个点/);
  assert.throws(() => punctFit([{ size: 1000, sec: 1 }, { size: 1000, sec: 2 }]), /一样长/);
  assert.throws(() => punctFit([{ size: 1000, sec: 3 }, { size: 10000, sec: 2 }]), /长文本 10000 个单位用时 2 秒不比短文本 1000 个单位的 3 秒长：测量噪声盖过了差别/);
  assert.throws(() => punctFit([{ size: 10000, sec: 2 }, { size: 1000, sec: 3 }]), /长文本 10000 个单位用时 2 秒不比短文本 1000 个单位的 3 秒长/); // 与点的顺序无关
  assert.throws(() => punctFit([{ size: 1000, sec: 2 }, { size: 10000, sec: 2 }]), /测量噪声盖过了差别/);
});
