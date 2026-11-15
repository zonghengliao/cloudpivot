/**
 * 报表项 ↑↓ 排序的纯逻辑单测。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 `report-item-logic.ts` 而不测 `.vue`：这个 bug 的全部内容就是**下标换算**，
 *  `.vue` 里只看得见 `move(rowIndex, ±1)` 和 `splice(rows.value)`，两处单看都对
 *  （表格给的是可见行下标，splice 用的是全表下标），错在两者相遇的地方；
 *  `vue-tsc` 抓不到（编译期都是 number），而且后端收的是整表全集，不会报错。
 *
 * 钉住四条口径（卡面验收 2）：
 * ① 无筛选（visibleRows === rows）时与旧行为逐格一致；
 * ② 有筛选且可见行不是整表前 N 条时，移动的是**被点的那一行**；
 * ③ 首行 ↑ / 末行 ↓ 无操作（`null`，不发请求）；
 * ④ 提交给后端的 id 序列恒为整表全集（长度 = rows.length、无重复、无丢行）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import {
  canMoveVisible,
  fullIndexOf,
  moveVisibleRow,
  sortPayloadIds,
  type SortableRow,
} from '@/views/business/erp/report/report-item/report-item-logic';

interface Row extends SortableRow {
  id: string;
  item_name: string;
}

function rows(...codes: string[]): Row[] {
  return codes.map((code) => ({ id: code, item_name: `项${code}` }));
}

/** 旧实现（bug 现场）：拿**表格给的可见行下标**去 splice 全表。仅作反证/一致性基准。 */
function legacyMove<T extends SortableRow>(rows: readonly T[], index: number, delta: number): T[] {
  const target = index + delta;
  if (target < 0 || target >= rows.length) {
    return rows.slice();
  }
  const next = rows.slice();
  const [moved] = next.splice(index, 1);
  next.splice(target, 0, moved);
  return next;
}

const codes = (list: readonly Row[]): string[] => list.map((row) => row.id);

// ==================== ① 无筛选：与旧行为一致 ====================

test('无筛选（visibleRows === rows）：任意位置任意方向上移/下移都与旧实现逐格一致', () => {
  const base = rows('A', 'B', 'C', 'D', 'E');
  for (let index = 0; index < base.length; index += 1) {
    for (const delta of [-1, 1] as const) {
      const result = moveVisibleRow(base, base, base[index].id, delta);
      const legacy = legacyMove(base, index, delta);
      const expectedNull = index + delta < 0 || index + delta >= base.length;
      if (expectedNull) {
        assert.equal(result, null, `无筛选 ${codes(base)[index]} ${delta > 0 ? '↓' : '↑'} 应无操作`);
        assert.deepEqual(codes(legacy), codes(base));
      } else {
        assert.notEqual(result, null);
        assert.deepEqual(codes(result!.next), codes(legacy), `${codes(base)[index]} ${delta > 0 ? '↓' : '↑'} 与旧行为一致`);
        assert.equal(result!.moved.id, base[index].id);
      }
    }
  }
});

test('无筛选：B ↓ / C ↑ 的具体顺序（旧行为的锚点）', () => {
  const base = rows('A', 'B', 'C', 'D');
  assert.deepEqual(codes(moveVisibleRow(base, base, 'B', 1)!.next), ['A', 'C', 'B', 'D']);
  assert.deepEqual(codes(moveVisibleRow(base, base, 'C', -1)!.next), ['A', 'C', 'B', 'D']);
});

// ==================== ② 有筛选：移动的必须是被点的那一行 ====================

test('有筛选 [A,B,C,D] / 筛出 [B,D]：在 D 上点 ↑ 移的是 D（卡面算例）', () => {
  const full = rows('A', 'B', 'C', 'D');
  const visible = full.filter((row) => row.id === 'B' || row.id === 'D');

  const result = moveVisibleRow(full, visible, 'D', -1);
  assert.notEqual(result, null);
  // 期望：D 移到上一个可见行 B 前面，中间的 C 原地不动 → [A, D, B, C]
  assert.deepEqual(codes(result!.next), ['A', 'D', 'B', 'C']);
  assert.equal(result!.moved.id, 'D', '被移动的行是 D，不是 B');
  // 反证：旧实现（splice 可见下标 1）移除的是 B → [B, A, C, D]
  assert.deepEqual(codes(legacyMove(full, 1, -1)), ['B', 'A', 'C', 'D'], '旧实现确实移错行');
  assert.notDeepEqual(codes(result!.next), codes(legacyMove(full, 1, -1)));
});

test('有筛选 [A,B,C,D] / 筛出 [B,D]：在 B 上点 ↓ 移的是 B，落到下一个可见行 D 后面', () => {
  const full = rows('A', 'B', 'C', 'D');
  const visible = full.filter((row) => row.id === 'B' || row.id === 'D');

  const result = moveVisibleRow(full, visible, 'B', 1);
  assert.notEqual(result, null);
  assert.equal(result!.moved.id, 'B');
  // B 越过 C 和 D → [A, C, D, B]
  assert.deepEqual(codes(result!.next), ['A', 'C', 'D', 'B']);
  assert.deepEqual(codes(legacyMove(full, 0, 1)), ['B', 'A', 'C', 'D'], '旧实现动的是 A（可见下标 0 = 全表首行），B 原地没动');
});

test('有筛选且可见行跨着多条不可见行：↑↓ 只与相邻可见行换位，夹在中间的保持原相对顺序', () => {
  const full = rows('A', 'B', 'C', 'D', 'E', 'F', 'G');
  // 筛出 A / D / G（可见下标 0,1,2 —— 与全表下标 0,3,6 全不相同）
  const visible = full.filter((row) => row.id === 'A' || row.id === 'D' || row.id === 'G');

  // D ↑ → 移到 A 前：D 跨过 A 之前的空位（A 是首行，A 前面没有东西，故 D 到最前）
  assert.deepEqual(codes(moveVisibleRow(full, visible, 'D', -1)!.next), ['D', 'A', 'B', 'C', 'E', 'F', 'G']);
  // G ↑ → 移到 D 前：G 跨过 E、F 落到 D 原位置
  assert.deepEqual(codes(moveVisibleRow(full, visible, 'G', -1)!.next), ['A', 'B', 'C', 'G', 'D', 'E', 'F']);
  // A ↓ → 移到 D 后：A 跨过 B、C 落到 D 原位置
  assert.deepEqual(codes(moveVisibleRow(full, visible, 'A', 1)!.next), ['B', 'C', 'D', 'A', 'E', 'F', 'G']);
  // D ↓ → 移到 G 后：D 跨过 E、F 落到 G 原位置之后
  assert.deepEqual(codes(moveVisibleRow(full, visible, 'D', 1)!.next), ['A', 'B', 'C', 'E', 'F', 'G', 'D']);
});

test('可见行正好是整表前 N 条（与全表前缀重合）：等同于旧行为', () => {
  const full = rows('A', 'B', 'C', 'D');
  const visible = full.slice(0, 3); // A B C，A 可见下标 0 = 全表 0
  assert.deepEqual(codes(moveVisibleRow(full, visible, 'C', -1)!.next), ['A', 'C', 'B', 'D']);
  assert.deepEqual(codes(moveVisibleRow(full, visible, 'B', 1)!.next), ['A', 'C', 'B', 'D']);
});

// ==================== ③ 首行 ↑ / 末行 ↓ 无操作 ====================

test('首行 ↑ 无操作：无筛选与有筛选都返回 null（不发请求、不提示）', () => {
  const full = rows('A', 'B', 'C', 'D');
  assert.equal(moveVisibleRow(full, full, 'A', -1), null);
  const visible = full.filter((row) => row.id === 'B' || row.id === 'D');
  assert.equal(moveVisibleRow(full, visible, 'B', -1), null, 'B 是首个可见行 → ↑ 无操作');
  assert.equal(canMoveVisible(0, visible.length, -1), false);
});

test('末行 ↓ 无操作：无筛选与有筛选都返回 null', () => {
  const full = rows('A', 'B', 'C', 'D');
  assert.equal(moveVisibleRow(full, full, 'D', 1), null);
  const visible = full.filter((row) => row.id === 'B' || row.id === 'D');
  assert.equal(moveVisibleRow(full, visible, 'D', 1), null, 'D 是末个可见行 → ↓ 无操作');
  assert.equal(canMoveVisible(visible.length - 1, visible.length, 1), false);
});

test('可见行只有 1 条 / 空表 / 行不在表里：一律 null', () => {
  const full = rows('A', 'B');
  const single = full.filter((row) => row.id === 'B');
  assert.equal(moveVisibleRow(full, single, 'B', -1), null);
  assert.equal(moveVisibleRow(full, single, 'B', 1), null);
  assert.equal(moveVisibleRow([], [], 'A', 1), null);
  assert.equal(moveVisibleRow(full, full, 'NOPE', 1), null);
  assert.equal(moveVisibleRow(full, full, 'NOPE', -1), null);
  assert.equal(fullIndexOf(full, 'NOPE'), -1);
  assert.equal(canMoveVisible(-1, 2, 1), false);
});

// ==================== ④ 提交给后端的 id 序列恒为整表全集 ====================

test('任何移动结果提交的 id 序列都是整表全集：长度不变、无重复、无丢行', () => {
  const full = rows('A', 'B', 'C', 'D', 'E', 'F');
  const filters: Row[][] = [
    full,
    full.filter((row) => ['B', 'D'].includes(row.id)),
    full.filter((row) => ['A', 'C', 'E', 'F'].includes(row.id)),
    full.filter((row) => ['C', 'D', 'E'].includes(row.id)),
    full.filter((row) => row.id === 'E'),
  ];
  for (const visible of filters) {
    for (const row of visible) {
      for (const delta of [-1, 1] as const) {
        const result = moveVisibleRow(full, visible, row.id, delta);
        if (!result) {
          continue;
        }
        const ids = sortPayloadIds(result.next);
        assert.equal(ids.length, full.length, `${row.id}/${delta}：长度必须等于整表行数`);
        assert.equal(new Set(ids).size, full.length, `${row.id}/${delta}：不许重复`);
        assert.deepEqual([...ids].sort(), codes(full).sort(), `${row.id}/${delta}：不许丢行`);
        assert.deepEqual(sortPayloadIds(full), codes(full), '未移动时提交的仍是整表顺序');
      }
    }
  }
});

test('随机筛选 × 随机点击：每次只与相邻可见行换位，整表其余相对顺序不动', () => {
  // 确定性伪随机（最小标准 LCG，乘数取 16807 以免超出 double 精确整数范围），失败可复现
  let seed = 20261115;
  const rand = (n: number): number => {
    seed = (seed * 16807) % 2147483647;
    return seed % n;
  };

  for (let round = 0; round < 200; round += 1) {
    const size = 2 + rand(9);
    const full = rows(...Array.from({ length: size }, (_, i) => `R${i}`));
    const visible = full.filter(() => rand(3) > 0); // 约 2/3 可见
    if (visible.length === 0) {
      continue;
    }
    const clicked = visible[rand(visible.length)];
    const delta = rand(2) === 0 ? -1 : 1;
    const result = moveVisibleRow(full, visible, clicked.id, delta);
    const visibleIndex = visible.findIndex((row) => row.id === clicked.id);
    const movable = canMoveVisible(visibleIndex, visible.length, delta);

    assert.equal(result === null, !movable, `round ${round}：可移动性与 null 必须一致`);
    if (!result) {
      continue;
    }
    // 整表仍是同一批行
    assert.deepEqual([...codes(result.next)].sort(), codes(full).sort());
    // 被移动行落在目标可见行的正确一侧
    const nextVisible = visible.slice();
    const [movedVisible] = nextVisible.splice(visibleIndex, 1);
    nextVisible.splice(delta < 0 ? visibleIndex - 1 : visibleIndex + 1, 0, movedVisible);
    assert.deepEqual(
      codes(result.next).filter((id) => codes(visible).includes(id)),
      codes(nextVisible),
      `round ${round}：可见行之间的相对顺序必须是相邻交换`,
    );
    // 不可见行彼此之间的相对顺序不变
    assert.deepEqual(
      codes(result.next).filter((id) => !codes(visible).includes(id)),
      codes(full).filter((id) => !codes(visible).includes(id)),
      `round ${round}：不可见行不得被牵连`,
    );
  }
});

// ==================== ⑤ 接线（源码即契约）：下标不许再传回模板/组件 ====================
//
// 纯函数再对，只要模板又把 `rowIndex` 传进来（`move(rowIndex, ±1)`），bug 就原样复发：
// 可见行下标被当全表下标用。这条钉的就是「谁把 id 交给 move」和「点按钮能不能被定位」。

const VIEW = readFileSync(
  resolve(process.cwd(), 'src/views/business/erp/report/report-item/index.vue'),
  'utf8',
);

test('模板把「行 id」交给 move，不再传表格的可见行下标', () => {
  assert.match(VIEW, /@click="move\(row\.id, -1\)"/, '↑ 必须传 row.id');
  assert.match(VIEW, /@click="move\(row\.id, 1\)"/, '↓ 必须传 row.id');
  assert.doesNotMatch(VIEW, /@click="move\(rowIndex/, '传 rowIndex 就是 bug 复发');
});

test('move() 的换算走 report-item-logic，自己不再 splice 下标', () => {
  const start = VIEW.indexOf('async function move(');
  const end = VIEW.indexOf('// ==================== 表单');
  assert.ok(start > 0 && end > start, 'move() 必须还在（找不到说明被改名，这条测试要跟着改）');
  const body = VIEW.slice(start, end);
  assert.match(body, /moveVisibleRow\(rows\.value, visibleRows\.value, rowId, delta\)/);
  assert.match(body, /sortPayloadIds\(result\.next\)/);
  assert.doesNotMatch(body, /\.splice\(/, 'move() 里不许再出现按下标 splice 全表');
});

test('↑↓ 按钮的 data-testid 是绑定的（字面量 `ri-move-up-${…}` 会让 e2e 永远定位不到）', () => {
  assert.match(VIEW, /:data-testid="`ri-move-up-\$\{row\.item_code\}`"/);
  assert.match(VIEW, /:data-testid="`ri-move-down-\$\{row\.item_code\}`"/);
});
