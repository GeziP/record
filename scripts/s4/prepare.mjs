// 准备评测输入（耗时几分钟，可以重复运行，已有的产物直接沿用）：
// 1. 每条近场录音按标注切成一段一个 wav（识别评测用）；
// 2. 每场会议把“我”（说话最多的那位）之外的近场录音相加，作为“系统声音”（说话人分离评测用）；
// 3. 远场阵列取第 1 通道（远场分离评测用），并核对它与标注的时间对齐。
// 用法：node scripts/s4/prepare.mjs --home <S4 目录>
import path from 'node:path';
import { log, parseArgs } from './lib/cli.mjs';
import { loadSessions } from './lib/dataset.mjs';
import { homeFrom, s4Paths, writeJsonAtomic } from './lib/home.mjs';
import { buildFarMix, buildMix, farLagSec, prepareTrackSegments } from './lib/prep.mjs';

/** 远场相对标注的错位超过这么多秒就停下：远场 DER 不可信，脚本里不悄悄做对齐修正（评分容差是 0.25 秒） */
const MAX_FAR_LAG_SEC = 0.5;

const { flags } = parseArgs(process.argv.slice(2));
const paths = s4Paths(homeFrom(flags.home));
const sessions = loadSessions(paths.dataset);
const out = { createdAt: new Date().toISOString(), sessions: [] };

for (const s of sessions) {
  log(`${s.id}：近场切段（${s.tracks.length} 条）`);
  const tracks = s.tracks.map((t) => ({ speaker: t.speaker, segments: prepareTrackSegments({ work: paths.work, sessionId: s.id, track: t }) }));
  log(`${s.id}：系统声音混合（“我”= ${s.me}）与远场第 1 通道`);
  const mix = buildMix({ work: paths.work, session: s, me: s.me });
  const far = buildFarMix({ work: paths.work, session: s });
  const lag = farLagSec({ nearWavs: s.tracks.map((t) => t.wav), farWav: far.wav });
  log(`${s.id}：远场相对标注的延迟 ${lag.lagSec.toFixed(2)} 秒`);
  if (Math.abs(lag.lagSec) > MAX_FAR_LAG_SEC) throw new Error(`${s.id}：远场相对标注错位 ${lag.lagSec.toFixed(2)} 秒，超过 ${MAX_FAR_LAG_SEC} 秒。远场评测不做对齐修正，停在这里。`);
  out.sessions.push({ id: s.id, me: s.me, tracks, mixSec: mix.durationSec, mixSpeakers: mix.actualSpeakers, farSec: far.durationSec, farLagSec: lag.lagSec, farLagPeak: lag.peak });
}

writeJsonAtomic(path.join(paths.work, 'prep.json'), out);
log(`准备完成：${sessions.length} 场会议，记录在 ${path.join(paths.work, 'prep.json')}`);
