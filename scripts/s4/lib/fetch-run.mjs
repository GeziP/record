// 下载并核对一批条目（只下载、只算摘要，不运行下载来的任何东西）。条目表与摘要规则见 fetch-plan.mjs。
import fs from 'node:fs';
import path from 'node:path';
import { download, sha256File } from './download.mjs';
import { expectedDigest, parseChecksums } from './fetch-plan.mjs';
import { readJson, writeJsonAtomic } from './home.mjs';

/**
 * 在 home 下建 downloads、meta，先取各分类的 checksum.txt（必须与写死的 GitHub API 摘要一致才信），
 * 再逐个条目：已有且摘要相符就不动；已有但不符就删掉重下；下完不符就改名 .BAD 并记入 failed，不拿来用。
 * 每个核对过的条目记入 home/fetch-manifest.json（别的条目的记录原样保留）。
 * 返回 { manifest, failed: [{ name, expected, actual }] }；摘要不符不抛错，由调用方决定怎么退出。
 */
export async function fetchAll({ home, items, checksumFiles, checksumUrlFor, say = () => {}, downloadOpts = {} }) {
  const downloads = path.join(home, 'downloads');
  const meta = path.join(home, 'meta');
  const opts = { ...downloadOpts, onLog: say };

  const checks = new Map();
  for (const c of checksumFiles) {
    const dest = path.join(meta, `checksum-${c.tag}.txt`);
    if (!(fs.existsSync(dest) && (await sha256File(dest)) === c.sha256)) {
      await download(checksumUrlFor(c.tag), dest, opts); // download 会建好目录，并在下完后替换掉旧文件
      const got = await sha256File(dest);
      if (got !== c.sha256) {
        fs.renameSync(dest, `${dest}.BAD`);
        throw new Error(`checksum.txt（${c.tag}）的摘要 ${got} 与写死的（GitHub API 给的）${c.sha256} 不一致：上游可能更新了这个文件，核对清楚后再更新 CHECKSUM_FILES`);
      }
    }
    checks.set(c.tag, parseChecksums(fs.readFileSync(dest, 'utf8')));
    say(`checksum.txt（${c.tag}）摘要与 API 一致，共 ${checks.get(c.tag).size} 条`);
  }

  const manifestPath = path.join(home, 'fetch-manifest.json');
  const manifest = fs.existsSync(manifestPath) ? readJson(manifestPath) : {};
  const failed = [];
  for (const item of items) {
    const dest = path.join(downloads, item.name);
    const { expected, source, official } = expectedDigest(item, checks);
    let sha;
    if (fs.existsSync(dest)) {
      sha = await sha256File(dest);
      if (expected === undefined || sha === expected) {
        say(`已有：${item.name}  ${sha.slice(0, 12)}…`);
      } else {
        say(`已有但摘要不符，重下：${item.name}`);
        sha = undefined;
      }
    }
    if (sha === undefined) {
      say(`开始：${item.name}`);
      const t0 = Date.now();
      await download(item.url, dest, opts);
      sha = await sha256File(dest);
      if (expected !== undefined && sha !== expected) {
        fs.renameSync(dest, `${dest}.BAD`);
        say(`！摘要不符：${item.name}  期望 ${expected}  实际 ${sha}（已改名为 .BAD，不会使用）`);
        failed.push({ name: item.name, expected, actual: sha });
        continue;
      }
      say(`完成：${item.name}  ${(fs.statSync(dest).size / 1e6).toFixed(1)} MB  ${((Date.now() - t0) / 1000).toFixed(0)} s  sha256=${sha}  （${source}${expected === undefined ? '' : '：一致'}）`);
    }
    manifest[item.name] = { url: item.url, bytes: fs.statSync(dest).size, sha256: sha, source, official, verified: expected !== undefined };
    writeJsonAtomic(manifestPath, manifest);
  }
  return { manifest, failed };
}
