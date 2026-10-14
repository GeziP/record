import assert from 'node:assert/strict';
import test from 'node:test';
import { ACTUAL, candidate, dataset, dr, ecell, esession, fullOf, inputs, MIXES, none, overCluster, pre8, sweepOf, TABLE, vadOf } from './fixtures.mjs';
import { aecRtf, asrOnlyMinutes, asrRow, asrSection, buildReport, charsPerSpeechSec, diarSection, echoSection, finalists, MEETING_MIN, meetingChars, peakMemGb, PUNCT_RATE_MIN, punctMinutes, speechFractions } from './report.mjs';

const track = (speaker, durationSec, segs) => ({ speaker, durationSec, segments: segs.map(([start, end]) => ({ start, end })) });

test('说话占比：各场合在一起算；“我”的轨与“其余人合起来”的轨分开；重叠的说话只算一次', () => {
  const sessions = [
    // 100 秒：我说 20 秒；对方两人 30 + 20 秒，重叠 10 秒 → 合起来 40 秒
    { me: 'A', tracks: [track('A', 100, [[0, 20]]), track('B', 100, [[10, 40]]), track('C', 100, [[30, 50]])] },
    // 300 秒：我说 100 秒；对方 50 秒
    { me: 'D', tracks: [track('D', 300, [[0, 60], [100, 140]]), track('E', 300, [[200, 250]])] },
  ];
  const f = speechFractions(sessions);
  assert.equal(f.me, (20 + 100) / (100 + 300));
  assert.equal(f.sys, (40 + 50) / (100 + 300));
});

test('说话占比：“我”自己的标注段重叠时也只算一次', () => {
  const f = speechFractions([{ me: 'A', tracks: [track('A', 100, [[0, 10], [5, 15]]), track('B', 100, [[50, 60]])] }]);
  assert.equal(f.me, 15 / 100);
  assert.equal(f.sys, 10 / 100);
});

test('说话占比：对方音轨长短不一时取最长的作分母；没有会议、没有“我”、只有“我”都报错', () => {
  const s = { me: 'A', tracks: [track('A', 100, [[0, 10]]), track('B', 90, [[0, 9]]), track('C', 120, [[0, 12]])] };
  assert.equal(speechFractions([s]).sys, (12) / 120);
  assert.throws(() => speechFractions([]), /没有会议/);
  assert.throws(() => speechFractions([{ me: 'Z', tracks: [track('A', 1, [])] }]), /没有“我”：Z/);
  assert.throws(() => speechFractions([{ me: 'A', tracks: [track('A', 1, [])] }]), /只有“我”一个人/);
});

test('只算识别的用时：两条轨的说话时间 × 实时倍数；默认按 60 分钟', () => {
  assert.equal(MEETING_MIN, 60);
  assert.equal(asrOnlyMinutes({ me: 0.3, sys: 0.7 }, 0.1), 6);
  assert.equal(asrOnlyMinutes({ me: 0.3, sys: 0.7 }, 0.1, 30), 3);
});

test('峰值内存：工作集与私有内存取大的，1 GB = 1024 MB', () => {
  assert.equal(peakMemGb({ peakWorkingSetMb: 2048, peakPrivateMb: 1024 }), 2);
  assert.equal(peakMemGb({ peakWorkingSetMb: 1024, peakPrivateMb: 3072 }), 3);
});

const full = (cer, rtf, wsMb = 500, privMb = 400) => ({ score: { cer: { rate: cer } }, summary: { rtf, peakWorkingSetMb: wsMb, peakPrivateMb: privMb } });
const frac = { me: 0.4, sys: 0.6 }; // 合计 1.0：只算识别的用时 = 60 × rtf 分钟

test('决赛圈：速度与内存都达标的里面 CER 最低的前三；跳过的、没跑的不参加', () => {
  const cands = [
    { id: 'a', full: full(0.1, 0.1) }, // 6 分钟
    { id: 'b', full: full(0.05, 0.6) }, // 36 分钟：太慢
    { id: 'c', full: full(0.07, 0.2, 5000, 100) }, // 12 分钟，但内存 4.9 GB：太大
    { id: 'd', full: full(0.12, 0.1) },
    { id: 'e', full: { skipped: true, reason: '太慢' } },
    { id: 'f', full: null },
    { id: 'g', full: full(0.08, 0.3) }, // 18 分钟
    { id: 'h', full: full(0.2, 0.01) },
  ];
  const r = finalists({ cands, speechFrac: frac });
  assert.deepEqual(r.map((x) => x.id), ['g', 'a', 'd']);
  assert.deepEqual(r[0], { id: 'g', cer: 0.08, asrOnlyMin: 60 * 0.3 * 1.0, peakMemGb: 500 / 1024 });
  assert.deepEqual(finalists({ cands, speechFrac: frac, n: 1 }).map((x) => x.id), ['g']);
  assert.deepEqual(finalists({ cands, speechFrac: frac, n: 10 }).map((x) => x.id), ['g', 'a', 'd', 'h']);
});

test('决赛圈：刚好 30 分钟、刚好 4 GB 算达标；CER 一样时按给定的先后', () => {
  const edge = [
    { id: 'x', full: full(0.1, 0.5, 4096, 100) }, // 30 分钟，4 GB
    { id: 'y', full: full(0.1, 0.5001) }, // 30.006 分钟
    { id: 'z', full: full(0.1, 0.1, 4097, 100) }, // 超过 4 GB 一点点
    { id: 'w', full: full(0.1, 0.1) },
  ];
  assert.deepEqual(finalists({ cands: edge, speechFrac: frac }).map((x) => x.id), ['x', 'w']);
  assert.deepEqual(finalists({ cands: [], speechFrac: frac }), []);
});

const near = (a, b, eps = 1e-9) => assert.ok(Math.abs(a - b) < eps, `${a} 与 ${b} 相差超过 ${eps}`);

test('转写稿字数密度：规整后的参考字数 ÷ 各条音轨自己的说话时长之和；同一条音轨里重叠的说话只算一次', () => {
  const seg = (start, end, text) => ({ start, end, text });
  // 4 + 2 + 6（abc 与 123 变成的“一二三”）= 12 字；20 + 10 = 30 秒
  near(charsPerSpeechSec([{ tracks: [{ segments: [seg(0, 10, '你好，世界！'), seg(10, 20, '再见')] }, { segments: [seg(5, 15, 'abc 123')] }] }]), 12 / 30);
  near(charsPerSpeechSec([{ tracks: [{ segments: [seg(0, 10, '一二三四五'), seg(5, 15, '六七八九十')] }] }]), 10 / 15); // 重叠的 5 秒只算一次
  near(charsPerSpeechSec([{ tracks: [{ segments: [seg(0, 10, '一二')] }] }, { tracks: [{ segments: [seg(0, 30, '三四五六')] }] }]), 6 / 40); // 各场合在一起算
  assert.throws(() => charsPerSpeechSec([{ tracks: [{ segments: [] }] }]), /没有说话时间/);
});

test('一场会议的转写稿字数与加标点的用时', () => {
  near(meetingChars({ charsPerSec: 0.4, speechFrac: { me: 0.3, sys: 0.7 } }), 0.4 * 1.0 * 60 * 60);
  near(meetingChars({ charsPerSec: 2, speechFrac: { me: 0.25, sys: 0.25 }, meetingMin: 30 }), 2 * 0.5 * 30 * 60);
  near(punctMinutes({ loadSec: 0.6, secPer1000: 0.12 }, 25000), (0.6 + 0.12 * 25) / 60);
});

const ctx = { speechFrac: { me: 0.4, sys: 0.6 }, rtf: { decode: 0.002, vad: 0.005, aec: 0.03, diar: 0.1 }, useAec: false, diarMic: false, punct: { loadSec: 0.6, secPer1000: 0.12 }, chars: 25000 };

test('识别候选的一行：批识别估计 = 解码 + VAD + 分离 + 识别 + 两次加载 + （不带标点时）加标点', () => {
  const row = asrRow({ id: 'a', name: 'A', prescreen8: pre8, prescreen4: { summary: { rtf: 0.15 } }, full: fullOf(0.1, 0.1), vad: null }, ctx);
  assert.equal(row.status, 'ok');
  near(row.stages.decode, 2 * 60 * 0.002);
  near(row.stages.vad, 2 * 60 * 0.005);
  near(row.stages.diar, 60 * 0.1);
  near(row.stages.asr, 1.0 * 60 * 0.1);
  assert.equal(row.stages.aec, 0);
  assert.equal(row.needsPunct, true);
  near(row.punctMin, (0.6 + 0.12 * 25) / 60);
  near(row.batchMin, 0.24 + 0.6 + 6 + 6 + (2 * 3) / 60 + 0.06);
  assert.equal(row.e2e, null);
  near(row.estMinutes, row.batchMin);
  assert.deepEqual([row.cer, row.rtf8, row.loadSec, row.punctRate], [0.1, 0.1, 3, 0]);
  near(row.peakMemGb, 500 / 1024);
  // 预筛（三场会议、每人前 5 分钟的标注段）：CER、两档线程的实时倍数、按 8 线程预筛算的“只算识别的估计用时”
  near(row.pre.cer, 0.09);
  near(row.pre.rtf8, 0.12);
  near(row.pre.rtf4, 0.15);
  near(row.pre.asrOnlyMin, 60 * 0.12);
  assert.equal(asrRow({ id: 'a', name: 'A', prescreen8: pre8, full: fullOf(0.1, 0.1), vad: null }, ctx).pre.rtf4, null); // 没做 4 线程的预筛
  assert.equal(asrRow({ id: 'a', name: 'A', full: fullOf(0.1, 0.1), vad: null }, ctx).pre, null); // 没做预筛
});

test('识别候选的一行：识别器自己带标点（占比不低于 1%）就不另加标点的用时', () => {
  assert.equal(PUNCT_RATE_MIN, 0.01);
  const at = (rate) => asrRow({ id: 'a', name: 'A', full: fullOf(0.1, 0.1, {}, rate), vad: null }, ctx);
  assert.equal(at(0.2).needsPunct, false);
  assert.equal(at(0.2).punctMin, 0);
  near(at(0.2).batchMin, 0.24 + 0.6 + 6 + 6 + 0.1);
  assert.equal(at(PUNCT_RATE_MIN).needsPunct, false); // 恰好 1% 算带标点
  assert.equal(at(PUNCT_RATE_MIN - 1e-6).needsPunct, true);
});

test('识别候选的一行：做过 VAD 端到端的取两种估计里大的，内存也取大的', () => {
  const slow = asrRow({ id: 'a', name: 'A', full: fullOf(0.1, 0.1, { ws: 2048, priv: 1024 }), vad: vadOf(0.11, 30, 100, { ws: 1024, priv: 3072 }) }, ctx);
  near(slow.e2e.rtfWall, 0.3);
  near(slow.e2e.totalMin, 0.24 + 6 + 2 * 60 * 0.3 + 0.06); // 解码 + 分离 + 两条轨的端到端 + 加标点；VAD 与识别不再单算
  assert.equal(slow.e2e.cer, 0.11);
  near(slow.e2e.peakMemGb, 3);
  near(slow.estMinutes, 42.3);
  near(slow.peakMemGb, 3);
  const fast = asrRow({ id: 'a', name: 'A', full: fullOf(0.1, 0.1, { ws: 2048, priv: 1024 }), vad: vadOf(0.11, 1, 100, { ws: 512, priv: 256 }) }, ctx);
  near(fast.e2e.totalMin, 0.24 + 6 + 1.2 + 0.06);
  near(fast.estMinutes, fast.batchMin); // 端到端更快：取批识别估计
  near(fast.peakMemGb, 2); // 批识别的内存更大
});

test('识别候选的一行：做回声消除与麦克风轨也分人时，对应阶段加进估计（批识别与端到端都加）', () => {
  const both = { ...ctx, useAec: true, diarMic: true };
  const row = asrRow({ id: 'a', name: 'A', full: fullOf(0.1, 0.1), vad: vadOf(0.1, 30, 100) }, both);
  near(row.stages.aec, 60 * 0.03);
  near(row.stages.diar, 2 * 60 * 0.1);
  near(row.e2e.totalMin, 0.24 + 1.8 + 12 + 36 + 0.06);
});

test('识别候选的一行：没跑、被跳过、没有加载用时都明确说出来', () => {
  assert.deepEqual(asrRow({ id: 'x', name: 'X', full: null }, ctx), { id: 'x', name: 'X', status: 'not-run', pre: null });
  assert.deepEqual(asrRow({ id: 'x', name: 'X', full: { skipped: true, reason: '太慢' } }, ctx), { id: 'x', name: 'X', status: 'skipped', reason: '太慢', pre: null });
  // 被跳过的候选只有预筛的数字，照样带在行里
  const skipped = asrRow({ id: 'x', name: 'X', prescreen8: pre8, prescreen4: { summary: { rtf: 0.2 } }, full: { skipped: true, reason: '太慢' } }, ctx);
  assert.deepEqual([skipped.pre.cer, skipped.pre.rtf8, skipped.pre.rtf4], [0.09, 0.12, 0.2]);
  near(skipped.pre.asrOnlyMin, 7.2);
  assert.equal(asrRow({ id: 'x', name: 'X', prescreen8: pre8, full: null }, ctx).pre.rtf4, null);
  assert.throws(() => asrRow({ id: 'x', name: 'X', full: fullOf(0.1, 0.1, { load: null }) }, ctx), /x：没有解析到模型加载用时/);
});

test('识别部分：只有跑完全量的候选参加挑选；CER 差在 0.5 个百分点内取更快的', () => {
  const c = (id, cer, rtf) => ({ id, name: id.toUpperCase(), full: fullOf(cer, rtf, { load: 2 }) });
  const cands = [c('a', 0.1, 0.2), c('b', 0.104, 0.05), c('c', 0.2, 0.01), { id: 'd', name: 'D', full: null }, { id: 'e', name: 'E', full: { skipped: true, reason: 'x' } }];
  const s = asrSection(cands, ctx);
  assert.deepEqual(s.rows.map((r) => [r.id, r.status]), [['a', 'ok'], ['b', 'ok'], ['c', 'ok'], ['d', 'not-run'], ['e', 'skipped']]);
  assert.equal(s.pick.status, 'ok');
  assert.equal(s.pick.chosen.name, 'b');
  assert.equal(s.pick.chosen.estMinutes, s.rows[1].estMinutes);
  // 都太慢：没有候选达标
  const slow = asrSection([c('a', 0.1, 1.0)], ctx);
  assert.equal(slow.pick.status, 'none-eligible');
  // 都不够准：速度达标的里最好的 CER 仍高于 15%
  const bad = asrSection([c('a', 0.2, 0.1)], ctx);
  assert.equal(bad.pick.status, 'cer-gate-failed');
});

test('识别部分：达标与否看“批识别与端到端取大”的用时；CER 用全量的，端到端的 CER 只报告不参加挑选', () => {
  const cand = (id, cer, vad) => ({ id, name: id.toUpperCase(), full: fullOf(cer, 0.1, { load: 3 }), vad });
  // a：批识别 13 分钟，但端到端 42.3 分钟，超过 30：不达标；b：CER 差一点，两种估计都达标
  const s = asrSection([cand('a', 0.1, vadOf(0.1, 30, 100)), cand('b', 0.12, vadOf(0.12, 1, 100))], ctx);
  assert.ok(s.rows[0].batchMin < 30 && s.rows[0].e2e.totalMin > 30);
  assert.equal(s.pick.chosen.name, 'b');
  // c 的端到端 CER 很差，但全量的 CER 最好：照样选 c
  const t = asrSection([cand('c', 0.1, vadOf(0.3, 1, 100)), cand('d', 0.12, vadOf(0.12, 1, 100))], ctx);
  assert.equal(t.pick.chosen.name, 'c');
  assert.equal(t.pick.chosen.cer, 0.1);
});


test('分离部分：中点不在已测的阈值上又没补测：只给出胜出的模型与阈值，不下结论', () => {
  const d = diarSection({ sweep: sweepOf(TABLE), sessionCount: 8 });
  assert.equal(d.pick.model, 'm1');
  near(d.pick.threshold, 0.6);
  assert.equal(d.pick.needsConfirm, true);
  assert.equal(d.status, 'needs-confirm');
  assert.equal(d.point, null);
  assert.equal(d.gate, null);
  assert.deepEqual(d.groups, []);
  assert.deepEqual(d.perSession, []);
  assert.equal(d.far, null);
  assert.equal(d.rotations, null);
  assert.equal(d.timing, null);
  assert.deepEqual(Object.keys(d.curves).sort(), ['m1', 'm2']);
});

test('分离部分：补测之后用中点的结果核对门槛；补测里别的模型与阈值的结果不算；缺场次报错', () => {
  const confirm = MIXES.map((s) => dr('m1', s, { threshold: 0.6, der: 0.108 }));
  const stray = [...MIXES.map((s) => dr('m2', s, { threshold: 0.6, der: 0.5 })), ...MIXES.map((s) => dr('m1', s, { threshold: 0.65, der: 0.5 }))];
  const d = diarSection({ sweep: sweepOf(TABLE), confirm: [...[...confirm].reverse(), ...stray], sessionCount: 8 }); // 结果的顺序是乱的，输出按会议编号排
  assert.equal(d.status, 'pass');
  near(d.point.der, 0.108);
  assert.equal(d.point.countHits, 8);
  assert.deepEqual(d.gate, { derOk: true, countOk: true, pass: true });
  assert.deepEqual(d.perSession.map((r) => [r.mixId, r.actual, r.estimated]), MIXES.map((s) => [s, ACTUAL[s], ACTUAL[s]]));
  assert.deepEqual(d.groups.map((g) => [g.actual, g.sessions, g.countHits]), [[1, 3, 3], [2, 1, 1], [3, 4, 4]]);
  near(d.groups[0].der, 0.108);
  assert.throws(() => diarSection({ sweep: sweepOf(TABLE), confirm: confirm.slice(1), sessionCount: 8 }), /中点 0\.6 的补测只有 7\/8 场/);
});

test('分离部分：DER 达标而人数不合格（把远端说话人数估多了）：结论是不通过，两项分开报', () => {
  const sweep = sweepOf(TABLE, overCluster);
  const confirm = MIXES.map((s) => dr('m1', s, { threshold: 0.6, der: 0.108, est: overCluster }));
  const d = diarSection({ sweep, confirm, sessionCount: 8 });
  assert.equal(d.status, 'fail');
  assert.deepEqual(d.gate, { derOk: true, countOk: false, pass: false });
  assert.equal(d.point.countHits, 4);
  assert.deepEqual(d.groups.map((g) => [g.actual, g.countHits]), [[1, 3], [2, 1], [3, 0]]);
  assert.deepEqual(d.perSession.filter((r) => r.actual === 3).map((r) => r.estimated), [6, 6, 6, 6]);
  const high = diarSection({ sweep: sweepOf({ m1: { 0.5: 0.3, 0.6: 0.4, 0.7: 0.5 } }), sessionCount: 8 }); // 最低点 30% 就是区间中点，已测过
  assert.equal(high.status, 'fail');
  assert.deepEqual(high.gate, { derOk: false, countOk: true, pass: false }); // DER 超过 25%
});

test('分离部分：区间中点算出来带浮点误差时，补测写的阈值仍然对得上', () => {
  // 区间 0.3～0.6，中点 (0.3 + 0.6) / 2 = 0.44999999999999996，补测写的是 0.45
  const table = { m1: { 0.3: 0.105, 0.4: 0.1, 0.5: 0.103, 0.6: 0.104, 0.7: 0.3 } };
  const confirm = MIXES.map((s) => dr('m1', s, { threshold: 0.45, der: 0.108 }));
  const d = diarSection({ sweep: sweepOf(table), confirm, sessionCount: 8 });
  assert.equal(d.pick.needsConfirm, true);
  assert.notEqual(d.pick.threshold, 0.45);
  near(d.pick.threshold, 0.45);
  assert.equal(d.status, 'pass');
  near(d.point.der, 0.108);
});

test('分离部分：区间中点本来就测过，直接用扫描里的结果（已知人数的结果不混进来）', () => {
  const table = { m1: { 0.3: 0.2, 0.5: 0.1, 0.6: 0.102, 0.7: 0.105, 0.9: 0.3 } };
  const known = MIXES.map((s) => dr('m1', s, { clusters: ACTUAL[s], der: 0.01 }));
  const d = diarSection({ sweep: [...sweepOf(table), ...known], sessionCount: 8 });
  assert.equal(d.pick.needsConfirm, false);
  assert.equal(d.status, 'pass');
  near(d.point.der, 0.102);
  // 补测与不补测两条路径给出的 point 字段一样全（渲染要用漏检、误检、混淆、场数与人数误差）
  near(d.point.miss, 0.051);
  near(d.point.fa, 0.0255);
  near(d.point.conf, 0.0255);
  assert.deepEqual([d.point.sessions, d.point.countHits, d.point.countMeanAbsError], [8, 8, 0]);
  assert.deepEqual(d.groups.map((g) => [g.actual, g.sessions, g.countHits]), [[1, 3, 3], [2, 1, 1], [3, 4, 4]]);
  assert.equal(d.perSession.length, 8);
  near(d.perSession[0].der, 0.102);
  near(d.curves.m1.knownCount.der, 0.01);
});

test('分离部分：远场、轮换、计时各自汇总；远场的场次不齐就报错', () => {
  const confirm = MIXES.map((s) => dr('m1', s, { threshold: 0.6, der: 0.108 }));
  const far = (der) => MIXES.map((s) => dr('m1', s, { threshold: 0.6, der }));
  const rotations = ['s1', 's2', 's3'].flatMap((s) => [0.2, 0.3].map((der) => dr('m1', s, { threshold: 0.6, der })));
  const timing = { summary: { rtfWall: 0.07, peakWorkingSetMb: 300 } };
  const d = diarSection({ sweep: sweepOf(TABLE), confirm, far: far(0.28), rotations, timing, sessionCount: 8 });
  near(d.far.point.der, 0.28);
  assert.deepEqual(d.far.gate, { pass: true });
  assert.equal(d.far.perSession.length, 8);
  assert.deepEqual(diarSection({ sweep: sweepOf(TABLE), confirm, far: far(0.31), sessionCount: 8 }).far.gate, { pass: false });
  assert.deepEqual(d.rotations, { sessions: 6, der: 0.25, countHits: 6, countMeanAbsError: 0 });
  assert.deepEqual(d.timing, { rtfWall: 0.07, peakWorkingSetMb: 300 });
  assert.throws(() => diarSection({ sweep: sweepOf(TABLE), confirm, far: far(0.28).slice(1), sessionCount: 8 }), /远场只有 7\/8 场/);
});

// ---- 回声消除 ----

test('回声消除的实时倍数：各条件各场的进程用时合计 ÷ 处理的音频时长合计', () => {
  const sessions = [
    esession('s1', 600, { c1: { none: none(), corti: ecell({ aec: { wallMs: 3000 } }) }, c2: { none: none(), corti: ecell({ aec: { wallMs: 6000 } }) } }),
    esession('s2', 300, { c1: { none: none(), corti: ecell({ aec: { wallMs: 1500 } }) }, c2: { none: none(), corti: ecell({ aec: { wallMs: 1500 } }) } }),
  ];
  near(aecRtf(sessions, 'corti'), (3 + 6 + 1.5 + 1.5) / (600 + 600 + 300 + 300));
  assert.throws(() => aecRtf(sessions, 'aec3'), /会议 s1 缺处理方式 aec3/);
  assert.throws(() => aecRtf(sessions, 'none'), /处理方式 none 没有回声消除程序的计时/);
});

test('回声消除部分：不处理不参加挑选；只有一个处理方式全部条件达标就选它，并给出它的实时倍数', () => {
  const sessions = [esession('s1', 600, { c1: { none: none(), corti: ecell({ aec: { wallMs: 3000 } }), aec3: ecell({ insHyp: 6 }) }, c2: { none: none(), corti: ecell({ aec: { wallMs: 6000 } }), aec3: ecell({}) } })];
  const e = echoSection(sessions);
  assert.equal(e.pick.status, 'ok');
  assert.equal(e.pick.chosen, 'corti');
  assert.deepEqual(e.pick.report.map((r) => r.name), ['corti', 'aec3']);
  assert.deepEqual(e.pick.report.find((r) => r.name === 'aec3').failed, ['c1']); // 插入率 6% 超过 5%
  near(e.rtfAec, 9 / 1200);
  near(e.baselineCer, 0.05);
  near(e.floor.insertion, 0.01);
  assert.deepEqual(e.none.map((c) => c.id), ['c1', 'c2']);
  near(e.none[0].insertion, 1);
  assert.deepEqual(Object.keys(e.variants).sort(), ['aec3', 'corti', 'none']);
});

test('回声消除部分：选中的不是第一个处理方式时，实时倍数用选中的那个', () => {
  const sessions = [esession('s1', 600, { c1: { none: none(), corti: ecell({ insHyp: 50, aec: { wallMs: 3000 } }), aec3: ecell({ aec: { wallMs: 12000 } }) } })];
  const e = echoSection(sessions);
  assert.equal(e.pick.chosen, 'aec3');
  near(e.rtfAec, 12 / 600);
});

test('回声消除部分：没有处理方式达标就不做回声消除，没有实时倍数', () => {
  const sessions = [esession('s1', 600, { c1: { none: none(), corti: ecell({ insHyp: 50 }) } })];
  const e = echoSection(sessions);
  assert.equal(e.pick.status, 'none');
  assert.equal(e.rtfAec, null);
});

const punctOf = (chars) => (0.6 + (0.12 * chars) / 1000) / 60;

test('汇总：各阶段齐了就给出报告；回声消除选中、远场过关，估算里就带上这两段（会议的先后顺序不影响）', () => {
  const r = buildReport(inputs({ sessions: [...dataset].reverse(), echo: inputs().echo.reverse() }));
  near(r.speechFrac.me, 0.1);
  near(r.speechFrac.sys, 0.2);
  near(r.chars, (13 / 300) * 0.3 * 3600); // 13 字 ÷ 300 秒说话 × 0.3 × 60 分钟
  assert.deepEqual([r.useAec, r.diarMic], [true, true]);
  near(r.rtf.decode, 0.002);
  near(r.rtf.vad, 0.005);
  near(r.rtf.aec, 8 * 3 / (8 * 600)); // 8 场 × 1 个条件 × 3 秒 ÷ 8 场 × 600 秒
  near(r.rtf.diar, 0.07);
  assert.deepEqual(r.punct, { loadSec: 0.6, secPer1000: 0.12 });
  assert.equal(r.asr.pick.status, 'ok');
  assert.equal(r.asr.pick.chosen.name, 'a');
  const a = r.asr.rows[0];
  near(a.stages.aec, 60 * 0.005);
  near(a.stages.diar, 2 * 60 * 0.07);
  near(a.batchMin, 0.24 + 0.6 + 0.3 + 8.4 + 1.8 + 0.1 + punctOf(r.chars));
  near(a.e2e.totalMin, 0.24 + 0.3 + 8.4 + 7.2 + punctOf(r.chars));
  assert.equal(r.diar.status, 'pass');
  assert.equal(r.echo.pick.chosen, 'corti');
  assert.equal(r.echo.asr, 'a');
  assert.equal(r.meetingMin, 60);
});

test('汇总：没有回声消除方式达标、远场没过关时，估算里不带这两段', () => {
  const noAec = inputs({ echo: MIXES.map((s) => ({ ...esession(s, 600, { c1: { none: none(), corti: ecell({ insHyp: 50 }) } }), asr: 'a' })) });
  const r1 = buildReport(noAec);
  assert.deepEqual([r1.useAec, r1.rtf.aec, r1.asr.rows[0].stages.aec], [false, 0, 0]);
  const farFail = inputs();
  farFail.diar.far = MIXES.map((s) => dr('m1', s, { threshold: 0.6, der: 0.31 }));
  const r2 = buildReport(farFail);
  assert.equal(r2.diarMic, false);
  near(r2.asr.rows[0].stages.diar, 60 * 0.07);
  assert.equal(r2.diar.far.gate.pass, false);
});

test('汇总：缺哪一项都报错，并说明先跑什么', () => {
  const noPre = inputs();
  noPre.candidates[1] = { ...noPre.candidates[1], prescreen8: null };
  assert.throws(() => buildReport(noPre), /b 还没有预筛结果，先跑 asr prescreen/);
  const noFull = inputs();
  noFull.candidates[1] = { ...noFull.candidates[1], full: null };
  assert.throws(() => buildReport(noFull), /b 还没有全量结果，先跑 asr full/);
  assert.throws(() => buildReport(inputs({ echo: inputs().echo.slice(1) })), /回声评测的会议与评测集不一致：有 s2、s3.*应有 s1、s2/);
  const mixedAsr = inputs();
  mixedAsr.echo[0] = { ...mixedAsr.echo[0], asr: 'b' };
  assert.throws(() => buildReport(mixedAsr), /各场回声评测用的识别候选不一致：a、b/);
  const noSweep = inputs();
  noSweep.diar.sweep = [];
  assert.throws(() => buildReport(noSweep), /分离：没有扫描结果，先跑 diar sweep/);
  const noConfirm = inputs();
  noConfirm.diar.confirm = [];
  assert.throws(() => buildReport(noConfirm), /区间中点 0\.6 还没有补测的结果，先跑 diar confirm/);
  const noFar = inputs();
  noFar.diar.far = [];
  assert.throws(() => buildReport(noFar), /缺远场结果，先跑 diar far/);
  const noRot = inputs();
  noRot.diar.rotations = [];
  assert.throws(() => buildReport(noRot), /缺“我”轮换的结果，先跑 diar rotations/);
  const noTiming = inputs();
  noTiming.diar.timing = null;
  assert.throws(() => buildReport(noTiming), /缺计时结果，先跑 diar timing/);
  const e2e = inputs();
  e2e.candidates[0] = { ...e2e.candidates[0], vad: null };
  assert.throws(() => buildReport(e2e), /选出的 a 没有 VAD 端到端的结果，先跑 asr vad --cands a/);
  // 选中的是第二个、而第一个做过端到端：还是要报错
  const second = inputs({ candidates: [candidate('a', 0.2, vadOf(0.2, 6, 100)), candidate('b', 0.1, null)] });
  assert.throws(() => buildReport(second), /选出的 b 没有 VAD 端到端的结果/);
});

test('汇总：没有候选达标时照常出报告（没有选中的识别器，也就不要求端到端）', () => {
  const tooSlow = { ...candidate('a', 0.1, null), full: fullOf(0.1, 5, { load: 3 }) }; // 只算识别就要 0.3 × 60 × 5 = 90 分钟
  const r = buildReport(inputs({ candidates: [tooSlow] }));
  assert.equal(r.asr.pick.status, 'none-eligible');
  assert.equal(r.asr.pick.chosen, undefined);
});

test('汇总：估算要用的实测数不是有限的数时报错，不让 NaN 悄悄变成“没有候选达标”', () => {
  const bad = (patch) => inputs({ speed: { ...inputs().speed, ...patch } });
  assert.throws(() => buildReport(bad({ decode: {} })), /解码实时倍数不是有限的数：undefined/);
  assert.throws(() => buildReport(bad({ vad: { rtf: NaN } })), /VAD 实时倍数不是有限的数：NaN/);
  assert.throws(() => buildReport(bad({ punct: { loadSec: 1 } })), /标点每千字用时不是有限的数/);
  assert.throws(() => buildReport(bad({ punct: { secPer1000: 1 } })), /标点加载用时不是有限的数/);
  const noDiarRtf = inputs();
  noDiarRtf.diar.timing = { summary: { rtfWall: Infinity } };
  assert.throws(() => buildReport(noDiarRtf), /分离实时倍数不是有限的数：Infinity/);
});
