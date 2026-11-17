/**
 * 客户信用面板（M3-14）纯逻辑单测 + 「源码即契约」对拍。
 *
 * 运行：`./node_modules/.bin/tsx --test tests/unit/m314-credit-panel.test.ts`
 * （或全量 `pnpm test:unit`，零新依赖）。
 *
 * # 为什么测 `.ts` 不测 `.vue`
 *
 * 「超额怎么判」「额度 0 显示什么」「行顺序」「百分比怎么夹」全在
 * `credit-panel-logic.ts` 里，`.vue` 只渲染。这些一旦漂移 `vue-tsc` 抓不到
 * （两边都是 string / number），线上表现是**财务看到错的钱**或**超额不报警**。
 *
 * # 断言口径照后端源码抄，不照卡面抄
 *
 * - `backend/app/src/business/erp/credit_handler.rs`：`CreditSnapshotRes` 的 21 个字段、
 *   `verdict_label` 的 5 个枚举名、路由字面量；
 * - `backend/app/src/business/erp/credit.rs`：`POLICY_*` 常量、`decide` 的
 *   「`limit <= 0` → Uncontrolled」与「`total <= limit` → Within」两条口径；
 * - `frontend/src/api/business/erp/credit.ts`：`CreditSnapshot` 接口的字段集。
 *
 * 其中字段集与枚举名直接 `readFileSync` 读源码逐字比对 —— 后端加字段 / 改枚举名、
 * 前端抄错都会当场红。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

import type { CreditSnapshot } from '@/api/business/erp/credit';

import {
  CREDIT_BASIS_FALLBACK,
  CREDIT_NEAR_LIMIT_PCT,
  CREDIT_POLICY_LABELS,
  CREDIT_UNLIMITED_TEXT,
  CREDIT_VERDICT_META,
  creditAvailableText,
  creditBasisText,
  creditDriftText,
  creditFormHint,
  creditFrozenText,
  creditHeadlineText,
  creditLimitText,
  creditNumber,
  creditPanelRows,
  creditPolicyText,
  creditProgressPercent,
  creditProgressStatus,
  creditUsagePercent,
  creditUsageText,
  creditVerdictMeta,
  isUncontrolled,
} from '@/views/business/erp/customer/credit-panel-logic';
import { formatMoney, toCents } from '@/views/business/erp/shared/money';

const HERE = dirname(fileURLToPath(import.meta.url));
const FRONTEND_ROOT = resolve(HERE, '../..');
const REPO_ROOT = resolve(FRONTEND_ROOT, '..');
const BACKEND_ERP = resolve(REPO_ROOT, 'backend/app/src/business/erp');

function readBackend(name: string): string {
  return readFileSync(resolve(BACKEND_ERP, name), 'utf8');
}

function readFrontend(relPath: string): string {
  return readFileSync(resolve(FRONTEND_ROOT, relPath), 'utf8');
}

/**
 * 一份「后端真实形状」的快照：金额都是 `NUMERIC(18,4)` 的字符串。
 *
 * 默认场景抄 PRD §4.7.2 的口径（额度 100,000、已占 18,080），
 * 让断言里的数看起来像财务报表上的数，而不是 `1.00`。
 */
function snapshot(overrides: Partial<CreditSnapshot> = {}): CreditSnapshot {
  return {
    customer_id: '11111111-1111-4111-8111-111111111111',
    customer_code: 'CUS-0001',
    customer_name: '北京示例客户',
    customer_status: 'ACTIVE',
    credit_limit: '100000.0000',
    credit_policy: 'BLOCK',
    used: '18080.0000',
    cached_used: '18080.0000',
    cache_drift: false,
    pending: '0.0000',
    open_receivable: '18080.0000',
    occupied: '18080.0000',
    order_amount: null,
    total: '18080.0000',
    available: '81920.0000',
    available_after: '81920.0000',
    over: '0.0000',
    is_over: false,
    verdict: 'WITHIN',
    allowed: true,
    basis: '已占 = 有效订单价税合计 − 已核销应收 − 已过账退货（下限 0）；…',
    ...overrides,
  };
}

/** 不控额度的客户（`credit_limit = 0`，后端 `available` / `available_after` 都是 `null`） */
function uncontrolledSnapshot(overrides: Partial<CreditSnapshot> = {}): CreditSnapshot {
  return snapshot({
    credit_limit: '0.0000',
    credit_policy: 'BLOCK',
    used: '5000.0000',
    cached_used: '5000.0000',
    pending: '1000.0000',
    open_receivable: '5000.0000',
    occupied: '6000.0000',
    total: '6000.0000',
    available: null,
    available_after: null,
    over: '0.0000',
    is_over: false,
    verdict: 'UNCONTROLLED',
    ...overrides,
  });
}

// ==================== ① 额度 / 可用的 0 与 null 必须能区分 ====================

test('额度 0 = 不控额度：文案写「不控额度（0.00）」，不是「额度用完」', () => {
  assert.equal(creditLimitText(uncontrolledSnapshot()), '不控额度（0.00）');
  assert.equal(isUncontrolled(uncontrolledSnapshot()), true);
  // 不控额度时后端给 null —— 显示「不控额度」而不是 0.00
  assert.equal(creditAvailableText(null, uncontrolledSnapshot()), CREDIT_UNLIMITED_TEXT);
  assert.equal(creditAvailableText(undefined, uncontrolledSnapshot()), CREDIT_UNLIMITED_TEXT);
});

test('可用 0.00（额度用光）与 不控额度 是两个不同的显示', () => {
  const usedUp = snapshot({ used: '100000.0000', occupied: '100000.0000', available: '0.0000' });
  assert.equal(creditAvailableText(usedUp.available, usedUp), '0.00');
  assert.notEqual(creditAvailableText(usedUp.available, usedUp), CREDIT_UNLIMITED_TEXT);
  assert.equal(isUncontrolled(usedUp), false);
  assert.equal(creditLimitText(usedUp), '100,000.00');
});

test('额度为负（脏数据）按不控额度处理 —— 与后端 decide 的首个分支同口径', () => {
  const dirty = snapshot({ credit_limit: '-1.0000', available: null });
  assert.equal(isUncontrolled(dirty), true);
  assert.equal(creditAvailableText(dirty.available, dirty), CREDIT_UNLIMITED_TEXT);
  assert.equal(creditUsagePercent(dirty), null);
});

// ==================== ② 占用率（超额不夹顶，进度条夹顶） ====================

test('占用率：四舍五入到整数，超额给出 >100 的真实数字', () => {
  assert.equal(creditUsagePercent(snapshot()), 18); // 18,080 / 100,000
  assert.equal(creditUsagePercent(snapshot({ occupied: '40000.0000' })), 40);
  assert.equal(creditUsagePercent(snapshot({ occupied: '50000.0000' })), 50);
  // 133,000 / 100,000 = 133%：进度条要夹到 100，但数字要保留 133
  const over = snapshot({ occupied: '133000.0000', available: '0.0000', is_over: true, over: '33000.0000' });
  assert.equal(creditUsagePercent(over), 133);
  assert.equal(creditProgressPercent(over), 100);
  // 不控额度没有分母
  assert.equal(creditUsagePercent(uncontrolledSnapshot()), null);
  assert.equal(creditProgressPercent(uncontrolledSnapshot()), 0);
});

test(`占用率 ≥ ${CREDIT_NEAR_LIMIT_PCT}% 转 warning，超额转 error，正常不给状态（默认色）`, () => {
  assert.equal(creditProgressStatus(snapshot()), undefined);
  // 79% 还是正常色；80% 起警告
  assert.equal(creditProgressStatus(snapshot({ occupied: '79000.0000' })), undefined);
  assert.equal(creditProgressStatus(snapshot({ occupied: '80000.0000' })), 'warning');
  assert.equal(creditProgressStatus(snapshot({ occupied: '99000.0000' })), 'warning');
  // 恰好用满（100%）就已经是 error
  assert.equal(creditProgressStatus(snapshot({ occupied: '100000.0000' })), 'error');
  // 超额只认 is_over（available 已被后端夹到 0 以上，不能拿它判）
  const over = snapshot({ occupied: '120000.0000', available: '0.0000', is_over: true, over: '20000.0000' });
  assert.equal(creditProgressStatus(over), 'error');
  assert.equal(creditProgressStatus(uncontrolledSnapshot()), undefined);
});

test('占用率文案：不控额度 / 带分母两种', () => {
  assert.equal(creditUsageText(uncontrolledSnapshot()), CREDIT_UNLIMITED_TEXT);
  assert.equal(
    creditUsageText(snapshot()),
    '18%（占用 18,080.00 / 额度 100,000.00）',
  );
});

// ==================== ③ 顶部一句话 ====================

test('顶部一句话：不控额度 / 已超 X / 可用 X 三种', () => {
  assert.equal(creditHeadlineText(uncontrolledSnapshot()), '不控额度，下单不校验信用');
  assert.equal(creditHeadlineText(snapshot()), '可用 81,920.00');
  const over = snapshot({
    occupied: '135600.0000',
    available: '0.0000',
    over: '35600.0000',
    is_over: true,
  });
  assert.equal(creditHeadlineText(over), '已超额度 35,600.00');
});

test('超额判定看 is_over / over，不看 available（它被后端夹在 0 以上）', () => {
  // 只有 available 为 0、is_over 为 false 的「刚好用满」：不能说已超
  const usedUp = snapshot({ occupied: '100000.0000', available: '0.0000', over: '0.0000', is_over: false });
  assert.equal(creditHeadlineText(usedUp), '可用 0.00');
  const logicSource = readFrontend('src/views/business/erp/customer/credit-panel-logic.ts');
  assert.ok(logicSource.includes('snapshot.is_over'), '判超额必须读 is_over');
  // 去掉注释再查：文档里**举例说明**「不许写 available < 0」，代码里更不能真有
  const codeOnly = logicSource
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .split('\n')
    .map((line) => line.replace(/\/\/.*$/, ''))
    .join('\n');
  assert.ok(
    !/available\s*<\s*0/.test(codeOnly),
    '不许写 available < 0 —— 后端已把 available 夹到 0 以上，那个条件永远不成立',
  );
});

// ==================== ④ 明细行（顺序 / 构成 / 试算三行） ====================

test('明细行顺序固定：额度 → 已占 → 在审 → 占用 → 可用 → 未清应收', () => {
  const rows = creditPanelRows(snapshot());
  assert.deepEqual(
    rows.map((row) => row.key),
    ['limit', 'used', 'pending', 'occupied', 'available', 'open_receivable'],
  );
  assert.deepEqual(
    rows.map((row) => row.label),
    ['信用额度', '已占额度', '在审订单', '占用合计', '可用额度', '未清应收'],
  );
  const byKey = new Map(rows.map((row) => [row.key, row]));
  assert.equal(byKey.get('used')?.value, '18,080.00');
  assert.equal(byKey.get('occupied')?.value, '18,080.00');
  assert.equal(byKey.get('available')?.value, '81,920.00');
  // 未清应收是对照口径：必须自己说明「不重复计入」
  assert.match(byKey.get('open_receivable')?.hint ?? '', /对照口径，不重复计入/);
  assert.equal(byKey.get('open_receivable')?.tone, 'muted');
});

test('已占 + 在审 = 占用：三行数各自独立显示，不合并成一格', () => {
  const rows = creditPanelRows(snapshot({ used: '18080.0000', pending: '7540.0000', occupied: '25620.0000' }));
  const byKey = new Map(rows.map((row) => [row.key, row]));
  assert.equal(byKey.get('used')?.value, '18,080.00');
  assert.equal(byKey.get('pending')?.value, '7,540.00');
  assert.equal(byKey.get('occupied')?.value, '25,620.00');
  // 占用 = 已占 + 在审（整数分口径，不靠浮点）
  assert.equal(
    toCents('18080.0000') + toCents('7540.0000'),
    toCents(byKey.get('occupied')?.value.replace(/,/g, '')),
  );
});

test('试算本单：传了 amount 才多出 本单金额 / 试算合计 / 本单后可用 三行', () => {
  const withAmount = creditPanelRows(
    snapshot({ order_amount: '12000.0000', total: '30080.0000', available_after: '69920.0000' }),
  );
  assert.deepEqual(
    withAmount.map((row) => row.key),
    [
      'limit',
      'used',
      'pending',
      'occupied',
      'available',
      'open_receivable',
      'order_amount',
      'total',
      'available_after',
    ],
  );
  const byKey = new Map(withAmount.map((row) => [row.key, row]));
  assert.equal(byKey.get('order_amount')?.value, '12,000.00');
  assert.equal(byKey.get('total')?.value, '30,080.00');
  assert.equal(byKey.get('available_after')?.value, '69,920.00');

  // 没试算时不出现「本单 0.00」的误导行
  const withoutAmount = creditPanelRows(snapshot());
  assert.equal(withoutAmount.some((row) => row.key === 'order_amount'), false);
});

test('超额行只在 over > 0 时出现，且是 danger 色', () => {
  const over = snapshot({
    occupied: '135600.0000',
    available: '0.0000',
    over: '35600.0000',
    is_over: true,
    verdict: 'BLOCK',
  });
  const rows = creditPanelRows(over);
  const last = rows[rows.length - 1];
  assert.equal(last?.key, 'over');
  assert.equal(last?.value, '35,600.00');
  assert.equal(last?.tone, 'danger');
  // 未超时一行 0.00 只是噪音 —— 不出现
  assert.equal(creditPanelRows(snapshot()).some((row) => row.key === 'over'), false);
});

test('不控额度：已占照实显示，可用写「不控额度」（不是 0.00）', () => {
  const rows = creditPanelRows(uncontrolledSnapshot());
  const byKey = new Map(rows.map((row) => [row.key, row]));
  assert.equal(byKey.get('used')?.value, '5,000.00');
  assert.equal(byKey.get('occupied')?.value, '6,000.00');
  assert.equal(byKey.get('available')?.value, CREDIT_UNLIMITED_TEXT);
  assert.equal(byKey.get('limit')?.value, '不控额度（0.00）');
});

// ==================== ⑤ 判定 / 策略文案 ====================

test('五种判定都有标签，BLOCK 才允许 blocked=true', () => {
  const labels = Object.values(CREDIT_VERDICT_META).map((meta) => meta.label);
  assert.deepEqual(labels, ['不控额度', '额度内', '超限·警告放行', '超限·直接放行', '超限·拦截']);
  assert.deepEqual(
    Object.values(CREDIT_VERDICT_META)
      .filter((meta) => meta.blocked)
      .map((meta) => meta.key),
    ['BLOCK'],
  );
  // WARN / PASS 是「超了但放行」，不能给 success 色（会让销售以为还有余量）
  assert.equal(CREDIT_VERDICT_META.WARN.theme, 'warning');
  assert.equal(CREDIT_VERDICT_META.PASS.theme, 'warning');
  assert.equal(CREDIT_VERDICT_META.WITHIN.theme, 'success');
});

test('未知判定 / 未知策略：原样显示，不猜成「额度内」「放行」', () => {
  const meta = creditVerdictMeta('SOMETHING_NEW');
  assert.equal(meta.label, 'SOMETHING_NEW');
  assert.equal(meta.theme, 'default');
  assert.equal(meta.blocked, false);
  assert.equal(creditVerdictMeta(null).label, '未知判定');
  assert.equal(creditPolicyText('SOMETHING_NEW'), 'SOMETHING_NEW');
  assert.equal(creditPolicyText(''), '—');
  assert.equal(creditPolicyText('BLOCK'), CREDIT_POLICY_LABELS.BLOCK);
});

// ==================== ⑥ 漂移 / 冻结 / 口径说明 ====================

test('缓存列漂移才提示，且写明「页面按现算值显示」', () => {
  assert.equal(creditDriftText(snapshot()), '');
  const drift = creditDriftText(snapshot({ cached_used: '20000.0000', cache_drift: true }));
  assert.match(drift, /20,000\.00/);
  assert.match(drift, /18,080\.00/);
  assert.match(drift, /页面按现算值显示/);
});

test('冻结文案：FROZEN 才提示，且说明「与额度无关」', () => {
  assert.equal(creditFrozenText(snapshot()), '');
  assert.match(creditFrozenText(snapshot({ customer_status: 'FROZEN' })), /与额度无关/);
});

test('口径说明优先用后端 basis，空则退回本地常量', () => {
  assert.equal(creditBasisText(snapshot()), snapshot().basis);
  assert.equal(creditBasisText(snapshot({ basis: '' })), CREDIT_BASIS_FALLBACK);
  assert.equal(creditBasisText(snapshot({ basis: '   ' })), CREDIT_BASIS_FALLBACK);
  assert.equal(creditBasisText(null), CREDIT_BASIS_FALLBACK);
});

// ==================== ⑦ 表单提示 / 数值兜底 ====================

test('额度输入框提示：0 说不控额度，非 0 报额度 + 策略', () => {
  assert.match(creditFormHint('0.0000', 'BLOCK'), /0 = 不控额度/);
  assert.match(creditFormHint(null, 'WARN'), /0 = 不控额度/);
  assert.equal(creditFormHint('50000.0000', 'WARN'), '额度 50,000.00，超限策略：警告放行');
});

test('金额兜底：字符串 / 空 / 非法值都不炸，且不拼字符串', () => {
  assert.equal(creditNumber('1000.0000'), 1000);
  assert.equal(creditNumber(null), 0);
  assert.equal(creditNumber(''), 0);
  assert.equal(creditNumber('abc'), 0);
  assert.equal(formatMoney('1000.0000'), '1,000.00');
  // 典型的浮点陷阱：0.1 + 0.2
  assert.equal(toCents('0.1000') + toCents('0.2000'), toCents('0.3000'));
});

// ==================== ⑧ 源码即契约：字段集 / 枚举名 / 路由字面量 ====================

/** 从 Rust 结构体里抽 `pub <name>:` 的字段名（按出现顺序） */
function rustStructFields(source: string, structName: string): string[] {
  const start = source.indexOf(`pub struct ${structName} {`);
  assert.ok(start >= 0, `后端找不到 struct ${structName}`);
  const body = source.slice(start + `pub struct ${structName} {`.length);
  const end = body.indexOf('\n}');
  assert.ok(end > 0, `struct ${structName} 没有闭合`);
  return body
    .slice(0, end)
    .split('\n')
    .map((line) => /^\s*pub\s+([a-z_][a-z0-9_]*)\s*:/.exec(line))
    .filter((match): match is RegExpExecArray => match !== null)
    .map((match) => match[1] as string);
}

/** 从 TS 接口里抽字段名（跳过注释行） */
function tsInterfaceFields(source: string, interfaceName: string): string[] {
  const start = source.indexOf(`export interface ${interfaceName} {`);
  assert.ok(start >= 0, `前端找不到 interface ${interfaceName}`);
  const body = source.slice(start + `export interface ${interfaceName} {`.length);
  const end = body.indexOf('\n}');
  assert.ok(end > 0, `interface ${interfaceName} 没有闭合`);
  return body
    .slice(0, end)
    .split('\n')
    .map((line) => /^\s{2}([a-z_][a-z0-9_]*)\??\s*:/.exec(line))
    .filter((match): match is RegExpExecArray => match !== null)
    .map((match) => match[1] as string);
}

test('字段集对拍：前端 CreditSnapshot == 后端 CreditSnapshotRes（21 个，双向相等）', () => {
  const backend = rustStructFields(readBackend('credit_handler.rs'), 'CreditSnapshotRes');
  const frontend = tsInterfaceFields(readFrontend('src/api/business/erp/credit.ts'), 'CreditSnapshot');
  assert.equal(backend.length, 21, `后端字段数变了（${backend.length}），本用例与面板行定义要一起改`);
  assert.deepEqual(frontend, backend, '前端接口字段与后端报文必须逐一对应（顺序也一致）');
  // 面板真正读到的关键字段不许少
  for (const key of ['credit_limit', 'used', 'pending', 'occupied', 'available', 'over', 'is_over', 'verdict', 'basis', 'cache_drift']) {
    assert.ok(backend.includes(key), `后端报文缺字段 ${key}`);
  }
});

test('判定枚举名对拍：CREDIT_VERDICT_META 的 key == 后端 verdict_label 的字面量（5 个）', () => {
  const source = readBackend('credit_handler.rs');
  const start = source.indexOf('pub fn verdict_label(');
  assert.ok(start >= 0, '后端找不到 verdict_label');
  const body = source.slice(start, source.indexOf('\n}', start));
  const literals = [...body.matchAll(/=>\s*"([A-Z_]+)"/g)].map((match) => match[1] as string);
  assert.deepEqual(literals.sort(), Object.keys(CREDIT_VERDICT_META).sort());
});

test('策略字面量对拍：CREDIT_POLICY_LABELS 的 key == 后端 POLICY_* 常量值', () => {
  const source = readBackend('credit.rs');
  const values = [...source.matchAll(/pub const POLICY_[A-Z]+:\s*&str\s*=\s*"([A-Z]+)"/g)].map(
    (match) => match[1] as string,
  );
  assert.deepEqual(values.sort(), Object.keys(CREDIT_POLICY_LABELS).sort());
  // fail-closed 口径：未知策略按 BLOCK 处理（页面也别把未知值显示成「放行」）
  assert.match(source, /POLICY_BLOCK:\s*&str\s*=\s*"BLOCK"/);
});

test('「额度 0 = 不控额度」「total <= limit = 额度内」两条后端口径还在', () => {
  const source = readBackend('credit.rs');
  assert.match(source, /if limit <= Decimal::ZERO \{\s*\n\s*return CreditVerdict::Uncontrolled;/);
  assert.match(source, /if snapshot\.total\(\) <= limit \{\s*\n\s*return CreditVerdict::Within;/);
});

test('接口路径对拍：前端 BASE 带 /api，后端宏不含 /api（漏前缀会命中 SPA fallback）', () => {
  const api = readFrontend('src/api/business/erp/credit.ts');
  assert.match(api, /const BASE = '\/api\/business\/erp\/credit'/);
  assert.match(api, /get<CreditSnapshot>\(`\$\{BASE\}\/\$\{customerId\}`/);
  const handler = readBackend('credit_handler.rs');
  assert.match(handler, /#\[nik::get\("\/business\/erp\/credit\/\{customer_id\}"\)\]/);
  assert.ok(!handler.includes('"/api/business/erp/credit'), '后端路由宏里不能写 /api 前缀');
});

test('面板挂载点：客户详情（编辑弹窗）内嵌，且只在已有客户时渲染', () => {
  const page = readFrontend('src/views/business/erp/customer/index.vue');
  assert.ok(page.includes("import CreditPanel from './CreditPanel.vue'"), '客户详情页必须引入信用面板');
  assert.match(page, /<CreditPanel[^>]*:customer-id="editing\.id"/, '面板必须挂在编辑弹窗里并传客户 id');
  // 挂在 `isEdit && editing` 的区块内：新建时没有 id，拉不到快照
  const section = page.slice(page.indexOf('客户信用（M3-14）'), page.indexOf('<CreditPanel'));
  assert.ok(section.includes('v-if="isEdit && editing"'), '信用面板必须只在编辑（已有客户）时渲染');
  // 额度字段旁边不能让用户以为「额度 0 是零额度」
  assert.match(page, /0 = 不控额度/);
});

test('面板取数失败降级：有重试、不抛错、不挡保存', () => {
  const panel = readFrontend('src/views/business/erp/customer/CreditPanel.vue');
  assert.ok(panel.includes('fetchCreditSnapshot'), '面板自己取数（唯一数据来源）');
  assert.ok(panel.includes('重试'), '取不到要能重试');
  assert.ok(panel.includes('catch'), '失败必须 catch 住（不能把异常抛给父页面）');
  // 父页面保存逻辑里不许读面板的数据（拨一下 refreshKey 可以，拿它当条件不行）
  const page = readFrontend('src/views/business/erp/customer/index.vue');
  const confirm = page.slice(page.indexOf('async function handleConfirm'), page.indexOf('// ==================== 状态切换'));
  for (const forbidden of ['fetchCreditSnapshot', 'creditSnapshot', 'cus-credit', 'errorText']) {
    assert.ok(!confirm.includes(forbidden), `保存流程不许读信用面板状态（出现 ${forbidden}）`);
  }
  assert.ok(!/if\s*\([^)]*credit/i.test(confirm), '保存流程不许按信用状态提前 return');
});

test('换客户必须清旧快照：A 的额度不许出现在 B 的表头下（实测踩过）', () => {
  const panel = readFrontend('src/views/business/erp/customer/CreditPanel.vue');
  // 面板自己记着「当前数属于哪个客户」，id 变了先清空
  assert.ok(panel.includes('snapshotCustomerId'), '面板要记快照归属的客户');
  const reload = panel.slice(panel.indexOf('async function reload'), panel.indexOf('onMounted(() =>'));
  assert.match(
    reload,
    /snapshotCustomerId\.value !== customerId[\s\S]{0,120}snapshot\.value = null/,
    'customerId 变化时必须先清空 snapshot（失败时保留旧值只对同一客户成立）',
  );
  // 父页面再上一道保险：key 绑客户 id，换客户直接重建组件
  const page = readFrontend('src/views/business/erp/customer/index.vue');
  assert.match(
    page,
    /<CreditPanel[\s\S]{0,200}?:key="editing\.id"/,
    '面板 key 必须绑客户 id（换客户重建）',
  );
});

test('同一家客户也要重拉：弹窗关闭不销毁内容，打开 / 保存后必须拨 refreshKey', () => {
  const panel = readFrontend('src/views/business/erp/customer/CreditPanel.vue');
  // 面板要把 refreshKey 放进 watch 依赖（否则父页面拨了也不生效）
  assert.match(
    panel,
    /\(\) => \[props\.customerId, props\.amount, props\.excludeOrderId, props\.refreshKey\]/,
    'watch 依赖必须含 refreshKey',
  );
  const page = readFrontend('src/views/business/erp/customer/index.vue');
  assert.match(page, /:refresh-key="creditRefreshKey"/, '父页面必须把刷新信号传给面板');
  // 打开详情（别人可能刚收款 / 改额度）与保存成功（自己刚改了额度）各拨一格
  const openEdit = page.slice(page.indexOf('function openEdit'), page.indexOf('function openEdit') + 400);
  assert.match(openEdit, /creditRefreshKey\.value \+= 1/, 'openEdit 要拨刷新信号');
  const confirm = page.slice(page.indexOf('async function handleConfirm'), page.indexOf('// ==================== 状态切换'));
  assert.match(confirm, /creditRefreshKey\.value \+= 1/, '保存成功后要拨刷新信号（额度 / 策略 / 冻结都变了）');
});

test('壳类样式：面板不自建壳类（§2.8 只允许 shell.less 定义）', () => {
  const panel = readFrontend('src/views/business/erp/customer/CreditPanel.vue');
  const style = panel.slice(panel.indexOf('<style'));
  for (const shell of ['panel-head', 'tool-row', 'tool-card', 'list-card', 'panel-empty']) {
    assert.ok(!style.includes(`.${shell}`), `面板 <style> 里不许出现壳类 .${shell}`);
  }
});
