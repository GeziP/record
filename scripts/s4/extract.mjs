// S4 解压：把 downloads 里核对过摘要的压缩包静态解压到 <S4 目录>（程序 → tools，模型 → models，评测集 → data）。
// 解压前先确认压缩包就是 fetch 核对过的那一份，再列出全部条目做安全检查（绝对路径、盘符或冒号、..、链接、特殊文件一律拒绝）；只解压，不运行里面的任何东西。
// 用法：node scripts/s4/extract.mjs --home <S4 目录> [--groups tools,small,asr,big,data]（不写 --groups 就是全部）
// 已经解压过的（<S4 目录>/extracted/ 里有标记且摘要一致）跳过。
import fs from 'node:fs';
import path from 'node:path';
import { log, parseArgs } from './lib/cli.mjs';
import { extractAll } from './lib/extract.mjs';
import { GROUPS, selectItems } from './lib/fetch-plan.mjs';
import { homeFrom, readJson } from './lib/home.mjs';

const { flags } = parseArgs(process.argv.slice(2));

try {
  const home = homeFrom(flags.home);
  const manifestPath = path.join(home, 'fetch-manifest.json');
  if (!fs.existsSync(manifestPath)) throw new Error(`${manifestPath} 不存在：先跑 fetch.mjs`);
  const items = selectItems(flags.groups === undefined ? GROUPS : String(flags.groups).split(','));
  const results = await extractAll({ home, items, manifest: readJson(manifestPath), say: log });
  log(`完成：新解压 ${results.filter((r) => r.status === 'extracted').length} 个，之前已解压 ${results.filter((r) => r.status === 'already').length} 个`);
} catch (e) {
  log(`！失败：${e.message}`);
  process.exitCode = 1;
}
