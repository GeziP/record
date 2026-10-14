// WAV 读写：只认 PCM（16/24/32 位整数）与 32 位浮点，只读需要的那一段，8 通道的远场录音不用整份读进内存。
import fs from 'node:fs';

/** 读文件头：返回采样率、通道数、位深、数据区位置，不读采样 */
export function readWavInfo(path) {
  const fd = fs.openSync(path, 'r');
  try {
    const head = Buffer.alloc(12);
    fs.readSync(fd, head, 0, 12, 0);
    if (head.toString('latin1', 0, 4) !== 'RIFF' || head.toString('latin1', 8, 12) !== 'WAVE') throw new Error(`${path}：不是 RIFF/WAVE 文件`);
    let pos = 12;
    let fmt = null;
    const size = fs.fstatSync(fd).size;
    const ch = Buffer.alloc(8);
    while (pos + 8 <= size) {
      fs.readSync(fd, ch, 0, 8, pos);
      const id = ch.toString('latin1', 0, 4);
      const len = ch.readUInt32LE(4);
      if (id === 'fmt ') {
        const b = Buffer.alloc(Math.min(len, 40));
        fs.readSync(fd, b, 0, b.length, pos + 8);
        let tag = b.readUInt16LE(0);
        if (tag === 0xfffe && b.length >= 26) tag = b.readUInt16LE(24); // WAVE_FORMAT_EXTENSIBLE：真正的格式在子格式里
        fmt = { tag, channels: b.readUInt16LE(2), sampleRate: b.readUInt32LE(4), bits: b.readUInt16LE(14) };
      } else if (id === 'data') {
        if (!fmt) throw new Error(`${path}：data 块出现在 fmt 块之前`);
        const bytesPerFrame = (fmt.bits / 8) * fmt.channels;
        const dataLen = Math.min(len, size - (pos + 8)); // 流式写出的文件长度字段可能不准
        return { ...fmt, dataStart: pos + 8, frames: Math.floor(dataLen / bytesPerFrame), bytesPerFrame };
      }
      pos += 8 + len + (len % 2);
    }
    throw new Error(`${path}：没有找到 data 块`);
  } finally {
    fs.closeSync(fd);
  }
}

function decode(buf, info, ch) {
  const n = Math.floor(buf.length / info.bytesPerFrame);
  const out = new Float32Array(n);
  const step = info.bits / 8;
  for (let i = 0; i < n; i++) {
    const o = i * info.bytesPerFrame + ch * step;
    if (info.tag === 3 && info.bits === 32) out[i] = buf.readFloatLE(o);
    else if (info.tag === 1 && info.bits === 16) out[i] = buf.readInt16LE(o) / 32768;
    else if (info.tag === 1 && info.bits === 24) out[i] = buf.readIntLE(o, 3) / 8388608;
    else if (info.tag === 1 && info.bits === 32) out[i] = buf.readInt32LE(o) / 2147483648;
    else throw new Error(`不支持的格式：tag=${info.tag} bits=${info.bits}`);
  }
  return out;
}

/** 读一个通道的一段（单位：帧）。分块读，内存只与这一段的长度有关。 */
export function readWavChannel(path, channel, startFrame = 0, frames = Infinity) {
  const info = readWavInfo(path);
  if (channel >= info.channels) throw new Error(`${path}：只有 ${info.channels} 个通道，没有第 ${channel} 个`);
  const start = Math.min(startFrame, info.frames);
  const n = Math.min(frames, info.frames - start);
  const out = new Float32Array(n);
  const fd = fs.openSync(path, 'r');
  try {
    const CHUNK = 1 << 16;
    const buf = Buffer.alloc(CHUNK * info.bytesPerFrame);
    for (let done = 0; done < n; ) {
      const want = Math.min(CHUNK, n - done);
      const got = fs.readSync(fd, buf, 0, want * info.bytesPerFrame, info.dataStart + (start + done) * info.bytesPerFrame);
      if (got < info.bytesPerFrame) break;
      const part = decode(buf.subarray(0, got), info, channel);
      out.set(part, done);
      done += part.length;
    }
  } finally {
    fs.closeSync(fd);
  }
  return { sampleRate: info.sampleRate, samples: out };
}

/** 写单通道 WAV：默认 16 位整数（超出范围的采样截断），`float: true` 写 32 位浮点 */
export function writeWavMono(path, samples, sampleRate, { float = false } = {}) {
  const bytes = float ? 4 : 2;
  const data = Buffer.alloc(samples.length * bytes);
  for (let i = 0; i < samples.length; i++) {
    if (float) data.writeFloatLE(samples[i], i * 4);
    else data.writeInt16LE(Math.max(-32768, Math.min(32767, Math.round(samples[i] * 32768))), i * 2);
  }
  const h = Buffer.alloc(44);
  h.write('RIFF', 0, 'latin1');
  h.writeUInt32LE(36 + data.length, 4);
  h.write('WAVEfmt ', 8, 'latin1');
  h.writeUInt32LE(16, 16);
  h.writeUInt16LE(float ? 3 : 1, 20);
  h.writeUInt16LE(1, 22);
  h.writeUInt32LE(sampleRate, 24);
  h.writeUInt32LE(sampleRate * bytes, 28);
  h.writeUInt16LE(bytes, 32);
  h.writeUInt16LE(bytes * 8, 34);
  h.write('data', 36, 'latin1');
  h.writeUInt32LE(data.length, 40);
  fs.writeFileSync(path, Buffer.concat([h, data]));
}
