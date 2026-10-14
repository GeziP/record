import test from 'node:test';
import assert from 'node:assert/strict';
import { parseTextGrid } from './textgrid.mjs';

const LONG = `File type = "ooTextFile"
Object class = "TextGrid"

xmin = 0
xmax = 12.5
tiers? <exists>
size = 2
item []:
    item [1]:
        class = "IntervalTier"
        name = "SPK1"
        xmin = 0
        xmax = 12.5
        intervals: size = 3
        intervals [1]:
            xmin = 0
            xmax = 2.5
            text = ""
        intervals [2]:
            xmin = 2.5
            xmax = 6
            text = "今天我们开会"
        intervals [3]:
            xmin = 6
            xmax = 12.5
            text = ""
    item [2]:
        class = "IntervalTier"
        name = "SPK2"
        xmin = 0
        xmax = 12.5
        intervals: size = 1
        intervals [1]:
            xmin = 0
            xmax = 12.5
            text = "他说 ""好"" 的"
`;

const SHORT = `File type = "ooTextFile"
Object class = "TextGrid"

0
12.5
<exists>
1
"IntervalTier"
"SPK9"
0
12.5
2
0
5
"一二三"
5
12.5
""
`;

test('长格式：两层标注，空段、中文、引号都读对', () => {
  const g = parseTextGrid(LONG);
  assert.equal(g.xmax, 12.5);
  assert.deepEqual(g.tiers.map((t) => t.name), ['SPK1', 'SPK2']);
  assert.deepEqual(g.tiers[0].intervals, [
    { xmin: 0, xmax: 2.5, text: '' },
    { xmin: 2.5, xmax: 6, text: '今天我们开会' },
    { xmin: 6, xmax: 12.5, text: '' },
  ]);
  assert.equal(g.tiers[1].intervals[0].text, '他说 "好" 的');
});

test('短格式读出同样的结构', () => {
  const g = parseTextGrid(SHORT);
  assert.deepEqual(g.tiers, [{ name: 'SPK9', intervals: [{ xmin: 0, xmax: 5, text: '一二三' }, { xmin: 5, xmax: 12.5, text: '' }] }]);
});

test('科学计数法写的时间也能读', () => {
  const g = parseTextGrid(SHORT.replace('\n0\n5\n"一二三"', '\n1.5e-05\n5E0\n"一二三"').replace('12.5\n<exists>', '1.25e+01\n<exists>'));
  assert.equal(g.xmax, 12.5);
  assert.deepEqual(g.tiers[0].intervals[0], { xmin: 0.000015, xmax: 5, text: '一二三' });
});

test('带 BOM 与 CRLF 也能读', () => {
  const g = parseTextGrid('\uFEFF' + LONG.replace(/\n/g, '\r\n'));
  assert.equal(g.tiers.length, 2);
});

test('文件头不对、层类型不支持、内容被截断都明确报错', () => {
  assert.throws(() => parseTextGrid('hello'), /文件头不对/);
  assert.throws(() => parseTextGrid(LONG.replace('IntervalTier', 'TextTier')), /只支持 IntervalTier/);
  assert.throws(() => parseTextGrid(LONG.slice(0, LONG.length - 60)), /提前结束|没有结束的引号/);
});
