/**
 * `tree-layout.ts` 单测（T8-B）：树形表格的「行样式 / 子任务进度 / 箭头列下标」三件事。
 *
 * 这三件事都是卡面验收的硬指标（箭头必须在标题列、父行/子行视觉分家、父行进度列显示 N/M），
 * 但它们本来是写在 `index.vue` 模板与 `rowClassName` 里的，`vue-tsc` 与 e2e 都覆盖不到细节
 * （e2e 只能看到「某个页面长什么样」，看不到「列顺序变了箭头会不会跟着走」）。
 *
 * 运行：`pnpm test:unit`（内部执行 `tsx --test tests/unit` 下的全部 `*.test.ts`）
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  ROW_CHILD,
  ROW_PARENT,
  TREE_INDENT,
  childProgressPercent,
  childProgressText,
  isParentRow,
  rowClassNameOf,
  titleColumnIndex,
  treeConfigOf,
} from '../../src/views/business/task/components/tree-layout.ts';

/** 列表页 `columns` 的真实 colKey 顺序（改了列顺序要同步这里，正是本文件要守的契约） */
const COLUMN_KEYS = [
  'task_no',
  'title',
  'status',
  'priority',
  'progress',
  'owner_name',
  'project_name',
  'due_date',
  'source',
  'op',
];

test('行样式：父任务 → task-row--parent，普通行 → 空串', () => {
  assert.equal(rowClassNameOf({ child_count: 3 }), ROW_PARENT);
  assert.equal(rowClassNameOf({ child_count: 0 }), '');
  assert.equal(rowClassNameOf({}), '');
});

test('行样式：子行优先——孙任务（_isSub 且自己有子任务）仍按子行渲染，不挂 4px 竖条', () => {
  assert.equal(rowClassNameOf({ _isSub: true, child_count: 2 }), ROW_CHILD);
  assert.equal(rowClassNameOf({ _isSub: true, child_count: 0 }), ROW_CHILD);
  assert.notEqual(rowClassNameOf({ _isSub: true, child_count: 2 }), ROW_PARENT);
});

test('行样式：脏数据（child_count 为 undefined / 负数）不判成父任务', () => {
  assert.equal(isParentRow({ child_count: undefined }), false);
  assert.equal(isParentRow({ child_count: -1 }), false);
  assert.equal(isParentRow(null), false);
  assert.equal(rowClassNameOf(undefined), '');
});

test('子任务进度百分比：1/3 取整 33、2/3 取整 67、3/3 = 100', () => {
  assert.equal(childProgressPercent(1, 3), 33);
  assert.equal(childProgressPercent(2, 3), 67);
  assert.equal(childProgressPercent(3, 3), 100);
  assert.equal(childProgressPercent(0, 3), 0);
});

test('子任务进度百分比：分母为 0 / undefined / NaN 一律 0（不出现 NaN% 或 Infinity）', () => {
  assert.equal(childProgressPercent(0, 0), 0);
  assert.equal(childProgressPercent(5, 0), 0);
  assert.equal(childProgressPercent(undefined, undefined), 0);
  assert.equal(childProgressPercent(1, Number.NaN), 0);
  assert.ok(Number.isFinite(childProgressPercent(1, Number.POSITIVE_INFINITY)));
});

test('子任务进度百分比：done 越界先夹紧（并发下架子任务时不会出现 -50% / 150%）', () => {
  assert.equal(childProgressPercent(-2, 4), 0);
  assert.equal(childProgressPercent(9, 4), 100);
  assert.equal(childProgressPercent(Number.NaN, 4), 0);
});

test('子任务分数文案：N/M 与百分比同源夹紧；分母 0 显示 0/0', () => {
  assert.equal(childProgressText(1, 3), '1/3');
  assert.equal(childProgressText(9, 3), '3/3');
  assert.equal(childProgressText(-1, 3), '0/3');
  assert.equal(childProgressText(undefined, undefined), '0/0');
});

test('箭头列下标：标题列 = 1（E6b 的 0 是任务号列，这是本卡的核心修复）', () => {
  assert.equal(titleColumnIndex(COLUMN_KEYS), 1);
  assert.equal(COLUMN_KEYS[titleColumnIndex(COLUMN_KEYS)], 'title');
});

test('箭头列下标：在任务号前插一列后，箭头仍然跟着标题列走（不写死数字的理由）', () => {
  const withCreator = ['avatar', ...COLUMN_KEYS];
  assert.equal(titleColumnIndex(withCreator), 2);
  assert.equal(withCreator[titleColumnIndex(withCreator)], 'title');
});

test('箭头列下标：找不到标题列时兜底 0（不让树形彻底哑掉）', () => {
  assert.equal(titleColumnIndex(['task_no', 'name']), 0);
  assert.equal(titleColumnIndex([]), 0);
});

test('树形配置：treeNodeColumnIndex 对准标题列、缩进 24px', () => {
  const config = treeConfigOf(COLUMN_KEYS);
  assert.equal(config.treeNodeColumnIndex, 1);
  assert.equal(config.indent, TREE_INDENT);
  assert.equal(TREE_INDENT, 24);
});
