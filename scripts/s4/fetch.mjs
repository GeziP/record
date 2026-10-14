// S4 下载：把官方 sherpa-onnx 程序、候选模型和 AliMeeting 评测集下载到 <S4 目录>/downloads，逐个核对 SHA-256；只下载，不运行任何东西。
// 用法：node scripts/s4/fetch.mjs --home <S4 目录> [--groups tools,small,asr,big,data]（不写 --groups 就是全部；meta 只取各分类的 checksum.txt）
// 已有且摘要相符的文件不重下；摘要不符的文件改名 .BAD、不会被使用，并以非 0 退出码结束。
// 下载清单写在 <S4 目录>/fetch-manifest.json，日志同时追加到 <S4 目录>/fetch.log。
import fs from 'node:fs';
import path from 'node:path';
import { log, parseArgs } from './lib/cli.mjs';
import { CHECKSUM_FILES, checksumUrl, GROUPS, selectItems } from './lib/fetch-plan.mjs';
import { fetchAll } from './lib/fetch-run.mjs';
import { homeFrom } from './lib/home.mjs';

const { flags } = parseArgs(process.argv.slice(2));
const home = homeFrom(flags.home);
fs.mkdirSync(home, { recursive: true });
const say = (message) => {
  const now = new Date();
  log(message, now);
  fs.appendFileSync(path.join(home, 'fetch.log'), `${now.toLocaleString('sv-SE')}  ${message}\n`);
};

try {
  const groups = flags.groups === undefined ? GROUPS : String(flags.groups).split(',');
  const items = selectItems(groups);
  say(`开始：S4 目录 ${home}，分组 ${groups.join(',')}（${items.length} 个文件）`);
  const { failed } = await fetchAll({ home, items, checksumFiles: CHECKSUM_FILES, checksumUrlFor: checksumUrl, say });
  if (failed.length > 0) {
    say(`！${failed.length} 个文件摘要不符，已改名为 .BAD、不会被使用：${failed.map((f) => f.name).join('、')}`);
    process.exitCode = 1;
  } else {
    say('本批完成');
  }
} catch (e) {
  say(`！失败：${e.message}`);
  process.exitCode = 1;
}
