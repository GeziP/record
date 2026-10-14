// 断点续传的下载（只管网络与文件；摘要对不对是调用方的事）。
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';

export function sha256File(file) {
  return new Promise((resolve, reject) => {
    const h = crypto.createHash('sha256');
    fs.createReadStream(file)
      .on('data', (c) => h.update(c))
      .on('end', () => resolve(h.digest('hex')))
      .on('error', reject);
  });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// 同步写，写完才返回：中断重试时 .part 的大小就是真正落盘的字节数（用流的话，上一次没写完的数据可能在重试后才落盘，续传位置就错了）
function writeAll(fd, chunk) {
  let off = 0;
  while (off < chunk.length) off += fs.writeSync(fd, chunk, off);
}

/**
 * 下载 url 到 dest：先写 dest.part，下完再改名为 dest（中途断开 fetch 会报错，长度它自己核对）。
 * 中途断开，或者 stallMs 毫秒内没有新数据，就用 Range 接着下；最多试 tries 次，仍不行就抛错。
 * 续传位置对不上（服务器无视 Range、给了别的起点、或已下的部分比服务器上的文件还长）时，丢弃已下载的部分从头来，不拼接可疑的数据。
 */
export async function download(url, dest, { tries = 8, stallMs = 90_000, retryDelayMs = 3000, progressMs = 30_000, onLog = () => {} } = {}) {
  const part = `${dest}.part`;
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  let lastError;
  for (let attempt = 1; attempt <= tries; attempt++) {
    const ctl = new AbortController();
    let timer;
    const arm = () => {
      clearTimeout(timer);
      timer = setTimeout(() => ctl.abort(), stallMs);
    };
    try {
      const have = fs.existsSync(part) ? fs.statSync(part).size : 0;
      const headers = { 'Accept-Encoding': 'identity' }; // 不要压缩传输：长度对不上就没法核对
      if (have > 0) headers.Range = `bytes=${have}-`;
      arm();
      const res = await fetch(url, { headers, signal: ctl.signal }); // 默认就会跟随重定向（GitHub 的下载链接会跳转）
      if (res.status === 416) {
        // 请求的起点已经在文件末尾之外：总长等于已有长度才说明早已下完，否则这份 .part 是错的
        await res.body?.cancel();
        const size = /^bytes \*\/(\d+)$/.exec(res.headers.get('content-range') ?? '');
        if (size && Number(size[1]) === have) {
          fs.renameSync(part, dest);
          return;
        }
        fs.rmSync(part, { force: true });
        throw new Error(`已下载的 ${have} 字节与服务器上的文件对不上，丢弃重来`);
      }
      if (res.status !== 200 && res.status !== 206) {
        await res.body?.cancel();
        throw new Error(`HTTP ${res.status}`);
      }
      let append = false;
      if (res.status === 206) {
        const range = res.headers.get('content-range') ?? '';
        const from = /^bytes (\d+)-\d+\/(?:\d+|\*)$/.exec(range);
        if (!from || Number(from[1]) !== have) {
          await res.body?.cancel();
          fs.rmSync(part, { force: true });
          throw new Error(`服务器给的续传起点（${range}）与请求的（从 ${have} 字节起）不符，丢弃已下载的部分重来`);
        }
        append = true;
      }
      // 只用于进度显示：中途断开时 fetch 自己会报错（长度够不够它会核对），到这里没报错就是下完了
      const length = res.headers.has('content-length') ? Number(res.headers.get('content-length')) : null;
      const total = length === null ? null : length + (append ? have : 0);
      let got = append ? have : 0;
      let lastLog = Date.now();
      const fd = fs.openSync(part, append ? 'a' : 'w');
      try {
        for await (const chunk of res.body) {
          arm();
          writeAll(fd, chunk);
          got += chunk.length;
          if (Date.now() - lastLog >= progressMs) {
            lastLog = Date.now();
            onLog(`  ${path.basename(dest)}  ${(got / 1e6).toFixed(0)} / ${total === null ? '?' : (total / 1e6).toFixed(0)} MB`);
          }
        }
      } finally {
        fs.closeSync(fd);
      }
      fs.renameSync(part, dest);
      return;
    } catch (e) {
      lastError = e;
      onLog(`  下载中断（第 ${attempt}/${tries} 次）：${e.message}`);
      if (attempt < tries) await sleep(retryDelayMs * attempt);
    } finally {
      clearTimeout(timer);
    }
  }
  throw new Error(`下载失败（试了 ${tries} 次）：${url}：${lastError.message}`);
}
