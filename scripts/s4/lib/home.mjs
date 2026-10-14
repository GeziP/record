// S4 工作目录（放在仓库之外，用 --home 或环境变量 S4_HOME 指定，没有默认值）的约定：<home>\{downloads,tools,models,data,work,results}。
import fs from 'node:fs';
import path from 'node:path';

/** 从命令行取到的 --home 或环境变量 S4_HOME 里取目录；都没有就报错，不猜 */
export function homeFrom(optionValue, env = process.env) {
  const h = optionValue || env.S4_HOME;
  if (!h) throw new Error('没有给 S4 目录：用 --home <目录> 或设置环境变量 S4_HOME');
  return h;
}

/** 目录约定与 sherpa-onnx 各程序的路径。tools 下必须恰有一份解压好的官方程序。 */
export function s4Paths(home) {
  const tools = path.join(home, 'tools');
  const bins = fs.existsSync(tools)
    ? fs
        .readdirSync(tools)
        .map((d) => path.join(tools, d, 'bin'))
        .filter((b) => fs.existsSync(path.join(b, 'sherpa-onnx-offline.exe')))
    : [];
  if (bins.length !== 1) throw new Error(`${tools} 下应当恰有一份 sherpa-onnx 官方程序，找到 ${bins.length} 份`);
  return {
    home,
    downloads: path.join(home, 'downloads'),
    models: path.join(home, 'models'),
    dataset: path.join(home, 'data', 'Eval_Ali'),
    work: path.join(home, 'work'),
    results: path.join(home, 'results'),
    exe: (name) => path.join(bins[0], `${name}.exe`),
  };
}

/** 先写临时文件再改名，中途被打断不会留下半截的结果文件 */
export function writeJsonAtomic(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const tmp = `${file}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(value, null, 1));
  fs.renameSync(tmp, file);
}

export function readJson(file) {
  return JSON.parse(fs.readFileSync(file, 'utf8'));
}
