// Praat TextGrid 解析：长格式（`xmin = 0`）和短格式（只有值）都认，只处理 IntervalTier（逐段标注）。
// 做法：把全文切成“带引号的字符串”和“数字”两种值，其余（键名、等号、方括号里的序号、<exists>）一律丢掉，
// 两种格式剩下的值序列就是同一个。

const NUMBER = /\d+(?:\.\d+)?(?:[eE][-+]?\d+)?/y; // 时间不会是负数，所以开头不认正负号

function values(text) {
  const out = [];
  let i = 0;
  while (i < text.length) {
    const c = text[i];
    if (c === '"') {
      let s = '';
      i++;
      for (;;) {
        if (i >= text.length) throw new Error('TextGrid：字符串没有结束的引号');
        if (text[i] === '"') {
          if (text[i + 1] === '"') { s += '"'; i += 2; continue; } // 两个引号表示一个引号
          i++;
          break;
        }
        s += text[i++];
      }
      out.push(s);
    } else if (c === '[') {
      i = text.indexOf(']', i) + 1 || text.length; // 方括号里是序号，丢掉
    } else {
      NUMBER.lastIndex = i;
      const m = /\d/.test(c) ? NUMBER.exec(text) : null;
      if (m) { out.push(Number(m[0])); i += m[0].length; } else i++;
    }
  }
  return out;
}

/** 返回 { xmin, xmax, tiers: [{ name, intervals: [{ xmin, xmax, text }] }] } */
export function parseTextGrid(text) {
  const v = values(text.replace(/^\uFEFF/, ''));
  if (v[0] !== 'ooTextFile' || v[1] !== 'TextGrid') throw new Error('TextGrid：文件头不对');
  let k = 2;
  const next = () => {
    if (k >= v.length) throw new Error('TextGrid：内容提前结束');
    return v[k++];
  };
  const xmin = next();
  const xmax = next();
  const nTiers = next();
  const tiers = [];
  for (let t = 0; t < nTiers; t++) {
    const cls = next();
    if (cls !== 'IntervalTier') throw new Error(`TextGrid：只支持 IntervalTier，遇到 ${cls}`);
    const name = next();
    next(); // 这一层的 xmin
    next(); // 这一层的 xmax
    const n = next();
    const intervals = [];
    for (let i = 0; i < n; i++) intervals.push({ xmin: next(), xmax: next(), text: next() });
    tiers.push({ name, intervals });
  }
  return { xmin, xmax, tiers };
}
