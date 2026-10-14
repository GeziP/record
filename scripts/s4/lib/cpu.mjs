// 整机 CPU 占用的快照，用来量“计时的那一段里别的程序占了多少”：测速要在安静的机器上，做不到时要如实写出别人占了多少。
import os from 'node:os';

/** 各核累计的忙碌时间与总时间（毫秒），忙碌 = 总时间 − 空闲 */
export function cpuSnapshot() {
  let busyMs = 0;
  let totalMs = 0;
  for (const c of os.cpus()) {
    const t = c.times;
    const all = t.user + t.nice + t.sys + t.idle + t.irq;
    totalMs += all;
    busyMs += all - t.idle;
  }
  return { busyMs, totalMs };
}

/** 两次快照之间，除被测程序自己（ourCpuMs，它的 CPU 毫秒数）以外的忙碌占整机的比例，0～1 */
export function othersLoad(before, after, ourCpuMs) {
  const total = after.totalMs - before.totalMs;
  if (total <= 0) return 0;
  return Math.max(0, (after.busyMs - before.busyMs - ourCpuMs) / total);
}
