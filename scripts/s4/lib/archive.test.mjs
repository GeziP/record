import assert from 'node:assert/strict';
import test from 'node:test';
import { outputLines, unsafeEntries } from './archive.mjs';

test('tar 输出按行切：CRLF 和 LF 都行，去掉末尾的空行，中间的空行保留，空输出没有行', () => {
  assert.deepEqual(outputLines('a/\r\na/b\r\n'), ['a/', 'a/b']);
  assert.deepEqual(outputLines('a\nb\n'), ['a', 'b']);
  assert.deepEqual(outputLines('a\nb'), ['a', 'b']);
  assert.deepEqual(outputLines('a\n\nb\n'), ['a', '', 'b']);
  assert.deepEqual(outputLines('a\n\n'), ['a', '']);
  assert.deepEqual(outputLines(''), []);
  assert.deepEqual(outputLines('\r\n'), ['']);
});

const file = (name) => `-rw-r--r--  0 0      0      1234 Oct  7  2025 ${name}`;
const dir = (name) => `drwxr-xr-x  0 0      0          0 Oct  7  2025 ${name}`;
const check = (names) => unsafeEntries(names, names.map(file));

test('正常的压缩包：目录、文件、名字里带点的都没问题', () => {
  const names = ['model/', 'model/a.onnx', 'model/test_wavs/0.wav', 'a..b', '..a/x', 'x/b..', 'x/..b/y', 'x/.../y', 'x/.hidden'];
  assert.deepEqual(unsafeEntries(names, ['model/', ...names.slice(1)].map((n, i) => (i === 0 ? dir(n) : file(n)))), []);
  assert.deepEqual(unsafeEntries([], []), []);
});

test('绝对路径（含 UNC）要拦住', () => {
  assert.deepEqual(check(['/etc/passwd']), ['/etc/passwd：绝对路径']);
  assert.deepEqual(check(['\\\\server\\share\\x']), ['\\\\server\\share\\x：绝对路径']);
  assert.deepEqual(check(['\\x']), ['\\x：绝对路径']);
});

test('盘符与带冒号的文件名要拦住（冒号后可能是备用数据流）', () => {
  assert.deepEqual(check(['C:\\x']), ['C:\\x：文件名里有冒号（盘符或备用数据流）']);
  assert.deepEqual(check(['c:x']), ['c:x：文件名里有冒号（盘符或备用数据流）']);
  assert.deepEqual(check(['a/b.txt:stream']), ['a/b.txt:stream：文件名里有冒号（盘符或备用数据流）']);
});

test('路径里的 .. 段要拦住，正反斜杠都算；名字里只是带两个点的不算', () => {
  assert.deepEqual(check(['../x']), ['../x：路径里有 ..']);
  assert.deepEqual(check(['a/../b']), ['a/../b：路径里有 ..']);
  assert.deepEqual(check(['a\\..\\b']), ['a\\..\\b：路径里有 ..']);
  assert.deepEqual(check(['a/..']), ['a/..：路径里有 ..']);
  assert.deepEqual(check(['a//../b']), ['a//../b：路径里有 ..']);
  assert.deepEqual(check(['..']), ['..：路径里有 ..']);
});

test('符号链接和硬链接要拦住，普通文件名里出现 l、h 开头不算', () => {
  const links = ['lrwxrwxrwx  0 0      0          0 Oct  7  2025 evil -> ../../x', 'hrw-r--r--  0 0      0          0 Oct  7  2025 hard link to a'];
  assert.deepEqual(unsafeEntries(['evil', 'hard'], links), [`${links[0]}：链接`, `${links[1]}：链接`]);
  assert.deepEqual(unsafeEntries(['link', 'hello'], [file('link'), file('hello')]), []);
  assert.deepEqual(unsafeEntries(['d'], [dir('lib')]), []);
});

test('管道、设备、套接字这类特殊文件也要拦住；不认识的类型字符和空行同样不放行（只放行普通文件和目录）', () => {
  for (const t of ['p', 'c', 'b', 's', '?', ' ']) {
    const line = `${t}rw-r--r--  0 0      0          0 Oct  7  2025 x`;
    assert.deepEqual(unsafeEntries(['x'], [line]), [`${line}：不是普通文件或目录`], `类型字符 ${JSON.stringify(t)}`);
  }
  assert.deepEqual(unsafeEntries(['x'], ['']), ['：不是普通文件或目录']);
});

test('两份清单行数对不上就不能证明检查到了每一条，要报出来', () => {
  assert.deepEqual(unsafeEntries(['a', 'b'], [file('a')]), ['条目名清单有 2 行，详细清单有 1 行，对不上，没法逐条检查']);
  assert.deepEqual(unsafeEntries(['a'], [file('a'), file('b')]), ['条目名清单有 1 行，详细清单有 2 行，对不上，没法逐条检查']);
});

test('多个问题按出现顺序全部列出：先名字后链接', () => {
  const names = ['ok', '../a', '/b', 'c'];
  const verbose = [file('ok'), file('../a'), file('/b'), 'lrwxrwxrwx 0 0 0 0 Oct 7 2025 c -> d'];
  assert.deepEqual(unsafeEntries(names, verbose), ['../a：路径里有 ..', '/b：绝对路径', 'lrwxrwxrwx 0 0 0 0 Oct 7 2025 c -> d：链接']);
});
