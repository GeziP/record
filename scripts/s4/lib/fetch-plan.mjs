// S4 要下载的东西（全部来自官方发布页）、它们该核对的摘要与解压到哪里。网络与文件的操作在 fetch.mjs / extract.mjs，这里只有数据与纯函数。

const GH = 'https://github.com/k2-fsa/sherpa-onnx/releases/download';

/**
 * 各发布分类下的 checksum.txt：先用 GitHub API 给出的资产摘要（sha256）核对 checksum.txt 本身，再信它里面各文件的摘要。
 * （下载这三个文件时从 API 取得的摘要，写死在这里，之后任何一次下载都与它比。）
 */
export const CHECKSUM_FILES = Object.freeze([
  { tag: 'asr-models', sha256: '4e34edcb64434bcf533afaee9dcc14b5b2f9c277ed3a745f263e79a4464b28d0' },
  { tag: 'speaker-recongition-models', sha256: 'c3a928cc16d165ce869553f71fcbe6fc1c1b513aee0665414548780a979b2325' },
  { tag: 'punctuation-models', sha256: 'b1bdfef119bd75fef8304c7f9abbd67bcaf5ab22ea3f5fbc1c258c4fd3ea4177' },
]);

/** 某个发布分类下 checksum.txt 的下载地址 */
export const checksumUrl = (tag) => `${GH}/${tag}/checksum.txt`;

const API = 'GitHub API digest';
const FIRST_USE = '首次下载时算出并记录（官方没有给摘要）';
const model = (group, tag, name, extra = {}) => ({ group, name, url: `${GH}/${tag}/${name}`, tag, ...extra });

/**
 * 条目：group 分组；name 文件名；url；摘要的来源三选一：sha256 + digestSource（写死的摘要）或 tag（用该分类的 checksum.txt）；
 * extractTo：压缩包解到 S4 目录下的哪个子目录（单个 .onnx 文件不用解）。
 */
export const ITEMS = Object.freeze([
  { group: 'tools', name: 'sherpa-onnx-v1.13.8-win-x64-shared-MD-Release-no-tts.tar.bz2', url: `${GH}/v1.13.8/sherpa-onnx-v1.13.8-win-x64-shared-MD-Release-no-tts.tar.bz2`, sha256: '876e6b89b8cf84a3a1b375a397507f2cfe9c227c2a945411a11a668475fcb5d3', digestSource: API, extractTo: 'tools' },
  model('small', 'asr-models', 'silero_vad.onnx'),
  { group: 'small', name: 'sherpa-onnx-pyannote-segmentation-3-0.tar.bz2', url: `${GH}/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2`, sha256: '24615ee884c897d9d2ba09bb4d30da6bb1b15e685065962db5b02e76e4996488', digestSource: FIRST_USE, extractTo: 'models' },
  model('small', 'speaker-recongition-models', '3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx'),
  model('small', 'speaker-recongition-models', '3dspeaker_speech_eres2net_large_sv_zh-cn_3dspeaker_16k.onnx'),
  model('small', 'speaker-recongition-models', '3dspeaker_speech_campplus_sv_zh-cn_16k-common.onnx'),
  model('small', 'speaker-recongition-models', '3dspeaker_speech_eres2netv2_sv_zh-cn_16k-common.onnx'),
  model('small', 'speaker-recongition-models', 'wespeaker_zh_cnceleb_resnet34.onnx'),
  model('small', 'punctuation-models', 'sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8.tar.bz2', { sha256: 'c0d5aa5f8eeb686032345e180bedf39319dc2e0556781c6264bcadba8328a6e1', digestSource: API, extractTo: 'models' }),
  model('asr', 'asr-models', 'sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09.tar.bz2', { extractTo: 'models' }),
  model('asr', 'asr-models', 'sherpa-onnx-paraformer-zh-int8-2025-10-07.tar.bz2', { extractTo: 'models' }),
  // 这个文件比 checksum.txt 新，清单里没有它：摘要取自 GitHub API 给的资产摘要
  model('asr', 'asr-models', 'sherpa-onnx-x-asr-zipformer-transducer-zh-en-punct-int8-2026-06-03.tar.bz2', { sha256: '5d02c36d7b44e886b7c8f0d8e051f8713acab96c264bb6ef9e718be39a6a2224', digestSource: API, extractTo: 'models' }),
  model('big', 'asr-models', 'sherpa-onnx-fire-red-asr2-ctc-zh_en-int8-2026-02-25.tar.bz2', { extractTo: 'models' }),
  model('big', 'asr-models', 'sherpa-onnx-fire-red-asr2-zh_en-int8-2026-02-26.tar.bz2', { extractTo: 'models' }),
  model('big', 'asr-models', 'sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25.tar.bz2', { extractTo: 'models' }),
  model('big', 'asr-models', 'sherpa-onnx-funasr-nano-int8-2025-12-30.tar.bz2', { extractTo: 'models' }),
  { group: 'data', name: 'AliMeeting-Eval_Ali.tar.gz', url: 'https://speech-lab-share-data.oss-cn-shanghai.aliyuncs.com/AliMeeting/openlr/Eval_Ali.tar.gz', sha256: 'dc47343b2474b5ebcf458927e878155f6ddeb59c85e685b3645c32a1f9578d92', digestSource: FIRST_USE, extractTo: 'data' },
]);

/** 全部分组（meta 只下 checksum.txt，不在 ITEMS 里） */
export const GROUPS = Object.freeze(['meta', ...new Set(ITEMS.map((i) => i.group))]);

/** checksum.txt：每行“文件名 空白 64 位十六进制摘要” → Map(文件名 → 小写摘要)；不符合格式的行忽略 */
export function parseChecksums(text) {
  const map = new Map();
  for (const line of text.split('\n')) {
    const m = /^(\S+)\s+([0-9a-fA-F]{64})$/.exec(line.trim());
    if (m) map.set(m[1], m[2].toLowerCase());
  }
  return map;
}

/**
 * 一个条目该核对的摘要与来源。checks：Map(分类 → Map(文件名 → 摘要))，来自已用 API 摘要核对过的 checksum.txt。
 * 没有可核对的摘要时 expected 为 undefined：第一次下载时算出来记录，并在清单里标明“无官方摘要”。
 */
export function expectedDigest(item, checks) {
  if (item.sha256) {
    if (!item.digestSource) throw new Error(`${item.name}：写死了摘要却没写来源`);
    return { expected: item.sha256.toLowerCase(), source: item.digestSource, official: item.digestSource === API };
  }
  const fromTag = item.tag ? checks.get(item.tag)?.get(item.name) : undefined;
  if (fromTag) return { expected: fromTag, source: 'checksum.txt', official: true };
  return { expected: undefined, source: '无官方摘要（首次计算，记录在案）', official: false };
}

/** 按分组取条目；不认识的分组报错（拼错了不能悄悄什么都不下） */
export function selectItems(groups) {
  const unknown = groups.filter((g) => !GROUPS.includes(g));
  if (unknown.length) throw new Error(`不认识的分组：${unknown.join('、')}（可选：${GROUPS.join('、')}）`);
  return ITEMS.filter((i) => groups.includes(i.group));
}
