import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { extractAll, runTar, tarPath } from './extract.mjs';
import { readJson } from './home.mjs';
import { tarGz } from './tar-fixture.mjs';

const sha = (b) => crypto.createHash('sha256').update(b).digest('hex');
const GOOD = [{ name: 'model/', type: '5' }, { name: 'model/a.onnx', data: 'AAA' }, { name: 'model/sub/b.txt', data: 'B'.repeat(700) }];

async function withHome(fn) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'record-extract-'));
  const home = path.join(root, 'home');
  fs.mkdirSync(path.join(home, 'downloads'), { recursive: true });
  const manifest = {};
  const items = [];
  const logs = [];
  const put = (name, entries, { extractTo = 'models', verified = true, raw } = {}) => {
    const bytes = raw ?? tarGz(entries);
    fs.writeFileSync(path.join(home, 'downloads', name), bytes);
    manifest[name] = { sha256: sha(bytes), verified };
    if (!items.some((i) => i.name === name)) items.push({ name, extractTo });
    return bytes;
  };
  const run = (over = {}) => extractAll({ home, items, manifest, say: (s) => logs.push(s), ...over });
  try {
    await fn({ root, home, put, run, items, manifest, logs, at: (...p) => path.join(home, ...p) });
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
}

test('正常的压缩包：解到 <home>/<extractTo>，记下“已解压”的标记和条目数，日志写明去向', async () => {
  await withHome(async ({ put, run, at, logs, manifest }) => {
    put('m.tar.gz', GOOD);
    assert.deepEqual(await run(), [{ name: 'm.tar.gz', status: 'extracted', entries: 3, unchecked: 0 }]);
    assert.equal(fs.readFileSync(at('models', 'model', 'a.onnx'), 'utf8'), 'AAA');
    assert.equal(fs.readFileSync(at('models', 'model', 'sub', 'b.txt'), 'utf8'), 'B'.repeat(700));
    const marker = readJson(at('extracted', 'm.tar.gz.json'));
    assert.equal(marker.sha256, manifest['m.tar.gz'].sha256);
    assert.equal(marker.entries, 3);
    assert.equal(marker.unchecked, 0);
    assert.ok(Math.abs(Date.now() - Date.parse(marker.extractedAt)) < 60_000, marker.extractedAt);
    assert.equal(logs.length, 1);
    assert.match(logs[0], /^解压：m\.tar\.gz → models（3 个条目，\d+ s）$/);
  });
});

test('解压目的地跟着条目走（tools / models / data）', async () => {
  await withHome(async ({ put, run, at }) => {
    put('t.tar.gz', GOOD, { extractTo: 'tools' });
    await run();
    assert.ok(fs.existsSync(at('tools', 'model', 'a.onnx')));
    assert.equal(fs.existsSync(at('models')), false);
  });
});

test('已经解压过（标记里的摘要与现在一致）：不再解压，也不动目录里现有的东西', async () => {
  await withHome(async ({ put, run, at, logs }) => {
    put('m.tar.gz', GOOD);
    await run();
    fs.rmSync(at('models', 'model'), { recursive: true });
    logs.length = 0;
    assert.deepEqual(await run(), [{ name: 'm.tar.gz', status: 'already' }]);
    assert.equal(fs.existsSync(at('models', 'model')), false, '没有重新解压');
    assert.deepEqual(logs, ['已解压：m.tar.gz']);
  });
});

test('压缩包换了（摘要变了，清单也更新了）：重新解压，覆盖旧内容，标记更新', async () => {
  await withHome(async ({ put, run, at, manifest }) => {
    put('m.tar.gz', GOOD);
    await run();
    const oldSha = manifest['m.tar.gz'].sha256;
    put('m.tar.gz', [{ name: 'model/a.onnx', data: 'NEW' }]);
    assert.deepEqual(await run(), [{ name: 'm.tar.gz', status: 'extracted', entries: 1, unchecked: 0 }]);
    assert.equal(fs.readFileSync(at('models', 'model', 'a.onnx'), 'utf8'), 'NEW');
    assert.notEqual(readJson(at('extracted', 'm.tar.gz.json')).sha256, oldSha);
  });
});

test('没有 extractTo 的条目（单个模型文件）直接跳过，清单里没有它也不要紧', async () => {
  await withHome(async ({ run, items, at }) => {
    items.push({ name: 'silero_vad.onnx' });
    assert.deepEqual(await run(), []);
    assert.equal(fs.existsSync(at('models')), false);
  });
});

test('下载清单里没有这个文件：先跑 fetch，什么都不解', async () => {
  await withHome(async ({ run, items, at }) => {
    items.push({ name: 'x.tar.gz', extractTo: 'models' });
    await assert.rejects(run(), /x\.tar\.gz：下载清单里没有它，先跑 fetch/);
    assert.equal(fs.existsSync(at('models')), false);
  });
});

test('清单里记着没核对过摘要：不解压', async () => {
  await withHome(async ({ put, run, at }) => {
    put('u.tar.gz', GOOD, { verified: false });
    await assert.rejects(run(), /u\.tar\.gz：下载时没有核对过摘要，不解压/);
    assert.equal(fs.existsSync(at('models')), false);
  });
});

test('压缩包在核对之后被改过（摘要与清单不一致）：不解压，说明两个摘要', async () => {
  await withHome(async ({ put, run, at, manifest }) => {
    put('m.tar.gz', GOOD);
    const changed = tarGz([{ name: 'model/a.onnx', data: 'tampered' }]);
    fs.writeFileSync(at('downloads', 'm.tar.gz'), changed);
    await assert.rejects(run(), (e) => e.message.includes('m.tar.gz：现在的摘要') && e.message.includes(sha(changed)) && e.message.includes(manifest['m.tar.gz'].sha256) && e.message.includes('下载之后被改过'));
    assert.equal(fs.existsSync(at('models')), false);
  });
});

test('有不安全的条目就整个不解压（..、绝对路径、盘符、符号链接、硬链接），什么都不写', async () => {
  const cases = {
    dotdot: [[{ name: 'model/', type: '5' }, { name: '../../evil.txt', data: 'x' }], '../../evil.txt：路径里有 ..'],
    abs: [[{ name: '/abs-evil.txt', data: 'x' }], '/abs-evil.txt：绝对路径'],
    drive: [[{ name: 'C:/drive-evil.txt', data: 'x' }], 'C:/drive-evil.txt：文件名里有冒号'],
    symlink: [[{ name: 'model/', type: '5' }, { name: 'model/link', type: '2', link: '../../x' }], 'model/link -> ../../x：链接'],
    hardlink: [[{ name: 'model/a', data: 'q' }, { name: 'model/hard', type: '1', link: 'model/a' }], 'model/hard link to model/a：链接'],
    fifo: [[{ name: 'model/a', data: 'q' }, { name: 'model/pipe', type: '6' }], 'model/pipe：不是普通文件或目录'],
    device: [[{ name: 'model/dev', type: '3' }], 'model/dev：不是普通文件或目录'],
  };
  for (const [kind, [entries, expected]] of Object.entries(cases)) {
    await withHome(async ({ root, put, run, at, logs }) => {
      put(`${kind}.tar.gz`, entries);
      await assert.rejects(run(), (e) => e.message.startsWith(`${kind}.tar.gz：压缩包里有不安全的条目，不解压：`) && e.message.includes(expected), kind);
      assert.equal(fs.existsSync(at('models')), false, `${kind}：目标目录都不该建`);
      assert.equal(fs.existsSync(at('extracted')), false, `${kind}：不该留下已解压标记`);
      assert.equal(fs.existsSync(path.join(root, 'evil.txt')), false);
      assert.deepEqual(logs, []);
    });
  }
});

test('不安全的条目太多时只列前 5 条并给出总数', async () => {
  await withHome(async ({ put, run }) => {
    put('many.tar.gz', Array.from({ length: 7 }, (_, i) => ({ name: `../x${i}.txt`, data: 'x' })));
    await assert.rejects(run(), (e) => (e.message.match(/路径里有 \.\./g) ?? []).length === 5 && e.message.includes('…共 7 条') && e.message.includes('../x0.txt') && !e.message.includes('../x5.txt'));
  });
});

test('不安全的条目 5 条时不写“共多少条”，6 条起写', async () => {
  for (const [count, total] of [[5, null], [6, 6]]) {
    await withHome(async ({ put, run }) => {
      put('n.tar.gz', Array.from({ length: count }, (_, i) => ({ name: `../x${i}.txt`, data: 'x' })));
      await assert.rejects(run(), (e) => (e.message.match(/路径里有 \.\./g) ?? []).length === 5 && (total === null ? !e.message.includes('共') : e.message.endsWith(`\n  …共 ${total} 条`)), `${count} 条`);
    });
  }
});

test('压缩包本身是坏的（摘要与清单一致但 tar 读不了）：报错并点名文件，不留标记', async () => {
  await withHome(async ({ put, run, at }) => {
    put('bad.tar.gz', [], { raw: Buffer.from('this is not an archive at all'.repeat(40)) });
    await assert.rejects(run(), (e) => /^bad\.tar\.gz：tar -tf 失败（退出码 \d+）：/.test(e.message) && e.message.includes('Unrecognized archive format') && e.message === e.message.trimEnd());
    assert.equal(fs.existsSync(at('extracted')), false);
    assert.equal(fs.existsSync(at('models')), false);
  });
});

test('tar 运行不起来（路径不对）：报错，说明是哪个文件、哪个程序', async () => {
  await withHome(async ({ put, run }) => {
    put('m.tar.gz', GOOD);
    await assert.rejects(run({ tar: 'Z:\\no\\such\\tar.exe' }), (e) => e.message.startsWith('m.tar.gz：没法运行 Z:\\no\\such\\tar.exe：'));
  });
});

test('解压本身失败（tar 报错）：把错误抛出去，不留“已解压”的标记（下次还会重来）', async () => {
  await withHome(async ({ put, run, at, logs }) => {
    put('m.tar.gz', GOOD);
    const exec = (tar, name, args) => {
      if (args[0] === '-xf') throw new Error(`${name}：tar -xf 失败（退出码 1）：disk full`);
      return runTar(tar, name, args);
    };
    await assert.rejects(run({ exec }), /^Error: m\.tar\.gz：tar -xf 失败（退出码 1）：disk full$/);
    assert.equal(fs.existsSync(at('extracted')), false);
    assert.deepEqual(logs, []);
  });
});

test('tar 退出码为 0 但文件根本没出来：报错，给出缺少的个数和例子，不留标记', async () => {
  await withHome(async ({ put, run, at, logs }) => {
    put('m.tar.gz', GOOD);
    const exec = (tar, name, args) => (args[0] === '-xf' ? '' : runTar(tar, name, args));
    await assert.rejects(run({ exec }), /^Error: m\.tar\.gz：tar 没有报错，但有 2 个文件不在磁盘上，例如 model\/a\.onnx$/);
    assert.equal(fs.existsSync(at('extracted')), false);
    assert.deepEqual(logs, []);
  });
});

test('只缺一个文件也要报（目录条目不算文件）', async () => {
  await withHome(async ({ put, run, at }) => {
    put('m.tar.gz', GOOD);
    const exec = (tar, name, args) => {
      const out = runTar(tar, name, args);
      if (args[0] === '-xf') fs.rmSync(at('models', 'model', 'sub', 'b.txt'));
      return out;
    };
    await assert.rejects(run({ exec }), /有 1 个文件不在磁盘上，例如 model\/sub\/b\.txt$/);
    assert.equal(fs.existsSync(at('extracted')), false);
  });
});

test('文件名里的空格、~ 等可见字符都能核对（不会被当成非 ASCII 跳过）', async () => {
  await withHome(async ({ put, run }) => {
    put('m.tar.gz', [{ name: 'model/a ~b!.txt', data: 'x' }, { name: 'model/ok.txt', data: 'y' }]);
    assert.deepEqual(await run(), [{ name: 'm.tar.gz', status: 'extracted', entries: 2, unchecked: 0 }]);
  });
});

test('文件名含非 ASCII 字符的文件没法核对（tar 列出来是乱码）：只核对 ASCII 的，没核对的个数记在标记和日志里', async () => {
  await withHome(async ({ put, run, at, logs }) => {
    put('m.tar.gz', GOOD);
    const exec = (tar, name, args) => {
      if (args[0] === '-tf') return 'model/\r\nmodel/a.onnx\r\nmodel/\u4f1a\ufffd\\256.txt\r\nmodel/\u4f1a2.txt\r\n';
      if (args[0] === '-tvf') return ['drwxr-xr-x  0 0 0 0 x', '-rw-r--r-- 0 0 0 3 x', '-rw-r--r-- 0 0 0 1 x', '-rw-r--r-- 0 0 0 1 x'].join('\r\n');
      fs.mkdirSync(path.join(args[3], 'model'), { recursive: true });
      fs.writeFileSync(path.join(args[3], 'model', 'a.onnx'), 'AAA');
      return '';
    };
    assert.deepEqual(await run({ exec }), [{ name: 'm.tar.gz', status: 'extracted', entries: 4, unchecked: 2 }]);
    assert.equal(readJson(at('extracted', 'm.tar.gz.json')).unchecked, 2);
    assert.equal(logs.length, 1);
    assert.match(logs[0], /（4 个条目，\d+ s；2 个文件名含非 ASCII 字符，没法逐个核对）$/);
  });
});

test('传给 tar 的参数：先 -tf、-tvf 列清单，再 -xf <压缩包> -C <目的地>，用的是传入的 tar', async () => {
  await withHome(async ({ put, run, at }) => {
    put('m.tar.gz', GOOD);
    const calls = [];
    const exec = (tar, name, args) => {
      calls.push([tar, name, ...args]);
      return runTar(tar, name, args);
    };
    await run({ exec, tar: tarPath() });
    const archive = at('downloads', 'm.tar.gz');
    assert.deepEqual(calls, [
      [tarPath(), 'm.tar.gz', '-tf', archive],
      [tarPath(), 'm.tar.gz', '-tvf', archive],
      [tarPath(), 'm.tar.gz', '-xf', archive, '-C', at('models')],
    ]);
  });
});

test('tar 的位置：系统目录下的 tar.exe，不走 PATH；没有 SystemRoot 就报错', () => {
  assert.equal(tarPath({ SystemRoot: 'C:\\Windows' }), path.join('C:\\Windows', 'System32', 'tar.exe'));
  assert.throws(() => tarPath({}), /SystemRoot 没有设置/);
  assert.ok(fs.existsSync(tarPath()), '这台机器上系统自带的 tar 应该存在');
});
