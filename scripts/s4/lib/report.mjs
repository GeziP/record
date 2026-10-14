// S4 结果的汇总：从各阶段的结果算出设计文档 3.9 需要的数字，并按 decide.mjs 里事先写死的门槛与决策规则下结论。
// 这里只有纯函数（读文件的是 report.mjs 脚本与各阶段脚本），所以可以用造出来的数据测。
import { normalizeText } from './cer.mjs';
import { checkDiarGate, checkFarFieldGate, GATES, pickAec, pickAsr, pickEmbedding } from './decide.mjs';
import { aggregate, curves } from './diar-eval.mjs';
import { aggregateEcho } from './echo-eval.mjs';
import { estimatePipeline, unionDuration } from './estimate.mjs';

/** 估算按一场多长的会议算（设计文档 3.5：60 分钟） */
export const MEETING_MIN = 60;

/** 识别器自己输出的标点占非空白字符的比例低于这个数，就认为它不带标点，流水线里要另加标点模型（估算里加上标点的用时） */
export const PUNCT_RATE_MIN = 0.01;

/**
 * 两条音轨里“有人说话的时间”占音轨时长的比例，把各场合在一起算（总说话时长 ÷ 总音轨时长）：
 * me 是“我”那条麦克风轨；sys 是其余参会者合起来的系统声音轨（重叠的说话只算一次）。
 * sessions：[{ me, tracks: [{ speaker, durationSec, segments: [{start,end}] }] }]。
 */
export function speechFractions(sessions) {
  if (sessions.length === 0) throw new Error('没有会议可算说话占比');
  let meSpeech = 0;
  let meTotal = 0;
  let sysSpeech = 0;
  let sysTotal = 0;
  for (const s of sessions) {
    const me = s.tracks.find((t) => t.speaker === s.me);
    if (!me) throw new Error(`会议里没有“我”：${s.me}`);
    const others = s.tracks.filter((t) => t !== me);
    if (others.length === 0) throw new Error(`会议只有“我”一个人：${s.me}`);
    meSpeech += unionDuration(me.segments);
    meTotal += me.durationSec;
    sysSpeech += unionDuration(others.flatMap((t) => t.segments));
    sysTotal += Math.max(...others.map((t) => t.durationSec));
  }
  return { me: meSpeech / meTotal, sys: sysSpeech / sysTotal };
}

/** 只算识别这一段、不含 VAD 与分离等别的阶段时，一场 meetingMin 分钟的会议要多少分钟：两条轨的说话时间 × 实时倍数 */
export function asrOnlyMinutes(speechFrac, rtf, meetingMin = MEETING_MIN) {
  return (speechFrac.me + speechFrac.sys) * meetingMin * rtf;
}

/** 峰值内存（GB）：工作集与私有内存的峰值里取大的那个，1 GB = 1024 MB（保守） */
export function peakMemGb(summary) {
  return Math.max(summary.peakWorkingSetMb, summary.peakPrivateMb) / 1024;
}

/**
 * 决赛圈：全量识别已经跑完、速度与内存都达标（只算识别的估计用时不超过 30 分钟、峰值内存不超过 4 GB）的候选里，
 * CER 最低的前 n 个（CER 一样时按给定的先后）。只有这些候选会再做 VAD 端到端的评测。
 * cands：[{ id, full }]，full 是全量识别的结果（跳过的候选 full.skipped 为真，不进决赛圈）。
 */
export function finalists({ cands, speechFrac, n = 3 }) {
  const g = GATES.asr;
  return cands
    .filter((c) => c.full && !c.full.skipped)
    .map((c, order) => ({ id: c.id, order, cer: c.full.score.cer.rate, asrOnlyMin: asrOnlyMinutes(speechFrac, c.full.summary.rtf), peakMemGb: peakMemGb(c.full.summary) }))
    .filter((c) => c.asrOnlyMin <= g.estMinutesMax && c.peakMemGb <= g.peakMemGbMax)
    .sort((a, b) => a.cer - b.cer || a.order - b.order)
    .slice(0, n)
    .map(({ order, ...rest }) => rest);
}

/**
 * 转写稿的字数密度：评测集里规整后的参考字数 ÷ 各条音轨自己的说话时长之和（字/秒）。
 * sessions：[{ tracks: [{ segments: [{ start, end, text }] }] }]。
 */
export function charsPerSpeechSec(sessions) {
  let chars = 0;
  let sec = 0;
  for (const s of sessions) {
    for (const t of s.tracks) {
      chars += t.segments.reduce((n, x) => n + Array.from(normalizeText(x.text)).length, 0);
      sec += unionDuration(t.segments);
    }
  }
  if (sec === 0) throw new Error('评测集里没有说话时间，算不出字数密度');
  return chars / sec;
}

/** 一场 meetingMin 分钟的会议转写稿大约多少字：字数密度 × 两条轨的说话时间 */
export function meetingChars({ charsPerSec, speechFrac, meetingMin = MEETING_MIN }) {
  return charsPerSec * (speechFrac.me + speechFrac.sys) * meetingMin * 60;
}

/** 加标点的用时（分钟）：一次加载 + 每千个单位（词或字）的处理 × 字数 */
export function punctMinutes(punct, chars) {
  return (punct.loadSec + (punct.secPer1000 * chars) / 1000) / 60;
}

/**
 * 一个识别候选的一行数字。cand：{ id, name, prescreen8, prescreen4, full, vad }（各阶段的结果文件内容，没跑的是 null；full 跳过的有 skipped）。
 * ctx：{ speechFrac, rtf: { decode, vad, aec, diar }, useAec, diarMic, punct: { loadSec, secPer1000 }, chars（一场会议的转写稿字数） }；会议长度固定按 MEETING_MIN。
 *
 * 一场 60 分钟会议“保存 → 出转写稿”要多久，按两种办法估，取大的（保守）：
 *  - 批识别估计：解码 + VAD + 分离（+ 回声消除）+ 识别（说话时长 × 全量识别的实时倍数）+ 两条轨各加载一次模型 + （不带标点时）加标点；
 *  - 端到端估计（只有做过 VAD 端到端的候选才有）：解码 + 分离（+ 回声消除）+ 两条轨 ×（VAD 与识别一体程序的整条录音实时倍数，含模型加载）+（不带标点时）加标点。
 * 峰值内存同样取批识别与端到端里大的。
 */
export function asrRow(cand, ctx) {
  const { id, name, prescreen8, prescreen4, full, vad } = cand;
  const { speechFrac, rtf, useAec, diarMic, punct, chars } = ctx;
  // 预筛（头、中、尾三场，每位参会者前 5 分钟的标注段）：每个候选都有，被跳过的候选只有这一项
  const pre = prescreen8
    ? { cer: prescreen8.score.cer.rate, rtf8: prescreen8.summary.rtf, rtf4: prescreen4 ? prescreen4.summary.rtf : null, asrOnlyMin: asrOnlyMinutes(speechFrac, prescreen8.summary.rtf) }
    : null;
  if (!full) return { id, name, status: 'not-run', pre };
  if (full.skipped) return { id, name, status: 'skipped', reason: full.reason, pre };
  if (full.summary.loadSecMean === null) throw new Error(`${id}：没有解析到模型加载用时，算不出加载的开销`);
  const needsPunct = full.punctRate < PUNCT_RATE_MIN;
  const punctMin = needsPunct ? punctMinutes(punct, chars) : 0;
  const batch = estimatePipeline({ meetingMin: MEETING_MIN, speechFrac, rtf: { ...rtf, asr: full.summary.rtf }, useAec, diarMic });
  const batchMin = batch.total + (2 * full.summary.loadSecMean) / 60 + punctMin;
  let e2e = null;
  if (vad) {
    const rtfWall = vad.summary.wallSec / vad.summary.audioSec;
    const rest = estimatePipeline({ meetingMin: MEETING_MIN, speechFrac, rtf: { ...rtf, vad: 0, asr: 0 }, useAec, diarMic }).total;
    e2e = { cer: vad.score.cer.rate, rtfWall, peakMemGb: peakMemGb(vad.summary), totalMin: rest + 2 * MEETING_MIN * rtfWall + punctMin };
  }
  return {
    id,
    name,
    status: 'ok',
    pre,
    cer: full.score.cer.rate,
    rtf8: full.summary.rtf,
    loadSec: full.summary.loadSecMean,
    punctRate: full.punctRate,
    needsPunct,
    punctMin,
    stages: batch.stages,
    batchMin,
    e2e,
    estMinutes: Math.max(batchMin, e2e ? e2e.totalMin : 0),
    peakMemGb: Math.max(peakMemGb(full.summary), e2e ? e2e.peakMemGb : 0),
  };
}

/** 全部候选的行与默认识别引擎的结论（只有跑完全量的候选参加；速度、内存取各自的保守值，规则见 decide.pickAsr） */
export function asrSection(cands, ctx) {
  const rows = cands.map((c) => asrRow(c, ctx));
  const pick = pickAsr(rows.filter((r) => r.status === 'ok').map((r) => ({ name: r.id, cer: r.cer, estMinutes: r.estMinutes, peakMemGb: r.peakMemGb })));
  return { rows, pick };
}

/** 每场结果的一行：会议编号、真实人数、估计人数（按每个说话人出现过计）、DER */
function sessionRows(results) {
  return [...results]
    .sort((a, b) => (a.mixId < b.mixId ? -1 : a.mixId > b.mixId ? 1 : 0))
    .map((r) => ({ mixId: r.mixId, actual: r.actualSpeakers, estimated: r.estimatedSpeakers, estimated3s: r.estimatedSpeakers3s, der: r.der, total: r.total }));
}

/** 按真实人数分组（评测里“系统声音”里的远端说话人数各场不同：1、2、3 人），每组的场数、DER 合计与人数合格的场数 */
function groupByActual(results) {
  const groups = new Map();
  for (const r of results) {
    if (!groups.has(r.actualSpeakers)) groups.set(r.actualSpeakers, []);
    groups.get(r.actualSpeakers).push(r);
  }
  return [...groups.entries()].sort((a, b) => a[0] - b[0]).map(([actual, list]) => ({ actual, sessions: list.length, ...pick(aggregate(list)) }));
}
const pick = ({ der, countHits }) => ({ der, countHits });

/**
 * 说话人分离部分。
 * sweep：全部扫描任务的结果；confirm：区间中点不在已测阈值上时在中点再测的结果；far：远场；rotations：“我”换成每位参会者；timing：计时的结果文件内容；
 * sessionCount：评测集的会议数（每个设置必须在每一场上都有结果）。
 * 返回 { pick, curves, point, gate, groups, perSession, far, rotations, timing }；中点要补测而没补测时 point 与 gate 为 null（status 'needs-confirm'）。
 */
export function diarSection({ sweep, confirm = [], far = [], rotations = [], timing = null, sessionCount }) {
  const c = curves(sweep, { sessions: sessionCount });
  const models = Object.entries(c).map(([name, v]) => ({ name, points: v.points.map(({ threshold, der, countHits }) => ({ threshold, der, countHits })) }));
  const chosen = pickEmbedding(models);
  const atChosen = (results) => results.filter((r) => r.embedding === chosen.model && r.clusters === null && Math.abs(r.threshold - chosen.threshold) < 1e-9);
  let results;
  let point;
  if (!chosen.needsConfirm) {
    results = atChosen(sweep);
    point = aggregate(results); // chosen.point 只有挑选用的三个字段（阈值、DER、人数合格场次）
  } else if (confirm.length > 0) {
    results = atChosen(confirm);
    if (results.length !== sessionCount) throw new Error(`中点 ${chosen.threshold} 的补测只有 ${results.length}/${sessionCount} 场的结果`);
    point = aggregate(results);
  } else {
    results = [];
    point = null;
  }
  const farResults = far.length > 0 ? far : null;
  if (farResults && farResults.length !== sessionCount) throw new Error(`远场只有 ${farResults.length}/${sessionCount} 场的结果`);
  const farPoint = farResults ? aggregate(farResults) : null;
  const rot = rotations.length > 0 ? aggregate(rotations) : null;
  return {
    pick: chosen,
    curves: c,
    status: point ? (checkDiarGate(point).pass ? 'pass' : 'fail') : 'needs-confirm',
    point,
    gate: point ? checkDiarGate(point) : null,
    groups: results.length > 0 ? groupByActual(results) : [],
    perSession: sessionRows(results),
    far: farPoint ? { point: farPoint, gate: checkFarFieldGate(farPoint.der), perSession: sessionRows(farResults) } : null,
    rotations: rot ? { sessions: rot.sessions, der: rot.der, countHits: rot.countHits, countMeanAbsError: rot.countMeanAbsError } : null,
    timing: timing ? timing.summary : null,
  };
}

/** 某个处理方式的实时倍数：各条件各场的“处理进程用时 ÷ 处理的音频时长”合计（进程用时含读写文件，保守） */
export function aecRtf(sessionResults, variant) {
  let wallSec = 0;
  let audioSec = 0;
  for (const s of sessionResults) {
    for (const cond of Object.values(s.conditions)) {
      const cell = cond[variant];
      if (!cell) throw new Error(`会议 ${s.session} 缺处理方式 ${variant} 的结果`);
      if (!cell.aec) throw new Error(`处理方式 ${variant} 没有回声消除程序的计时（它是“不处理”吗？）`);
      wallSec += cell.aec.wallMs / 1000;
      audioSec += s.seconds;
    }
  }
  return wallSec / audioSec;
}

/** 回声消除部分：汇总 → 去掉“不处理”后按事先写死的规则选 → 选中的处理方式的实时倍数 */
export function echoSection(sessionResults) {
  const agg = aggregateEcho(sessionResults);
  const cands = Object.entries(agg.variants)
    .filter(([name]) => name !== 'none')
    .map(([name, v]) => ({ name, conditions: v.conditions }));
  const chosen = pickAec(cands);
  return { baselineCer: agg.baselineCer, floor: agg.floor, none: agg.variants.none?.conditions ?? null, variants: agg.variants, pick: chosen, rtfAec: chosen.status === 'ok' ? aecRtf(sessionResults, chosen.chosen) : null };
}

const finite = (x, what) => {
  if (!Number.isFinite(x)) throw new Error(`${what}不是有限的数：${x}`);
  return x;
};

/**
 * 把各阶段的结果汇成一份报告（纯函数；读文件的是 report.mjs 脚本）。缺哪一项都报错并说明先跑什么，不拿残缺的数字下结论。
 * sessions：评测集的会议（dataset.loadSessions）；speed：speed.json；candidates：[{ id, name, prescreen8, prescreen4, full, vad }]；
 * diar：{ sweep, confirm, far, rotations, timing }；echo：每场一份的回声评测结果。
 * 估算里的 useAec 取决于回声消除有没有选中处理方式，diarMic 取决于远场是否过了门槛；挑出的默认识别器必须做过 VAD 端到端（否则估算没有取到保守值）。
 */
export function buildReport({ sessions, speed, candidates, diar, echo }) {
  const ids = sessions.map((s) => s.id).sort();
  const speechFrac = speechFractions(sessions);
  const chars = meetingChars({ charsPerSec: charsPerSpeechSec(sessions), speechFrac });
  for (const c of candidates) {
    if (!c.prescreen8) throw new Error(`${c.id} 还没有预筛结果，先跑 asr prescreen`);
    if (!c.full) throw new Error(`${c.id} 还没有全量结果，先跑 asr full`);
  }
  const echoIds = echo.map((s) => s.session).sort();
  if (echoIds.join() !== ids.join()) throw new Error(`回声评测的会议与评测集不一致：有 ${echoIds.join('、')}，应有 ${ids.join('、')}`);
  const echoAsr = [...new Set(echo.map((s) => s.asr))].sort();
  if (echoAsr.length !== 1) throw new Error(`各场回声评测用的识别候选不一致：${echoAsr.join('、')}`);
  const echoRes = echoSection(echo);
  const useAec = echoRes.pick.status === 'ok';

  if (diar.sweep.length === 0) throw new Error('分离：没有扫描结果，先跑 diar sweep');
  const diarRes = diarSection({ ...diar, sessionCount: sessions.length });
  if (diarRes.status === 'needs-confirm') throw new Error(`分离：区间中点 ${diarRes.pick.threshold} 还没有补测的结果，先跑 diar confirm`);
  if (!diarRes.far) throw new Error('分离：缺远场结果，先跑 diar far');
  if (!diarRes.rotations) throw new Error('分离：缺“我”轮换的结果，先跑 diar rotations');
  if (!diarRes.timing) throw new Error('分离：缺计时结果，先跑 diar timing');
  const diarMic = diarRes.far.gate.pass;

  const rtf = {
    decode: finite(speed.decode.rtf, '解码实时倍数'),
    vad: finite(speed.vad.rtf, 'VAD 实时倍数'),
    aec: useAec ? finite(echoRes.rtfAec, '回声消除实时倍数') : 0,
    diar: finite(diarRes.timing.rtfWall, '分离实时倍数'),
  };
  const punct = { loadSec: finite(speed.punct.loadSec, '标点加载用时'), secPer1000: finite(speed.punct.secPer1000, '标点每千字用时') };
  const asr = asrSection(candidates, { speechFrac, rtf, useAec, diarMic, punct, chars });
  const chosen = asr.pick.chosen;
  if (chosen && !asr.rows.find((r) => r.id === chosen.name).e2e) throw new Error(`选出的 ${chosen.name} 没有 VAD 端到端的结果，先跑 asr vad --cands ${chosen.name}，再重新汇总`);
  return { meetingMin: MEETING_MIN, speechFrac, chars, rtf, punct, useAec, diarMic, asr, diar: diarRes, echo: { ...echoRes, asr: echoAsr[0] } };
}
