/** tar 的输出按行切开（Windows 下换行是 CRLF），去掉末尾那个空行 */
export function outputLines(text) {
  const lines = text.split(/\r?\n/);
  if (lines.at(-1) === '') lines.pop();
  return lines;
}

// 解压前的安全检查（纯函数）：压缩包里有没有会写到目标目录之外的条目，或者除普通文件、目录之外的东西。
// names 是 `tar -tf` 的每行条目名；verboseLines 是 `tar -tvf` 的每行（第一个字符是类型：- 文件，d 目录，l 符号链接，h 硬链接，p 管道，c / b 设备，s 套接字）。
// 返回有问题的条目说明；空数组表示可以解压。
export function unsafeEntries(names, verboseLines) {
  const bad = [];
  if (names.length !== verboseLines.length) {
    bad.push(`条目名清单有 ${names.length} 行，详细清单有 ${verboseLines.length} 行，对不上，没法逐条检查`);
  }
  for (const name of names) {
    if (/^[\\/]/.test(name)) bad.push(`${name}：绝对路径`);
    // 盘符，也顺带挡住 Windows 的备用数据流（文件名里带冒号）
    else if (name.includes(':')) bad.push(`${name}：文件名里有冒号（盘符或备用数据流）`);
    else if (name.split(/[\\/]+/).includes('..')) bad.push(`${name}：路径里有 ..`);
  }
  for (const line of verboseLines) {
    if (/^[lh]/.test(line)) bad.push(`${line}：链接`);
    else if (!/^[-d]/.test(line)) bad.push(`${line}：不是普通文件或目录`);
  }
  return bad;
}
