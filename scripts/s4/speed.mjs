// 前后处理的速度：ffmpeg 把 FLAC 解成 16 kHz 单声道、官方 VAD 程序、官方标点程序。结果写在 <S4 目录>\results\speed.json。
//   node scripts/s4/speed.mjs --home <目录> [--ffmpeg <ffmpeg.exe>] [--runs 3] [--force]
// 这是计时：跑的时候别的程序不要占 CPU（别的程序的占用记在结果里）。每项测 --runs 次取中位数。
// 输入：第一场会议里“我”的那条近场录音（16 kHz 单声道），先用 ffmpeg 变成 48 kHz 立体声的 FLAC（和录音软件的系统声音轨一个格式，是较重的一种），再计时解回 16 kHz 单声道。
// 标点程序：中文文本经命令行传给官方程序时，会被本机的 ANSI 代码页（GBK）转一次，程序按 UTF-8 读，全是乱码，所以不能拿真实的中文来测；
// 改用英文文本当替身（两种长度：400 与 4000 词），算出“每千个词”的用时，换算时 1 个词按 1 个汉字算。这是个替身，结果里写明；它的用时本来就很小（见结果）。
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { cpuSnapshot, othersLoad } from './lib/cpu.mjs';
import { log, parseArgs } from './lib/cli.mjs';
import { loadSessions } from './lib/dataset.mjs';
import { homeFrom, s4Paths, writeJsonAtomic } from './lib/home.mjs';
import { measureAsync } from './lib/measure.mjs';
import { findFfmpeg, median, punctFit, rtfOf } from './lib/speed.mjs';

const { flags } = parseArgs(process.argv.slice(2));
if (flags.ffmpeg === true) throw new Error('--ffmpeg 后面要跟 ffmpeg.exe 的路径');
const paths = s4Paths(homeFrom(flags.home));
const root = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
const ffmpeg = findFfmpeg({ flag: flags.ffmpeg, root });
const runs = Number(flags.runs ?? 3);
if (!Number.isInteger(runs) || runs < 1) throw new Error(`--runs 要是正整数：${flags.runs}`);
const file = path.join(paths.results, 'speed.json');
if (fs.existsSync(file) && flags.force !== true) {
  log(`已有，跳过：${file}`);
  process.exit(0);
}

const work = path.join(paths.work, 'speed');
fs.rmSync(work, { recursive: true, force: true });
fs.mkdirSync(work, { recursive: true });

/** 起一个程序、计时，返回 { wallMs, cpuMs, peakWorkingSetMb, othersLoad }；失败（退出码非零、超时）直接报错 */
async function timed(what, exe, args, tag) {
  const stdoutFile = path.join(work, `${tag}.out.txt`);
  const stderrFile = path.join(work, `${tag}.err.txt`);
  const before = cpuSnapshot();
  const m = await measureAsync({ exe, args, stdoutFile, stderrFile, priority: 'Normal', timeoutSec: 900 });
  const others = othersLoad(before, cpuSnapshot(), m.cpuMs);
  if (m.timedOut || m.exitCode !== 0) throw new Error(`${what}：退出码 ${m.exitCode}${m.timedOut ? '（超时）' : ''}，见 ${stderrFile}`);
  return { wallMs: m.wallMs, cpuMs: m.cpuMs, peakWorkingSetMb: m.peakWorkingSetMb, othersLoad: others };
}

async function repeat(what, exe, args, tag) {
  const out = [];
  for (let i = 0; i < runs; i++) {
    out.push(await timed(what, exe, args, `${tag}-${i}`));
    log(`  ${what} 第 ${i + 1}/${runs} 次：${(out.at(-1).wallMs / 1000).toFixed(2)} 秒，别的程序占用 ${(out.at(-1).othersLoad * 100).toFixed(0)}%`);
  }
  return out;
}

const sessions = loadSessions(paths.dataset);
const session = sessions[0];
const track = session.tracks.find((t) => t.speaker === session.me);
const flac = path.join(work, 'test.mka');
const wav16 = path.join(work, 'test16k.wav');

log(`输入：${session.id} 的 ${track.speaker}，${track.durationSec.toFixed(1)} 秒`);
await timed('生成 FLAC', ffmpeg, ['-nostdin', '-y', '-i', track.wav, '-ar', '48000', '-ac', '2', '-c:a', 'flac', flac], 'make-flac');
await timed('ffmpeg 版本', ffmpeg, ['-version'], 'version');
const version = fs.readFileSync(path.join(work, 'version.out.txt'), 'utf8');
const decodeRuns = await repeat('ffmpeg 解 FLAC 并转 16 kHz 单声道', ffmpeg, ['-nostdin', '-y', '-i', flac, '-ac', '1', '-ar', '16000', '-c:a', 'pcm_s16le', wav16], 'decode');
const vadRuns = await repeat('VAD', paths.exe('sherpa-onnx-vad'), [`--silero-vad-model=${path.join(paths.downloads, 'silero_vad.onnx')}`, wav16, path.join(work, 'vad-out.wav')], 'vad');

const sentence = 'we will discuss the current state of the catering industry and its future development';
const words = sentence.split(' ');
const proxy = (n) => Array.from({ length: n }, (_, i) => words[i % words.length]).join(' ');
const model = path.join(paths.models, 'sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8', 'model.int8.onnx');
const sizes = [400, 4000];
const punctRuns = {};
for (const n of sizes) punctRuns[n] = await repeat(`标点（英文替身 ${n} 词）`, paths.exe('sherpa-onnx-offline-punctuation'), [`--ct-transformer=${model}`, proxy(n)], `punct-${n}`);
const fit = punctFit(sizes.map((n) => ({ size: n, sec: median(punctRuns[n].map((r) => r.wallMs)) / 1000 })));

const summary = (list) => ({ runs: list, medianWallMs: median(list.map((r) => r.wallMs)), peakWorkingSetMb: Math.max(...list.map((r) => r.peakWorkingSetMb)), othersLoadMean: list.reduce((a, r) => a + r.othersLoad, 0) / list.length });
const decode = summary(decodeRuns);
const vad = summary(vadRuns);
writeJsonAtomic(file, {
  track: { session: session.id, speaker: track.speaker, durationSec: track.durationSec },
  ffmpegVersion: version.split(/\r?\n/)[0],
  decode: { ...decode, rtf: rtfOf(decode.medianWallMs, track.durationSec), format: '48 kHz 立体声 FLAC → 16 kHz 单声道 wav' },
  vad: { ...vad, rtf: rtfOf(vad.medianWallMs, track.durationSec) },
  punct: { proxy: '英文文本，1 个词按 1 个汉字算（中文参数经 ANSI 代码页转换后程序读不了）', sizes, bySize: Object.fromEntries(sizes.map((n) => [n, summary(punctRuns[n])])), loadSec: fit.loadSec, secPer1000: fit.secPer1000 },
  runs,
  finishedAt: new Date().toISOString(),
});
for (const f of ['test.mka', 'test16k.wav', 'vad-out.wav']) fs.rmSync(path.join(work, f), { force: true });
log(`解码实时倍数 ${rtfOf(decode.medianWallMs, track.durationSec).toFixed(4)}；VAD 实时倍数 ${rtfOf(vad.medianWallMs, track.durationSec).toFixed(4)}；标点 加载 ${fit.loadSec.toFixed(2)} 秒 + 每千词 ${fit.secPer1000.toFixed(3)} 秒`);
