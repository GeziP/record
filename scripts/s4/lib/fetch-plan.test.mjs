import assert from 'node:assert/strict';
import test from 'node:test';
import { CHECKSUM_FILES, checksumUrl, expectedDigest, GROUPS, ITEMS, parseChecksums, selectItems } from './fetch-plan.mjs';

const H1 = 'a'.repeat(64);
const H2 = 'B'.repeat(64);

test('checksum.txt：每行“文件名 空白 64 位摘要”，换行用 LF 或 CRLF 都行，摘要统一成小写，别的行忽略', () => {
  const text = `x.onnx\t${H1}\r\ny.tar.bz2 ${H2}\n\n# 注释\nz.onnx\t${H1.slice(1)}\nw.onnx ${H1} 多余\n  v.onnx   ${H1}  \nu.onnx ${H1}${H1.slice(0, 1)}\n${H1} q.onnx\n前缀 t.onnx ${H1}\n`;
  const m = parseChecksums(text);
  assert.deepEqual([...m], [['x.onnx', H1], ['y.tar.bz2', 'b'.repeat(64)], ['v.onnx', H1]]);
});

test('该核对的摘要：写死的（带来源）、checksum.txt 里的、都没有的', () => {
  const checks = new Map([['t1', new Map([['m.onnx', H1]])]]);
  assert.deepEqual(expectedDigest({ name: 'a', sha256: H2, digestSource: 'GitHub API digest' }, checks), { expected: 'b'.repeat(64), source: 'GitHub API digest', official: true });
  assert.deepEqual(expectedDigest({ name: 'a', sha256: H1, digestSource: '首次下载时算出并记录（官方没有给摘要）' }, checks), { expected: H1, source: '首次下载时算出并记录（官方没有给摘要）', official: false });
  assert.deepEqual(expectedDigest({ name: 'm.onnx', tag: 't1' }, checks), { expected: H1, source: 'checksum.txt', official: true });
  // 写死的摘要优先于 checksum.txt
  assert.deepEqual(expectedDigest({ name: 'm.onnx', tag: 't1', sha256: H2, digestSource: 'GitHub API digest' }, checks), { expected: 'b'.repeat(64), source: 'GitHub API digest', official: true });
  assert.deepEqual(expectedDigest({ name: 'new.onnx', tag: 't1' }, checks), { expected: undefined, source: '无官方摘要（首次计算，记录在案）', official: false });
  assert.deepEqual(expectedDigest({ name: 'm.onnx', tag: 'nope' }, checks).expected, undefined);
  assert.deepEqual(expectedDigest({ name: 'n' }, checks).expected, undefined);
  assert.throws(() => expectedDigest({ name: 'a', sha256: H1 }, checks), /a：写死了摘要却没写来源/);
});

test('按分组取条目；meta 只下 checksum.txt 所以没有条目；不认识的分组报错', () => {
  assert.deepEqual(GROUPS, ['meta', 'tools', 'small', 'asr', 'big', 'data']);
  assert.deepEqual(selectItems(['meta']), []);
  assert.deepEqual(selectItems(['data']).map((i) => i.name), ['AliMeeting-Eval_Ali.tar.gz']);
  assert.equal(selectItems(GROUPS).length, ITEMS.length);
  assert.throws(() => selectItems(['tools', 'smal']), /不认识的分组：smal/);
});

test('下载清单自洽：文件名与网址一致、摘要来源齐全、压缩包都有解压目的地、网址只来自官方发布页与评测集的发布处', () => {
  assert.equal(ITEMS.length, 17);
  assert.deepEqual(Object.fromEntries(GROUPS.slice(1).map((g) => [g, ITEMS.filter((i) => i.group === g).length])), { tools: 1, small: 8, asr: 3, big: 4, data: 1 });
  assert.equal(new Set(ITEMS.map((i) => i.name)).size, ITEMS.length);
  const tags = new Set(CHECKSUM_FILES.map((c) => c.tag));
  for (const i of ITEMS) {
    // 评测集在本地改了名（Eval_Ali.tar.gz → AliMeeting-Eval_Ali.tar.gz），免得和别的东西混；其余文件名与网址一致
    assert.equal(i.url.split('/').at(-1), i.group === 'data' ? 'Eval_Ali.tar.gz' : i.name, `${i.name}：网址的文件名对不上`);
    assert.ok(/^https:\/\/(github\.com\/k2-fsa\/sherpa-onnx\/releases\/download\/|speech-lab-share-data\.oss-cn-shanghai\.aliyuncs\.com\/AliMeeting\/)/.test(i.url), `${i.name}：网址不在预期的发布处：${i.url}`);
    if (i.sha256) {
      assert.match(i.sha256, /^[0-9a-f]{64}$/, `${i.name}：摘要格式不对`);
      assert.ok(i.digestSource, `${i.name}：缺摘要来源`);
    } else {
      assert.ok(tags.has(i.tag), `${i.name}：既没有写死的摘要，也没有可用的 checksum.txt 分类（${i.tag}）`);
    }
    const archive = /\.tar\.(bz2|gz)$/.test(i.name);
    assert.equal(Boolean(i.extractTo), archive, `${i.name}：压缩包要有解压目的地，单文件不能有`);
    if (archive) assert.ok(['tools', 'models', 'data'].includes(i.extractTo));
  }
  for (const c of CHECKSUM_FILES) assert.match(c.sha256, /^[0-9a-f]{64}$/);
});

test('checksum.txt 的地址：官方发布页下该分类的 checksum.txt', () => {
  assert.equal(checksumUrl('asr-models'), 'https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/checksum.txt');
  assert.deepEqual(CHECKSUM_FILES.map((c) => checksumUrl(c.tag)), [
    'https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/checksum.txt',
    'https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/checksum.txt',
    'https://github.com/k2-fsa/sherpa-onnx/releases/download/punctuation-models/checksum.txt',
  ]);
});

test('解压目的地：程序到 tools，模型到 models，评测集到 data', () => {
  const dest = (name) => ITEMS.find((i) => i.name === name).extractTo;
  assert.equal(dest('sherpa-onnx-v1.13.8-win-x64-shared-MD-Release-no-tts.tar.bz2'), 'tools');
  assert.equal(dest('sherpa-onnx-paraformer-zh-int8-2025-10-07.tar.bz2'), 'models');
  assert.equal(dest('sherpa-onnx-pyannote-segmentation-3-0.tar.bz2'), 'models');
  assert.equal(dest('sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8.tar.bz2'), 'models');
  assert.equal(dest('AliMeeting-Eval_Ali.tar.gz'), 'data');
});
