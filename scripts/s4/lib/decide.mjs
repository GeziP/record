// S4 的通过标准与决策规则（对应设计文档 3.6、3.7）。数字在测之前写死在这里，测完不改；比例一律用小数（0.15 就是 15%）。
export const GATES = Object.freeze({
  asr: Object.freeze({ cerMax: 0.15, estMinutesMax: 30, peakMemGbMax: 4 }),
  diar: Object.freeze({ derMax: 0.25, countToleranceMax: 1, countSessionsMin: 6, farFieldDerMax: 0.3 }),
  aec: Object.freeze({ insertionMax: 0.05, nearCerIncreaseMax: 0.02 }),
  /** 识别：CER 相差不超过这么多（0.5 个百分点）时取更快的 */
  asrTieBand: 0.005,
  /** 阈值：DER 比最低点高出不超过这么多（1 个百分点）的连续区间，取中点 */
  thresholdBand: 0.01,
});

// 浮点比较的容差：0.15 与 0.150000000001 这类误差不应该改变结论
const EPS = 1e-9;

/**
 * 默认识别引擎：先留下速度与内存都达标的候选，其中 CER 最低者；与最低者相差在 0.5 个百分点内的取更快的；
 * 选出的这个若 CER 仍高于 15%，不给默认，如实报告。
 * 候选：{ name, cer, estMinutes, peakMemGb }
 */
export function pickAsr(cands) {
  const g = GATES.asr;
  const eligible = cands.filter((c) => c.estMinutes <= g.estMinutesMax + EPS && c.peakMemGb <= g.peakMemGbMax + EPS);
  if (eligible.length === 0) return { status: 'none-eligible', reason: '没有候选同时满足速度与内存的门槛', eligible: [] };
  const lowest = Math.min(...eligible.map((c) => c.cer));
  const near = eligible.filter((c) => c.cer <= lowest + GATES.asrTieBand + EPS);
  const chosen = near.reduce((a, b) => (b.estMinutes < a.estMinutes ? b : a));
  if (chosen.cer > g.cerMax + EPS) {
    return { status: 'cer-gate-failed', reason: `速度与内存达标的候选里最好的 CER 是 ${(chosen.cer * 100).toFixed(2)}%，高于 ${g.cerMax * 100}%`, chosen, eligible };
  }
  return { status: 'ok', chosen, eligible };
}

/**
 * 阈值区间：points 为 [{ threshold, der }]。找到 DER 最低的点，向两侧延伸，直到 DER 比最低点高出超过 1 个百分点为止，
 * 返回区间两端与中点。
 */
export function thresholdBand(points) {
  const pts = [...points].sort((a, b) => a.threshold - b.threshold);
  let best = 0;
  pts.forEach((p, i) => {
    if (p.der < pts[best].der) best = i;
  });
  const limit = pts[best].der + GATES.thresholdBand + EPS;
  let lo = best;
  let hi = best;
  while (lo > 0 && pts[lo - 1].der <= limit) lo--;
  while (hi < pts.length - 1 && pts[hi + 1].der <= limit) hi++;
  const loT = pts[lo].threshold;
  const hiT = pts[hi].threshold;
  return { best: pts[best], lo: loT, hi: hiT, mid: (loT + hiT) / 2 };
}

/**
 * 声纹模型与阈值：每个模型取它在阈值扫描里最低的平均 DER，选其中最低的模型；再按该模型的阈值曲线取区间中点。
 * 中点不在已测的阈值里时，needsConfirm 为真（要在中点再测一次才能核对门槛）。
 * 模型：{ name, points: [{ threshold, der, countHits }] }，countHits 是人数相差不超过 1 的场次数。
 */
export function pickEmbedding(models) {
  const scored = models.map((m) => ({ name: m.name, minDer: Math.min(...m.points.map((p) => p.der)) }));
  const winner = scored.reduce((a, b) => (b.minDer < a.minDer ? b : a));
  const model = models.find((m) => m.name === winner.name);
  const band = thresholdBand(model.points);
  const at = model.points.find((p) => Math.abs(p.threshold - band.mid) < 1e-9);
  return { model: model.name, minDer: winner.minDer, threshold: band.mid, band: { lo: band.lo, hi: band.hi }, needsConfirm: !at, point: at ?? null, ranking: scored.sort((a, b) => a.minDer - b.minDer) };
}

/** 分离门槛：DER 不超过 25%，并且人数相差不超过 1 的场次不少于 6 场 */
export function checkDiarGate(point) {
  const g = GATES.diar;
  const derOk = point.der <= g.derMax + EPS;
  const countOk = point.countHits >= g.countSessionsMin;
  return { derOk, countOk, pass: derOk && countOk };
}

/** 开放“麦克风轨也分人”：远场 DER 不超过 30% */
export function checkFarFieldGate(der) {
  return { pass: der <= GATES.diar.farFieldDerMax + EPS };
}

/**
 * 回声消除：每个候选在所有条件下都要满足“回声插入率 ≤ 5%”与“近端 CER 增加 ≤ 2 个百分点”才算达标；
 * 达标的候选里取各条件平均近端 CER 增加最小者；都不达标就不做回声消除。
 * 候选：{ name, conditions: [{ id, insertion, nearCerIncrease }] }
 */
export function pickAec(cands) {
  const g = GATES.aec;
  const report = cands.map((c) => {
    const failed = c.conditions.filter((x) => !(x.insertion <= g.insertionMax + EPS && x.nearCerIncrease <= g.nearCerIncreaseMax + EPS)).map((x) => x.id);
    const meanIncrease = c.conditions.reduce((s, x) => s + x.nearCerIncrease, 0) / c.conditions.length;
    return { name: c.name, pass: failed.length === 0, failed, meanIncrease };
  });
  const passers = report.filter((r) => r.pass);
  if (passers.length === 0) return { status: 'none', reason: '没有候选在全部条件下达标：不做回声消除，提示“外放时请戴耳机”', report };
  const chosen = passers.reduce((a, b) => (b.meanIncrease < a.meanIncrease ? b : a));
  return { status: 'ok', chosen: chosen.name, report };
}
