/**
 * ③「付款提交后详情抽屉自动刷新」的源码契约测（t_27ef744a 回归护栏）。
 *
 * # 为什么要测源码
 *
 * 这个缺陷的形态是「**写接口回包当唯一数据源**」：`submitPay()` 成功后把
 * `PaySubmitRes.detail`（写事务提交前后的那份快照）直接塞进抽屉，于是抽屉停在
 * 「付款记录 0 笔 / 旧未付」，必须关掉重开才更新（手册 12-付款.md 的复现截图）。
 * 它的修法不是某个纯函数，而是**一条调用链**：
 *
 *   submitPay() ──> refreshDetail(id) ──> GET /payable/{id}（读接口才是权威）──> loadPayInfo(id)
 *
 * 纯逻辑测（`d5-payable-to-payment.test.ts`）碰不到这条链，删掉 `refreshDetail`
 * 调用也照样全绿；所以这里按本仓既有「源码即契约」姿势（见 c2-approval-unified.test.ts）
 * 直接断言 SFC 源码，防止「刷了个新快照又只信回包」的退化。
 *
 * 断言只做子串 / 正则，不做 AST。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

const ROOT = resolve(process.cwd());
const PAGE = 'src/views/business/erp/payable/index.vue';

function sourceOf(relative: string): string {
  return readFileSync(join(ROOT, relative), 'utf8');
}

/** 截出某个顶层函数的函数体（从 `function <name>` 到下一个顶层 `\n}`），找不到就 fail */
function bodyOf(source: string, name: string): string {
  const start = source.indexOf(`async function ${name}(`) >= 0
    ? source.indexOf(`async function ${name}(`)
    : source.indexOf(`function ${name}(`);
  assert.notEqual(start, -1, `源码里找不到函数 ${name}()`);
  const end = source.indexOf('\n}\n', start);
  assert.notEqual(end, -1, `函数 ${name}() 的闭合大括号找不到`);
  return source.slice(start, end + 3);
}

test('③ 付款成功后必须走 refreshDetail(id)，不能只把写回包的 detail 塞给抽屉', () => {
  const page = sourceOf(PAGE);
  const submitPay = bodyOf(page, 'submitPay');

  assert.match(
    submitPay,
    /await refreshDetail\(/,
    'submitPay() 成功后必须 await refreshDetail(...)：只写 detail.value = result.detail 会让抽屉停在提交前快照',
  );
  assert.match(
    submitPay,
    /refreshDetail\(\s*target\.id/,
    'refreshDetail 必须用当前应付单 id 去读接口重取（不是拿 result.detail 再赋值一遍）',
  );
  // 回包里的 detail 可以「先落一下」给即时反馈，但不能是唯一动作：
  // 只要出现 refreshDetail，上面两条就保证了权威数据来自读接口。
});

test('③ refreshDetail() 从读接口 fetchPayableDetail(id) 取权威快照，并重算可付信息', () => {
  const refreshDetail = bodyOf(sourceOf(PAGE), 'refreshDetail');

  assert.match(refreshDetail, /fetchPayableDetail\(/, 'refreshDetail 必须调读接口 fetchPayableDetail(id)');
  assert.match(refreshDetail, /detail\.value\s*=\s*fresh/, '读接口结果要落进 detail（抽屉数据源）');
  assert.match(refreshDetail, /loadPayInfo\(/, '未付变了 ⇒ 可付信息（按钮文案 / 能否再付）必须一起重取');
  assert.match(refreshDetail, /refreshApprovalProgress\(/, '审批进度也要跟着重取（付款可能是审批中的那笔）');
});

test('③ 刷新失败不能让抽屉静默停在旧数据（必须告警）', () => {
  const refreshDetail = bodyOf(sourceOf(PAGE), 'refreshDetail');
  assert.match(
    refreshDetail,
    /MessagePlugin\.warning|MessagePlugin\.error/,
    '读接口失败时要明确告诉用户「没刷新成功」，否则用户会以为付款没登记上',
  );
});
