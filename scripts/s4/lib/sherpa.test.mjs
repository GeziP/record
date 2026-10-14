import test from 'node:test';
import assert from 'node:assert/strict';
import { ASR_CANDIDATES, asrArgs, chunkFiles, diarArgs, parseAsrStderr, parseAsrStdout, parseDiarStderr, parseDiarStdout, parseVadAsrStdout, vadAsrArgs } from './sherpa.mjs';

// 下面的样本都是第一次真跑时的输出（截短）。
const ASR_STDOUT = [
  '{"lang": "<|yue|>", "emotion": "<|NEUTRAL|>", "event": "<|Speech|>", "text": "开饭时间早上九点至下午五点", "timestamps": [0.66, 0.90], "durations": [], "tokens":["开", "饭"], "ys_log_probs": [], "words": []}',
  '{"lang": "<|yue|>", "emotion": "<|NEUTRAL|>", "event": "<|Speech|>", "text": "THE TRIVBAL CHIEF \\"THIN\\" CALLED", "timestamps": [0.84], "durations": [], "tokens":["T"], "ys_log_probs": [], "words": []}',
  '',
].join('\n');
const ASR_STDERR = [
  'OfflineRecognizerConfig(feat_config=FeatureExtractorConfig(sampling_rate=16000))',
  'Creating recognizer ...',
  'recognizer created in 0.990 s',
  'Started',
  'Done!',
  '',
  'E:/x/zh.wav',
  '----',
  'E:/x/en.wav',
  '----',
  'num threads: 4',
  'decoding method: greedy_search',
  'Elapsed seconds: 0.162 s',
  'Real time factor (RTF): 0.162 / 12.744 = 0.013',
  '',
].join('\n');
const DIAR_STDOUT = ['OfflineSpeakerDiarizationConfig(segmentation=...)', 'Started', '0.318 -- 10.831 speaker_00', '1.263 -- 2.039 speaker_01', '11.506 -- 17.969 speaker_04', ''].join('\n');
const DIAR_STDERR = ['progress 99.81%', 'progress 100.00%', 'Duration : 300.000 s', 'Elapsed seconds: 40.726 s', 'Real time factor (RTF): 40.726 / 300.000 = 0.136', ''].join('\n');

test('识别候选：七个，编号与目录各不相同，参数都是 --开头的字符串', () => {
  assert.equal(ASR_CANDIDATES.length, 7);
  assert.equal(new Set(ASR_CANDIDATES.map((c) => c.id)).size, 7);
  assert.equal(new Set(ASR_CANDIDATES.map((c) => c.dir)).size, 7);
  for (const c of ASR_CANDIDATES) {
    const a = asrArgs(c.id, 'E:/m', { threads: 8 });
    assert.ok(a.length >= 3, c.id);
    assert.ok(a.every((x) => typeof x === 'string' && x.startsWith('--')), `${c.id}：${a.join(' ')}`);
    assert.equal(a.at(-1), '--num-threads=8');
    assert.ok(a.some((x) => x.includes(`E:/m/${c.dir}/`)), `${c.id} 的参数里应当有模型目录`);
    assert.ok(a.some((x) => x.startsWith('--tokens=')) || c.id === 'qwen3-asr' || c.id === 'funasr-nano', `${c.id} 缺少 tokens`);
  }
});

test('识别参数：具体的几个（SenseVoice 关逆文本规范化、固定中文；FunASR-nano 关逆文本规范化；转录器要三个模型文件）', () => {
  assert.deepEqual(asrArgs('sensevoice', 'E:/m/', { threads: 4 }), [
    '--sense-voice-model=E:/m/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09/model.int8.onnx',
    '--tokens=E:/m/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09/tokens.txt',
    '--sense-voice-language=zh',
    '--sense-voice-use-itn=false',
    '--num-threads=4',
  ]);
  assert.ok(asrArgs('funasr-nano', 'E:/m', { threads: 1 }).includes('--funasr-nano-itn=false'));
  const z = asrArgs('zipformer', 'E:\\m\\', { threads: 2 }); // 反斜杠结尾也去掉
  assert.ok(z.some((x) => x.startsWith('--encoder=E:\\m/') || x.startsWith('--encoder=E:\\m/sherpa')), z.join('\n'));
  assert.equal(z.filter((x) => /^--(encoder|decoder|joiner)=/.test(x)).length, 3);
});

test('识别参数：不认识的候选与不合法的线程数都报错', () => {
  assert.throws(() => asrArgs('nope', 'E:/m', { threads: 1 }), /不认识/);
  assert.throws(() => asrArgs('sensevoice', 'E:/m', { threads: 0 }), /线程数/);
  assert.throws(() => asrArgs('sensevoice', 'E:/m', { threads: 1.5 }), /线程数/);
  assert.throws(() => asrArgs('sensevoice', 'E:/m', {}), /线程数/);
});

test('分批：按命令行长度与个数切，顺序不变、一个不丢、空输入得空', () => {
  assert.deepEqual(chunkFiles([]), []);
  const files = Array.from({ length: 10 }, (_, i) => `f${i}.wav`); // 每个 6 + 3 = 9
  const byLen = chunkFiles(files, { baseLen: 10, maxChars: 10 + 9 * 4 });
  assert.deepEqual(byLen.map((b) => b.length), [4, 4, 2]);
  assert.deepEqual(byLen.flat(), files);
  const byCount = chunkFiles(files, { maxCount: 3 });
  assert.deepEqual(byCount.map((b) => b.length), [3, 3, 3, 1]);
  assert.deepEqual(byCount.flat(), files);
  // 恰好放满（不超过上限）
  assert.deepEqual(chunkFiles(files.slice(0, 4), { baseLen: 10, maxChars: 10 + 9 * 4 }).map((b) => b.length), [4]);
  // baseLen 要算进每批
  assert.deepEqual(chunkFiles(files.slice(0, 4), { baseLen: 11, maxChars: 10 + 9 * 4 }).map((b) => b.length), [3, 1]);
});

test('分批：单个文件放不进一条命令行时报错，不硬塞', () => {
  assert.throws(() => chunkFiles(['x'.repeat(100)], { baseLen: 0, maxChars: 50 }), /太长/);
  assert.throws(() => chunkFiles(['x'.repeat(40)], { baseLen: 20, maxChars: 50 }), /太长/);
  // 边界：恰好放满一条命令行可以，多一个字符就不行
  assert.deepEqual(chunkFiles(['x'.repeat(27)], { baseLen: 20, maxChars: 50 }), [['x'.repeat(27)]]);
  assert.throws(() => chunkFiles(['x'.repeat(28)], { baseLen: 20, maxChars: 50 }), /太长/);
});

test('识别 stdout：按行取 text，转义的引号还原；非 JSON 行忽略；行数对不上、坏 JSON、缺 text 都报错', () => {
  assert.deepEqual(parseAsrStdout(ASR_STDOUT, 2), ['开饭时间早上九点至下午五点', 'THE TRIVBAL CHIEF "THIN" CALLED']);
  assert.deepEqual(parseAsrStdout(`some log\n${ASR_STDOUT}`, 2).length, 2);
  assert.throws(() => parseAsrStdout(ASR_STDOUT, 3), /期望 3，得到 2/);
  assert.throws(() => parseAsrStdout(ASR_STDOUT, 1), /期望 1，得到 2/);
  assert.throws(() => parseAsrStdout('{"text": "好"\n', 1), /不是 JSON/);
  assert.throws(() => parseAsrStdout('{"lang": "x"}\n', 1), /没有 text/);
  assert.deepEqual(parseAsrStdout('', 0), []);
  assert.deepEqual(parseAsrStdout('{"text": ""}\n', 1), ['']); // 空识别结果是合法的
});

test('识别 stderr：加载用时、解码用时、音频时长；RTF 用秒数相除', () => {
  const r = parseAsrStderr(ASR_STDERR);
  assert.equal(r.loadSec, 0.99);
  assert.equal(r.elapsedSec, 0.162);
  assert.equal(r.audioSec, 12.744);
  assert.ok(Math.abs(r.rtf - 0.162 / 12.744) < 1e-12);
  assert.equal(parseAsrStderr('Elapsed seconds: 2.5 s\nReal time factor (RTF): 2.5 / 10.000 = 0.250\n').loadSec, null);
  assert.equal(parseAsrStderr('Elapsed seconds: 2.5 s\nReal time factor (RTF): 2.5 / 0.000 = inf\n').rtf, Infinity);
  assert.throws(() => parseAsrStderr('Started\nDone!\n'), /Elapsed seconds/);
  assert.throws(() => parseAsrStderr('Elapsed seconds: 1.0 s\n'), /Elapsed seconds/);
});

// 真实输出（AliMeeting 近场一条轨，SenseVoice + silero VAD）：stdout 只有分段行，配置与 RTF 在 stderr
const VAD_STDOUT = '7.430 -- 12.844: 我们今天对咱们公司新出上的新出的一款这个手机啊产品啊进行一下这个研讨会\n21.158 -- 22.572: 呃你\n58.310 -- 58.924: 嗯\n';
const VAD_STDERR = 'Creating recognizer ...\nRecognizer created!\nStarted\nReading: a.wav\nStarted!\nnum threads: 8\ndecoding method: greedy_search\nElapsed seconds: 15.271 s\nReal time factor (RTF): 15.271 / 1551.820 = 0.010\n';

test('VAD+识别一体程序：参数在识别参数之后加 VAD 模型；stdout 取 "起点 -- 终点: 文本"；stderr 没有加载用时也能解析', () => {
  const a = vadAsrArgs('paraformer', 'E:/m', { threads: 4, vadModel: 'E:/v/silero_vad.onnx' });
  assert.deepEqual(a, [...asrArgs('paraformer', 'E:/m', { threads: 4 }), '--silero-vad-model=E:/v/silero_vad.onnx']);
  assert.throws(() => vadAsrArgs('paraformer', 'E:/m', { threads: 4 }), /VAD/);
  assert.throws(() => vadAsrArgs('paraformer', 'E:/m', { threads: 4, vadModel: '' }), /VAD/);
  assert.throws(() => vadAsrArgs('nope', 'E:/m', { threads: 4, vadModel: 'v' }), /不认识/);

  assert.deepEqual(parseVadAsrStdout(VAD_STDOUT), [
    { start: 7.43, end: 12.844, text: '我们今天对咱们公司新出上的新出的一款这个手机啊产品啊进行一下这个研讨会' },
    { start: 21.158, end: 22.572, text: '呃你' },
    { start: 58.31, end: 58.924, text: '嗯' },
  ]);
  assert.deepEqual(parseVadAsrStdout(''), []);
  assert.deepEqual(parseVadAsrStdout('Started\n1.0 -- 2.0: a: b\r\n3 -- 4:\nx 5.0 -- 6.0: 前面有别的字\n7.0 -- 8.0 没有冒号\n'), [
    { start: 1, end: 2, text: 'a: b' }, // 文本里的冒号保留，只认第一个
    { start: 3, end: 4, text: '' }, // 空文本合法
  ]);
  const e = parseAsrStderr(VAD_STDERR);
  assert.equal(e.loadSec, null);
  assert.equal(e.elapsedSec, 15.271);
  assert.equal(e.audioSec, 1551.82);
});

test('分离 stdout：只取 "起点 -- 终点 speaker_NN" 的行，数值按浮点', () => {
  assert.deepEqual(parseDiarStdout(DIAR_STDOUT), [
    { start: 0.318, end: 10.831, speaker: 'speaker_00' },
    { start: 1.263, end: 2.039, speaker: 'speaker_01' },
    { start: 11.506, end: 17.969, speaker: 'speaker_04' },
  ]);
  assert.deepEqual(parseDiarStdout(''), []);
  assert.deepEqual(parseDiarStdout('1.0 -- 2.0 speaker_x\n1.0 - 2.0 speaker_01\nabc -- 2.0 speaker_01\n'), []);
  assert.deepEqual(parseDiarStdout('3 -- 4 speaker_07\r\n'), [{ start: 3, end: 4, speaker: 'speaker_07' }]);
  assert.deepEqual(parseDiarStdout('  3 -- 4 speaker_07  \n'), [{ start: 3, end: 4, speaker: 'speaker_07' }]); // 首尾空白不影响
  assert.deepEqual(parseDiarStdout('foo 1.0 -- 2.0 speaker_01\n1.0 -- 2.0 speaker_01 bar\n'), []); // 行首行尾有别的字就不算
});

test('分离 stderr：时长、用时、RTF；缺项报错', () => {
  const r = parseDiarStderr(DIAR_STDERR);
  assert.equal(r.durationSec, 300);
  assert.equal(r.elapsedSec, 40.726);
  assert.ok(Math.abs(r.rtf - 40.726 / 300) < 1e-12);
  assert.throws(() => parseDiarStderr('Duration : 300.000 s\n'), /Duration/);
  assert.throws(() => parseDiarStderr('Elapsed seconds: 1.0 s\n'), /Duration/);
  assert.equal(parseDiarStderr('Duration : 0.000 s\nElapsed seconds: 1.0 s\n').rtf, Infinity);
});

test('分离参数：阈值、已知人数、线程数、窗口平移比例', () => {
  const base = { segmentationModel: 'S.onnx', embeddingModel: 'E.onnx' };
  assert.deepEqual(diarArgs({ ...base, threshold: 0.8 }), ['--segmentation.pyannote-model=S.onnx', '--embedding.model=E.onnx', '--segmentation.num-threads=1', '--embedding.num-threads=1', '--clustering.cluster-threshold=0.8']);
  const k = diarArgs({ ...base, clusters: 3, threshold: 0.8, threads: 2, windowShiftRatio: 0.25 });
  assert.ok(k.includes('--clustering.num-clusters=3'));
  assert.ok(!k.some((x) => x.startsWith('--clustering.cluster-threshold')));
  assert.ok(k.includes('--segmentation.num-threads=2') && k.includes('--embedding.num-threads=2'));
  assert.ok(k.includes('--segmentation.pyannote-window-shift-ratio=0.25'));
  assert.throws(() => diarArgs(base), /阈值/);
  assert.throws(() => diarArgs({ ...base, threshold: 0 }), /阈值/);
  assert.throws(() => diarArgs({ ...base, clusters: 0 }), /人数要是正整数/);
  assert.throws(() => diarArgs({ ...base, clusters: 2.5 }), /人数要是正整数/);
  assert.ok(diarArgs({ ...base, threshold: 0.8, windowShiftRatio: 1 }).includes('--segmentation.pyannote-window-shift-ratio=1'));
  for (const bad of [0, -0.1, 1.5, Number.NaN]) assert.throws(() => diarArgs({ ...base, threshold: 0.8, windowShiftRatio: bad }), /窗口平移比例/, String(bad));
});
