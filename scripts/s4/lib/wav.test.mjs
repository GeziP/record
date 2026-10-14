import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { readWavChannel, readWavInfo, writeWavMono } from './wav.mjs';

const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), 'record-s4-wav-'));

/** 手工拼一个 WAV：可指定格式、通道数、位深，样本按通道交错 */
function rawWav({ tag = 1, channels = 1, bits = 16, rate = 16000, frames, extra = false, extensible = false, lieLength = 0, dataFirst = false }) {
  const bytes = bits / 8;
  const data = Buffer.alloc(frames.length * channels * bytes);
  frames.forEach((f, i) => {
    for (let c = 0; c < channels; c++) {
      const v = f[c];
      const o = (i * channels + c) * bytes;
      if (tag === 3) data.writeFloatLE(v, o);
      else if (bits === 16) data.writeInt16LE(v, o);
      else if (bits === 24) data.writeIntLE(v, o, 3);
      else data.writeInt32LE(v, o);
    }
  });
  const fmt = Buffer.alloc(extensible ? 40 : 16);
  fmt.writeUInt16LE(extensible ? 0xfffe : tag, 0);
  if (extensible) {
    fmt.writeUInt16LE(22, 16); // cbSize
    fmt.writeUInt16LE(bits, 18); // 有效位数
    fmt.writeUInt32LE(3, 20); // 声道掩码
    fmt.writeUInt16LE(tag, 24); // 子格式 GUID 的前两个字节才是真正的格式
  }
  fmt.writeUInt16LE(channels, 2);
  fmt.writeUInt32LE(rate, 4);
  fmt.writeUInt32LE(rate * channels * bytes, 8);
  fmt.writeUInt16LE(channels * bytes, 12);
  fmt.writeUInt16LE(bits, 14);
  const chunk = (id, body) => {
    const h = Buffer.alloc(8);
    h.write(id, 0, 'latin1');
    h.writeUInt32LE(body.length, 4);
    return Buffer.concat([h, body, body.length % 2 ? Buffer.alloc(1) : Buffer.alloc(0)]);
  };
  const parts = [chunk('fmt ', fmt)];
  if (extra) parts.push(chunk('LIST', Buffer.from('abc'))); // 奇数长度的无关块，要跳过（含补齐字节）
  parts.push(chunk('data', data));
  if (dataFirst) parts.reverse();
  const body = Buffer.concat([Buffer.from('WAVE', 'latin1'), ...parts]);
  const head = Buffer.alloc(8);
  head.write('RIFF', 0, 'latin1');
  head.writeUInt32LE(body.length, 4);
  const out = Buffer.concat([head, body]);
  if (lieLength) out.writeUInt32LE(lieLength, out.indexOf(Buffer.from('data', 'latin1')) + 4); // 流式写出的文件，长度字段不准
  return out;
}

test('写出的单通道 16 位文件能原样读回，超出范围的采样被截断', () => {
  const p = path.join(tmp(), 'a.wav');
  writeWavMono(p, Float32Array.from([0, 0.5, -0.5, 2, -2]), 16000);
  const info = readWavInfo(p);
  assert.deepEqual({ rate: info.sampleRate, ch: info.channels, bits: info.bits, frames: info.frames }, { rate: 16000, ch: 1, bits: 16, frames: 5 });
  const { samples } = readWavChannel(p, 0);
  assert.deepEqual(Array.from(samples), [0, 0.5, -0.5, 32767 / 32768, -1]);
});

test('写出的文件头各字段齐全（外部程序会读字节率、块对齐与两个长度）', () => {
  const p = path.join(tmp(), 'h.wav');
  writeWavMono(p, new Float32Array(10), 16000);
  const b = fs.readFileSync(p);
  assert.equal(b.length, 44 + 20);
  assert.equal(b.readUInt32LE(4), 36 + 20); // RIFF 长度
  assert.equal(b.readUInt32LE(16), 16); // fmt 块长度
  assert.equal(b.readUInt32LE(28), 16000 * 2); // 字节率
  assert.equal(b.readUInt16LE(32), 2); // 块对齐
  assert.equal(b.readUInt16LE(34), 16); // 位深
  assert.equal(b.readUInt32LE(40), 20); // data 长度
});

test('浮点写法读回的值不变', () => {
  const p = path.join(tmp(), 'f.wav');
  writeWavMono(p, Float32Array.from([0.25, -0.125, 1.5]), 48000, { float: true });
  assert.equal(readWavInfo(p).tag, 3);
  assert.equal(readWavInfo(p).sampleRate, 48000);
  assert.deepEqual(Array.from(readWavChannel(p, 0).samples), [0.25, -0.125, 1.5]);
});

test('多通道交错的 24 位文件按通道取，并且能只读一段', () => {
  const p = path.join(tmp(), 'm.wav');
  const frames = Array.from({ length: 10 }, (_, i) => [i * 1000, -i * 1000, i]);
  fs.writeFileSync(p, rawWav({ channels: 3, bits: 24, frames }));
  const c1 = readWavChannel(p, 1, 2, 3).samples;
  assert.deepEqual(Array.from(c1), [-2000, -3000, -4000].map((v) => v / 8388608));
  assert.throws(() => readWavChannel(p, 3), /只有 3 个通道/);
});

test('32 位整数与奇数长度的无关块', () => {
  const p = path.join(tmp(), 'x.wav');
  fs.writeFileSync(p, rawWav({ bits: 32, frames: [[1073741824], [-1073741824]], extra: true }));
  assert.deepEqual(Array.from(readWavChannel(p, 0).samples), [0.5, -0.5]);
});

test('WAVE_FORMAT_EXTENSIBLE：真正的格式在子格式里（这里是 24 位整数，3 通道）', () => {
  const p = path.join(tmp(), 'ext.wav');
  fs.writeFileSync(p, rawWav({ tag: 1, channels: 3, bits: 24, extensible: true, frames: [[100, 200, 300], [-100, -200, -300]] }));
  const info = readWavInfo(p);
  assert.equal(info.tag, 1);
  assert.equal(info.channels, 3);
  assert.deepEqual(Array.from(readWavChannel(p, 2).samples), [300 / 8388608, -300 / 8388608]);
});

test('数据长度字段比文件实际大（流式写出）时，以文件实际大小为准', () => {
  const p = path.join(tmp(), 'lie.wav');
  fs.writeFileSync(p, rawWav({ frames: [[1], [2], [3]], lieLength: 1_000_000 }));
  assert.equal(readWavInfo(p).frames, 3);
  assert.deepEqual(Array.from(readWavChannel(p, 0).samples), [1 / 32768, 2 / 32768, 3 / 32768]);
});

test('跨过分块读取的边界（65536 帧）时一个采样也不丢、不重', () => {
  const p = path.join(tmp(), 'long.wav');
  const N = 140_000;
  const frames = Array.from({ length: N }, (_, i) => [i % 30000, -(i % 30000)]);
  fs.writeFileSync(p, rawWav({ channels: 2, frames }));
  const whole = readWavChannel(p, 1).samples;
  assert.equal(whole.length, N);
  for (const i of [0, 1, 65535, 65536, 65537, 131071, 131072, N - 1]) assert.equal(whole[i], (0 - (i % 30000)) / 32768, `第 ${i} 帧`);
  const part = readWavChannel(p, 0, 65000, 70000).samples;
  assert.equal(part.length, 70000);
  assert.equal(part[0], 65000 % 30000 / 32768);
  assert.equal(part[69999], (65000 + 69999) % 30000 / 32768);
});

test('起点超出文件末尾时返回空，不抛错', () => {
  const p = path.join(tmp(), 'e.wav');
  writeWavMono(p, new Float32Array(100), 16000);
  assert.equal(readWavChannel(p, 0, 500, 10).samples.length, 0);
});

test('data 块出现在 fmt 块之前时明确报错', () => {
  const p = path.join(tmp(), 'order.wav');
  fs.writeFileSync(p, rawWav({ frames: [[1], [2]], dataFirst: true }));
  assert.throws(() => readWavInfo(p), /data 块出现在 fmt 块之前/);
});

test('不是 WAV 的文件明确报错', () => {
  const p = path.join(tmp(), 'bad.wav');
  fs.writeFileSync(p, Buffer.from('not a wav file at all'));
  assert.throws(() => readWavInfo(p), /不是 RIFF\/WAVE/);
});
