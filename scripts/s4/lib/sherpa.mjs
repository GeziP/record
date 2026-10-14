// sherpa-onnx 官方预编译命令行（v1.13.8）的参数构造与输出解析。这里只有纯函数；真正去跑的是 measure.mjs（asr-eval.mjs、diar-eval.mjs 把它包成注入的 run）。
// 输出格式来自第一次真跑：识别程序把每个文件的结果写成 stdout 里的一行 JSON（按输入顺序），路径、耗时与 RTF 写在 stderr；
// 分离程序把每一段写成 stdout 里的 "起点 -- 终点 speaker_NN"，时长与 RTF 写在 stderr。

/** 识别候选：id → { name, dir（模型目录名）, args(dir, threads)（不含程序、不含 wav） }。顺序固定，报告按这个顺序列。 */
export const ASR_CANDIDATES = Object.freeze([
  {
    id: 'sensevoice',
    name: 'SenseVoice-Small int8',
    dir: 'sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09',
    args: (d) => [`--sense-voice-model=${d}/model.int8.onnx`, `--tokens=${d}/tokens.txt`, '--sense-voice-language=zh', '--sense-voice-use-itn=false'],
  },
  {
    id: 'paraformer',
    name: 'Paraformer-zh int8',
    dir: 'sherpa-onnx-paraformer-zh-int8-2025-10-07',
    args: (d) => [`--paraformer=${d}/model.int8.onnx`, `--tokens=${d}/tokens.txt`],
  },
  {
    id: 'zipformer',
    name: 'Zipformer transducer 中英（带标点）int8',
    dir: 'sherpa-onnx-x-asr-zipformer-transducer-zh-en-punct-int8-2026-06-03',
    args: (d) => [`--encoder=${d}/encoder-epoch-99-avg-1.int8.onnx`, `--decoder=${d}/decoder-epoch-99-avg-1.onnx`, `--joiner=${d}/joiner-epoch-99-avg-1.int8.onnx`, `--tokens=${d}/tokens.txt`, '--decoding-method=greedy_search'],
  },
  {
    id: 'firered-ctc',
    name: 'FireRedASR2 CTC int8',
    dir: 'sherpa-onnx-fire-red-asr2-ctc-zh_en-int8-2026-02-25',
    args: (d) => [`--fire-red-asr-ctc=${d}/model.int8.onnx`, `--tokens=${d}/tokens.txt`],
  },
  {
    id: 'firered-aed',
    name: 'FireRedASR2 编码解码 int8',
    dir: 'sherpa-onnx-fire-red-asr2-zh_en-int8-2026-02-26',
    args: (d) => [`--fire-red-asr-encoder=${d}/encoder.int8.onnx`, `--fire-red-asr-decoder=${d}/decoder.int8.onnx`, `--tokens=${d}/tokens.txt`],
  },
  {
    id: 'qwen3-asr',
    name: 'Qwen3-ASR 0.6B int8',
    dir: 'sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25',
    args: (d) => [`--qwen3-asr-conv-frontend=${d}/conv_frontend.onnx`, `--qwen3-asr-encoder=${d}/encoder.int8.onnx`, `--qwen3-asr-decoder=${d}/decoder.int8.onnx`, `--qwen3-asr-tokenizer=${d}/tokenizer`],
  },
  {
    id: 'funasr-nano',
    name: 'Fun-ASR-Nano int8',
    dir: 'sherpa-onnx-funasr-nano-int8-2025-12-30',
    args: (d) => [
      `--funasr-nano-encoder-adaptor=${d}/encoder_adaptor.int8.onnx`,
      `--funasr-nano-llm=${d}/llm.int8.onnx`,
      `--funasr-nano-embedding=${d}/embedding.int8.onnx`,
      `--funasr-nano-tokenizer=${d}/Qwen3-0.6B`,
      '--funasr-nano-itn=false',
    ],
  },
]);

/** 某个识别候选的命令行参数（不含程序与 wav）。modelsRoot：放各模型目录的目录。 */
export function asrArgs(id, modelsRoot, { threads }) {
  const c = ASR_CANDIDATES.find((x) => x.id === id);
  if (!c) throw new Error(`不认识的识别候选：${id}`);
  if (!Number.isInteger(threads) || threads < 1) throw new Error(`线程数要是正整数：${threads}`);
  const root = modelsRoot.replace(/[\\/]+$/, '');
  return [...c.args(`${root}/${c.dir}`), `--num-threads=${threads}`];
}

/**
 * 把一长串文件分成若干批，使每批的命令行不超过 maxChars 个字符（Windows 的上限是 32767，留出余量给程序路径与参数）。
 * baseLen：不含文件的那部分命令行长度；每个文件算 路径长度 + 3（空格与一对引号）。单个文件就超限时报错而不是硬塞。
 */
export function chunkFiles(files, { baseLen = 0, maxChars = 30000, maxCount = 400 } = {}) {
  const batches = [];
  let cur = [];
  let len = baseLen;
  for (const f of files) {
    const cost = f.length + 3;
    if (baseLen + cost > maxChars) throw new Error(`单个文件的路径太长，放不进一条命令行：${f}`);
    if (cur.length > 0 && (len + cost > maxChars || cur.length >= maxCount)) {
      batches.push(cur);
      cur = [];
      len = baseLen;
    }
    cur.push(f);
    len += cost;
  }
  if (cur.length > 0) batches.push(cur);
  return batches;
}

/** 识别程序的 stdout：每个输入文件一行 JSON，取其中的 text。行数与文件数对不上就报错（宁可停下也不错位）。 */
export function parseAsrStdout(stdout, expectedCount) {
  const lines = stdout.split(/\r?\n/).filter((l) => l.trim().startsWith('{'));
  if (lines.length !== expectedCount) throw new Error(`识别输出的结果行数不对：期望 ${expectedCount}，得到 ${lines.length}`);
  return lines.map((l, i) => {
    let j;
    try {
      j = JSON.parse(l);
    } catch (e) {
      throw new Error(`第 ${i + 1} 行不是 JSON：${l.slice(0, 80)}`);
    }
    if (typeof j.text !== 'string') throw new Error(`第 ${i + 1} 行没有 text 字段：${l.slice(0, 80)}`);
    return j.text;
  });
}

function num(s) {
  return Number(s);
}

/** 识别程序的 stderr：模型加载用时、解码用时、音频总时长；RTF 用后两者相除（程序自己打印的只有三位小数）。 */
export function parseAsrStderr(stderr) {
  const created = stderr.match(/recognizer created in ([\d.]+) s/);
  const elapsed = stderr.match(/Elapsed seconds: ([\d.]+) s/);
  const rtf = stderr.match(/Real time factor \(RTF\): ([\d.]+) \/ ([\d.]+) =/);
  if (!elapsed || !rtf) throw new Error('识别程序的 stderr 里没有找到 Elapsed seconds / Real time factor');
  const elapsedSec = num(elapsed[1]);
  const audioSec = num(rtf[2]);
  return { loadSec: created ? num(created[1]) : null, elapsedSec, audioSec, rtf: audioSec > 0 ? elapsedSec / audioSec : Infinity };
}

/** VAD+识别一体程序的参数：与 asrArgs 相同，再加 silero VAD 模型。 */
export function vadAsrArgs(id, modelsRoot, { threads, vadModel }) {
  if (typeof vadModel !== 'string' || vadModel === '') throw new Error('没有给 VAD 模型路径');
  return [...asrArgs(id, modelsRoot, { threads }), `--silero-vad-model=${vadModel}`];
}

/** VAD+识别一体程序的 stdout：每段一行 "起点 -- 终点: 文本"，其余行忽略。 */
export function parseVadAsrStdout(stdout) {
  const out = [];
  for (const l of stdout.split(/\r?\n/)) {
    const m = l.match(/^(\d+(?:\.\d+)?) -- (\d+(?:\.\d+)?): ?(.*)$/);
    if (m) out.push({ start: num(m[1]), end: num(m[2]), text: m[3] });
  }
  return out;
}

/** 分离程序的 stdout：每段一行 "起点 -- 终点 speaker_NN"，其余行（配置、Started）忽略。 */
export function parseDiarStdout(stdout) {
  const out = [];
  for (const l of stdout.split(/\r?\n/)) {
    const m = l.trim().match(/^(\d+(?:\.\d+)?) -- (\d+(?:\.\d+)?) (speaker_\d+)$/);
    if (m) out.push({ start: num(m[1]), end: num(m[2]), speaker: m[3] });
  }
  return out;
}

/** 分离程序的 stderr：音频时长、用时与 RTF */
export function parseDiarStderr(stderr) {
  const dur = stderr.match(/Duration : ([\d.]+) s/);
  const elapsed = stderr.match(/Elapsed seconds: ([\d.]+) s/);
  if (!dur || !elapsed) throw new Error('分离程序的 stderr 里没有找到 Duration / Elapsed seconds');
  const durationSec = num(dur[1]);
  const elapsedSec = num(elapsed[1]);
  return { durationSec, elapsedSec, rtf: durationSec > 0 ? elapsedSec / durationSec : Infinity };
}

/** 分离程序的命令行参数（不含程序与 wav）。clusters 给定时按已知人数聚类，否则按阈值。 */
export function diarArgs({ segmentationModel, embeddingModel, threshold, clusters, threads = 1, windowShiftRatio }) {
  const a = [`--segmentation.pyannote-model=${segmentationModel}`, `--embedding.model=${embeddingModel}`, `--segmentation.num-threads=${threads}`, `--embedding.num-threads=${threads}`];
  if (clusters !== undefined) {
    if (!Number.isInteger(clusters) || clusters < 1) throw new Error(`人数要是正整数：${clusters}`);
    a.push(`--clustering.num-clusters=${clusters}`);
  } else {
    if (typeof threshold !== 'number' || !(threshold > 0)) throw new Error(`没有给阈值也没有给人数：${threshold}`);
    a.push(`--clustering.cluster-threshold=${threshold}`);
  }
  if (windowShiftRatio !== undefined) {
    if (!(windowShiftRatio > 0 && windowShiftRatio <= 1)) throw new Error(`窗口平移比例要在 (0, 1] 内：${windowShiftRatio}`);
    a.push(`--segmentation.pyannote-window-shift-ratio=${windowShiftRatio}`);
  }
  return a;
}
