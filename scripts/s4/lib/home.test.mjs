import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { homeFrom, readJson, s4Paths, writeJsonAtomic } from './home.mjs';

const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), 'record-s4-home-'));

test('目录来源：命令行的值优先，其次环境变量 S4_HOME，都没有就报错而不是猜', () => {
  assert.equal(homeFrom('E:/a', { S4_HOME: 'E:/b' }), 'E:/a');
  assert.equal(homeFrom(undefined, { S4_HOME: 'E:/b' }), 'E:/b');
  assert.equal(homeFrom('', { S4_HOME: 'E:/b' }), 'E:/b');
  assert.throws(() => homeFrom(undefined, {}), /S4_HOME/);
  assert.throws(() => homeFrom('', { S4_HOME: '' }), /S4_HOME/);
});

test('路径约定：各子目录、数据集目录、官方程序的路径', () => {
  const home = tmp();
  const bin = path.join(home, 'tools', 'sherpa-onnx-v9-win-x64', 'bin');
  fs.mkdirSync(bin, { recursive: true });
  fs.writeFileSync(path.join(bin, 'sherpa-onnx-offline.exe'), '');
  const p = s4Paths(home);
  assert.equal(p.home, home);
  assert.equal(p.downloads, path.join(home, 'downloads'));
  assert.equal(p.models, path.join(home, 'models'));
  assert.equal(p.dataset, path.join(home, 'data', 'Eval_Ali'));
  assert.equal(p.work, path.join(home, 'work'));
  assert.equal(p.results, path.join(home, 'results'));
  assert.equal(p.exe('sherpa-onnx-offline'), path.join(bin, 'sherpa-onnx-offline.exe'));
  assert.equal(p.exe('sherpa-onnx-offline-speaker-diarization'), path.join(bin, 'sherpa-onnx-offline-speaker-diarization.exe'));
  fs.rmSync(home, { recursive: true, force: true });
});

test('路径约定：没有 tools 目录、没有程序、有两份程序，都明确报错', () => {
  const home = tmp();
  assert.throws(() => s4Paths(home), /找到 0 份/);
  fs.mkdirSync(path.join(home, 'tools', 'other', 'bin'), { recursive: true });
  assert.throws(() => s4Paths(home), /找到 0 份/);
  for (const v of ['a', 'b']) {
    fs.mkdirSync(path.join(home, 'tools', v, 'bin'), { recursive: true });
    fs.writeFileSync(path.join(home, 'tools', v, 'bin', 'sherpa-onnx-offline.exe'), '');
  }
  assert.throws(() => s4Paths(home), /找到 2 份/);
  fs.rmSync(home, { recursive: true, force: true });
});

test('原子写 JSON：建好目录、写完整、不留临时文件、能覆盖，读回一致', () => {
  const dir = tmp();
  const f = path.join(dir, 'a', 'b', 'x.json');
  writeJsonAtomic(f, { n: 1, s: '中文', arr: [1, 2.5] });
  assert.deepEqual(readJson(f), { n: 1, s: '中文', arr: [1, 2.5] });
  writeJsonAtomic(f, { n: 2 });
  assert.deepEqual(readJson(f), { n: 2 });
  assert.deepEqual(fs.readdirSync(path.dirname(f)), ['x.json']);
  fs.rmSync(dir, { recursive: true, force: true });
});
