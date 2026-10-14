import assert from 'node:assert/strict';
import test from 'node:test';
import { candidate, dr, ecell, esession, fullOf, inputs, MIXES, none, pre8, vadOf } from './fixtures.mjs';
import { pct, renderReport, table } from './render.mjs';
import { buildReport } from './report.mjs';

const render = (over) => renderReport(buildReport(inputs(over)), { generatedAt: '2000-01-01 00:00' });
const lines = (md) => md.split('\n');
const has = (md, line) => assert.ok(lines(md).includes(line), `缺这一行：${line}`);
const lacks = (md, text) => assert.ok(!md.includes(text), `不该出现：${text}`);

test('比例写成百分数；表格转义竖线并补上分隔行', () => {
  assert.equal(pct(0.1), '10.0%');
  assert.equal(pct(0.12345, 2), '12.35%');
  assert.equal(pct(0.15, 0), '15%');
  assert.equal(table(['a', 'b'], [['1', 'x|y'], [2, 3]]), '| a | b |\n| --- | --- |\n| 1 | x\\|y |\n| 2 | 3 |');
});

test('结论一览：每项门槛、实测与结论（全部达标的情形）', () => {
  const md = render();
  assert.equal(lines(md)[0], '# S4 结果汇总（生成于 2000-01-01 00:00）');
  has(md, '| 识别准确度（近场、按标注切段） | CER ≤ 15% | a：10.00% | 达标 |');
  has(md, '| 速度（估算 60 分钟的会议） | ≤ 30 分钟 | a：16.2 分钟 | 达标 |');
  has(md, '| 峰值内存 | ≤ 4 GB | a：0.49 GB | 达标 |');
  has(md, '| 说话人分离 DER（干净混合） | ≤ 25% | m1，阈值 0.60：10.80% | 达标 |');
  has(md, '| 人数（估计与实际相差不超过 1 的场次） | ≥ 6/8 | 8/8 | 达标 |');
  has(md, '| 麦克风轨也分人（远场 DER） | ≤ 30% 才开放 | 28.00% | 开放 |');
  has(md, '| 回声消除 | 插入率 ≤ 5% 且近端 CER 增加 ≤ 2 个百分点（全部条件） | corti：0/1 个条件未达标 | 做：corti |');
});

test('结论一览：不达标的项用粗体标出；回声不做、麦克风轨不开放', () => {
  const o = inputs({ echo: MIXES.map((s) => ({ ...esession(s, 600, { c1: { none: none(), corti: ecell({ insHyp: 50 }) } }), asr: 'a' })) });
  o.diar.far = MIXES.map((s) => dr('m1', s, { threshold: 0.6, der: 0.31 }));
  o.diar.confirm = MIXES.map((s) => dr('m1', s, { threshold: 0.6, der: 0.27 }));
  const md = renderReport(buildReport(o), { generatedAt: 'T' });
  has(md, '| 麦克风轨也分人（远场 DER） | ≤ 30% 才开放 | 31.00% | **v1 不开放** |');
  has(md, '| 回声消除 | 插入率 ≤ 5% 且近端 CER 增加 ≤ 2 个百分点（全部条件） | corti：1/1 个条件未达标 | **不做**：提示“外放时请戴耳机” |');
  has(md, '| 说话人分离 DER（干净混合） | ≤ 25% | m1，阈值 0.60：27.00% | **不达标** |');
  has(md, '| 人数（估计与实际相差不超过 1 的场次） | ≥ 6/8 | 8/8 | 达标 |');
  has(md, '| 回声消除（没有选中处理方式，不做） | 0.00 |');
  has(md, '| 说话人分离（系统轨） | 4.20 |');
  has(md, '| 回声消除实时倍数 | 没有选中处理方式，不算 |');
  has(md, '| corti | 否 | c1 | 0.0 |');
});

test('结论一览：人数不合格、识别 CER 不达标与没有候选达标各自标出', () => {
  const over = inputs();
  const overCluster = (r) => ({ ...r, estimatedSpeakers: r.actualSpeakers === 3 ? 6 : r.actualSpeakers, estimatedSpeakers3s: r.actualSpeakers === 3 ? 5 : r.actualSpeakers });
  over.diar.sweep = over.diar.sweep.map(overCluster);
  over.diar.confirm = over.diar.confirm.map(overCluster);
  const md0 = renderReport(buildReport(over), { generatedAt: 'T' });
  has(md0, '| 人数（估计与实际相差不超过 1 的场次） | ≥ 6/8 | 4/8 | **不达标** |');
  has(md0, '| 3 | 4 | 10.80% | 0/4 |'); // 三个远端说话人的四场一场也没合格
  has(md0, '| s5 | 3 | 6 / 5 | 10.8% |'); // 实际 3 人，估计 6 人（说话满 3 秒的 5 人）
  has(md0, '| s1 | 1 | 1 / 1 | 10.8% |');

  const bad = inputs({ candidates: [candidate('b', 0.2, vadOf(0.2, 6, 100))] }); // 速度与内存达标的里面最好的 CER 是 20%
  const md1 = renderReport(buildReport(bad), { generatedAt: 'T' });
  has(md1, '| 识别准确度（近场、按标注切段） | CER ≤ 15% | b：20.00% | **不达标** |');
  has(md1, '| 速度（估算 60 分钟的会议） | ≤ 30 分钟 | b：16.2 分钟 | 达标 |');

  const slow = inputs({ candidates: [{ ...candidate('a', 0.1, null), full: fullOf(0.1, 5, { load: 3 }) }] });
  const md2 = renderReport(buildReport(slow), { generatedAt: 'T' });
  has(md2, '| 识别准确度（近场、按标注切段） | CER ≤ 15% | — | — |');
  has(md2, '| 速度（估算 60 分钟的会议） | ≤ 30 分钟 | — | **不达标** |');
  has(md2, '| 峰值内存 | ≤ 4 GB | — | **不达标** |');
  has(md2, '没有选中的识别候选，不列各阶段的用时。');
  has(md2, '| a | 9.00% | 0.1200 / — | 10.00% | 5.0000 | 3.0 | 否（另加 0.01 分钟） | 99.7 / — | 0.49 | 速度或内存超出 |');
  lacks(md2, '**默认**');
});

test('识别表：预筛与全量的数字、是否自带标点、批识别与端到端的估计、结论；跳过的候选只有预筛', () => {
  const md = render({
    candidates: [
      candidate('a', 0.1, vadOf(0.11, 6, 100)),
      { ...candidate('b', 0.2, null), full: fullOf(0.2, 0.1, { load: 3 }, 0.2) }, // 自带标点
      { id: 'c', name: 'C', prescreen8: pre8, prescreen4: { summary: { rtf: 0.3 } }, full: { skipped: true, reason: '太慢' }, vad: null },
    ],
  });
  has(md, '| a | 9.00% | 0.1200 / — | 10.00% | 0.1000 | 3.0 | 否（另加 0.01 分钟） | 11.5 / 16.2 | 0.49 | **默认** |');
  has(md, '| b | 9.00% | 0.1200 / — | 20.00% | 0.1000 | 3.0 | 是 | 11.4 / — | 0.49 | 速度与内存达标 |');
  has(md, '| c | 9.00% | 0.1200 / 0.3000 | — | — | — | — | — | — | 跳过：太慢 |');
  has(md, '| a | 11.00% | 0.0600 | 0.49 |'); // 端到端
  lacks(md, '| b | 11.00%');
  has(md, '候选：a = A；b = B；c = C。');
});

test('识别表：速度或内存超出的候选照样列出', () => {
  const md = render({ candidates: [candidate('a', 0.1, vadOf(0.11, 6, 100)), { ...candidate('b', 0.12, null), full: fullOf(0.12, 5, { load: 3 }) }] });
  has(md, '| b | 9.00% | 0.1200 / — | 12.00% | 5.0000 | 3.0 | 否（另加 0.01 分钟） | 99.7 / — | 0.49 | 速度或内存超出 |');
});

test('流水线用时估算：各阶段、批识别与端到端两种合计、取大者；没有选回声消除时标出', () => {
  const md = render();
  has(md, '默认识别器 a，一场 60 分钟的会议（分钟）：');
  has(md, '| 解码（两条轨） | 0.24 |');
  has(md, '| 回声消除（麦克风轨） | 0.30 |');
  has(md, '| VAD（两条轨） | 0.60 |');
  has(md, '| 说话人分离（两条轨） | 8.40 |');
  has(md, '| 识别（说话时间 × 全量实时倍数） | 1.80 |');
  has(md, '| 模型加载（两条轨各一次） | 0.10 |');
  has(md, '| 加标点（识别器不带标点） | 0.01 |');
  has(md, '| **批识别估计合计** | 11.45 |');
  has(md, '| **端到端估计**（解码 + 回声 + 分离 + 两条轨的 VAD 与识别一体程序 + 标点） | 16.15 |');
  has(md, '| **取大者（用于门槛）** | 16.15 |');
  has(md, '| “我”轨说话占比 / 系统轨说话占比（评测集实测） | 10.0% / 20.0% |');
  has(md, '| 一场 60 分钟会议的转写稿字数（按评测集的字数密度） | 47 字 |');
  has(md, '| 回声消除实时倍数 | 0.0050 |');
  has(md, '| 说话人分离实时倍数 | 0.0700 |');
  has(md, '| 加标点：模型加载 / 每千个单位 | 0.60 秒 / 0.12 秒 |');
  const own = render({ candidates: [{ ...candidate('a', 0.1, vadOf(0.11, 6, 100)), full: fullOf(0.1, 0.1, { load: 3 }, 0.2) }] });
  has(own, '| 加标点（识别器自带，不另算） | 0.00 |');
  // 选中的不是第一个候选：阶段表要取它自己的那一行
  const second = render({ candidates: [candidate('b', 0.2, null), candidate('a', 0.1, vadOf(0.11, 6, 100))] });
  has(second, '默认识别器 a，一场 60 分钟的会议（分钟）：');
  has(second, '| **取大者（用于门槛）** | 16.15 |');
});

test('说话人分离：曲线表、胜出者、分组、每场、远场、轮换与计时', () => {
  const md = render();
  has(md, '| 声纹模型 | 0.3 | 0.5 | 0.7 | 0.9 | 已知人数 |');
  has(md, '| m1 | 20.0% · 8/8 | 10.0% · 8/8 | 10.5% · 8/8 | 30.0% · 8/8 | — |');
  has(md, '胜出：m1，最低 DER 10.00%，距最低点 1 个百分点内的阈值区间 0.5～0.7，取中点 0.60（不在扫描的阈值上，已补测）。');
  has(md, '该设置合计：DER 10.80%（漏检 5.40%、误检 2.70%、混淆 2.70%），人数合格 8/8 场，平均人数误差 0.00。');
  has(md, '| 1 | 3 | 10.80% | 3/3 |');
  has(md, '| 3 | 4 | 10.80% | 4/4 |');
  has(md, '| s4 | 2 | 2 / 2 | 10.8% |');
  has(md, '远场（阵列第 1 通道，含全部参会者）：DER 28.00%，人数合格 8/8 场；过 30% 的门槛。');
  has(md, '| s4 | 2 | 2 / 2 | 28.0% |');
  has(md, '“我”换成每位其余参会者（补充，不是门槛）：3 种组合，DER 20.00%，人数合格 3/3，平均人数误差 0.00。');
  has(md, '计时（三场、8 线程、含模型加载）：实时倍数 0.0700，峰值内存 300 MB，别的程序的 CPU 占用均值 30%。');
});

test('说话人分离：各声纹模型扫的阈值不一样时，列按阈值从小到大排，缺的格子写横线', () => {
  const o = inputs();
  o.diar.sweep = [
    ...[[0.5, 0.1], [0.9, 0.3]].flatMap(([t, der]) => MIXES.map((s) => dr('m1', s, { threshold: t, der }))),
    ...[[0.3, 0.25], [0.7, 0.13]].flatMap(([t, der]) => MIXES.map((s) => dr('m2', s, { threshold: t, der }))),
  ];
  o.diar.confirm = [];
  const md = renderReport(buildReport(o), { generatedAt: 'T' });
  has(md, '| 声纹模型 | 0.3 | 0.5 | 0.7 | 0.9 | 已知人数 |');
  has(md, '| m1 | — | 10.0% · 8/8 | — | 30.0% · 8/8 | — |');
  has(md, '| m2 | 25.0% · 8/8 | — | 13.0% · 8/8 | — | — |');
});

test('说话人分离：区间中点本来就测过时不写“已补测”；已知人数那一列有数', () => {
  const o = inputs();
  const table2 = { m1: { 0.3: 0.2, 0.5: 0.1, 0.6: 0.102, 0.7: 0.105, 0.9: 0.3 } };
  o.diar.sweep = [
    ...Object.entries(table2.m1).flatMap(([t, der]) => MIXES.map((s) => dr('m1', s, { threshold: Number(t), der }))),
    ...MIXES.map((s) => dr('m1', s, { clusters: 1, der: 0.05 })),
  ];
  o.diar.confirm = [];
  const md = renderReport(buildReport(o), { generatedAt: 'T' });
  has(md, '胜出：m1，最低 DER 10.00%，距最低点 1 个百分点内的阈值区间 0.5～0.7，取中点 0.60。');
  has(md, '| m1 | 20.0% · 8/8 | 10.0% · 8/8 | 10.2% · 8/8 | 10.5% · 8/8 | 30.0% · 8/8 | 5.0% · 8/8 |');
});

test('回声消除：三张表与对照门槛；底线与基线', () => {
  const o = inputs({
    echo: MIXES.map((s) => ({
      ...esession(s, 600, {
        c1: { none: none(), corti: ecell({ insHyp: 4, nearErrors: 6, erleDb: 31.25 }), aec3: ecell({ insHyp: 50 }) },
        c2: { none: none({ nearErrors: 9 }), corti: ecell({ insHyp: 1 }), aec3: ecell({ nearErrors: 9 }) },
      }),
      floor: { nearErrors: 7, nearRefLen: 100, insHyp: 3, insRef: 100 }, // 底线与基线（5%）不同，免得写混
      asr: 'a',
    })),
  });
  const md = renderReport(buildReport(o), { generatedAt: 'T' });
  has(md, '识别器：a。基线（干净近端、不处理）CER 5.00%；没有回声时同样流程的底线：远端单讲段里识别出的字数占参考的 3.00%，近端 CER 7.00%。');
  has(md, '| 处理方式 | c1 | c2 |');
  has(md, '| none | 100.0% | 100.0% |');
  has(md, '| corti | 4.0% | 1.0% |');
  has(md, '| aec3 | 50.0% | 2.0% |');
  has(md, '| none | 0.0 | 4.0 |');
  has(md, '| corti | 1.0 | 0.0 |');
  has(md, '| corti | 31.3 | 20.0 |');
  has(md, '| corti | 是 | — | 0.5 |');
  has(md, '| aec3 | 否 | c1、c2 | 2.0 |');
});

test('回声评测用的识别器与默认识别器不同时，写一句注意', () => {
  const o = inputs({ echo: inputs().echo.map((s) => ({ ...s, asr: 'z' })) });
  const md = renderReport(buildReport(o), { generatedAt: 'T' });
  const note = '注意：回声评测用的识别器是 z，默认识别器是 a；回声部分的数字是用 z 测的。';
  has(md, note);
  assert.equal(lines(md)[lines(md).indexOf(note) - 1], ''); // 与上面的表之间空一行
  lacks(render(), '注意：回声评测用的识别器');
});

test('报告里出现 NaN、undefined、null 就报错，不交出残缺的报告', () => {
  const r = buildReport(inputs());
  const withTiming = (v) => ({ ...r, diar: { ...r.diar, timing: { ...r.diar.timing, ...v } } });
  assert.throws(() => renderReport(withTiming({ othersLoadMean: undefined }), { generatedAt: 'T' }), /无效的值（NaN）/);
  assert.throws(() => renderReport(withTiming({ peakWorkingSetMb: undefined }), { generatedAt: 'T' }), /无效的值（undefined）/);
  assert.throws(() => renderReport(withTiming({ peakWorkingSetMb: null }), { generatedAt: 'T' }), /无效的值（null）/);
  assert.throws(() => renderReport(withTiming({ rtfWall: Infinity }), { generatedAt: 'T' }), /无效的值（Infinity）/);
});
