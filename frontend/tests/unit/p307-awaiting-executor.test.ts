/**
 * P3-07 前端配套：「审批通过但未执行」的付款 → 「执行付款」按钮。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * # 为什么这些判据必须单测
 *
 * 后端（`payable_payment.rs::execute_approved_payment` + `payable_bridge.rs`）在
 * `erp.separation_of_duties.approve_execute` 打开时把付款留在 `PENDING` 且**账一分没动**，
 * 需要另一位有 `business:erp:payable:pay` 的人点一下。前端只要把「什么时候该显示按钮」
 * 判错，后果是两种：
 *
 * * 假阳性 → 用户在「钱已经付掉了」的记录上看到按钮，点下去 409；
 * * 假阴性 → 钱永远卡在「审批中」，没人知道该点哪里（卡面 §8「已知边界」就是这条）。
 *
 * 判据分两源（后端 `awaiting_executor` 优先、老后端退回实例状态），两个分支都要钉住。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

import {
  executePaymentConfirmText,
  executorProbeInstanceId,
  isAwaitingExecutor,
  type ExecutorGatePayment,
} from '@/views/business/erp/payable/payable-logic';

const ROOT = resolve(process.cwd());

/** 读一页源码做契约断言（与既有源码契约测同风格：只做子串 / 正则） */
function sourceOf(relative: string): string {
  return readFileSync(join(ROOT, relative), 'utf8');
}

/** 造一条付款记录（默认「审批中 + 有实例」= 开关打开后审批通过那一刻的样子） */
function payment(overrides: Partial<ExecutorGatePayment> = {}): ExecutorGatePayment {
  return {
    approval_status: 'PENDING',
    approval_instance_id: '11111111-1111-4111-8111-111111111111',
    ...overrides,
  };
}

// ---------- 兜底分支：付款 PENDING + 实例 APPROVED ----------

test('p307 付款审批中 + 实例 APPROVED ⇒ 需要有人执行（按钮出现）', () => {
  assert.equal(isAwaitingExecutor(payment(), 'APPROVED'), true);
});

test('p307 付款审批中 + 实例仍 RUNNING ⇒ 只是在等审批，不给执行按钮', () => {
  assert.equal(isAwaitingExecutor(payment(), 'RUNNING'), false);
  assert.equal(isAwaitingExecutor(payment(), 'PENDING'), false);
  assert.equal(isAwaitingExecutor(payment(), null), false);
  assert.equal(isAwaitingExecutor(payment(), ''), false);
});

test('p307 实例状态大小写 / 前后空格不影响判定（后端 status 是文本列）', () => {
  assert.equal(isAwaitingExecutor(payment(), ' approved '), true);
});

test('p307 付款已是 APPROVED ⇒ 账已经记过（开关关闭 / 已执行），不给按钮', () => {
  // 开关关闭时审批通过即自动记账：付款直接是 APPROVED，实例也是 APPROVED —— 这里不能误判
  assert.equal(isAwaitingExecutor(payment({ approval_status: 'APPROVED' }), 'APPROVED'), false);
  assert.equal(isAwaitingExecutor(payment({ approval_status: 'CANCELLED' }), 'APPROVED'), false);
  assert.equal(isAwaitingExecutor(payment({ approval_status: 'REJECTED' }), 'APPROVED'), false);
  assert.equal(isAwaitingExecutor(payment({ approval_status: 'NONE' }), 'APPROVED'), false);
});

test('p307 付款没有审批实例（直付路径）⇒ 不存在「批过再执行」这一步', () => {
  assert.equal(isAwaitingExecutor(payment({ approval_instance_id: null }), 'APPROVED'), true);
  // 实例 id 不参与兜底判定（判据是「付款 PENDING 且实例 APPROVED」），但没实例时按钮点下去
  // 必然 409 —— 所以探针函数（见下）不会去查，页面也就拿不到 APPROVED 状态。
  assert.equal(executorProbeInstanceId([payment({ approval_instance_id: null })]), null);
});

test('p307 空值安全（模板在详情还没加载时照样求值）', () => {
  assert.equal(isAwaitingExecutor(null, 'APPROVED'), false);
  assert.equal(isAwaitingExecutor(undefined, 'APPROVED'), false);
});

// ---------- 后端权威字段优先 ----------

test('p307 后端下发 awaiting_executor=true ⇒ 直接显示（不依赖实例状态）', () => {
  assert.equal(isAwaitingExecutor(payment({ awaiting_executor: true }), null), true);
  assert.equal(isAwaitingExecutor(payment({ awaiting_executor: true, approval_status: 'PENDING' }), 'RUNNING'), true);
});

test('p307 后端下发 awaiting_executor=false ⇒ 不显示，哪怕实例恰好是 APPROVED', () => {
  // 后端的判据里有「开关状态 + 审批票数」两件前端看不到的事实，冲突时以后端为准
  assert.equal(isAwaitingExecutor(payment({ awaiting_executor: false }), 'APPROVED'), false);
});

// ---------- 探针：什么时候才多发那一次「实例详情」请求 ----------

test('p307 只有「付款 PENDING + 有实例 + 后端没给字段」才探实例状态', () => {
  assert.equal(executorProbeInstanceId([payment()]), payment().approval_instance_id);
});

test('p307 后端已下发 awaiting_executor ⇒ 一次都不探（老前端多发请求的坑）', () => {
  assert.equal(executorProbeInstanceId([payment({ awaiting_executor: true })]), null);
  assert.equal(executorProbeInstanceId([payment({ awaiting_executor: false })]), null);
});

test('p307 没有审批中的付款 / 空列表 ⇒ 不探', () => {
  assert.equal(executorProbeInstanceId([]), null);
  assert.equal(executorProbeInstanceId(null), null);
  assert.equal(executorProbeInstanceId([payment({ approval_status: 'APPROVED' })]), null);
});

test('p307 多笔付款里挑「审批中那笔」的实例（部分付款场景）', () => {
  const list: ExecutorGatePayment[] = [
    payment({ approval_status: 'APPROVED', approval_instance_id: 'aaaaaaaa-1111-4111-8111-111111111111' }),
    payment({ approval_instance_id: 'bbbbbbbb-2222-4222-8222-222222222222' }),
  ];
  assert.equal(executorProbeInstanceId(list), 'bbbbbbbb-2222-4222-8222-222222222222');
});

// ---------- 二次确认文案 ----------

test('p307 确认文案必须讲清「还没付 / 立即记账 / 审批人不能执行」', () => {
  const text = executePaymentConfirmText('PM-20261008-000001', '¥1,130.00');
  assert.match(text, /PM-20261008-000001/);
  assert.match(text, /1,130\.00/);
  assert.match(text, /还没付/);
  assert.match(text, /应付回写/);
  assert.match(text, /付款凭证/);
  assert.match(text, /审批.{0,6}执行.{0,6}分离/);
});

// ---------- 源码契约：按钮 / 权限码 / 接口路径 ----------

const PAGE = 'src/views/business/erp/payable/index.vue';
const API = 'src/views/business/erp/payable/payable-write-api.ts';

test('p307 页面：按钮挂在付款记录行内，且带 business:erp:payable:pay 权益门禁', () => {
  const page = sourceOf(PAGE);
  // 卡面 §1 的硬要求：`v-privilege="['business:erp:payable:pay']"`（这里用同值常量）
  assert.match(page, /v-privilege="\[PAY_PERMISSION\]"/);
  assert.match(page, /const PAY_PERMISSION = 'business:erp:payable:pay';/);
  // 按钮文案与 testid（e2e 用 testid 定位）
  assert.match(page, /payable-payment-execute-\$\{item\.payment_no\}/);
  assert.match(page, />\s*执行付款\s*</);
  // 只在「审批通过但未执行」时出现，判据走纯函数
  assert.match(page, /v-if="isAwaitingExecutor\(item, instanceStatusOf\(item\)\)"/);
});

test('p307 页面：失败原样弹后端 msg（409+66013 中文明文不许改写）', () => {
  const page = sourceOf(PAGE);
  const block = page.slice(page.indexOf('async function doExecutePayment'), page.indexOf('// ==================== 付款审批进度'));
  assert.ok(block.length > 0, 'doExecutePayment 必须存在');
  // 错误分支必须是 `messageOf(error)` 直出，不能拼自己的话
  assert.match(block, /catch \(error\) \{\s*MessagePlugin\.error\(messageOf\(error\)\);/);
  // 成功提示用后端 data.message
  assert.match(block, /MessagePlugin\.success\(result\.message\);/);
  // 成功后必须重取详情（不信写接口回包）
  assert.match(block, /await refreshDetail\(payableId\)/);
});

test('p307 API：路径逐字对齐后端注解（/business/erp/payable/payment/{id}/execute，前缀补 /api）', () => {
  const api = sourceOf(API);
  assert.match(api, /`\$\{BASE\}\/payable\/payment\/\$\{paymentId\}\/execute`/);
  const backend = sourceOf('../backend/src/business/erp/payable_payment.rs');
  assert.match(backend, /#\[nik::post\("\/business\/erp\/payable\/payment\/\{id\}\/execute"\)\]/);
  // 权限码与「发起付款」同一个（后端 handler 上声明的那个）
  assert.match(backend, /permission = "business:erp:payable:pay"/);
});
