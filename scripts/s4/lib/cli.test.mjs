import assert from 'node:assert/strict';
import test from 'node:test';
import { log, parseArgs, pickThree, runPool } from './cli.mjs';

test('参数：--键 值、--键=值、单独开关、位置参数', () => {
  const r = parseArgs(['asr', '--home', 'E:/x', '--threads=8,4', '--force', '--cands', 'a,b', 'full']);
  assert.deepEqual(r.positional, ['asr', 'full']);
  assert.deepEqual(r.flags, { home: 'E:/x', threads: '8,4', force: true, cands: 'a,b' });
});

test('参数：开关在末尾、开关后面紧跟另一个 --、值里有等号', () => {
  assert.deepEqual(parseArgs(['--a', '--b', '1', '--c']).flags, { a: true, b: '1', c: true });
  assert.deepEqual(parseArgs(['--k=a=b']).flags, { k: 'a=b' });
  assert.deepEqual(parseArgs([]), { positional: [], flags: {} });
});

test('取三场：头、中、尾；八场时是第 1、5、8 场，三场时是全部，不足三场报错', () => {
  const eight = ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h'];
  assert.deepEqual(pickThree(eight), ['a', 'e', 'h']);
  assert.deepEqual(pickThree(['x', 'y', 'z']), ['x', 'y', 'z']);
  assert.deepEqual(pickThree(['a', 'b', 'c', 'd', 'e']), ['a', 'c', 'e']);
  assert.throws(() => pickThree(['a', 'b']), /至少要有 3 场/);
});

test('任务池：并发不超过上限、结果按输入顺序、下标传给任务', async () => {
  let running = 0;
  let peak = 0;
  const out = await runPool([30, 5, 20, 1, 10], 2, async (ms, i) => {
    running++;
    peak = Math.max(peak, running);
    await new Promise((r) => setTimeout(r, ms));
    running--;
    return `${i}:${ms}`;
  });
  assert.deepEqual(out, ['0:30', '1:5', '2:20', '3:1', '4:10']);
  assert.equal(peak, 2);
  assert.deepEqual(await runPool([], 4, async () => 1), []);
});

test('任务池：一个失败后不再开始新任务，已在跑的跑完，抛出最先的错误', async () => {
  const started = [];
  const finished = [];
  await assert.rejects(
    runPool([1, 2, 3, 4, 5, 6], 2, async (n) => {
      started.push(n);
      await new Promise((r) => setTimeout(r, n === 1 ? 5 : 40));
      if (n === 1) throw new Error('第一个失败');
      finished.push(n);
    }),
    /第一个失败/,
  );
  assert.deepEqual(started, [1, 2], '1 在 5 毫秒时失败，那时 2 还在跑；之后谁也不再开始新的任务（3 到 6 都没开始）');
  assert.deepEqual(finished, [2], '已在跑的 2 要等它跑完');
});

test('任务池：先后两个失败时抛出先失败的那个', async () => {
  await assert.rejects(
    runPool([1, 2], 2, async (n) => {
      await new Promise((r) => setTimeout(r, n === 1 ? 5 : 30));
      throw new Error(`第 ${n} 个失败`);
    }),
    /第 1 个失败/,
  );
});

test('任务池：并发数不是正整数就报错', async () => {
  await assert.rejects(runPool([1], 0, async () => 1), /并发数要是正整数/);
  await assert.rejects(runPool([1], 1.5, async () => 1), /并发数要是正整数/);
});

test('日志：带本机时间', () => {
  const lines = [];
  const orig = console.log;
  console.log = (s) => lines.push(s);
  try {
    log('开始', new Date(2026, 9, 14, 23, 31, 9));
  } finally {
    console.log = orig;
  }
  assert.deepEqual(lines, ['2026-10-14 23:31:09  开始']);
});
