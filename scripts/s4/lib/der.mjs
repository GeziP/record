// 说话人分离的评分：DER = (漏检 + 误检 + 说话人混淆) / 参考里的说话总量。
// 做法与 NIST / dscore 一致：10 ms 一帧；参考每段起止点前后各 collar 秒内的帧不计；
// 说话人映射用匈牙利算法取“重合时间最大”的一一对应；允许重叠说话（一帧里可以有多个人）。
const FRAME = 0.01;

/** RTTM：每行 `SPEAKER 文件 1 起点 时长 <NA> <NA> 说话人 <NA> <NA>` */
export function parseRttm(text) {
  const out = [];
  for (const line of text.split(/\r?\n/)) {
    const f = line.trim().split(/\s+/);
    if (f[0] !== 'SPEAKER') continue;
    const start = Number(f[3]);
    out.push({ start, end: start + Number(f[4]), speaker: f[7] });
  }
  return out;
}

export function toRttm(segments, file) {
  return segments.map((s) => `SPEAKER ${file} 1 ${s.start.toFixed(3)} ${(s.end - s.start).toFixed(3)} <NA> <NA> ${s.speaker} <NA> <NA>`).join('\n') + '\n';
}

/** 说话总时长不少于 minSeconds 的说话人个数 */
export function countSpeakers(segments, minSeconds = 0) {
  const total = new Map();
  for (const s of segments) total.set(s.speaker, (total.get(s.speaker) ?? 0) + (s.end - s.start));
  return [...total.values()].filter((t) => t >= minSeconds).length;
}

/** 匈牙利算法（求最大总和）。matrix 是 行×列 的非负数，行列数可以不同。返回每一行配到的列号，没配上的是 -1。 */
export function assignMax(matrix) {
  const rows = matrix.length;
  const cols = rows ? matrix[0].length : 0;
  const n = Math.max(rows, cols);
  if (n === 0) return [];
  let top = 0;
  for (const r of matrix) for (const v of r) top = Math.max(top, v);
  // 补成方阵，最大化改成最小化 (top - v)
  const cost = Array.from({ length: n + 1 }, (_, i) => Array.from({ length: n + 1 }, (_, j) => (i >= 1 && j >= 1 && i <= rows && j <= cols ? top - matrix[i - 1][j - 1] : top)));
  const u = new Array(n + 1).fill(0);
  const v = new Array(n + 1).fill(0);
  const p = new Array(n + 1).fill(0);
  const way = new Array(n + 1).fill(0);
  for (let i = 1; i <= n; i++) {
    p[0] = i;
    let j0 = 0;
    const minv = new Array(n + 1).fill(Infinity);
    const used = new Array(n + 1).fill(false);
    do {
      used[j0] = true;
      const i0 = p[j0];
      let delta = Infinity;
      let j1 = 0;
      for (let j = 1; j <= n; j++) {
        if (used[j]) continue;
        const cur = cost[i0][j] - u[i0] - v[j];
        if (cur < minv[j]) { minv[j] = cur; way[j] = j0; }
        if (minv[j] < delta) { delta = minv[j]; j1 = j; }
      }
      for (let j = 0; j <= n; j++) {
        if (used[j]) { u[p[j]] += delta; v[j] -= delta; } else minv[j] -= delta;
      }
      j0 = j1;
    } while (p[j0] !== 0);
    do { const j1 = way[j0]; p[j0] = p[j1]; j0 = j1; } while (j0);
  }
  const result = new Array(rows).fill(-1);
  for (let j = 1; j <= cols; j++) if (p[j] >= 1 && p[j] <= rows) result[p[j] - 1] = j - 1;
  return result;
}

function masks(segments, nFrames) {
  const byName = new Map();
  for (const s of segments) {
    if (!byName.has(s.speaker)) byName.set(s.speaker, new Uint8Array(nFrames));
    const m = byName.get(s.speaker);
    const a = Math.max(0, Math.floor(s.start / FRAME));
    const b = Math.min(nFrames, Math.ceil(s.end / FRAME));
    for (let i = a; i < b; i++) {
      const centre = (i + 0.5) * FRAME;
      if (centre >= s.start && centre < s.end) m[i] = 1;
    }
  }
  return byName;
}

/**
 * 打分。ref、hyp：[{start, end, speaker}]（单位秒，可重叠）。
 * 返回各项的秒数与比例；total 是参考里的说话总量（重叠时按人数计）。
 */
export function scoreDer(ref, hyp, { collar = 0.25, duration } = {}) {
  const end = duration ?? Math.max(0, ...ref.map((s) => s.end), ...hyp.map((s) => s.end));
  const nFrames = Math.ceil(end / FRAME);
  const R = masks(ref, nFrames);
  const H = masks(hyp, nFrames);
  const scored = new Uint8Array(nFrames).fill(1);
  if (collar > 0) {
    for (const s of ref) {
      for (const b of [s.start, s.end]) {
        const a = Math.max(0, Math.floor((b - collar) / FRAME));
        const z = Math.min(nFrames, Math.ceil((b + collar) / FRAME));
        for (let i = a; i < z; i++) {
          const centre = (i + 0.5) * FRAME;
          if (centre >= b - collar && centre <= b + collar) scored[i] = 0;
        }
      }
    }
  }
  const rNames = [...R.keys()];
  const hNames = [...H.keys()];
  const overlap = rNames.map((r) => hNames.map((h) => {
    let t = 0;
    const rm = R.get(r);
    const hm = H.get(h);
    for (let i = 0; i < nFrames; i++) if (scored[i] && rm[i] && hm[i]) t++;
    return t;
  }));
  const map = assignMax(overlap);
  let total = 0, miss = 0, fa = 0, conf = 0;
  for (let i = 0; i < nFrames; i++) {
    if (!scored[i]) continue;
    let nRef = 0, nHyp = 0, nCorrect = 0;
    for (const r of rNames) nRef += R.get(r)[i];
    for (const h of hNames) nHyp += H.get(h)[i];
    rNames.forEach((r, ri) => { if (map[ri] >= 0 && R.get(r)[i] && H.get(hNames[map[ri]])[i]) nCorrect++; });
    total += nRef;
    miss += Math.max(0, nRef - nHyp);
    fa += Math.max(0, nHyp - nRef);
    conf += Math.min(nRef, nHyp) - nCorrect;
  }
  const sec = (x) => x * FRAME;
  const out = { total: sec(total), miss: sec(miss), fa: sec(fa), conf: sec(conf) };
  return { ...out, der: total === 0 ? (fa + conf + miss === 0 ? 0 : Infinity) : (miss + fa + conf) / total, refSpeakers: rNames.length, hypSpeakers: hNames.length };
}
