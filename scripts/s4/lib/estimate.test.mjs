import test from 'node:test';
import assert from 'node:assert/strict';
import { estimatePipeline, unionDuration } from './estimate.mjs';

const seg = (start, end) => ({ start, end });
const near = (a, b, tol = 1e-9) => assert.ok(Math.abs(a - b) <= tol, `期望 ${b}，得到 ${a}`);

test('并集时长：重叠与相接的段合并，不重叠的相加，乱序与空段不影响', () => {
  assert.equal(unionDuration([]), 0);
  assert.equal(unionDuration([seg(0, 10)]), 10);
  assert.equal(unionDuration([seg(0, 10), seg(5, 8)]), 10); // 包含
  assert.equal(unionDuration([seg(0, 10), seg(5, 15)]), 15); // 部分重叠
  assert.equal(unionDuration([seg(0, 10), seg(10, 20)]), 20); // 恰好相接
  assert.equal(unionDuration([seg(20, 30), seg(0, 10)]), 20); // 乱序、不相交
  assert.equal(unionDuration([seg(3, 3), seg(5, 4), seg(0, 2)]), 2); // 空段与反向段忽略
  assert.equal(unionDuration([seg(0, 5), seg(1, 2), seg(1.5, 9), seg(20, 21)]), 10);
});

const rtf = { decode: 0.01, vad: 0.02, aec: 0.05, diar: 0.1, asr: 0.2 };
const speechFrac = { me: 0.25, sys: 0.5 };

test('估算：各阶段按公式累加（解码与 VAD 两条轨，回声消除只对麦克风轨，分离只对系统轨，识别只算说话时长）', () => {
  const r = estimatePipeline({ meetingMin: 60, speechFrac, rtf, useAec: true });
  near(r.stages.decode, 1.2);
  near(r.stages.vad, 2.4);
  near(r.stages.aec, 3);
  near(r.stages.diar, 6);
  near(r.stages.asr, 9);
  near(r.total, 21.6);
  assert.equal(r.budget, 30);
  assert.equal(r.fits, true);
});

test('估算：不做回声消除时这一项为 0；麦克风轨也分离时分离翻倍', () => {
  const noAec = estimatePipeline({ meetingMin: 60, speechFrac, rtf });
  near(noAec.stages.aec, 0);
  near(noAec.total, 18.6);
  const both = estimatePipeline({ meetingMin: 60, speechFrac, rtf, diarMic: true });
  near(both.stages.diar, 12);
  near(both.total, 24.6);
});

test('估算：会议时长按比例缩放；超过 30 分钟就不达标，恰好 30 分钟算达标', () => {
  near(estimatePipeline({ meetingMin: 30, speechFrac, rtf }).total, 9.3);
  const slow = estimatePipeline({ meetingMin: 60, speechFrac, rtf: { ...rtf, asr: 0.5 } });
  near(slow.stages.asr, 22.5);
  assert.equal(slow.fits, false);
  // 构造恰好 30：其余阶段 18.6 − 9 = 9.6 分钟，识别要 20.4 分钟 = 0.75 × 60 × rtf → rtf = 0.4533…
  const exact = estimatePipeline({ meetingMin: 60, speechFrac, rtf: { ...rtf, asr: 20.4 / 45 } });
  near(exact.total, 30, 1e-9);
  assert.equal(exact.fits, true);
});
