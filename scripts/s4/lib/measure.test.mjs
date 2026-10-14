import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { measure, measureAsync } from './measure.mjs';

const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), 'record-s4-mtest-'));
const run = (dir, script, extra = [], opts = {}) => {
  const out = path.join(dir, 'out.txt');
  const err = path.join(dir, 'err.txt');
  const m = measure({ exe: process.execPath, args: ['-e', script, ...extra], stdoutFile: out, stderrFile: err, ...opts });
  return { m, stdout: fs.readFileSync(out, 'utf8'), stderr: fs.readFileSync(err, 'utf8') };
};

test('退出码、耗时、CPU 时间、峰值内存都量得出来', () => {
  const dir = tmp();
  // 占约 400 MB 并逐页写入，再空转 1.5 秒，退出码 3
  const script = `const b = Buffer.alloc(400 * 1024 * 1024); for (let i = 0; i < b.length; i += 4096) b[i] = 1; const t = Date.now(); while (Date.now() - t < 1500); console.log('done ' + b[4096]); process.exit(3);`;
  const { m, stdout } = run(dir, script);
  assert.equal(m.exitCode, 3);
  assert.equal(m.timedOut, false);
  assert.ok(m.wallMs >= 1500 && m.wallMs < 8000, `wallMs=${m.wallMs}`);
  assert.ok(m.cpuMs >= 1200 && m.cpuMs < 12000, `cpuMs=${m.cpuMs}`);
  assert.ok(m.userMs + m.kernelMs > 0 && Math.abs(m.userMs + m.kernelMs - m.cpuMs) < 50, '用户态加内核态应约等于总 CPU 时间');
  assert.ok(m.peakWorkingSetMb >= 380 && m.peakWorkingSetMb < 900, `peakWorkingSetMb=${m.peakWorkingSetMb}`);
  assert.equal(stdout.trim(), 'done 1');
});

test('带空格、引号、结尾反斜杠、空串的参数原样传给被测程序', () => {
  const dir = tmp();
  const args = ['a b', 'say "hi"', 'C:\\dir with space\\', '', 'plain', 'back\\"slash', 'two\\\\"quotes and space'];
  const { m, stdout } = run(dir, 'console.log(JSON.stringify(process.argv.slice(1)))', args);
  assert.equal(m.exitCode, 0);
  assert.deepEqual(JSON.parse(stdout), args);
});

test('优先级：Idle 时被测程序自己看到的是最低优先级（19）', () => {
  const dir = tmp();
  const script = `const t = Date.now(); while (Date.now() - t < 300); console.log('priority=' + require('os').getPriority());`;
  assert.equal(run(dir, script, [], { priority: 'Idle' }).stdout.trim(), 'priority=19');
  assert.equal(run(dir, script, [], { priority: 'BelowNormal' }).stdout.trim(), 'priority=10');
  assert.equal(run(dir, script, [], { priority: 'Normal' }).stdout.trim(), 'priority=0');
});

test('超时：到点杀掉被测程序并如实标记，不等它自己结束', () => {
  const dir = tmp();
  const { m } = run(dir, 'setTimeout(() => {}, 30000)', [], { timeoutSec: 2 });
  assert.equal(m.timedOut, true);
  assert.ok(m.wallMs >= 2000 && m.wallMs < 8000, `wallMs=${m.wallMs}`);
});

test('标准错误输出单独写到自己的文件', () => {
  const dir = tmp();
  const { stdout, stderr } = run(dir, `console.log('to-out'); console.error('to-err');`);
  assert.equal(stdout.trim(), 'to-out');
  assert.equal(stderr.trim(), 'to-err');
});

test('启动不了的程序明确报错', () => {
  const dir = tmp();
  assert.throws(() => measure({ exe: path.join(dir, 'no-such.exe'), stdoutFile: path.join(dir, 'o'), stderrFile: path.join(dir, 'e') }), /measure\.ps1 失败/);
});

test('异步版：几个同时测，各自的退出码、输出、耗时都对，总耗时接近最长的那个而不是相加', async () => {
  const dir = tmp();
  const job = (i) => {
    const out = path.join(dir, `out${i}.txt`);
    const err = path.join(dir, `err${i}.txt`);
    const script = `const t = Date.now(); while (Date.now() - t < 1500); console.log('job ' + ${i}); process.exit(${i});`;
    return measureAsync({ exe: process.execPath, args: ['-e', script], stdoutFile: out, stderrFile: err }).then((m) => ({ m, out: fs.readFileSync(out, 'utf8').trim() }));
  };
  const t0 = Date.now();
  const rs = await Promise.all([1, 2, 3, 4].map(job));
  const total = Date.now() - t0;
  rs.forEach((r, k) => {
    assert.equal(r.m.exitCode, k + 1);
    assert.equal(r.out, `job ${k + 1}`);
    assert.ok(r.m.wallMs >= 1500 && r.m.cpuMs >= 1200, JSON.stringify(r.m));
  });
  assert.ok(total < 5500, `四个 1.5 秒的任务同时测，总共 ${total} 毫秒（串行要 6 秒以上）`);
});

test('异步版：启动不了的程序以拒绝的 Promise 报错，超时同样标记', async () => {
  const dir = tmp();
  await assert.rejects(measureAsync({ exe: path.join(dir, 'no-such.exe'), stdoutFile: path.join(dir, 'o'), stderrFile: path.join(dir, 'e') }), /measure\.ps1 失败/);
  const m = await measureAsync({ exe: process.execPath, args: ['-e', 'setTimeout(() => {}, 30000)'], stdoutFile: path.join(dir, 'o2'), stderrFile: path.join(dir, 'e2'), timeoutSec: 2 });
  assert.equal(m.timedOut, true);
  assert.ok(m.wallMs >= 2000 && m.wallMs < 8000, `wallMs=${m.wallMs}`);
});

test('异步版：优先级与带空格引号的参数同样原样传达', async () => {
  const dir = tmp();
  const args = ['a b', 'say "hi"', 'C:\\dir with space\\', ''];
  const out = path.join(dir, 'o.txt');
  await measureAsync({ exe: process.execPath, args: ['-e', `console.log(JSON.stringify([require('os').getPriority(), ...process.argv.slice(1)]))`, ...args], stdoutFile: out, stderrFile: path.join(dir, 'e.txt'), priority: 'BelowNormal' });
  assert.deepEqual(JSON.parse(fs.readFileSync(out, 'utf8')), [10, ...args]);
});
