import test from 'node:test';
import assert from 'node:assert/strict';
import { cerOne, cerTotal, editCounts, normalizeText } from './cer.mjs';

test('规整：标点、空白、符号去掉，全角转半角，英文转小写，汉字保留', () => {
  assert.equal(normalizeText('你好，世界！ Hello  World。'), '你好世界helloworld');
  assert.equal(normalizeText('ＡＢＣ（测试）'), 'abc测试');
  assert.equal(normalizeText('  ，。！？ '), '');
});

test('规整：阿拉伯数字（含全角）逐位换成汉字数字，所以“2024年”和“二零二四年”算一致', () => {
  assert.equal(normalizeText('2024年'), '二零二四年');
  assert.equal(normalizeText('１２３'), '一二三');
  assert.equal(normalizeText('第0到9'), '第零到九');
  assert.deepEqual(cerOne('二零二四年', '2024年'), { sub: 0, del: 0, ins: 0, refLen: 5 });
});

test('一致时三种错误都是 0', () => {
  assert.deepEqual(cerOne('今天开会', '今天，开会。'), { sub: 0, del: 0, ins: 0, refLen: 4 });
});

test('替换、删除、插入分别数对', () => {
  assert.deepEqual(cerOne('今天开会', '今天开课'), { sub: 1, del: 0, ins: 0, refLen: 4 });
  assert.deepEqual(cerOne('今天开会', '今天会'), { sub: 0, del: 1, ins: 0, refLen: 4 });
  assert.deepEqual(cerOne('今天开会', '今天开个会'), { sub: 0, del: 0, ins: 1, refLen: 4 });
});

test('码点而不是 UTF-16 单元：生僻字（两个单元）只算一个字', () => {
  assert.deepEqual(cerOne('𠮷野家', '吉野家'), { sub: 1, del: 0, ins: 0, refLen: 3 });
});

test('合计是先加计数再相除，不是各句比例的平均', () => {
  const r = cerTotal([['一二三四五六七八九十', '一二三四五六七八九十'], ['甲乙', '丙丁戊']]);
  assert.equal(r.refLen, 12);
  assert.equal(r.errors, 3); // 甲乙 → 丙丁戊：2 个替换 + 1 个插入
  assert.equal(r.sub, 2);
  assert.equal(r.ins, 1);
  assert.equal(r.rate, 3 / 12);
});

test('错误可以超过 100%；参考为空时有输出就是无穷大，没有输出就是 0', () => {
  assert.equal(cerTotal([['你好', '一二三四五六']]).rate, 3); // 2 个替换 + 4 个插入，除以参考的 2 个字
  assert.equal(cerTotal([['', '啊']]).rate, Infinity);
  assert.equal(cerTotal([['', '']]).rate, 0);
});

test('编辑距离在有多种对齐方式时，总数仍然是最小的', () => {
  const c = editCounts(Array.from('abcde'), Array.from('badce'));
  assert.equal(c.sub + c.del + c.ins, 3);
});
