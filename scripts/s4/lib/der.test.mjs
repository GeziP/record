import test from 'node:test';
import assert from 'node:assert/strict';
import { assignMax, countSpeakers, parseRttm, scoreDer, toRttm } from './der.mjs';

const seg = (start, end, speaker) => ({ start, end, speaker });
const close = (a, b, eps = 0.02) => assert.ok(Math.abs(a - b) <= eps, `${a} 和 ${b} 相差超过 ${eps}`);

test('完全一致，DER 为 0；换了说话人的名字也一样', () => {
  const ref = [seg(0, 10, 'A'), seg(10, 20, 'B')];
  assert.equal(scoreDer(ref, ref, { collar: 0 }).der, 0);
  const renamed = [seg(0, 10, 'x'), seg(10, 20, 'y')];
  assert.equal(scoreDer(ref, renamed, { collar: 0 }).der, 0);
  // 假设里先出现的说话人，对应参考里后出现的那个：映射必须按重合时间来配，不能按出场顺序
  const reordered = [seg(10, 20, 'x'), seg(0, 10, 'y')];
  assert.equal(scoreDer(ref, reordered, { collar: 0 }).der, 0);
});

test('什么都没识别出来：全是漏检', () => {
  const r = scoreDer([seg(0, 10, 'A')], [], { collar: 0 });
  close(r.miss, 10);
  assert.equal(r.fa, 0);
  assert.equal(r.der, 1);
});

test('参考是静音的地方说了话：算误检，分母不变', () => {
  const r = scoreDer([seg(0, 10, 'A')], [seg(0, 10, 'A'), seg(10, 15, 'A')], { collar: 0, duration: 20 });
  close(r.fa, 5);
  close(r.der, 0.5);
});

test('一半时间说话人认错：算混淆', () => {
  const ref = [seg(0, 10, 'A'), seg(10, 20, 'B')];
  const hyp = [seg(0, 15, 'x'), seg(15, 20, 'y')];
  const r = scoreDer(ref, hyp, { collar: 0 });
  close(r.conf, 5);
  close(r.der, 0.25);
});

test('边界偏了 0.1 秒：没有容差时有错，容差 0.25 秒内就不计', () => {
  const ref = [seg(0, 10, 'A'), seg(10, 20, 'B')];
  const hyp = [seg(0.1, 10.1, 'A'), seg(10.1, 20, 'B')];
  assert.ok(scoreDer(ref, hyp, { collar: 0 }).der > 0);
  assert.equal(scoreDer(ref, hyp, { collar: 0.25 }).der, 0);
});

test('重叠说话：识别出两个人就两个都算对，只识别出一个人就漏检一个', () => {
  const ref = [seg(0, 10, 'A'), seg(0, 10, 'B')];
  const both = scoreDer(ref, [seg(0, 10, 'x'), seg(0, 10, 'y')], { collar: 0 });
  assert.equal(both.der, 0);
  const one = scoreDer(ref, [seg(0, 10, 'x')], { collar: 0 });
  close(one.miss, 10);
  close(one.der, 0.5);
});

test('把一个人拆成两个说话人：多出来的那部分算混淆', () => {
  const ref = [seg(0, 20, 'A')];
  const hyp = [seg(0, 12, 'x'), seg(12, 20, 'y')];
  const r = scoreDer(ref, hyp, { collar: 0 });
  close(r.conf, 8);
  assert.equal(r.hypSpeakers, 2);
});

test('匈牙利算法：取总和最大的一一对应，行多于列时多出的行没有配对', () => {
  assert.deepEqual(assignMax([[5, 0], [0, 5]]), [0, 1]);
  assert.deepEqual(assignMax([[1, 9], [8, 2]]), [1, 0]);
  assert.deepEqual(assignMax([[9], [8], [7]]), [0, -1, -1]);
  assert.deepEqual(assignMax([[3, 1, 0]]), [0]);
  assert.deepEqual(assignMax([]), []);
});

test('RTTM 读写互逆，人数按最短说话时长过滤', () => {
  const segs = [seg(1.5, 4, 'A'), seg(4, 5, 'B'), seg(6, 6.5, 'B')];
  const back = parseRttm(toRttm(segs, 'f'));
  assert.deepEqual(back.map((s) => [s.start, s.end, s.speaker]), [[1.5, 4, 'A'], [4, 5, 'B'], [6, 6.5, 'B']]);
  assert.equal(countSpeakers(segs), 2);
  assert.equal(countSpeakers(segs, 2), 1);
});
