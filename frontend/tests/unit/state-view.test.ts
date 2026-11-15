/**
 * 三态组件判据单测（P3-17）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 为什么这几个点非测不可
 *
 * 三态「画错也看不出是错的」——用户只会觉得「这页怪」，不会报 bug。所以断言全落在判据上：
 *
 * 1. **拼错 state 不能永久转圈**：`loading` 的拼写错误若被当成 loading，页面就永远停在骨架屏；
 * 2. **error 必须带得动重试**：不给文案也要有「重试」，否则用户只能自己再点一次菜单；
 *    但显式给空串（403 这类不可重试）必须一颗按钮都不画 —— `undefined` 与 `''` 语义不同；
 * 3. **error 不许用「暂无数据」那张图**：加载失败配空态插画，用户会以为「真的没数据」；
 * 4. **骨架屏行数要钳住**：传 999 会顶出滚动条，比不加载更吓人；
 * 5. **文案单行化**：后端 msg 带换行会把 t-empty 描述撑成报错块。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  STATE_VIEW_TEXT,
  errorMessageOf,
  oneLineText,
  resolveStateView,
  skeletonRowCol,
} from '../../src/components/StateView/state-view-pure.ts';

test('state 归一：非法值按 empty 画，绝不因为拼错永久转圈', () => {
  assert.equal(resolveStateView({ state: 'loading' }).kind, 'loading');
  assert.equal(resolveStateView({ state: 'empty' }).kind, 'empty');
  assert.equal(resolveStateView({ state: 'error' }).kind, 'error');
  // 拼错 / 大小写不对 / 空串 → empty
  assert.equal(resolveStateView({ state: 'loadng' }).kind, 'empty');
  assert.equal(resolveStateView({ state: 'LOADING' }).kind, 'empty');
  assert.equal(resolveStateView({ state: '' }).kind, 'empty');
});

test('loading：默认骨架屏 4 行，spinner 只有显式声明才用', () => {
  const skeleton = resolveStateView({ state: 'loading' });
  assert.equal(skeleton.variant, 'skeleton');
  assert.equal(skeleton.rows, 4);
  assert.deepEqual(skeleton.rowCol, [
    { width: '100%' },
    { width: '100%' },
    { width: '82%' },
    { width: '58%' },
  ]);
  assert.equal(skeleton.showAction, false);
  assert.equal(skeleton.title, STATE_VIEW_TEXT.loading);

  const spinner = resolveStateView({ state: 'loading', variant: 'spinner' });
  assert.equal(spinner.variant, 'spinner');

  // 非法 variant 回骨架屏（画得出来 > 画错）
  assert.equal(resolveStateView({ state: 'loading', variant: 'bounce' }).variant, 'skeleton');
});

test('loading：骨架屏每行必须是对象（t-skeleton 对字符串抛 Cannot use in operator）', () => {
  for (const rows of [1, 2, 3, 4, 7, 12, 999]) {
    for (const cell of resolveStateView({ state: 'loading', rows }).rowCol) {
      assert.equal(Object.prototype.toString.call(cell), '[object Object]', `rows=${rows} 的单元格必须是对象`);
      assert.equal(typeof cell.width, 'string');
      // 复刻 TDesign 的判据：`'width' in item`（对字符串会抛 TypeError）
      assert.equal('width' in cell, true);
    }
  }
});

test('loading：行数钳在 1..12（三行以下不收窄末行）', () => {
  assert.equal(resolveStateView({ state: 'loading', rows: 999 }).rows, 12);
  assert.equal(resolveStateView({ state: 'loading', rows: 0 }).rows, 1);
  assert.equal(resolveStateView({ state: 'loading', rows: -5 }).rows, 1);
  assert.equal(resolveStateView({ state: 'loading', rows: Number.NaN }).rows, 4);
  assert.deepEqual(skeletonRowCol(2), [{ width: '100%' }, { width: '100%' }]);
  assert.deepEqual(skeletonRowCol(3), [{ width: '100%' }, { width: '100%' }, { width: '58%' }]);
  assert.deepEqual(skeletonRowCol(1), [{ width: '100%' }]);
});

test('empty：默认「暂无数据」，不给 actionText 就不画按钮', () => {
  const empty = resolveStateView({ state: 'empty' });
  assert.equal(empty.title, STATE_VIEW_TEXT.emptyTitle);
  assert.equal(empty.description, '');
  assert.equal(empty.showAction, false);
  assert.equal(empty.actionText, '');
  assert.equal(empty.emptyType, 'empty');

  const withAction = resolveStateView({ state: 'empty', actionText: '新建' });
  assert.equal(withAction.showAction, true);
  assert.equal(withAction.actionText, '新建');
});

test('empty：插画只认内置 5 种，非法值回 empty 默认图', () => {
  assert.equal(resolveStateView({ state: 'empty', emptyType: 'success' }).emptyType, 'success');
  assert.equal(resolveStateView({ state: 'empty', emptyType: 'maintenance' }).emptyType, 'maintenance');
  assert.equal(resolveStateView({ state: 'empty', emptyType: 'trophy' }).emptyType, 'empty');
});

test('error：不给文案也有「重试」；显式空串才不画按钮（403 不可重试）', () => {
  const def = resolveStateView({ state: 'error' });
  assert.equal(def.title, STATE_VIEW_TEXT.errorTitle);
  assert.equal(def.description, STATE_VIEW_TEXT.errorDescription);
  assert.equal(def.actionText, STATE_VIEW_TEXT.retry);
  assert.equal(def.showAction, true);

  const custom = resolveStateView({ state: 'error', description: '采购单状态已变更', actionText: '重新加载' });
  assert.equal(custom.description, '采购单状态已变更');
  assert.equal(custom.actionText, '重新加载');

  // 显式 '' = 不可重试：一颗按钮都没有（与 undefined 的默认「重试」相反）
  const noRetry = resolveStateView({ state: 'error', actionText: '' });
  assert.equal(noRetry.actionText, '');
  assert.equal(noRetry.showAction, false);

  // 描述给了但全是空白 → 仍落兜底话术（不许渲染一条空描述）
  assert.equal(resolveStateView({ state: 'error', description: '   ' }).description, STATE_VIEW_TEXT.errorDescription);
});

test('error：插画固定 network-error，调用方传 empty/fail 也不许配「暂无数据」的图', () => {
  assert.equal(resolveStateView({ state: 'error' }).emptyType, 'network-error');
  assert.equal(resolveStateView({ state: 'error', emptyType: 'empty' }).emptyType, 'network-error');
  assert.equal(resolveStateView({ state: 'error', emptyType: 'success' }).emptyType, 'network-error');
  // 明确表达「不是网络问题」的两种图可以换
  assert.equal(resolveStateView({ state: 'error', emptyType: 'fail' }).emptyType, 'fail');
  assert.equal(resolveStateView({ state: 'error', emptyType: 'maintenance' }).emptyType, 'maintenance');
});

test('文案单行化：折叠换行 / 去首尾 / 超 240 字截断加省略号', () => {
  assert.equal(oneLineText('  加载\n失败   请重试 '), '加载 失败 请重试');
  assert.equal(oneLineText(undefined), '');
  assert.equal(oneLineText(null), '');
  assert.equal(oneLineText(42), '42');

  const long = 'x'.repeat(300);
  const clipped = oneLineText(long);
  assert.equal(clipped.length, 240);
  assert.equal(clipped.endsWith('…'), true);

  // error 态也要走同一条口径（后端 msg 带换行不会撑爆描述）
  assert.equal(resolveStateView({ state: 'error', description: 'A\n\nB' }).description, 'A B');
});

test('errorMessageOf：认 string / Error / 后端 {code,msg} 三种形状，取不到给空串', () => {
  assert.equal(errorMessageOf('网络错误'), '网络错误');
  assert.equal(errorMessageOf(new Error('请求超时')), '请求超时');
  assert.equal(errorMessageOf({ code: 40001, msg: '会话已过期' }), '会话已过期');
  assert.equal(errorMessageOf({ code: 50000, message: '服务器内部错误' }), '服务器内部错误');
  assert.equal(errorMessageOf(null), '');
  assert.equal(errorMessageOf(undefined), '');
  // 空串 → error 态落到兜底描述（界面不会出现 "undefined" / "[object Object]"）
  assert.equal(resolveStateView({ state: 'error', description: errorMessageOf({}) }).description, STATE_VIEW_TEXT.errorDescription);
});
