import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { download, sha256File } from './download.mjs';

// 不重复的确定内容：每 32 字节是一个序号的 SHA-256，起点错一个字节都能看出来
const CONTENT = Buffer.concat(Array.from({ length: 31_250 }, (_, i) => crypto.createHash('sha256').update(String(i)).digest()));
const FAST = { retryDelayMs: 0, stallMs: 5000 };

async function withServer(handler, fn) {
  const requests = [];
  const server = http.createServer((req, res) => {
    requests.push({ range: req.headers.range, encoding: req.headers['accept-encoding'] });
    handler(req, res, requests.length);
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'record-dl-'));
  try {
    await fn({ url: `http://127.0.0.1:${server.address().port}/f.bin`, dest: path.join(dir, 'sub', 'f.bin'), requests });
  } finally {
    server.closeAllConnections();
    await new Promise((r) => server.close(r));
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

// 正常的服务器：支持 Range（range=false 则无视 Range 总是整份返回）
function send(req, res, { range = true, content = CONTENT } = {}) {
  const m = range && req.headers.range ? /^bytes=(\d+)-$/.exec(req.headers.range) : null;
  if (m) {
    const start = Number(m[1]);
    if (start >= content.length) {
      res.writeHead(416, { 'Content-Range': `bytes */${content.length}` });
      return res.end();
    }
    res.writeHead(206, { 'Content-Length': content.length - start, 'Content-Range': `bytes ${start}-${content.length - 1}/${content.length}` });
    return res.end(content.subarray(start));
  }
  res.writeHead(200, { 'Content-Length': content.length });
  return res.end(content);
}

// 声明了完整长度，只发 n 字节就断开连接
function cutAfter(res, n, content = CONTENT) {
  res.writeHead(200, { 'Content-Length': content.length });
  res.write(content.subarray(0, n), () => setTimeout(() => res.socket.destroy(), 30));
}

const partOf = (dest) => `${dest}.part`;

test('sha256File：与 crypto 直接算的一致；文件不存在就报错', async () => {
  const f = path.join(os.tmpdir(), `record-sha-${process.pid}.bin`);
  fs.writeFileSync(f, CONTENT);
  try {
    assert.equal(await sha256File(f), crypto.createHash('sha256').update(CONTENT).digest('hex'));
  } finally {
    fs.rmSync(f);
  }
  await assert.rejects(sha256File(f), /ENOENT/);
});

test('一次下完：内容一致，建好目录，不留 .part，请求里不要压缩传输、不带 Range', async () => {
  await withServer(send, async ({ url, dest, requests }) => {
    await download(url, dest, FAST);
    assert.ok(fs.readFileSync(dest).equals(CONTENT));
    assert.equal(fs.existsSync(partOf(dest)), false);
    assert.deepEqual(requests, [{ range: undefined, encoding: 'identity' }]);
  });
});

test('中途断开后用 Range 接着下，拼出来的内容一字节不差', async () => {
  await withServer(
    (req, res, n) => (n === 1 ? cutAfter(res, 300_000) : send(req, res)),
    async ({ url, dest, requests }) => {
      await download(url, dest, FAST);
      assert.ok(fs.readFileSync(dest).equals(CONTENT));
      assert.equal(requests.length, 2);
      assert.match(requests[1].range, /^bytes=[1-9]\d*-$/);
      assert.equal(fs.existsSync(partOf(dest)), false);
    },
  );
});

test('续传时服务器无视 Range、整份重发：丢掉旧的部分，不能把整份接在后面', async () => {
  await withServer(
    (req, res, n) => (n === 1 ? cutAfter(res, 300_000) : send(req, res, { range: false })),
    async ({ url, dest, requests }) => {
      await download(url, dest, FAST);
      assert.ok(fs.readFileSync(dest).equals(CONTENT));
      assert.match(requests[1].range, /^bytes=[1-9]\d*-$/);
    },
  );
});

test('续传时服务器给的起点不是请求的起点：丢弃已下的部分从头来，不拼接', async () => {
  await withServer(
    (req, res, n) => {
      if (n === 1) return cutAfter(res, 300_000);
      if (n === 2) {
        res.writeHead(206, { 'Content-Length': CONTENT.length, 'Content-Range': `bytes 0-${CONTENT.length - 1}/${CONTENT.length}` });
        return res.end(CONTENT);
      }
      return send(req, res);
    },
    async ({ url, dest, requests }) => {
      await download(url, dest, FAST);
      assert.ok(fs.readFileSync(dest).equals(CONTENT));
      assert.equal(requests.length, 3);
      assert.equal(requests[2].range, undefined, '第三次应当从头下，不带 Range');
    },
  );
});

test('206 没带 Content-Range：同样丢弃重来', async () => {
  await withServer(
    (req, res, n) => {
      if (n === 1) return cutAfter(res, 300_000);
      if (n === 2) {
        res.writeHead(206, { 'Content-Length': 10 });
        return res.end(CONTENT.subarray(0, 10));
      }
      return send(req, res);
    },
    async ({ url, dest, requests }) => {
      await download(url, dest, FAST);
      assert.ok(fs.readFileSync(dest).equals(CONTENT));
      assert.equal(requests.length, 3);
      assert.equal(requests[2].range, undefined, '丢弃后第三次应当从头下');
    },
  );
});

test('.part 其实已经下完（改名前被打断）：416 并且总长相符就直接改名，不再下载内容', async () => {
  await withServer(send, async ({ url, dest, requests }) => {
    fs.mkdirSync(path.dirname(dest), { recursive: true });
    fs.writeFileSync(partOf(dest), CONTENT);
    await download(url, dest, FAST);
    assert.ok(fs.readFileSync(dest).equals(CONTENT));
    assert.equal(requests.length, 1);
    assert.equal(requests[0].range, `bytes=${CONTENT.length}-`);
    assert.equal(fs.existsSync(partOf(dest)), false);
  });
});

test('.part 比服务器上的文件还长：416 但总长对不上，丢弃重下', async () => {
  await withServer(send, async ({ url, dest, requests }) => {
    fs.mkdirSync(path.dirname(dest), { recursive: true });
    fs.writeFileSync(partOf(dest), Buffer.concat([CONTENT, Buffer.from('多出来的')]));
    await download(url, dest, FAST);
    assert.ok(fs.readFileSync(dest).equals(CONTENT));
    assert.equal(requests.length, 2);
    assert.equal(requests[1].range, undefined);
  });
});

test('416 没有给总长：不敢认，丢弃重下', async () => {
  await withServer(
    (req, res, n) => {
      if (n === 1) {
        res.writeHead(416);
        return res.end();
      }
      return send(req, res);
    },
    async ({ url, dest, requests }) => {
      fs.mkdirSync(path.dirname(dest), { recursive: true });
      fs.writeFileSync(partOf(dest), CONTENT);
      await download(url, dest, FAST);
      assert.ok(fs.readFileSync(dest).equals(CONTENT));
      assert.equal(requests.length, 2);
      assert.equal(requests[1].range, undefined, '丢弃后应当从头下');
    },
  );
});

test('卡住（超过 stallMs 没有新数据）就放弃这次连接，接着下', async () => {
  await withServer(
    (req, res, n) => {
      if (n > 1) return send(req, res);
      res.writeHead(200, { 'Content-Length': CONTENT.length });
      res.write(CONTENT.subarray(0, 200_000)); // 发一部分后不再有动静
    },
    async ({ url, dest, requests }) => {
      await download(url, dest, { retryDelayMs: 0, stallMs: 400 });
      assert.ok(fs.readFileSync(dest).equals(CONTENT));
      assert.match(requests[1].range, /^bytes=[1-9]\d*-$/);
    },
  );
});

test('连响应头都迟迟不来也算卡住', async () => {
  await withServer(
    (req, res, n) => {
      if (n > 1) send(req, res);
    },
    async ({ url, dest, requests }) => {
      await download(url, dest, { retryDelayMs: 0, stallMs: 300 });
      assert.ok(fs.readFileSync(dest).equals(CONTENT));
      assert.equal(requests.length, 2);
    },
  );
});

test('服务器一直回错误码：试满 tries 次后报错，说明原因，不留下目标文件', async () => {
  await withServer(
    (req, res) => {
      res.writeHead(404);
      res.end('no');
    },
    async ({ url, dest, requests }) => {
      await assert.rejects(download(url, dest, { ...FAST, tries: 3 }), (e) => /下载失败（试了 3 次）：http:\/\/127\.0\.0\.1:\d+\/f\.bin：HTTP 404/.test(e.message));
      assert.equal(requests.length, 3);
      assert.equal(fs.existsSync(dest), false);
    },
  );
});

test('每次中断都记到日志里；两次之间等的时间随次数加长（100、200 ms），最后一次失败后不再多等', async () => {
  await withServer(
    (req, res) => {
      res.writeHead(500);
      res.end();
    },
    async ({ url, dest }) => {
      const logs = [];
      const t0 = Date.now();
      await assert.rejects(download(url, dest, { tries: 3, stallMs: 5000, retryDelayMs: 100, onLog: (s) => logs.push(s) }), /HTTP 500/);
      assert.deepEqual(logs, ['  下载中断（第 1/3 次）：HTTP 500', '  下载中断（第 2/3 次）：HTTP 500', '  下载中断（第 3/3 次）：HTTP 500']);
      const spent = Date.now() - t0;
      assert.ok(spent >= 290 && spent < 550, `该等 100 + 200 = 300 ms，实际 ${spent} ms`);
    },
  );
});

test('慢但一直有数据，不算卡住：每收到数据就重新计时，不会因为总时间超过 stallMs 被打断', async () => {
  await withServer(
    (req, res) => {
      res.writeHead(200, { 'Content-Length': CONTENT.length });
      const pieces = 6;
      const size = Math.ceil(CONTENT.length / pieces);
      let i = 0;
      const timer = setInterval(() => {
        res.write(CONTENT.subarray(i * size, (i + 1) * size));
        if (++i === pieces) {
          clearInterval(timer);
          res.end();
        }
      }, 120);
    },
    async ({ url, dest, requests }) => {
      await download(url, dest, { retryDelayMs: 0, stallMs: 400 }); // 总共约 720 ms，超过 400 ms，但每片之间只隔 120 ms
      assert.ok(fs.readFileSync(dest).equals(CONTENT));
      assert.equal(requests.length, 1);
    },
  );
});

test('进度日志：已下 / 总长；续传时已下的数从已有的部分算起，总长包含已有的部分', async () => {
  const BIG = Buffer.concat([CONTENT, CONTENT, CONTENT, CONTENT]);
  await withServer(
    (req, res, n) => (n === 1 ? cutAfter(res, 1_500_000, BIG) : send(req, res, { content: BIG })),
    async ({ url, dest }) => {
      const logs = [];
      await download(url, dest, { ...FAST, progressMs: 0, onLog: (s) => logs.push(s) });
      assert.ok(fs.readFileSync(dest).equals(BIG));
      const progress = logs.filter((s) => s.endsWith(' MB'));
      assert.ok(progress.length > 10);
      assert.ok(progress.every((s) => /^ {2}f\.bin {2}\d \/ 4 MB$/.test(s)), progress.join('\n'));
      assert.equal(progress.at(-1), '  f.bin  4 / 4 MB');
      const done = progress.map((s) => Number(/ (\d) \//.exec(s)[1]));
      assert.deepEqual(done, [...done].sort((a, b) => a - b), '续传后已下的数不能倒退');
    },
  );
});

test('默认每 30 秒才记一次进度，短下载里一条也没有', async () => {
  await withServer(send, async ({ url, dest }) => {
    const logs = [];
    await download(url, dest, { ...FAST, onLog: (s) => logs.push(s) });
    assert.deepEqual(logs, []);
  });
});

test('下完后不留计时器：进程能马上退出，不用等 stallMs', async () => {
  const script = path.join(os.tmpdir(), `record-dl-child-${process.pid}.mjs`);
  fs.writeFileSync(script, `import { download } from ${JSON.stringify(pathToFileURL(fileURLToPath(new URL('./download.mjs', import.meta.url))).href)};\nawait download(process.argv[2], process.argv[3], { stallMs: 60_000 });\n`);
  try {
    await withServer(send, async ({ url, dest }) => {
      const child = spawn(process.execPath, [script, url, dest], { stdio: 'inherit' });
      const code = await new Promise((resolve) => {
        const t = setTimeout(() => {
          child.kill();
          resolve('超过 10 秒还没退出');
        }, 10_000);
        child.on('exit', (c) => {
          clearTimeout(t);
          resolve(c);
        });
      });
      assert.equal(code, 0);
      assert.ok(fs.readFileSync(dest).equals(CONTENT));
    });
  } finally {
    fs.rmSync(script, { force: true });
  }
});

test('服务器没给长度（分块传输）也能下；没给长度时中途断开同样会重试续传', async () => {
  await withServer(
    (req, res, n) => {
      if (n === 1) {
        res.writeHead(200); // 没有 Content-Length
        res.write(CONTENT.subarray(0, 300_000), () => setTimeout(() => res.socket.destroy(), 30));
        return;
      }
      res.writeHead(206, { 'Content-Range': `bytes ${/\d+/.exec(req.headers.range)[0]}-${CONTENT.length - 1}/${CONTENT.length}` });
      res.end(CONTENT.subarray(Number(/\d+/.exec(req.headers.range)[0])));
    },
    async ({ url, dest, requests }) => {
      const logs = [];
      await download(url, dest, { ...FAST, progressMs: 0, onLog: (s) => logs.push(s) });
      assert.ok(fs.readFileSync(dest).equals(CONTENT));
      assert.equal(requests.length, 2);
      const progress = logs.filter((s) => s.endsWith(' MB'));
      assert.ok(progress.length > 0);
      assert.ok(progress.every((s) => /^ {2}f\.bin {2}\d \/ \? MB$/.test(s)), '不知道总长时总长显示 ?');
    },
  );
});
