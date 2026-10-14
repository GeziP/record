// 字错误率（CER）：参考与识别结果都先规整（全角转半角、小写、去掉标点与空白），再按 Unicode 码点逐字比较，
// 分别数出替换、删除、插入。多句合计时先把各句的计数加起来再除以参考总字数（不是各句比例的平均）。

const HANZI_DIGITS = '零一二三四五六七八九';

/** 规整：NFKC（全角转半角）、转小写、只留字母与数字（汉字算字母），其余（标点、空白、符号）全部去掉，阿拉伯数字逐位换成汉字数字（2024 → 二零二四） */
export function normalizeText(s) {
  return s.normalize('NFKC').toLowerCase().replace(/[^\p{L}\p{N}]/gu, '').replace(/[0-9]/g, (d) => HANZI_DIGITS[d]);
}

/** 编辑距离的三种错误各有多少个。ref、hyp 是码点数组。 */
export function editCounts(ref, hyp) {
  const n = ref.length;
  const m = hyp.length;
  const w = m + 1;
  const d = new Int32Array((n + 1) * w);
  for (let i = 0; i <= n; i++) d[i * w] = i;
  for (let j = 0; j <= m; j++) d[j] = j;
  for (let i = 1; i <= n; i++) {
    for (let j = 1; j <= m; j++) {
      const diag = d[(i - 1) * w + j - 1] + (ref[i - 1] === hyp[j - 1] ? 0 : 1);
      d[i * w + j] = Math.min(diag, d[(i - 1) * w + j] + 1, d[i * w + j - 1] + 1);
    }
  }
  let i = n;
  let j = m;
  let sub = 0;
  let del = 0;
  let ins = 0;
  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && d[i * w + j] === d[(i - 1) * w + j - 1] + (ref[i - 1] === hyp[j - 1] ? 0 : 1)) {
      if (ref[i - 1] !== hyp[j - 1]) sub++;
      i--;
      j--;
    } else if (i > 0 && d[i * w + j] === d[(i - 1) * w + j] + 1) {
      del++;
      i--;
    } else {
      ins++;
      j--;
    }
  }
  return { sub, del, ins, refLen: n };
}

/** 一句：规整后比较 */
export function cerOne(ref, hyp) {
  return editCounts(Array.from(normalizeText(ref)), Array.from(normalizeText(hyp)));
}

/** 多句合计。pairs：[[参考, 识别结果], ...] */
export function cerTotal(pairs) {
  const t = { sub: 0, del: 0, ins: 0, refLen: 0 };
  for (const [r, h] of pairs) {
    const c = cerOne(r, h);
    t.sub += c.sub;
    t.del += c.del;
    t.ins += c.ins;
    t.refLen += c.refLen;
  }
  const errors = t.sub + t.del + t.ins;
  return { ...t, errors, rate: t.refLen === 0 ? (errors === 0 ? 0 : Infinity) : errors / t.refLen };
}
