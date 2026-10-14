// 汇总 S4 的全部结果：按事先写死的门槛与决策规则（lib/decide.mjs）下结论，写出 <S4 目录>\results\report.json 与 report.md（填设计文档 3.9 用）。
//   node scripts/s4/report.mjs --home <S4 目录>
// 缺哪一阶段的结果就报错并说明先跑什么，不拿残缺的数字下结论。
import fs from 'node:fs';
import path from 'node:path';
import { log, parseArgs } from './lib/cli.mjs';
import { loadSessions } from './lib/dataset.mjs';
import { homeFrom, readJson, s4Paths, writeJsonAtomic } from './lib/home.mjs';
import { renderReport } from './lib/render.mjs';
import { buildReport } from './lib/report.mjs';
import { ASR_CANDIDATES } from './lib/sherpa.mjs';

const { flags } = parseArgs(process.argv.slice(2));
const paths = s4Paths(homeFrom(flags.home));

const readIf = (file) => (fs.existsSync(file) ? readJson(file) : null);
const readDir = (dir) => (fs.existsSync(dir) ? fs.readdirSync(dir).filter((f) => f.endsWith('.json')).sort().map((f) => readJson(path.join(dir, f))) : []);

const speedFile = path.join(paths.results, 'speed.json');
if (!fs.existsSync(speedFile)) throw new Error(`没有 ${speedFile}，先跑 node scripts/s4/speed.mjs`);

const asrFile = (id, name) => path.join(paths.results, 'asr', id, name);
const candidates = ASR_CANDIDATES.map((c) => ({
  id: c.id,
  name: c.name,
  prescreen8: readIf(asrFile(c.id, 'prescreen-t8.json')),
  prescreen4: readIf(asrFile(c.id, 'prescreen-t4.json')),
  full: readIf(asrFile(c.id, 'full-t8.json')),
  vad: readIf(asrFile(c.id, 'vad-t8.json')),
}));
const diarDir = (sub) => path.join(paths.results, 'diar', sub);
const diar = {
  sweep: readDir(diarDir('sweep')),
  confirm: readDir(diarDir('confirm')),
  far: readDir(diarDir('far')),
  rotations: readDir(diarDir('rotations')),
  timing: readIf(path.join(diarDir('timing'), 'timing.json')),
};

const report = buildReport({ sessions: loadSessions(paths.dataset), speed: readJson(speedFile), candidates, diar, echo: readDir(path.join(paths.results, 'echo')) });
const md = renderReport(report, { generatedAt: `${new Date().toISOString()}（UTC）` });

writeJsonAtomic(path.join(paths.results, 'report.json'), report);
fs.writeFileSync(path.join(paths.results, 'report.md'), md);
log(`已写出 ${path.join(paths.results, 'report.md')} 与 report.json`);
console.log(md.split('\n## ')[1].replace(/^/, '## '));
