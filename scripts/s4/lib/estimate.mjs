// 整条流水线的耗时估算（设计文档 3.5 最后一条）：用实测的实时倍数（处理耗时 ÷ 音频时长）与评测集里实测的说话占比，
// 估算两条各 meetingMin 分钟的音轨从保存到出转写稿要多久。各阶段按先后顺序累加（不算并行），所以是保守的。
import { GATES } from './decide.mjs';

/** 一组 {start, end}（秒）合并重叠后的总时长 */
export function unionDuration(segments) {
  const s = segments.filter((x) => x.end > x.start).sort((a, b) => a.start - b.start);
  let total = 0;
  let curStart = null;
  let curEnd = null;
  for (const x of s) {
    if (curEnd === null || x.start > curEnd) {
      if (curEnd !== null) total += curEnd - curStart;
      curStart = x.start;
      curEnd = x.end;
    } else if (x.end > curEnd) {
      curEnd = x.end;
    }
  }
  if (curEnd !== null) total += curEnd - curStart;
  return total;
}

/**
 * 估算（单位：分钟）。
 * - speechFrac：{ me, sys } 两条音轨里有人说话的时间占比（识别只处理说话的部分）；
 * - rtf：{ decode, vad, aec, diar, asr }，除 asr 以外都按整条音轨的时长算，asr 按说话时长算；
 * - useAec：是否对麦克风轨做回声消除；diarMic：是否对麦克风轨也做说话人分离（默认只分系统轨）。
 */
export function estimatePipeline({ meetingMin = 60, speechFrac, rtf, useAec = false, diarMic = false }) {
  const stages = {
    decode: 2 * meetingMin * rtf.decode,
    vad: 2 * meetingMin * rtf.vad,
    aec: useAec ? meetingMin * rtf.aec : 0,
    diar: (diarMic ? 2 : 1) * meetingMin * rtf.diar,
    asr: (speechFrac.me + speechFrac.sys) * meetingMin * rtf.asr,
  };
  const total = Object.values(stages).reduce((a, b) => a + b, 0);
  return { stages, total, budget: GATES.asr.estMinutesMax, fits: total <= GATES.asr.estMinutesMax + 1e-9 };
}
