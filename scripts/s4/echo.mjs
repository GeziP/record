// 回声消除评测（合成回声）。结果写在 <S4 目录>\results\echo\<会议>.json，已有的结果不重跑（加 --force 重跑）。
//   node scripts/s4/echo.mjs --home <目录> --asr <识别候选编号> [--seconds 600] [--batch 32] [--harness <aec-harness.exe>]
// 三场会议（头、中、尾）各取前 --seconds 秒：“我”的近场录音（只留自己说话的部分）当近端；其余参会者各自静音掉没说话的部分后相加当远端。
// 对 3 种回声通路 × 3 档能量比共 9 个条件，把远端经脉冲响应混进近端当作麦克风信号，分别用 不处理 / corti-aec / aec3（各有对齐与不对齐）处理，
// 再用 --asr 指定的识别器识别：近端段逐段识别量近端 CER；回声插入在远端单讲段（我没说话、对方在说）里量，
// 做法是整条处理后的信号只留远端单讲段、其余置零，交给 VAD + 识别一体程序（和产品里一样先过 VAD），数识别出的字。
// 另外每场会议还量一个“底线”：麦克风里没有回声时，同一套办法在远端单讲段里识别出多少字（VAD 偶尔把底噪当成话，识别器会吐“嗯”“好的”）。
// 要先编译 aec-harness：见 scripts/s4/aec-harness；默认在 <S4 目录>\target-aec\release\aec-harness.exe。
import fs from 'node:fs';
import path from 'node:path';
import { decodeFiles, decodeWithVad } from './lib/asr-eval.mjs';
import { log, parseArgs, pickThree } from './lib/cli.mjs';
import { loadSessions, mixTracks, RATE } from './lib/dataset.mjs';
import { baselineCounts, conditions, cutSignal, evalVariants, excerptSegments, farOnlySegments, makeMic, SCENARIOS, scoreOutput, VARIANTS } from './lib/echo-eval.mjs';
import { homeFrom, s4Paths, writeJsonAtomic } from './lib/home.mjs';
import { measureAsync } from './lib/measure.mjs';
import { gateToSpeech } from './lib/prep.mjs';
import { ASR_CANDIDATES } from './lib/sherpa.mjs';
import { readWavChannel, readWavInfo } from './lib/wav.mjs';

const { flags } = parseArgs(process.argv.slice(2));
const paths = s4Paths(homeFrom(flags.home));
const asrId = flags.asr;
if (!ASR_CANDIDATES.some((c) => c.id === asrId)) throw new Error(`要用 --asr 指定识别候选：${ASR_CANDIDATES.map((c) => c.id).join('、')}`);
const seconds = Number(flags.seconds ?? 600);
const batchSize = Number(flags.batch ?? 32);
const force = flags.force === true;
const harness = flags.harness ?? path.join(paths.home, 'target-aec', 'release', 'aec-harness.exe');
if (!fs.existsSync(harness)) throw new Error(`没有找到 ${harness}。先编译：cd scripts/s4/aec-harness; cargo build --release --ignore-rust-version（环境变量 CARGO_TARGET_DIR 指到 <S4 目录>\\target-aec）`);
const run = (req) => measureAsync({ priority: 'Normal', ...req });

const writeF32 = (file, samples) => fs.writeFileSync(file, Buffer.from(samples.buffer, samples.byteOffset, samples.byteLength));
function readF32(file) {
  const b = fs.readFileSync(file);
  return new Float32Array(b.buffer.slice(b.byteOffset, b.byteOffset + b.length));
}

/** 回声消除：把远端与麦克风信号写成 f32 文件交给 aec-harness，读回处理后的信号与它打印的信息 */
async function aec(v, far, mic, dir) {
  fs.mkdirSync(dir, { recursive: true });
  const [farF, micF, outF, stdoutFile, stderrFile] = ['far.f32', 'mic.f32', `${v.id}.f32`, `${v.id}.out.txt`, `${v.id}.err.txt`].map((n) => path.join(dir, n));
  writeF32(farF, far);
  writeF32(micF, mic);
  const m = await run({ exe: harness, args: [v.algo, String(RATE), farF, micF, outF, ...v.opts], stdoutFile, stderrFile });
  if (m.timedOut || m.exitCode !== 0) throw new Error(`${v.id}：aec-harness 退出码 ${m.exitCode}，见 ${stderrFile}`);
  const line = fs.readFileSync(stdoutFile, 'utf8').trim().split(/\r?\n/).at(-1);
  const samples = readF32(outF);
  fs.rmSync(outF);
  return { samples, info: { ...JSON.parse(line), wallMs: m.wallMs, cpuMs: m.cpuMs, peakWorkingSetMb: m.peakWorkingSetMb } };
}

/** 识别：按文件长度从短到长排序后分批（官方程序一次调用的文件按同一个批次补齐解码，长短相差大时又慢又占内存），结果按原来的顺序还回去 */
async function decode(files, dir) {
  const frames = files.map((f) => readWavInfo(f).frames);
  const order = files.map((_, i) => i).sort((a, b) => frames[a] - frames[b]);
  const { texts } = await decodeFiles({ run, exe: paths.exe('sherpa-onnx-offline'), candId: asrId, modelsRoot: paths.models, threads: 8, files: order.map((i) => files[i]), outDir: path.join(dir, 'decode'), chunk: { maxCount: batchSize } });
  const out = new Array(files.length);
  order.forEach((src, k) => (out[src] = texts[k]));
  return out;
}

/** 远端单讲段的整条信号：先过 VAD 再识别（和产品里一样），返回识别出的全部文字 */
async function decodeFar(file) {
  const { segments } = await decodeWithVad({
    run,
    exe: paths.exe('sherpa-onnx-vad-with-offline-asr'),
    candId: asrId,
    modelsRoot: paths.models,
    threads: 8,
    vadModel: path.join(paths.downloads, 'silero_vad.onnx'),
    wav: file,
    outDir: path.join(path.dirname(file), 'vad'),
    tag: path.basename(file, '.wav'),
  });
  return segments.map((x) => x.text).join('');
}

const pct = (a, b) => `${((a / b) * 100).toFixed(1)}%`;

async function evalSession(s) {
  const file = path.join(paths.results, 'echo', `${s.id}.json`);
  if (fs.existsSync(file) && !force) return log(`已有，跳过：${file}`);
  const me = s.tracks.find((t) => t.speaker === s.me);
  const others = s.tracks.filter((t) => t !== me);
  const want = Math.round(seconds * RATE);
  const near = gateToSpeech(readWavChannel(me.wav, 0, 0, want).samples, me.segments);
  const n = near.length;
  const far = new Float32Array(n);
  far.set(mixTracks(others.map((t) => gateToSpeech(readWavChannel(t.wav, 0, 0, n).samples, t.segments))).samples.subarray(0, n));
  const meSegs = excerptSegments(me.segments, seconds);
  const otherSegs = excerptSegments(others.flatMap((t) => t.segments), seconds).sort((a, b) => a.start - b.start || a.end - b.end);
  const farOnly = farOnlySegments(meSegs, otherSegs);
  const refs = { nearRefs: meSegs.map((x) => x.text), farRefs: farOnly.map((x) => x.text) };
  log(`${s.id}：前 ${(n / RATE).toFixed(0)} 秒，“我”${meSegs.length} 段，对方 ${otherSegs.length} 段，其中远端单讲 ${farOnly.length} 段`);

  const base = path.join(paths.work, 'echo', s.id);
  fs.rmSync(base, { recursive: true, force: true });
  const baseline = baselineCounts(refs.nearRefs, await decode(cutSignal(near, meSegs, RATE, path.join(base, 'clean'), 'clean-'), path.join(base, 'clean')));
  log(`  基线（干净近端，不处理）CER ${((baseline.errors / baseline.refLen) * 100).toFixed(2)}%`);

  const evalAt = (scenario, ratioDb, dir, variants) => {
    const { mic } = makeMic({ near, far, meSegments: meSegs, otherSegments: otherSegs, scenario, ratioDb, rate: RATE });
    return evalVariants({ rate: RATE, far, mic, nearSegs: meSegs, farOnlySegs: farOnly, variants, deps: { aec: (v, f, m) => aec(v, f, m, dir), decode: (files) => decode(files, dir), decodeFar }, dir });
  };

  // 底线：麦克风里只有近端和底噪（能量比取负无穷 = 没有回声），不处理
  const floorDir = path.join(base, 'floor');
  const [floorRow] = await evalAt(SCENARIOS[0], -Infinity, floorDir, VARIANTS.filter((v) => v.id === 'none'));
  const floor = scoreOutput(refs, { nearHyps: floorRow.nearHyps, farText: floorRow.farText });
  fs.rmSync(floorDir, { recursive: true, force: true });
  log(`  底线（没有回声、不处理）：近端 CER ${pct(floor.nearErrors, floor.nearRefLen)}，远端单讲段里识别出 ${floor.insHyp} 字（参考 ${floor.insRef} 字，${pct(floor.insHyp, floor.insRef)}）`);

  const cells = {};
  for (const cond of conditions()) {
    const dir = path.join(base, cond.id);
    const rows = await evalAt(cond.scenario, cond.ratioDb, dir, VARIANTS);
    cells[cond.id] = Object.fromEntries(rows.map((r) => [r.variant, { ...scoreOutput(refs, { nearHyps: r.nearHyps, farText: r.farText }), farText: r.farText, erleDb: r.erleDb, aec: r.info }]));
    fs.rmSync(dir, { recursive: true, force: true });
    log(`  ${cond.id}：${rows.map((r) => `${r.variant} 消减 ${r.erleDb.toFixed(1)} dB 插入 ${pct(cells[cond.id][r.variant].insHyp, cells[cond.id][r.variant].insRef)}`).join('；')}`);
  }
  writeJsonAtomic(file, { session: s.id, me: s.me, asr: asrId, seconds: n / RATE, batchSize, baseline, floor, nearRefChars: baseline.refLen, conditions: cells, finishedAt: new Date().toISOString() });
}

for (const s of pickThree(loadSessions(paths.dataset))) await evalSession(s);
log('回声消除评测完成');
