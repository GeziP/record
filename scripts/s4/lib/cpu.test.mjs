import test from 'node:test';
import assert from 'node:assert/strict';
import { cpuSnapshot, othersLoad } from './cpu.mjs';

test('别人的占用 = (整机忙碌增量 − 被测程序自己的 CPU) ÷ 总时间增量', () => {
  const a = { busyMs: 1000, totalMs: 10000 };
  const b = { busyMs: 3000, totalMs: 20000 };
  assert.equal(othersLoad(a, b, 0), 0.2);
  assert.equal(othersLoad(a, b, 1500), 0.05);
  assert.equal(othersLoad(a, b, 2000), 0);
});

test('别人的占用：被测程序自己占的比整机忙碌增量还多（计时误差）时按 0，总时间没增加时也是 0', () => {
  const a = { busyMs: 0, totalMs: 0 };
  assert.equal(othersLoad(a, { busyMs: 100, totalMs: 1000 }, 500), 0);
  assert.equal(othersLoad(a, a, 0), 0);
  assert.equal(othersLoad({ busyMs: 5, totalMs: 10 }, { busyMs: 5, totalMs: 10 }, 3), 0);
});

test('快照：忙碌不超过总时间，两次之间只增不减', async () => {
  const s1 = cpuSnapshot();
  const t = Date.now();
  while (Date.now() - t < 200); // 占一下 CPU
  const s2 = cpuSnapshot();
  assert.ok(s1.totalMs > 0 && s1.busyMs > 0 && s1.busyMs < s1.totalMs, '开机以来总有空闲，所以忙碌严格小于总时间');
  assert.ok(s2.totalMs > s1.totalMs && s2.busyMs >= s1.busyMs);
  assert.ok(s2.busyMs - s1.busyMs >= 100, `200 毫秒的空转应当让忙碌至少增加 100 毫秒，实际 ${s2.busyMs - s1.busyMs}`);
});
