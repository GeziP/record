// 仅供测试：手写最小的 tar（可以造出 .. 、绝对路径、盘符、符号链接、硬链接这类正常工具不肯造的条目），再 gzip 压缩。
import zlib from 'node:zlib';

function header({ name, size, type, link }) {
  const h = Buffer.alloc(512);
  h.write(name, 0, 100, 'utf8');
  h.write('0000644\0', 100);
  h.write('0000000\0', 108);
  h.write('0000000\0', 116);
  h.write(`${size.toString(8).padStart(11, '0')}\0`, 124);
  h.write('00000000000\0', 136);
  h.write('        ', 148); // 校验和先按 8 个空格算
  h.write(type, 156);
  h.write(link, 157, 100, 'utf8');
  h.write('ustar\0', 257);
  h.write('00', 263);
  let sum = 0;
  for (const b of h) sum += b;
  h.write(`${sum.toString(8).padStart(6, '0')}\0 `, 148);
  return h;
}

/**
 * entries：[{ name, data?, type?, link? }]；type 默认 '0'（文件），'5' 目录，'2' 符号链接，'1' 硬链接。
 * 返回 .tar.gz 的内容。
 */
export function tarGz(entries) {
  const parts = [];
  for (const e of entries) {
    const data = Buffer.from(e.data ?? '');
    parts.push(header({ name: e.name, size: data.length, type: e.type ?? '0', link: e.link ?? '' }));
    if (data.length) parts.push(data, Buffer.alloc((512 - (data.length % 512)) % 512));
  }
  parts.push(Buffer.alloc(1024));
  return zlib.gzipSync(Buffer.concat(parts));
}
