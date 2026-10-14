// 把下载好的压缩包静态解压（不运行里面的任何东西）：先确认它就是核对过摘要的那一份，再列出全部条目做安全检查，通过了才解压。
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { outputLines, unsafeEntries } from './archive.mjs';
import { sha256File } from './download.mjs';
import { readJson, writeJsonAtomic } from './home.mjs';

/** Windows 自带的 tar（bsdtar）：直接用系统目录里的，不走 PATH（PATH 里可能有别的 tar，参数和行为不一样） */
export function tarPath(env = process.env) {
  if (!env.SystemRoot) throw new Error('找不到 Windows 目录（环境变量 SystemRoot 没有设置），定位不了系统自带的 tar');
  return path.join(env.SystemRoot, 'System32', 'tar.exe');
}

/** 运行 tar，成功返回它的标准输出，失败抛错（说明是哪个压缩包、哪个操作） */
export function runTar(tar, name, args) {
  const r = spawnSync(tar, args, { encoding: 'utf8', maxBuffer: 1 << 28 });
  if (r.error) throw new Error(`${name}：没法运行 ${tar}：${r.error.message}`);
  if (r.status !== 0) throw new Error(`${name}：tar ${args[0]} 失败（退出码 ${r.status}）：${r.stderr.trim()}`);
  return r.stdout;
}

// tar 列出的非 ASCII 文件名在这台机器上是乱码（按系统代码页输出），没法拿去核对磁盘上的文件
const ASCII_NAME = /^[\x20-\x7e]+$/;

/**
 * 解压 items 里有 extractTo 的压缩包到 home/<extractTo>。manifest 是 fetch 写的下载清单：
 * 没有记录、或记录说没核对过摘要的不解压；压缩包现在的摘要与清单不一致（下载之后被改过）也不解压。
 * 已经解压过的（home/extracted/<文件名>.json 里记的摘要与现在一致）跳过。
 * 解压后逐个核对文件名是 ASCII 的普通文件确实在磁盘上（tar 退出码为 0 也不一定全解出来了）；
 * 出错就停下，不留“已解压”的标记。exec 可替换，测试用。
 * 返回 [{ name, status: 'extracted' | 'already', entries?, unchecked? }]（unchecked：文件名含非 ASCII 字符、没法逐个核对的文件数）。
 */
export async function extractAll({ home, items, manifest, tar = tarPath(), exec = runTar, say = () => {} }) {
  const results = [];
  for (const item of items) {
    if (!item.extractTo) continue;
    const entry = manifest[item.name];
    if (!entry) throw new Error(`${item.name}：下载清单里没有它，先跑 fetch`);
    if (!entry.verified) throw new Error(`${item.name}：下载时没有核对过摘要，不解压`);
    const archive = path.join(home, 'downloads', item.name);
    const sha = await sha256File(archive);
    if (sha !== entry.sha256) throw new Error(`${item.name}：现在的摘要 ${sha} 与下载清单里记的 ${entry.sha256} 不一致（下载之后被改过？），不解压`);
    const marker = path.join(home, 'extracted', `${item.name}.json`);
    if (fs.existsSync(marker) && readJson(marker).sha256 === sha) {
      say(`已解压：${item.name}`);
      results.push({ name: item.name, status: 'already' });
      continue;
    }
    const names = outputLines(exec(tar, item.name, ['-tf', archive]));
    const bad = unsafeEntries(names, outputLines(exec(tar, item.name, ['-tvf', archive])));
    if (bad.length) {
      throw new Error(`${item.name}：压缩包里有不安全的条目，不解压：\n  ${bad.slice(0, 5).join('\n  ')}${bad.length > 5 ? `\n  …共 ${bad.length} 条` : ''}`);
    }
    const dest = path.join(home, item.extractTo);
    fs.mkdirSync(dest, { recursive: true });
    const t0 = Date.now();
    exec(tar, item.name, ['-xf', archive, '-C', dest]);
    const files = names.filter((n) => !n.endsWith('/'));
    const missing = files.filter((n) => ASCII_NAME.test(n) && !fs.existsSync(path.join(dest, n)));
    if (missing.length) throw new Error(`${item.name}：tar 没有报错，但有 ${missing.length} 个文件不在磁盘上，例如 ${missing[0]}`);
    const unchecked = files.filter((n) => !ASCII_NAME.test(n)).length;
    writeJsonAtomic(marker, { sha256: sha, entries: names.length, unchecked, extractedAt: new Date().toISOString() });
    say(`解压：${item.name} → ${item.extractTo}（${names.length} 个条目，${((Date.now() - t0) / 1000).toFixed(0)} s${unchecked ? `；${unchecked} 个文件名含非 ASCII 字符，没法逐个核对` : ''}）`);
    results.push({ name: item.name, status: 'extracted', entries: names.length, unchecked });
  }
  return results;
}
