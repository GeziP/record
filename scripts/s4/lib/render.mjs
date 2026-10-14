// 把汇总（report.mjs 的 buildReport）写成 Markdown，填设计文档 3.9 用。只负责排版：数字全部来自汇总结果，
// 达标与否的结论来自 decide.mjs 里事先写死的决策规则（这里不重新比大小，免得和规则在边界上不一致）。
import { GATES } from './decide.mjs';

/** 比例写成百分数 */
export const pct = (x, digits = 1) => `${(x * 100).toFixed(digits)}%`;
const n1 = (x) => x.toFixed(1);
const n2 = (x) => x.toFixed(2);
const n4 = (x) => x.toFixed(4);
const dash = '—';

/** Markdown 表格；单元格里的竖线要转义 */
export function table(header, rows) {
  const line = (cols) => `| ${cols.map((c) => String(c).replaceAll('|', '\\|')).join(' | ')} |`;
  return [line(header), line(header.map(() => '---')), ...rows.map(line)].join('\n');
}

const fits = (ok) => (ok ? '达标' : '**不达标**');

function verdicts(r) {
  const { pick } = r.asr;
  const c = pick.chosen;
  const { gate, point, far } = r.diar;
  const aec = r.echo.pick;
  const aecMeasured = aec.report.map((x) => `${x.name}：${x.failed.length}/${r.echo.variants[x.name].conditions.length} 个条件未达标`).join('；');
  return table(
    ['项', '门槛（测之前定下）', '实测', '结论'],
    [
      ['识别准确度（近场、按标注切段）', `CER ≤ ${pct(GATES.asr.cerMax, 0)}`, c ? `${c.name}：${pct(c.cer, 2)}` : dash, c ? fits(pick.status === 'ok') : dash],
      ['速度（估算 60 分钟的会议）', `≤ ${GATES.asr.estMinutesMax} 分钟`, c ? `${c.name}：${n1(c.estMinutes)} 分钟` : dash, fits(Boolean(c))],
      ['峰值内存', `≤ ${GATES.asr.peakMemGbMax} GB`, c ? `${c.name}：${n2(c.peakMemGb)} GB` : dash, fits(Boolean(c))],
      ['说话人分离 DER（干净混合）', `≤ ${pct(GATES.diar.derMax, 0)}`, `${r.diar.pick.model}，阈值 ${n2(r.diar.pick.threshold)}：${pct(point.der, 2)}`, fits(gate.derOk)],
      ['人数（估计与实际相差不超过 1 的场次）', `≥ ${GATES.diar.countSessionsMin}/${point.sessions}`, `${point.countHits}/${point.sessions}`, fits(gate.countOk)],
      ['麦克风轨也分人（远场 DER）', `≤ ${pct(GATES.diar.farFieldDerMax, 0)} 才开放`, pct(far.point.der, 2), far.gate.pass ? '开放' : '**v1 不开放**'],
      ['回声消除', `插入率 ≤ ${pct(GATES.aec.insertionMax, 0)} 且近端 CER 增加 ≤ ${(GATES.aec.nearCerIncreaseMax * 100).toFixed(0)} 个百分点（全部条件）`, aecMeasured, aec.status === 'ok' ? `做：${aec.chosen}` : '**不做**：提示“外放时请戴耳机”'],
    ],
  );
}

function asrSection(a) {
  const eligible = new Set(a.pick.eligible.map((c) => c.name));
  const chosen = a.pick.chosen?.name;
  // 汇总（buildReport）保证每个候选都有预筛与全量的结果（全量可以是“跳过”），所以这里只有“跳过”与“正常”两种行
  const rows = a.rows.map((r) => {
    const pre = [pct(r.pre.cer, 2), `${n4(r.pre.rtf8)} / ${r.pre.rtf4 === null ? dash : n4(r.pre.rtf4)}`];
    if (r.status === 'skipped') return [r.id, ...pre, dash, dash, dash, dash, dash, dash, `跳过：${r.reason}`];
    const verdict = r.id === chosen ? '**默认**' : eligible.has(r.id) ? '速度与内存达标' : '速度或内存超出';
    return [r.id, ...pre, pct(r.cer, 2), n4(r.rtf8), n1(r.loadSec), r.needsPunct ? `否（另加 ${n2(r.punctMin)} 分钟）` : '是', `${n1(r.batchMin)} / ${r.e2e ? n1(r.e2e.totalMin) : dash}`, n2(r.peakMemGb), verdict];
  });
  const e2e = a.rows.filter((r) => r.e2e).map((r) => [r.id, pct(r.e2e.cer, 2), n4(r.e2e.rtfWall), n2(r.e2e.peakMemGb)]);
  return [
    table(['候选', '预筛 CER', '预筛实时倍数（8 / 4 线程）', '全量 CER', '全量实时倍数（8 线程）', '加载（秒）', '自带标点', '估计用时（分钟）：批识别 / 端到端', '峰值内存（GB）', '结论'], rows),
    '',
    `候选：${a.rows.map((r) => `${r.id} = ${r.name}`).join('；')}。`,
    '',
    '端到端（VAD 自动切段 + 识别，整条录音）：',
    '',
    table(['候选', '端到端 CER', '实时倍数（整条录音，含加载）', '峰值内存（GB）'], e2e),
  ].join('\n');
}

function stagesSection(r) {
  const chosen = r.asr.pick.chosen;
  if (!chosen) return '没有选中的识别候选，不列各阶段的用时。';
  const row = r.asr.rows.find((x) => x.id === chosen.name);
  const st = row.stages;
  const rows = [
    ['解码（两条轨）', n2(st.decode)],
    [r.useAec ? '回声消除（麦克风轨）' : '回声消除（没有选中处理方式，不做）', n2(st.aec)],
    ['VAD（两条轨）', n2(st.vad)],
    [r.diarMic ? '说话人分离（两条轨）' : '说话人分离（系统轨）', n2(st.diar)],
    ['识别（说话时间 × 全量实时倍数）', n2(st.asr)],
    ['模型加载（两条轨各一次）', n2((2 * row.loadSec) / 60)],
    [row.needsPunct ? '加标点（识别器不带标点）' : '加标点（识别器自带，不另算）', n2(row.punctMin)],
    ['**批识别估计合计**', n2(row.batchMin)],
    ['**端到端估计**（解码 + 回声 + 分离 + 两条轨的 VAD 与识别一体程序 + 标点）', n2(row.e2e.totalMin)],
    ['**取大者（用于门槛）**', n2(row.estMinutes)],
  ];
  return [`默认识别器 ${chosen.name}，一场 ${r.meetingMin} 分钟的会议（分钟）：`, '', table(['阶段', '用时'], rows)].join('\n');
}

function diarSection(d) {
  const thresholds = [...new Set(Object.values(d.curves).flatMap((c) => c.points.map((p) => p.threshold)))].sort((a, b) => a - b);
  const curveRows = Object.entries(d.curves).map(([name, v]) => [
    name,
    ...thresholds.map((t) => {
      const p = v.points.find((x) => x.threshold === t);
      return p ? `${pct(p.der)} · ${p.countHits}/${p.sessions}` : dash;
    }),
    v.knownCount ? `${pct(v.knownCount.der)} · ${v.knownCount.countHits}/${v.knownCount.sessions}` : dash,
  ]);
  const { pick, point, far, rotations, timing } = d;
  const sessionRows = (list) => list.map((s) => [s.mixId, s.actual, `${s.estimated} / ${s.estimated3s}`, pct(s.der)]);
  const sessionHeader = ['会议', '实际远端人数', '估计人数（出现过 / 说话满 3 秒）', 'DER'];
  return [
    '声纹模型 × 聚类阈值：DER · 人数合格场次（最后一列是已知人数时的 DER，只作参考）：',
    '',
    table(['声纹模型', ...thresholds.map((t) => n1(t)), '已知人数'], curveRows),
    '',
    `胜出：${pick.model}，最低 DER ${pct(pick.minDer, 2)}，距最低点 1 个百分点内的阈值区间 ${n1(pick.band.lo)}～${n1(pick.band.hi)}，取中点 ${n2(pick.threshold)}${pick.needsConfirm ? '（不在扫描的阈值上，已补测）' : ''}。`,
    '',
    `该设置合计：DER ${pct(point.der, 2)}（漏检 ${pct(point.miss, 2)}、误检 ${pct(point.fa, 2)}、混淆 ${pct(point.conf, 2)}），人数合格 ${point.countHits}/${point.sessions} 场，平均人数误差 ${n2(point.countMeanAbsError)}。`,
    '',
    '按远端说话人数分组：',
    '',
    table(['远端说话人数', '场数', 'DER', '人数合格场次'], d.groups.map((g) => [g.actual, g.sessions, pct(g.der, 2), `${g.countHits}/${g.sessions}`])),
    '',
    '每场：',
    '',
    table(sessionHeader, sessionRows(d.perSession)),
    '',
    `远场（阵列第 1 通道，含全部参会者）：DER ${pct(far.point.der, 2)}，人数合格 ${far.point.countHits}/${far.point.sessions} 场；${far.gate.pass ? '过' : '没过'} ${pct(GATES.diar.farFieldDerMax, 0)} 的门槛。`,
    '',
    table(sessionHeader, sessionRows(far.perSession)),
    '',
    `“我”换成每位其余参会者（补充，不是门槛）：${rotations.sessions} 种组合，DER ${pct(rotations.der, 2)}，人数合格 ${rotations.countHits}/${rotations.sessions}，平均人数误差 ${n2(rotations.countMeanAbsError)}。`,
    '',
    `计时（三场、8 线程、含模型加载）：实时倍数 ${n4(timing.rtfWall)}，峰值内存 ${timing.peakWorkingSetMb} MB，别的程序的 CPU 占用均值 ${pct(timing.othersLoadMean, 0)}。`,
  ].join('\n');
}

function echoSection(e) {
  const names = Object.keys(e.variants);
  const ids = e.variants[names[0]].conditions.map((c) => c.id);
  const by = (fmt) => names.map((n) => [n, ...e.variants[n].conditions.map(fmt)]);
  const head = ['处理方式', ...ids];
  return [
    `识别器：${e.asr}。基线（干净近端、不处理）CER ${pct(e.baselineCer, 2)}；没有回声时同样流程的底线：远端单讲段里识别出的字数占参考的 ${pct(e.floor.insertion, 2)}，近端 CER ${pct(e.floor.nearCer, 2)}。`,
    '',
    '回声插入率（远端单讲段里识别出的字数 ÷ 远端实际字数）：',
    '',
    table(head, by((c) => pct(c.insertion))),
    '',
    '近端 CER 比基线增加（百分点）：',
    '',
    table(head, by((c) => n1(c.nearCerIncrease * 100))),
    '',
    '远端单讲段的回声消减量 ERLE（dB，各场平均）：',
    '',
    table(head, by((c) => n1(c.erleDb))),
    '',
    '各处理方式对照门槛：',
    '',
    table(['处理方式', '全部条件达标', '未达标的条件', '近端 CER 平均增加（百分点）'], e.pick.report.map((x) => [x.name, x.pass ? '是' : '否', x.failed.length ? x.failed.join('、') : dash, n1(x.meanIncrease * 100)])),
  ].join('\n');
}

function basisSection(r) {
  const rows = [
    ['“我”轨说话占比 / 系统轨说话占比（评测集实测）', `${pct(r.speechFrac.me)} / ${pct(r.speechFrac.sys)}`],
    ['一场 60 分钟会议的转写稿字数（按评测集的字数密度）', `${Math.round(r.chars)} 字`],
    ['解码实时倍数（ffmpeg 到 16 kHz 单声道）', n4(r.rtf.decode)],
    ['VAD 实时倍数', n4(r.rtf.vad)],
    ['回声消除实时倍数', r.useAec ? n4(r.rtf.aec) : '没有选中处理方式，不算'],
    ['说话人分离实时倍数', n4(r.rtf.diar)],
    ['加标点：模型加载 / 每千个单位', `${n2(r.punct.loadSec)} 秒 / ${n2(r.punct.secPer1000)} 秒`],
  ];
  return table(['量', '值'], rows);
}

/**
 * 整份报告。generatedAt：生成时间的文字（调用方从系统时钟取，这里不取）。
 * 某个结果文件缺字段时，排出来的会是 NaN / undefined：这种报告不能交出去，直接报错。
 */
export function renderReport(r, { generatedAt }) {
  const chosen = r.asr.pick.chosen;
  const notes = [];
  if (chosen && r.echo.asr !== chosen.name) notes.push(`注意：回声评测用的识别器是 ${r.echo.asr}，默认识别器是 ${chosen.name}；回声部分的数字是用 ${r.echo.asr} 测的。`);
  const md = [
    `# S4 结果汇总（生成于 ${generatedAt}）`,
    '',
    '## 结论一览',
    '',
    verdicts(r),
    ...(notes.length ? ['', ...notes] : []),
    '',
    '## 识别',
    '',
    asrSection(r.asr),
    '',
    '## 流水线用时估算',
    '',
    stagesSection(r),
    '',
    '估算用到的实测量：',
    '',
    basisSection(r),
    '',
    '## 说话人分离',
    '',
    diarSection(r.diar),
    '',
    '## 回声消除',
    '',
    echoSection(r.echo),
    '',
  ].join('\n');
  const bad = md.match(/NaN|undefined|Infinity|\bnull\b/);
  if (bad) throw new Error(`报告里出现了无效的值（${bad[0]}）：某个阶段的结果文件缺字段`);
  return md;
}
