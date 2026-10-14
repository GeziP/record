import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fetchAll } from './fetch-run.mjs';
import { readJson } from './home.mjs';

const sha = (b) => crypto.createHash('sha256').update(b).digest('hex');
const A = Buffer.from('文件 A 的内容'.repeat(2000));
const B = Buffer.from('file B contents'.repeat(3000));
const C = Buffer.from('c'.repeat(777));
const SUMS = Buffer.from(`b.bin\t${sha(B)}\nother.bin\t${sha(A)}\n`);
const FAST = { retryDelayMs: 0, stallMs: 5000, tries: 2 };

async function withEnv(fn, { routes = {} } = {}) {
  const served = { '/a.bin': A, '/b.bin': B, '/c.bin': C, '/t1/checksum.txt': SUMS, ...routes };
  const requests = [];
  const server = http.createServer((req, res) => {
    requests.push(req.url);
    const body = served[req.url];
    if (!body) {
      res.writeHead(404);
      return res.end();
    }
    res.writeHead(200, { 'Content-Length': body.length });
    return res.end(body);
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const base = `http://127.0.0.1:${server.address().port}`;
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'record-fetch-'));
  const logs = [];
  const items = [
    { group: 'g', name: 'a.bin', url: `${base}/a.bin`, sha256: sha(A), digestSource: 'GitHub API digest' },
    { group: 'g', name: 'b.bin', url: `${base}/b.bin`, tag: 't1' },
    { group: 'g', name: 'c.bin', url: `${base}/c.bin` },
  ];
  const run = (over = {}) => fetchAll({ home, items, checksumFiles: [{ tag: 't1', sha256: sha(SUMS) }], checksumUrlFor: (t) => `${base}/${t}/checksum.txt`, say: (s) => logs.push(s), downloadOpts: FAST, ...over });
  const file = (...p) => path.join(home, ...p);
  try {
    await fn({ run, home, file, base, items, requests, logs, served });
  } finally {
    server.closeAllConnections();
    await new Promise((r) => server.close(r));
    fs.rmSync(home, { recursive: true, force: true });
  }
}

test('第一次：全部下载、核对、落盘，清单里记下来源与是否核对过', async () => {
  await withEnv(async ({ run, file, base, requests, logs }) => {
    const { manifest, failed } = await run();
    const done = (name) => logs.find((l) => l.startsWith(`完成：${name}  `));
    assert.match(done('a.bin'), /^完成：a\.bin {2}0\.0 MB {2}\d+ s {2}sha256=[0-9a-f]{64} {2}（GitHub API digest：一致）$/);
    assert.match(done('b.bin'), /^完成：b\.bin {2}0\.0 MB {2}\d+ s {2}sha256=[0-9a-f]{64} {2}（checksum\.txt：一致）$/);
    assert.match(done('c.bin'), /^完成：c\.bin {2}0\.0 MB {2}\d+ s {2}sha256=[0-9a-f]{64} {2}（无官方摘要（首次计算，记录在案））$/);
    assert.deepEqual(logs.filter((l) => l.startsWith('开始：')), ['开始：a.bin', '开始：b.bin', '开始：c.bin']);
    assert.deepEqual(failed, []);
    assert.deepEqual(manifest, {
      'a.bin': { url: `${base}/a.bin`, bytes: A.length, sha256: sha(A), source: 'GitHub API digest', official: true, verified: true },
      'b.bin': { url: `${base}/b.bin`, bytes: B.length, sha256: sha(B), source: 'checksum.txt', official: true, verified: true },
      'c.bin': { url: `${base}/c.bin`, bytes: C.length, sha256: sha(C), source: '无官方摘要（首次计算，记录在案）', official: false, verified: false },
    });
    assert.deepEqual(readJson(file('fetch-manifest.json')), manifest);
    assert.ok(fs.readFileSync(file('downloads', 'a.bin')).equals(A));
    assert.ok(fs.readFileSync(file('downloads', 'b.bin')).equals(B));
    assert.ok(fs.readFileSync(file('downloads', 'c.bin')).equals(C));
    assert.ok(fs.readFileSync(file('meta', 'checksum-t1.txt')).equals(SUMS));
    assert.deepEqual([...requests].sort(), ['/a.bin', '/b.bin', '/c.bin', '/t1/checksum.txt']);
    assert.equal(fs.readdirSync(file('downloads')).filter((n) => n.endsWith('.part') || n.endsWith('.BAD')).length, 0);
  });
});

test('再跑一次：文件和 checksum.txt 都在且相符，一个网络请求也不发', async () => {
  await withEnv(async ({ run, requests, logs }) => {
    const first = await run();
    requests.length = 0;
    logs.length = 0;
    const second = await run();
    assert.deepEqual(requests, []);
    assert.deepEqual(second.manifest, first.manifest);
    assert.deepEqual(second.failed, []);
    assert.ok(logs.includes(`已有：a.bin  ${sha(A).slice(0, 12)}…`), logs.join('\n'));
    assert.ok(logs.includes(`已有：c.bin  ${sha(C).slice(0, 12)}…`), logs.join('\n'));
    assert.ok(logs.some((l) => l.includes('checksum.txt（t1）摘要与 API 一致，共 2 条')), logs.join('\n'));
  });
});

test('已有的文件摘要不符：删掉重下一次，下对了就用', async () => {
  await withEnv(async ({ run, file, requests, logs }) => {
    await run();
    fs.writeFileSync(file('downloads', 'a.bin'), 'damaged');
    requests.length = 0;
    logs.length = 0;
    const { manifest, failed } = await run();
    assert.deepEqual(failed, []);
    assert.deepEqual(requests, ['/a.bin']);
    assert.ok(fs.readFileSync(file('downloads', 'a.bin')).equals(A));
    assert.equal(manifest['a.bin'].sha256, sha(A));
    assert.ok(logs.includes('已有但摘要不符，重下：a.bin'), logs.join('\n'));
  });
});

test('下来的内容摘要不符：改名 .BAD，记入失败，不进清单，后面的条目照常处理', async () => {
  await withEnv(
    async ({ run, file, logs }) => {
      const { manifest, failed } = await run();
      assert.deepEqual(failed, [{ name: 'b.bin', expected: sha(B), actual: sha(Buffer.from('evil')) }]);
      assert.equal(fs.existsSync(file('downloads', 'b.bin')), false);
      assert.equal(fs.readFileSync(file('downloads', 'b.bin.BAD'), 'utf8'), 'evil');
      assert.deepEqual(Object.keys(manifest), ['a.bin', 'c.bin']);
      assert.deepEqual(Object.keys(readJson(file('fetch-manifest.json'))), ['a.bin', 'c.bin']);
      assert.ok(logs.some((l) => l.startsWith('！摘要不符：b.bin') && l.includes(`期望 ${sha(B)}`) && l.includes(`实际 ${sha(Buffer.from('evil'))}`)), logs.join('\n'));
    },
    { routes: { '/b.bin': Buffer.from('evil') } },
  );
});

test('已有的文件摘要不符、重下的也不符：只重下一次，改名 .BAD，记入失败', async () => {
  await withEnv(
    async ({ run, file, requests }) => {
      fs.mkdirSync(file('downloads'), { recursive: true });
      fs.writeFileSync(file('downloads', 'a.bin'), 'damaged');
      const { failed } = await run();
      assert.deepEqual(failed, [{ name: 'a.bin', expected: sha(A), actual: sha(Buffer.from('also wrong')) }]);
      assert.equal(requests.filter((r) => r === '/a.bin').length, 1);
      assert.equal(fs.readFileSync(file('downloads', 'a.bin.BAD'), 'utf8'), 'also wrong');
      assert.equal(fs.existsSync(file('downloads', 'a.bin')), false);
    },
    { routes: { '/a.bin': Buffer.from('also wrong') } },
  );
});

test('checksum.txt 与写死的摘要不一致：报错并改名 .BAD，一个条目也不下', async () => {
  await withEnv(
    async ({ run, file, requests }) => {
      await assert.rejects(run(), (e) => e.message.includes('checksum.txt（t1）的摘要') && e.message.includes('不一致') && e.message.includes(sha(Buffer.from('tampered sums'))) && e.message.includes(sha(SUMS)));
      assert.equal(fs.existsSync(file('meta', 'checksum-t1.txt')), false);
      assert.equal(fs.readFileSync(file('meta', 'checksum-t1.txt.BAD'), 'utf8'), 'tampered sums');
      assert.deepEqual(requests, ['/t1/checksum.txt']);
      assert.equal(fs.existsSync(file('downloads', 'a.bin')), false);
    },
    { routes: { '/t1/checksum.txt': Buffer.from('tampered sums') } },
  );
});

test('本地的 checksum.txt 被改过：重新下载，而不是相信它', async () => {
  await withEnv(async ({ run, file, requests }) => {
    await run();
    fs.writeFileSync(file('meta', 'checksum-t1.txt'), `b.bin\t${'0'.repeat(64)}\n`);
    requests.length = 0;
    const { failed } = await run();
    assert.deepEqual(failed, []);
    assert.deepEqual(requests, ['/t1/checksum.txt']);
    assert.ok(fs.readFileSync(file('meta', 'checksum-t1.txt')).equals(SUMS));
  });
});

test('没有官方摘要的条目：已有就只记录当前摘要并标明没核对过，不重下', async () => {
  await withEnv(async ({ run, file, requests }) => {
    fs.mkdirSync(file('downloads'), { recursive: true });
    fs.writeFileSync(file('downloads', 'c.bin'), 'my own copy');
    const { manifest } = await run();
    assert.equal(manifest['c.bin'].sha256, sha(Buffer.from('my own copy')));
    assert.equal(manifest['c.bin'].verified, false);
    assert.equal(manifest['c.bin'].official, false);
    assert.equal(requests.includes('/c.bin'), false);
  });
});

test('清单里别的条目的记录原样保留；这次处理的条目覆盖旧记录', async () => {
  await withEnv(async ({ run, home, file }) => {
    fs.mkdirSync(home, { recursive: true });
    fs.writeFileSync(file('fetch-manifest.json'), JSON.stringify({ 'old.bin': { sha256: 'x' }, 'a.bin': { sha256: 'stale' } }));
    const { manifest } = await run();
    assert.deepEqual(manifest['old.bin'], { sha256: 'x' });
    assert.equal(manifest['a.bin'].sha256, sha(A));
    assert.deepEqual(readJson(file('fetch-manifest.json')), manifest);
  });
});

test('下载本身失败（服务器没有这个文件）：把原因抛出去，中断记进日志，前面已核对的条目已经写进清单', async () => {
  await withEnv(async ({ run, items, base, file, logs }) => {
    await assert.rejects(run({ items: [...items, { group: 'g', name: 'missing.bin', url: `${base}/missing.bin`, sha256: sha(A), digestSource: 'GitHub API digest' }] }), /下载失败（试了 2 次）.*HTTP 404/);
    assert.ok(logs.includes('  下载中断（第 1/2 次）：HTTP 404'), logs.join('\n'));
    assert.deepEqual(Object.keys(readJson(file('fetch-manifest.json'))), ['a.bin', 'b.bin', 'c.bin']);
  });
});

test('没有条目也行（只取 checksum.txt）：不写清单文件', async () => {
  await withEnv(async ({ run, file }) => {
    const { manifest, failed } = await run({ items: [] });
    assert.deepEqual(manifest, {});
    assert.deepEqual(failed, []);
    assert.ok(fs.existsSync(file('meta', 'checksum-t1.txt')));
    assert.equal(fs.existsSync(file('fetch-manifest.json')), false);
  });
});
