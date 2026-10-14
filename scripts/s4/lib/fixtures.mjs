// 汇总（report.mjs）与排版（render.mjs）的测试共用的造数据函数；只给测试用。
// 评测集里 8 场会议，远端说话人数与真实评测集一致：三场 1 人、一场 2 人、四场 3 人。
export const summaryOf = (rtf, { ws = 500, priv = 400, load = 3 } = {}) => ({ rtf, loadSecMean: load, peakWorkingSetMb: ws, peakPrivateMb: priv });
export const fullOf = (cer, rtf, opts = {}, punctRate = 0) => ({ score: { cer: { rate: cer } }, summary: summaryOf(rtf, opts), punctRate });
export const vadOf = (cer, wallSec, audioSec, { ws = 500, priv = 400 } = {}) => ({ score: { cer: { rate: cer } }, summary: { wallSec, audioSec, peakWorkingSetMb: ws, peakPrivateMb: priv } });
export const pre8 = { score: { cer: { rate: 0.09 } }, summary: { rtf: 0.12 } };

// ---- 说话人分离：造 8 场会议的结果。真实人数（远端说话人数）与评测集一致：三场 1 人、一场 2 人、四场 3 人 ----
export const MIXES = ['s1', 's2', 's3', 's4', 's5', 's6', 's7', 's8'];
export const ACTUAL = { s1: 1, s2: 1, s3: 1, s4: 2, s5: 3, s6: 3, s7: 3, s8: 3 };
export const dr = (embedding, mixId, { threshold = null, clusters = null, der, est }) => ({
  key: `${mixId}__${embedding}__${clusters !== null ? `k${clusters}` : `t${threshold.toFixed(2)}`}`,
  mixId,
  embedding,
  threshold,
  clusters,
  total: 100,
  miss: der * 50,
  fa: der * 25,
  conf: der * 25,
  der,
  actualSpeakers: ACTUAL[mixId],
  estimatedSpeakers: est ? est(mixId) : ACTUAL[mixId],
  estimatedSpeakers3s: est ? est(mixId) : ACTUAL[mixId],
});
export const sweepOf = (table, est) => Object.entries(table).flatMap(([model, byT]) => Object.entries(byT).flatMap(([t, der]) => MIXES.map((s) => dr(model, s, { threshold: Number(t), der, est }))));
// m1 的最低点在 0.5（0.10），0.7 也只高 0.5 个百分点：区间 0.5～0.7，中点 0.6 不在已测的阈值里
export const TABLE = { m1: { 0.3: 0.2, 0.5: 0.1, 0.7: 0.105, 0.9: 0.3 }, m2: { 0.3: 0.25, 0.5: 0.12, 0.7: 0.13, 0.9: 0.4 } };
export const overCluster = (s) => (ACTUAL[s] === 3 ? 6 : ACTUAL[s]); // 三个远端说话人的场次都估成 6 个：人数相差 3，不合格

export const ecell = (o) => ({ nearErrors: 5, nearRefLen: 100, insHyp: 2, insRef: 100, erleDb: 20, farText: '', aec: { wallMs: 1000 }, ...o });
export const esession = (session, seconds, conditions) => ({ session, seconds, baseline: { errors: 5, refLen: 100 }, floor: { nearErrors: 5, nearRefLen: 100, insHyp: 1, insRef: 100 }, conditions });
export const none = (o) => ecell({ insHyp: 100, erleDb: 0, aec: null, ...o });

// ---- 汇总：8 场的评测集（“我”说 100 秒、对方说 200 秒，各 1000 秒）加各阶段的结果 ----
export const dataset = MIXES.map((id) => ({
  id,
  me: 'ME',
  tracks: [
    { speaker: 'ME', durationSec: 1000, segments: [{ start: 0, end: 100, text: '一二三四五' }] },
    { speaker: 'R1', durationSec: 1000, segments: [{ start: 0, end: 200, text: '一二三四五六七八' }] },
  ],
}));
export const candidate = (id, cer, vad) => ({ id, name: id.toUpperCase(), prescreen8: pre8, prescreen4: null, full: fullOf(cer, 0.1, { load: 3 }), vad });
export const inputs = (over = {}) => ({
  sessions: dataset,
  speed: { decode: { rtf: 0.002 }, vad: { rtf: 0.005 }, punct: { loadSec: 0.6, secPer1000: 0.12 } },
  candidates: [candidate('a', 0.1, vadOf(0.11, 6, 100)), candidate('b', 0.2, null)],
  diar: {
    sweep: sweepOf(TABLE),
    confirm: MIXES.map((s) => dr('m1', s, { threshold: 0.6, der: 0.108 })),
    far: MIXES.map((s) => dr('m1', s, { threshold: 0.6, der: 0.28 })),
    rotations: ['s1', 's2', 's3'].map((s) => dr('m1', s, { threshold: 0.6, der: 0.2 })),
    timing: { summary: { rtfWall: 0.07, peakWorkingSetMb: 300, othersLoadMean: 0.3 } },
  },
  echo: MIXES.map((s) => ({ ...esession(s, 600, { c1: { none: none(), corti: ecell({ aec: { wallMs: 3000 } }) } }), asr: 'a' })),
  ...over,
});
