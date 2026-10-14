// 调 measure.ps1 测量一个命令行程序：返回 { exitCode, wallMs, cpuMs, userMs, kernelMs, peakWorkingSetMb, peakPrivateMb, timedOut }。
// measure 阻塞到被测程序结束；measureAsync 返回 Promise，可以同时测多个。标准输出与错误输出写到调用方给的文件里。
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn, spawnSync } from 'node:child_process';

const PS1 = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'measure.ps1');

function prepare({ exe, args = [], stdoutFile, stderrFile, priority = 'Normal', workDir = process.cwd(), timeoutSec = 0, pollMs = 50 }) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'record-s4-measure-'));
  const request = path.join(dir, 'request.json');
  const result = path.join(dir, 'result.json');
  fs.writeFileSync(request, JSON.stringify({ exe, args, stdout: stdoutFile, stderr: stderrFile, result, priority, workDir, timeoutSec, pollMs }));
  return { dir, result, psArgs: ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', PS1, '-Request', request] };
}

function finish({ dir, result }, status, text) {
  try {
    if (status !== 0 || !fs.existsSync(result)) throw new Error(`measure.ps1 失败（退出码 ${status}）：${text.trim()}`);
    return JSON.parse(fs.readFileSync(result, 'utf8'));
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

export function measure(opts) {
  const p = prepare(opts);
  const r = spawnSync('powershell.exe', p.psArgs, { encoding: 'utf8' });
  return finish(p, r.status, r.stderr || r.stdout || '');
}

export function measureAsync(opts) {
  const p = prepare(opts);
  return new Promise((resolve, reject) => {
    const child = spawn('powershell.exe', p.psArgs, { stdio: ['ignore', 'pipe', 'pipe'] });
    let text = '';
    child.stdout.on('data', (d) => (text += d));
    child.stderr.on('data', (d) => (text += d));
    child.on('error', (e) => {
      fs.rmSync(p.dir, { recursive: true, force: true });
      reject(e);
    });
    child.on('close', (code) => {
      try {
        resolve(finish(p, code, text));
      } catch (e) {
        reject(e);
      }
    });
  });
}
