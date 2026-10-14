// S4 各命令行脚本共用的小工具：参数解析、取三场会议、带并发上限的任务池、带本机时间的日志。

/** 解析命令行：--键 值、--键=值、单独的 --开关（后面是另一个 -- 或没有了）；其余是位置参数。 */
export function parseArgs(argv) {
  const positional = [];
  const flags = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (!a.startsWith('--')) {
      positional.push(a);
      continue;
    }
    const eq = a.indexOf('=');
    if (eq >= 0) flags[a.slice(2, eq)] = a.slice(eq + 1);
    else if (i + 1 < argv.length && !argv[i + 1].startsWith('--')) flags[a.slice(2)] = argv[++i];
    else flags[a.slice(2)] = true;
  }
  return { positional, flags };
}

/** 从排好序的会议里取第一场、中间一场（下标为总数的一半向下取整）、最后一场；不足三场就报错 */
export function pickThree(list) {
  if (list.length < 3) throw new Error(`至少要有 3 场会议才能取头、中、尾，现在只有 ${list.length} 场`);
  return [list[0], list[Math.floor(list.length / 2)], list[list.length - 1]];
}

/**
 * 同时最多跑 workers 个任务，结果按 items 的顺序返回。
 * 有任务失败后不再开始新的任务，等已经在跑的结束，再抛出最先失败的那个错误（不吞错、不继续往下蒙混）。
 */
export async function runPool(items, workers, fn) {
  if (!Number.isInteger(workers) || workers < 1) throw new Error(`并发数要是正整数：${workers}`);
  const results = new Array(items.length);
  let next = 0;
  let failure = null;
  async function worker() {
    while (failure === null && next < items.length) {
      const i = next++;
      try {
        results[i] = await fn(items[i], i);
      } catch (e) {
        failure ??= { error: e };
      }
    }
  }
  await Promise.all(Array.from({ length: Math.min(workers, items.length) }, worker));
  if (failure) throw failure.error;
  return results;
}

/** 带本机时间的一行日志（时间取自本机时钟，格式 2026-10-14 23:31:09） */
export function log(message, now = new Date()) {
  console.log(`${now.toLocaleString('sv-SE')}  ${message}`);
}
