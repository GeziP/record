import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { aggregateEcho, ALIGN_MARGIN_MS, baselineCounts, conditions, cutSignal, evalVariants, excerptSegments, farOnlySegments, keepOnly, makeMic, RATIOS_DB, SCENARIOS, scoreOutput, VARIANTS } from './echo-eval.mjs';
import { mulberry32 } from './echo.mjs';
import { pickAec } from './decide.mjs';
import { readWavChannel, readWavInfo } from './wav.mjs';

const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), 'record-s4-echoeval-'));
const RATE = 8000;

test('条件网格：三种通路 × 三档能量比 = 九个，编号不重复；处理方式五种', () => {
  assert.deepEqual(SCENARIOS.map((s) => [s.delayMs, s.rt60]), [[20, 0.2], [100, 0.4], [300, 0.6]]);
  assert.deepEqual(RATIOS_DB, [-10, 0, 5]);
  const c = conditions();
  assert.equal(c.length, 9);
  assert.equal(new Set(c.map((x) => x.id)).size, 9);
  assert.deepEqual(c.slice(0, 3).map((x) => x.id), ['d020r02_r-10', 'd020r02_r0', 'd020r02_r5']);
  assert.equal(c[8].id, 'd300r06_r5');
  assert.equal(c[8].scenario.delayMs, 300);
  assert.equal(c[8].ratioDb, 5);
  assert.deepEqual(VARIANTS.map((v) => v.id), ['none', 'corti', 'corti-align', 'aec3', 'aec3-align']);
  assert.ok(VARIANTS.filter((v) => v.algo === 'corti').every((v) => v.opts.includes('filter_len=16384')));
  assert.deepEqual(VARIANTS.filter((v) => v.id.endsWith('-align')).map((v) => v.opts.includes('align=1')), [true, true]);
  assert.ok(VARIANTS.filter((v) => !v.id.endsWith('-align')).every((v) => !v.opts.includes('align=1') && !v.opts.some((o) => o.startsWith('margin_ms='))));
  assert.equal(ALIGN_MARGIN_MS, 10);
  assert.ok(VARIANTS.filter((v) => v.id.endsWith('-align')).every((v) => v.opts.includes('margin_ms=10')), '对齐要留余量，零余量会让 aec3 失效');
});

test('截取与远端单讲段：终点不超过截取点；与“我”的段（前后放宽 guard 秒）重叠的不算', () => {
  const segs = [{ start: 0, end: 5 }, { start: 5, end: 10 }, { start: 8, end: 10.5 }];
  assert.deepEqual(excerptSegments(segs, 10), [segs[0], segs[1]]);
  assert.deepEqual(excerptSegments(segs, 10.5), segs);
  const me = [{ start: 10, end: 12 }];
  const others = [{ start: 4, end: 9.75 }, { start: 4, end: 9.76 }, { start: 12.25, end: 14 }, { start: 12.24, end: 14 }, { start: 0, end: 1 }];
  assert.deepEqual(farOnlySegments(me, others, 0.25), [others[0], others[2], others[4]]); // 恰好碰到放宽边界的不算重叠
  assert.deepEqual(farOnlySegments([], others, 0.25), others);
  assert.deepEqual(farOnlySegments(me, others, 0), [others[0], others[1], others[2], others[3], others[4]]);
});

// 一个小场景：20 秒，“我”说三段，对方说三段，其中 9～11 秒与“我”的 8～10 秒重叠（双讲，不算远端单讲）
const ME = [{ start: 1, end: 3, text: '我说第一段' }, { start: 8, end: 10, text: '我说第二段' }, { start: 14, end: 16, text: '我说第三段' }];
const OTHERS = [{ start: 4, end: 6, text: '对方第一段话' }, { start: 9, end: 11, text: '对方第二段话' }, { start: 17, end: 19, text: '对方第三段话' }];
function signals() {
  const r = mulberry32(5);
  const burst = (segs) => {
    const x = new Float32Array(RATE * 20);
    for (const s of segs) for (let i = Math.round(s.start * RATE); i < Math.round(s.end * RATE); i++) x[i] = (r() - 0.5) * 0.4;
    return x;
  };
  return { near: burst(ME), far: burst(OTHERS) };
}
const rms = (x) => Math.sqrt(x.reduce((a, v) => a + v * v, 0) / x.length);

test('合成麦克风信号：近端 + 回声 + 底噪，同样的输入得到同样的结果', () => {
  const { near, far } = signals();
  const args = { near, far, meSegments: ME, otherSegments: OTHERS, scenario: SCENARIOS[1], ratioDb: 0, rate: RATE };
  const a = makeMic(args);
  const b = makeMic(args);
  assert.deepEqual(a.mic, b.mic);
  assert.equal(a.mic.length, near.length);
  // 回声段（远端单讲的 4～6 秒）里：麦克风 = 回声 + 很小的底噪
  const slice = (x) => x.subarray(4 * RATE, 6 * RATE);
  const resid = Float32Array.from(slice(a.mic), (v, i) => v - slice(a.echo)[i]);
  assert.ok(rms(resid) < 0.01 * rms(slice(a.mic)) * 10 && rms(slice(a.echo)) > 0.02, `回声 ${rms(slice(a.echo))}，残差 ${rms(resid)}`);
  for (const sc of [SCENARIOS[0], SCENARIOS[2]]) {
    const { echo } = makeMic({ ...args, scenario: sc });
    const from = (t) => Math.round(t * RATE);
    const early = rms(echo.subarray(from(4), from(4 + sc.delayMs / 1000 - 0.005))); // 对方 4 秒开口，直达声要晚 delayMs 才到
    const late = rms(echo.subarray(from(4 + sc.delayMs / 1000 + 0.01), from(4 + sc.delayMs / 1000 + 0.3)));
    assert.ok(early < 1e-6 && late > 0.01, `延迟 ${sc.delayMs} ms：之前 ${early}，之后 ${late}`);
  }
  const louder = makeMic({ ...args, ratioDb: 5 });
  assert.ok(rms(louder.echo) > 1.5 * rms(a.echo), '能量比抬高 5 dB 回声应更响');
});

test('合成麦克风信号：能量比取负无穷就是没有回声（只有近端和底噪），底噪与有回声时相同', () => {
  const { near, far } = signals();
  const args = { near, far, meSegments: ME, otherSegments: OTHERS, scenario: SCENARIOS[0], rate: RATE };
  const none = makeMic({ ...args, ratioDb: -Infinity });
  assert.ok(none.echo.every((v) => v === 0));
  const withEcho = makeMic({ ...args, ratioDb: 0 });
  for (let i = 0; i < near.length; i += 997) assert.ok(Math.abs(none.mic[i] - (withEcho.mic[i] - withEcho.echo[i])) < 1e-5, `第 ${i} 点：底噪应当相同`);
  const quiet = rms(none.mic.subarray(4 * RATE, 6 * RATE)); // 远端单讲的 4～6 秒：近端静默、没有回声，只剩底噪（约为近端有声段的 1%）
  const speech = rms(near.subarray(RATE, 3 * RATE));
  assert.ok(quiet > 0.005 * speech && quiet < 0.02 * speech, `底噪 ${quiet}，近端 ${speech}`);
});

test('切信号：按段写 wav，文件名带前缀与序号，帧数对得上；有段落在信号之外就报错', () => {
  const dir = tmp();
  const x = Float32Array.from({ length: RATE * 20 }, (_, i) => Math.sin(i / 30) * 0.5);
  const files = cutSignal(x, ME, RATE, dir, 'v-near-');
  assert.deepEqual(files.map((f) => path.basename(f)), ['v-near-0000.wav', 'v-near-0001.wav', 'v-near-0002.wav']);
  assert.deepEqual(files.map((f) => readWavInfo(f).frames), [16000, 16000, 16000]);
  const got = readWavChannel(files[1], 0).samples;
  assert.ok(Math.abs(got[100] - x[8 * RATE + 100]) < 1e-3);
  assert.throws(() => cutSignal(x, [{ start: 25, end: 26, text: '外' }], RATE, dir, 'v-'), /1 个段落在信号之外/);
  assert.deepEqual(cutSignal(x, [], RATE, dir, 'v-'), []);
  fs.rmSync(dir, { recursive: true, force: true });
});

// 假的回声消除与假的识别：识别器只认能量（均方根大于 0.05 就“认出”参考文本），所以回声残留与否直接决定远端单讲段有没有文字
function fakes(echo, expect) {
  const calls = { aec: [], decode: [], far: [] };
  const factors = { corti: (m, e) => m - e, 'corti-align': (m) => m, aec3: (m, e) => m - 0.7 * e, 'aec3-align': (m) => m * 0.01 };
  const deps = {
    aec: async (v, far, mic) => {
      calls.aec.push(v.id);
      assert.equal(far, expect.far, '远端参考应当是原来的远端信号');
      assert.equal(mic, expect.mic, '要处理的应当是麦克风信号');
      return { samples: Float32Array.from(mic, (m, i) => factors[v.id](m, echo[i])), info: { delay_ms: 100, wall_s: 1.5 } };
    },
    decode: async (files) => {
      calls.decode.push(files);
      return files.map((f) => {
        const m = /-near-(\d{4})\.wav$/.exec(f);
        assert.ok(m, `只有近端段才逐段识别：${f}`);
        const x = readWavChannel(f, 0).samples;
        return rms(x) > 0.05 ? ME[Number(m[1])].text : '';
      });
    },
    // 假的 VAD + 识别：整条信号里，每个远端单讲段只要有能量就“识别”出参考文本；并检查远端单讲段之外确实被置成了 0
    decodeFar: async (file) => {
      calls.far.push(path.basename(file));
      const x = readWavChannel(file, 0).samples;
      assert.equal(x.length, RATE * 20);
      const inside = new Uint8Array(x.length);
      for (const s of OTHERS_FAR_ONLY) inside.fill(1, Math.round(s.start * RATE), Math.round(s.end * RATE));
      for (let i = 0; i < x.length; i++) if (!inside[i]) assert.equal(x[i], 0, `远端单讲段之外应当置零，第 ${i} 点`);
      return OTHERS_FAR_ONLY.filter((s) => rms(x.subarray(Math.round(s.start * RATE), Math.round(s.end * RATE))) > 0.05).map((s) => s.text).join('');
    },
  };
  return { deps, calls };
}
const OTHERS_FAR_ONLY = farOnlySegments(ME, OTHERS, 0.25);

test('一个条件下各处理方式：回声消减量、近端识别结果、远端单讲段（VAD + 识别）的文字各归各的', async () => {
  const dir = tmp();
  const { near, far } = signals();
  const { mic, echo } = makeMic({ near, far, meSegments: ME, otherSegments: OTHERS, scenario: SCENARIOS[0], ratioDb: 0, rate: RATE });
  assert.deepEqual(OTHERS_FAR_ONLY.map((s) => s.text), ['对方第一段话', '对方第三段话']); // 第二段与“我”重叠
  const { deps, calls } = fakes(echo, { far, mic });
  const out = await evalVariants({ rate: RATE, far, mic, nearSegs: ME, farOnlySegs: OTHERS_FAR_ONLY, deps, dir });
  assert.deepEqual(out.map((o) => o.variant), VARIANTS.map((v) => v.id));
  assert.deepEqual(calls.aec, ['corti', 'corti-align', 'aec3', 'aec3-align']); // none 不调用
  assert.equal(calls.decode.length, 1); // 所有近端文件一次交给识别
  assert.equal(calls.decode[0].length, 5 * 3);
  assert.deepEqual(calls.decode[0].slice(0, 4).map((f) => path.basename(f)), ['none-near-0000.wav', 'none-near-0001.wav', 'none-near-0002.wav', 'corti-near-0000.wav']);
  assert.deepEqual(calls.far, VARIANTS.map((v) => `${v.id}-far.wav`)); // 每种处理一条整条信号，按处理方式的顺序
  const by = Object.fromEntries(out.map((o) => [o.variant, o]));
  const refsNear = ME.map((s) => s.text);
  const textFar = OTHERS_FAR_ONLY.map((s) => s.text).join('');

  assert.equal(by.none.erleDb, 0);
  assert.equal(by.none.info, null);
  assert.deepEqual(by.none.nearHyps, refsNear); // 近端有声，认得出
  assert.equal(by.none.farText, textFar); // 没处理：回声被认成了对方的话

  assert.ok(by.corti.erleDb > 30, `完全消掉时 ERLE ${by.corti.erleDb}`);
  assert.deepEqual(by.corti.nearHyps, refsNear);
  assert.equal(by.corti.farText, '');
  assert.deepEqual(by.corti.info, { delay_ms: 100, wall_s: 1.5 });

  assert.ok(Math.abs(by['corti-align'].erleDb) < 1e-9); // 原样返回
  assert.equal(by['corti-align'].farText, textFar);

  assert.ok(by.aec3.erleDb > 9 && by.aec3.erleDb < 12, `只消掉 70% 的幅度时 ERLE 约 10.5 dB，实际 ${by.aec3.erleDb}`);
  assert.equal(by.aec3.farText, ''); // 剩下的回声低于识别器的听觉门槛

  assert.ok(Math.abs(by['aec3-align'].erleDb - 40) < 0.01); // 整体压到 1%：回声压下去了，近端也压没了
  assert.deepEqual(by['aec3-align'].nearHyps, ['', '', '']);
  assert.equal(by['aec3-align'].farText, '');
  fs.rmSync(dir, { recursive: true, force: true });
});

test('一个条件下各处理方式：处理后长度变了、识别返回的个数不对，都明确报错', async () => {
  const dir = tmp();
  const { near, far } = signals();
  const { mic } = makeMic({ near, far, meSegments: ME, otherSegments: OTHERS, scenario: SCENARIOS[0], ratioDb: 0, rate: RATE });
  const base = { rate: RATE, far, mic, nearSegs: ME, farOnlySegs: OTHERS_FAR_ONLY, dir, variants: [VARIANTS[1]] };
  const decodeFar = async () => '';
  await assert.rejects(evalVariants({ ...base, deps: { aec: async () => ({ samples: mic.subarray(1), info: null }), decode: async (f) => f.map(() => ''), decodeFar } }), /corti：处理后长度 \d+ 与输入 \d+ 不同/);
  await assert.rejects(evalVariants({ ...base, deps: { aec: async () => ({ samples: mic, info: null }), decode: async (f) => f.slice(1).map(() => ''), decodeFar } }), /识别返回了 2 个结果，应当是 3 个/);
  fs.rmSync(dir, { recursive: true, force: true });
});

test('只留掩码内的点：其余置零、长度不变、不改原信号', () => {
  const x = Float32Array.from([0.5, -0.25, 0.75, 0.125, -1]);
  const kept = keepOnly(x, Uint8Array.from([0, 1, 1, 0, 1]));
  assert.deepEqual(Array.from(kept), [0, -0.25, 0.75, 0, -1]);
  assert.deepEqual(Array.from(x), [0.5, -0.25, 0.75, 0.125, -1]);
  assert.notEqual(kept, x);
  assert.deepEqual(Array.from(keepOnly(x, new Uint8Array(5))), [0, 0, 0, 0, 0]);
  assert.deepEqual(Array.from(keepOnly(new Float32Array(0), new Uint8Array(0))), []);
});

test('打分：近端 CER 的计数与回声插入的计数（按规整后的字数）', () => {
  const s = scoreOutput({ nearRefs: ['你好世界', '再见'], farRefs: ['对方的话', '。，'] }, { nearHyps: ['你好世', '再见了'], farText: '对，方的话啊啊！' });
  assert.equal(s.nearRefLen, 6);
  assert.equal(s.nearErrors, 2); // 删一个字、多一个字
  assert.equal(s.insHyp, 6); // 标点不算字：对方的话啊啊
  assert.equal(s.insRef, 4);
  assert.throws(() => scoreOutput({ nearRefs: ['a'], farRefs: [] }, { nearHyps: [], farText: '' }), /个数对不上/);
  assert.equal(scoreOutput({ nearRefs: [], farRefs: [] }, { nearHyps: [], farText: '嗯 嗯。' }).insHyp, 2); // 远端文字是整条信号的，不要求与参考段数对应
  assert.deepEqual(baselineCounts(['你好世界'], ['你好']), { errors: 2, refLen: 4 });
  assert.throws(() => baselineCounts(['a', 'b'], ['a']), /个数对不上/);
});

const cell = (o) => ({ nearErrors: 5, nearRefLen: 100, insHyp: 2, insRef: 100, erleDb: 20, ...o });
const floorOf = (o) => ({ nearErrors: 4, nearRefLen: 100, insHyp: 3, insRef: 100, ...o });

test('汇总：基线与各格都按总量合计，近端 CER 增加 = 该格 CER − 基线；ERLE 取各场的平均', () => {
  const s1 = { baseline: { errors: 10, refLen: 100 }, floor: floorOf({}), conditions: { c1: { none: cell({ insHyp: 50, erleDb: 0 }), corti: cell({ nearErrors: 12, insHyp: 0, erleDb: 30 }) } } };
  const s2 = { baseline: { errors: 10, refLen: 300 }, floor: floorOf({ nearRefLen: 300, nearErrors: 6, insHyp: 5, insRef: 300 }), conditions: { c1: { none: cell({ nearRefLen: 300, nearErrors: 15, insHyp: 150, insRef: 300, erleDb: 0 }), corti: cell({ nearRefLen: 300, nearErrors: 18, insHyp: 6, insRef: 300, erleDb: 10 }) } } };
  const a = aggregateEcho([s1, s2]);
  assert.equal(a.baselineCer, 20 / 400);
  assert.equal(a.floor.insertion, 8 / 400); // 底线也按总量合计：(3 + 5) / (100 + 300)
  assert.equal(a.floor.nearCer, 10 / 400);
  const none = a.variants.none.conditions[0];
  assert.equal(none.id, 'c1');
  assert.equal(none.insertion, 200 / 400);
  assert.equal(none.nearCer, 20 / 400);
  assert.equal(none.nearCerIncrease, 0);
  const corti = a.variants.corti.conditions[0];
  assert.equal(corti.insertion, 6 / 400);
  assert.equal(corti.nearCer, 30 / 400);
  assert.ok(Math.abs(corti.nearCerIncrease - 10 / 400) < 1e-12);
  assert.equal(corti.erleDb, 20);
});

test('汇总：可以直接交给 pickAec（去掉 none）；缺条件、缺处理方式、没有远端参考字、没有底线都报错', () => {
  const good = (insHyp, nearErrors) => ({ baseline: { errors: 5, refLen: 100 }, floor: floorOf({}), conditions: { a: { none: cell({}), corti: cell({ insHyp, nearErrors }) }, b: { none: cell({}), corti: cell({ insHyp, nearErrors }) } } });
  const ok = aggregateEcho([good(4, 6)]);
  const pick = pickAec(Object.entries(ok.variants).filter(([n]) => n !== 'none').map(([name, v]) => ({ name, conditions: v.conditions })));
  assert.equal(pick.status, 'ok');
  assert.equal(pick.chosen, 'corti');
  const bad = aggregateEcho([good(6, 6)]); // 插入率 6% 超过 5%
  assert.equal(pickAec(Object.entries(bad.variants).filter(([n]) => n !== 'none').map(([name, v]) => ({ name, conditions: v.conditions }))).status, 'none');

  assert.throws(() => aggregateEcho([]), /没有会议/);
  assert.throws(() => aggregateEcho([good(1, 1), { baseline: { errors: 1, refLen: 10 }, floor: floorOf({}), conditions: { a: good(1, 1).conditions.a } }]), /缺条件 b/);
  assert.throws(() => aggregateEcho([good(1, 1), { baseline: { errors: 1, refLen: 10 }, floor: floorOf({}), conditions: { a: { none: cell({}) }, b: { none: cell({}) } } }]), /缺 corti 在条件 a 下的结果/);
  assert.throws(() => aggregateEcho([{ baseline: { errors: 1, refLen: 10 }, floor: floorOf({}), conditions: { a: { none: cell({ insRef: 0 }) } } }]), /没有远端单讲段的参考字/);
  const { floor, ...noFloor } = good(1, 1);
  assert.throws(() => aggregateEcho([good(1, 1), noFloor]), /没有底线结果/);
  assert.throws(() => aggregateEcho([{ ...good(1, 1), floor: floorOf({ insRef: 0 }) }]), /底线：没有远端单讲段的参考字/);
});
