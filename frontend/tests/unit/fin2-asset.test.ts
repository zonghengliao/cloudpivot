/**
 * FIN-2 固定资产三页的纯逻辑单测（卡片 / 分类 / 折旧计提）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零依赖、无 DOM）。
 *
 * 测 `.ts` 不测 `.vue`：这页最容易错的是口径 ——
 * 月折旧额怎么四舍五入、最后一个月怎么补尾差、本月应提怎么按月序数推、
 * 处置弹窗上三个数从哪来、哪些字段能改。这些 `vue-tsc` 抓不到，页面渲染也看不出来。
 *
 * 断言口径照后端抄：
 * - `asset_compute::{residual_value, depreciable_base, monthly_depreciation}`（四位 ROUND_HALF_UP）
 * - `asset_model::scan_assets_for_period` + `month_amount`（起提日 / 处置日 / 提足即停 / 尾差）
 * - `asset_dto::{validate_create, validate_dispose, validate_category}`
 * - 折旧批次状态 4 值、计提结果 4 值（词表契约，加值会红 `biz_type_domain` 巡检）
 *
 * 最后两条用例是本页对 n08（菜单路径可解析）的镜像：三个菜单的 path / component
 * 都从**迁移**里读出来，再逐条钉住路由与视图文件真实存在 —— 防止哪天删了文件却以为菜单还能点。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  ASSET_ERROR_HINTS,
  addMoney,
  applyStockInSource,
  assetStatusLabel,
  canCancelDispose,
  canDeleteAsset,
  canDisposeAsset,
  canEditAsset,
  canEnableAsset,
  canIdleAsset,
  canTransferAsset,
  changeSummary,
  clearSource,
  cmpMoney,
  depBatchStatusLabel,
  disposalGainPreview,
  disposeSummaryOf,
  emptyAssetForm,
  emptyCategoryForm,
  emptyDisposeForm,
  errorText,
  formatMoney,
  formatRate,
  isAmountLocked,
  isZeroMoney,
  moneyToPlain,
  monthsBetween,
  parseMoney,
  periodAmountOf,
  periodEnd,
  periodOfDate,
  periodStart,
  previewDepreciation,
  requireReasonError,
  subMoney,
  summarizeDepreciation,
  toCreatePayload,
  toDisposePayload,
  validateAssetForm,
  validateCategoryForm,
  validateDisposeForm,
  validateReverseReason,
  type AssetFormState,
} from '@/views/business/erp/asset/asset-logic';
import {
  canReverseBatch,
  defaultPeriod,
  detailTotalMatches,
  diffDisplay,
  periodLabel,
  periodOptions,
  runResultText,
} from '@/views/business/erp/depreciation/depreciation-logic';
import {
  accountLabel,
  accountOptions,
  categoryStatusLabel,
  categoryToForm,
  methodLabel,
} from '@/views/business/erp/asset-category/category-logic';
import type { AssetCategory, AssetDetail, AssetRow } from '@/api/business/erp/asset';
import type { DepBatch } from '@/api/business/erp/depreciation';

const HERE = dirname(fileURLToPath(import.meta.url));
const FRONTEND_ROOT = resolve(HERE, '../..');

// ==================== 夹具 ====================

/** 造一张资产卡（只填逻辑层会读的字段）。 */
function asset(overrides: Partial<AssetRow> = {}): AssetRow {
  return {
    id: 'a1',
    asset_no: 'FA-20260815-000001',
    asset_name: '投影仪',
    category_id: 'c1',
    category_code: 'ELEC',
    spec: null,
    use_dept_id: 'd1',
    use_dept_name: '研发部',
    custodian_id: null,
    custodian_name: null,
    location: null,
    original_value: '100000.0000',
    tax_amount: '0.0000',
    residual_value: '5000.0000',
    depreciable_base: '95000.0000',
    monthly_depreciation: '2638.8889',
    start_depreciation_date: '2026-09-01',
    accumulated_depreciation: '0.0000',
    net_value: '100000.0000',
    depreciation_months_done: 0,
    useful_life_months: 36,
    last_depreciation_period: null,
    purchase_date: '2026-08-15',
    source_type: 'MANUAL',
    source_no: null,
    buy_mode: 'DIRECT_BUY',
    supplier_name: null,
    voucher_id: null,
    status: 'IN_USE',
    status_label: '在役',
    disposal_type: null,
    disposal_date: null,
    disposal_voucher_id: null,
    remark: null,
    created_at: '2026-08-15T02:00:00Z',
    ...overrides,
  };
}

/** 造一个批次。 */
function batch(overrides: Partial<DepBatch> = {}): DepBatch {
  return {
    id: 'b1',
    batch_no: 'DP-202609-0001',
    period_code: '2026-09',
    status: 'POSTED',
    asset_count: 1,
    total_amount: '2638.8889',
    voucher_id: 'v1',
    reverse_voucher_id: null,
    skip_reason: null,
    run_at: '2026-10-01T01:30:00Z',
    posted_at: '2026-10-01T01:30:00Z',
    reversed_at: null,
    remark: null,
    ...overrides,
  };
}

/** 造一张分类。 */
function category(overrides: Partial<AssetCategory> = {}): AssetCategory {
  return {
    id: 'c1',
    category_code: 'ELEC',
    category_name: '电子设备',
    depreciation_method: 'STRAIGHT_LINE',
    useful_life_months: 36,
    salvage_rate: '0.0500',
    asset_account_id: 'acc-1601',
    accum_account_id: 'acc-1602',
    expense_account_id: 'acc-560201',
    status: 1,
    sort_order: 10,
    remark: null,
    ...overrides,
  };
}

// ==================== ① 四位定点金额 ====================

test('parseMoney：后端字符串 → 四位定点 bigint，不经过 Number', () => {
  assert.equal(parseMoney('2638.8889'), 26388889n);
  assert.equal(parseMoney('100000.0000'), 1000000000n);
  assert.equal(parseMoney('0.05'), 500n);
  assert.equal(parseMoney('-1.5'), -15000n);
  assert.equal(parseMoney('1,234.5'), 12345000n);
  assert.equal(parseMoney(''), 0n);
  assert.equal(parseMoney(null), 0n);
  // 第 5 位四舍五入（后端不会给，但手工输入会）
  assert.equal(parseMoney('0.00005'), 1n);
  assert.equal(parseMoney('0.00004'), 0n);
});

test('加减比较：0.1 + 0.2 精确等于 0.3', () => {
  assert.equal(addMoney('0.1', '0.2'), '0.3000');
  assert.equal(subMoney('2638.8889', '2638.8889'), '0.0000');
  assert.equal(cmpMoney('2638.8889', '2638.8889'), 0);
  assert.equal(cmpMoney('1', '1.0000'), 0);
  assert.equal(cmpMoney('1.0001', '1.0000'), 1);
  assert.equal(cmpMoney('-0.0001', '0'), -1);
  assert.equal(moneyToPlain(-12345n), '-1.2345');
});

test('isZeroMoney / formatMoney：-0.0001 展示不出现 -0.00', () => {
  assert.equal(isZeroMoney('0.0000'), true);
  assert.equal(isZeroMoney('-0.0001'), false);
  assert.equal(formatMoney('2638.8889'), '2,638.89');
  assert.equal(formatMoney('100000'), '100,000.00');
  assert.equal(formatMoney('-0.0001'), '0.00');
  assert.equal(formatMoney('0.005'), '0.01');
  assert.equal(formatMoney(null), '0.00');
});

test('formatRate：0.0500 → 5.00%', () => {
  assert.equal(formatRate('0.0500'), '5.00%');
  assert.equal(formatRate('0'), '0.00%');
  assert.equal(formatRate('0.125'), '12.50%');
});

// ==================== ② 折旧预览（交互细节①） ====================

test('previewDepreciation：100000 / 残值 5% / 36 个月 = 2638.8889，起提次月 1 日', () => {
  const preview = previewDepreciation({
    original_value: '100000.0000',
    salvage_rate: '0.0500',
    useful_life_months: 36,
    purchase_date: '2026-08-15',
  });
  assert.ok(preview);
  assert.equal(preview.residual_value, '5000.0000');
  assert.equal(preview.depreciable_base, '95000.0000');
  assert.equal(preview.monthly_depreciation, '2638.8889');
  // 尾差月 = 95000 − 2638.8889 × 35
  assert.equal(preview.last_month_amount, '2638.8885');
  assert.equal(preview.start_date, '2026-09-01');
  assert.equal(preview.last_period, '2029-08');
  assert.equal(preview.finish_date, '2029-08-31');
  assert.equal(preview.months, 36);
});

test('previewDepreciation：短年限尾差补齐 / 入参不全返回 null', () => {
  const preview = previewDepreciation({
    original_value: '100',
    salvage_rate: '0',
    useful_life_months: 3,
    purchase_date: '2026-01-31',
  });
  assert.ok(preview);
  assert.equal(preview.monthly_depreciation, '33.3333');
  // 100 − 33.3333 × 2 = 33.3334
  assert.equal(preview.last_month_amount, '33.3334');
  assert.equal(preview.start_date, '2026-02-01');
  assert.equal(preview.last_period, '2026-04');

  assert.equal(
    previewDepreciation({
      original_value: '0',
      salvage_rate: '0.05',
      useful_life_months: 36,
      purchase_date: '2026-08-15',
    }),
    null,
  );
  assert.equal(
    previewDepreciation({
      original_value: '100',
      salvage_rate: '1',
      useful_life_months: 36,
      purchase_date: '2026-08-15',
    }),
    null,
  );
  assert.equal(
    previewDepreciation({
      original_value: '100',
      salvage_rate: '0.05',
      useful_life_months: 0,
      purchase_date: '2026-08-15',
    }),
    null,
  );
});

// ==================== ③ 本月应提 / 已提 / 差异（交互细节③） ====================

test('periodAmountOf：第 1 月 = 月折旧，最后 1 月补尾差，提足后为 0', () => {
  const row = asset();
  assert.equal(periodAmountOf(row, '2026-09'), '2638.8889');
  assert.equal(periodAmountOf(row, '2026-10'), '2638.8889');
  assert.equal(periodAmountOf(row, '2029-08'), '2638.8885');
  assert.equal(periodAmountOf(row, '2029-09'), '0.0000');
  // 起提日之前不计提（当月增加当月不提）
  assert.equal(periodAmountOf(row, '2026-08'), '0.0000');
});

test('periodAmountOf：处置当月仍提整月，处置次月起为 0；闲置照提', () => {
  const disposed = asset({ disposal_date: '2026-10-05', disposal_type: 'SCRAP', status: 'DISPOSED' });
  // 状态已 DISPOSED → 不计提（后端扫描只认 IN_USE / IDLE）
  assert.equal(periodAmountOf(disposed, '2026-10'), '0.0000');
  // 但在役期间处置日落在期间内 → 仍提整月；处置日早于期间 → 0
  const inUse = asset({ disposal_date: '2026-10-05', status: 'IN_USE' });
  assert.equal(periodAmountOf(inUse, '2026-10'), '2638.8889');
  assert.equal(periodAmountOf(asset({ disposal_date: '2026-10-05' }), '2026-11'), '0.0000');
  assert.equal(periodAmountOf(asset({ status: 'IDLE' }), '2026-09'), '2638.8889');
  assert.equal(periodAmountOf(asset({ status: 'CANCELLED' }), '2026-09'), '0.0000');
});

test('summarizeDepreciation：已提 = POSTED 批次合计，差异给方向', () => {
  const rows = [asset(), asset({ id: 'a2', monthly_depreciation: '10000.0000', depreciable_base: '360000.0000' })];

  // 无批次 → 应提 12638.8889，差异 = 应提（少提）
  const none = summarizeDepreciation({ assets: rows, batches: [], period_code: '2026-09' });
  assert.equal(none.expected_amount, '12638.8889');
  assert.equal(none.expected_count, 2);
  assert.equal(none.posted_amount, '0.0000');
  assert.equal(none.diff_amount, '12638.8889');
  assert.equal(none.diff_level, 'UNDER');
  assert.equal(none.has_posted_batch, false);

  // 已过账且相符 → 差异 0
  const posted = summarizeDepreciation({
    assets: rows,
    batches: [batch({ total_amount: '12638.8889', asset_count: 2 })],
    period_code: '2026-09',
  });
  assert.equal(posted.posted_amount, '12638.8889');
  assert.equal(posted.diff_amount, '0.0000');
  assert.equal(posted.diff_level, 'OK');
  assert.equal(posted.posted_count, 2);

  // 别期间 / REVERSED 批次不算「已提」
  const other = summarizeDepreciation({
    assets: rows,
    batches: [batch({ period_code: '2026-08' }), batch({ id: 'b2', status: 'REVERSED' })],
    period_code: '2026-09',
  });
  assert.equal(other.posted_amount, '0.0000');
  assert.equal(other.has_posted_batch, false);

  // 多提 → OVER
  const over = summarizeDepreciation({
    assets: rows,
    batches: [batch({ total_amount: '13000.0000' })],
    period_code: '2026-09',
  });
  assert.equal(over.diff_amount, '-361.1111');
  assert.equal(over.diff_level, 'OVER');

  // SKIPPED 批次的跳过原因要透出
  const skipped = summarizeDepreciation({
    assets: rows,
    batches: [batch({ status: 'SKIPPED', total_amount: '0.0000', skip_reason: '本期无在役资产' })],
    period_code: '2026-09',
  });
  assert.deepEqual(skipped.skipped_reasons, ['本期无在役资产']);
});

test('diffDisplay / runResultText / K4 自检', () => {
  const ok = summarizeDepreciation({
    assets: [asset()],
    batches: [batch()],
    period_code: '2026-09',
  });
  assert.equal(diffDisplay(ok).theme, 'success');
  assert.equal(diffDisplay(ok).value, '0.00');

  const under = summarizeDepreciation({ assets: [asset()], batches: [], period_code: '2026-09' });
  assert.equal(diffDisplay(under).theme, 'warning');
  assert.equal(diffDisplay(under).label, '差异（少提）');

  assert.equal(detailTotalMatches(batch(), '2638.8889'), true);
  assert.equal(detailTotalMatches(batch(), '2638.8888'), false);

  assert.match(runResultText('POSTED', '已生成凭证', '2638.8889'), /计提成功（本次 2,638.89）/);
  assert.match(runResultText('EXISTS', '本期已有批次', '0'), /幂等/);
  assert.match(runResultText('SKIPPED', '本期无应提', '0'), /无应提/);
});

test('期间工具：上一会计月 / 期初期末 / 月差 / 下拉', () => {
  assert.equal(periodOfDate('2026-09-15'), '2026-09');
  assert.equal(periodStart('2026-09'), '2026-09-01');
  assert.equal(periodEnd('2026-02'), '2026-02-28');
  assert.equal(monthsBetween('2026-09', '2029-08'), 35);
  assert.equal(monthsBetween('2026-09', '2026-08'), -1);
  assert.equal(defaultPeriod('2026-01-15'), '2025-12');
  assert.equal(periodLabel('2026-09'), '2026 年 09 月');
  const options = periodOptions('2026-11-05');
  assert.equal(options[0].value, '2026-11');
  assert.equal(options[12].value, '2025-11');
});

// ==================== ④ 建卡表单（交互细节②） ====================

test('validateAssetForm：必填与税额口径（镜像后端 400）', () => {
  const empty = validateAssetForm(emptyAssetForm());
  assert.ok(empty.asset_name);
  assert.ok(empty.category_id);
  assert.ok(empty.use_dept_id);
  assert.ok(empty.original_value);
  assert.ok(empty.purchase_date === undefined); // 默认今天，日期本身合法

  const base: AssetFormState = {
    ...emptyAssetForm(),
    asset_name: '投影仪',
    category_id: 'c1',
    use_dept_id: 'd1',
    original_value: '100000',
    tax_amount: '13000',
  };
  assert.deepEqual(validateAssetForm(base), {});

  // 税额 ≥ 原值
  assert.ok(validateAssetForm({ ...base, original_value: '100', tax_amount: '100' }).tax_amount);
  // 负原值
  assert.ok(validateAssetForm({ ...base, original_value: '-1' }).original_value);
  // MANUAL 不能带来源单（后端同款）
  assert.ok(validateAssetForm({ ...base, source_id: 'x' }).source_id);
  // 非 MANUAL 必须有来源单
  assert.ok(validateAssetForm({ ...base, source_type: 'STOCK_IN' }).source_id);
});

test('applyStockInSource：金额预填 + 锁定，税额清零，购物口径 FROM_STOCK', () => {
  const form = {
    ...emptyAssetForm(),
    asset_name: '服务器',
    category_id: 'c1',
    use_dept_id: 'd1',
  };
  const applied = applyStockInSource(form, {
    id: 'si1',
    in_no: 'IN-20260810-000001',
    total_amount: '88000.5',
  });
  assert.equal(applied.locked, true);
  assert.equal(applied.form.source_type, 'STOCK_IN');
  assert.equal(applied.form.source_id, 'si1');
  assert.equal(applied.form.source_no, 'IN-20260810-000001');
  assert.equal(applied.form.original_value, '88000.5000');
  assert.equal(applied.form.tax_amount, '0.0000');
  assert.equal(isAmountLocked(applied.form), true);
  // 收货单转入不得带进项税（此用例里 tax=0，所以校验通过）
  assert.deepEqual(validateAssetForm(applied.form), {});

  const payload = toCreatePayload(applied.form);
  assert.equal(payload.buy_mode, 'FROM_STOCK');
  assert.equal(payload.source_id, 'si1');
  assert.equal(payload.source_type, 'STOCK_IN');
  assert.equal(payload.original_value, '88000.5000');
  assert.equal(payload.tax_amount, '0.0000');

  // 切回手工录入要把来源清干净，否则后端 400「MANUAL 时不能给来源单」
  const cleared = clearSource(applied.form);
  assert.equal(cleared.source_type, 'MANUAL');
  assert.equal(cleared.source_id, '');
  assert.equal(isAmountLocked(cleared), false);
});

test('toCreatePayload：金额补足四位、空串字段不发', () => {
  const payload = toCreatePayload({
    ...emptyAssetForm(),
    asset_name: '  投影仪  ',
    category_id: 'c1',
    use_dept_id: 'd1',
    original_value: '100000',
    tax_amount: '13000',
    spec: '  ',
    remark: '备注',
    purchase_date: '2026-08-15',
  });
  assert.equal(payload.asset_name, '投影仪');
  assert.equal(payload.original_value, '100000.0000');
  assert.equal(payload.tax_amount, '13000.0000');
  assert.equal(payload.buy_mode, 'DIRECT_BUY');
  assert.equal(payload.spec, undefined);
  assert.equal(payload.remark, '备注');
  assert.equal(payload.source_id, undefined);
});

// ==================== ⑤ 处置（交互细节④） ====================

test('validateDisposeForm：SALE 必须有售价，SCRAP/LOSS 不许有金额', () => {
  const saleEmpty = validateDisposeForm({ ...emptyDisposeForm(), disposal_type: 'SALE' });
  assert.ok(saleEmpty.sale_price);
  assert.ok(saleEmpty.reason);

  const saleOk = validateDisposeForm({
    disposal_type: 'SALE',
    disposal_date: '2026-10-20',
    reason: '闲置出售',
    sale_price: '20000',
    output_tax: '2600',
  });
  assert.deepEqual(saleOk, {});

  const scrapWithPrice = validateDisposeForm({
    disposal_type: 'SCRAP',
    disposal_date: '2026-10-20',
    reason: '报废',
    sale_price: '100',
    output_tax: '0',
  });
  assert.ok(scrapWithPrice.sale_price);

  const scrapOk = validateDisposeForm({
    disposal_type: 'SCRAP',
    disposal_date: '2026-10-20',
    reason: '报废',
    sale_price: '',
    output_tax: '',
  });
  assert.deepEqual(scrapOk, {});

  // 非 SALE 的 payload 不带金额字段
  const payload = toDisposePayload({
    disposal_type: 'LOSS',
    disposal_date: '2026-10-20',
    reason: '盘亏',
    sale_price: '0',
    output_tax: '0',
  });
  assert.equal(payload.sale_price, undefined);
  assert.equal(payload.output_tax, undefined);
  assert.equal(payload.disposal_type, 'LOSS');
});

test('disposeSummaryOf：净值 / 已提月数 / 剩余可提 全部取接口值', () => {
  const detail = {
    asset: asset({ accumulated_depreciation: '5277.7778', net_value: '94722.2222', depreciation_months_done: 2 }),
    changes: [],
    depreciations: [],
    next_statuses: [],
    pending_disposal: null,
    months_left: 34,
    amount_left: '89722.2222',
  } as unknown as AssetDetail;
  const summary = disposeSummaryOf(detail);
  assert.equal(summary.net_value, '94722.2222');
  assert.equal(summary.months_done, 2);
  assert.equal(summary.useful_life_months, 36);
  assert.equal(summary.months_left, 34);
  assert.equal(summary.amount_left, '89722.2222');
  // 出售损益参考 = 售价 − 净值（仅供决策；入账以凭证为准）
  assert.equal(disposalGainPreview(summary.net_value, '80000'), '-14722.2222');
  assert.equal(disposalGainPreview(summary.net_value, ''), '-94722.2222');
});

test('动作可见性：处置卡只读，在跑审批不再允许提交，已处置可作废', () => {
  assert.equal(canEditAsset(asset()), true);
  assert.equal(canEditAsset(asset({ status: 'DISPOSED' })), false);
  assert.equal(canTransferAsset(asset({ status: 'IDLE' })), true);
  assert.equal(canDisposeAsset(asset(), null), true);
  assert.equal(canDisposeAsset(asset(), 'inst-1'), false);
  assert.equal(canDisposeAsset(asset({ status: 'DISPOSED' }), null), false);
  assert.equal(canCancelDispose(asset({ status: 'DISPOSED' })), true);
  assert.equal(canCancelDispose(asset()), false);
  assert.equal(canDeleteAsset(asset({ status: 'DISPOSED' })), false);
  assert.equal(canReverseBatch(batch()), true);
  assert.equal(canReverseBatch(batch({ status: 'REVERSED' })), false);
  assert.equal(canReverseBatch(batch({ status: 'SKIPPED' })), false);
});

// ==================== ⑤b 停用 / 启用（IN_USE ⇄ IDLE） ====================

test('停用 / 启用按钮可见性真值表：IN_USE 只停用、IDLE 只启用、DISPOSED 都不给', () => {
  // 在役：只能停用（启用按钮不出现）
  assert.equal(canIdleAsset(asset({ status: 'IN_USE' })), true);
  assert.equal(canEnableAsset(asset({ status: 'IN_USE' })), false);
  // 闲置：只能启用
  assert.equal(canIdleAsset(asset({ status: 'IDLE' })), false);
  assert.equal(canEnableAsset(asset({ status: 'IDLE' })), true);
  // 已处置 / 已作废：两个都不给（已处置要先作废处置）
  for (const status of ['DISPOSED', 'CANCELLED']) {
    assert.equal(canIdleAsset(asset({ status })), false, `${status} 不该出现停用`);
    assert.equal(canEnableAsset(asset({ status })), false, `${status} 不该出现启用`);
  }
});

// ==================== ⑥ 文案 / 校验 / 错误码 ====================

test('状态与变动文案：后端没给 label 时按词表兜底，未知值原样显示', () => {
  assert.equal(assetStatusLabel('IN_USE'), '在役');
  assert.equal(assetStatusLabel('DISPOSED'), '已处置');
  assert.equal(assetStatusLabel('WHATEVER'), 'WHATEVER');
  assert.equal(depBatchStatusLabel('POSTED'), '已过账');
  assert.equal(depBatchStatusLabel('SKIPPED'), '已跳过');
  assert.equal(changeSummary({
    id: 'x',
    change_type: 'TRANSFER',
    change_label: '部门转移',
    change_date: '2026-10-01',
    from_dept_name: '研发部',
    to_dept_name: '财务部',
    from_user_name: null,
    to_user_name: null,
    from_status: null,
    to_status: null,
    reason: '组织调整',
    voucher_id: null,
    approval_instance_id: null,
    operator_name: null,
    remark: null,
    created_at: '2026-10-01T02:00:00Z',
  }), '研发部 → 财务部');
});

test('errorText：66038/66039/66040/65019 补「怎么办」，普通错误带错误码', () => {
  assert.match(errorText({ code: 66038, message: '该会计期间已结账' }), /反结账/);
  assert.match(errorText({ code: 66039, message: '状态冲突' }), /只有已过账/);
  assert.match(errorText({ code: 66040, message: '已有在跑的处置审批' }), /流程中心/);
  assert.match(errorText({ code: 65019, message: '发起人不能审批' }), /内控/);
  assert.match(errorText({ code: 10001, message: '参数错误' }), /错误码 10001/);
  assert.match(errorText('网络错误'), /网络错误/);
  assert.match(errorText(null), /没有返回说明/);
});

test('66046：停用 / 启用状态冲突有专属「怎么办」，不被 66040 的处置文案抢走', () => {
  const hint = ASSET_ERROR_HINTS[66046];
  assert.ok(typeof hint === 'string' && hint.trim().length > 0, '缺 66046 的提示文案');
  assert.match(hint, /停用 \/ 启用/);
  assert.match(hint, /作废处置/);
  assert.match(hint, /重复通知/);
  // 页面只走 errorText，禁止把 66046 硬编码在 .vue 里
  assert.match(errorText({ code: 66046, message: '该资产当前状态不允许本次停用 / 启用' }), /作废处置/);
  assert.match(errorText({ code: 66046, message: '状态冲突' }), /66046/);
});

test('changeSummary：状态留痕行（部门 / 使用人全 null）自动输出「在役 → 闲置」', () => {
  const base = {
    id: 'c1',
    change_label: '停用',
    change_date: '2026-11-07',
    from_dept_name: null,
    to_dept_name: null,
    from_user_name: null,
    to_user_name: null,
    voucher_id: null,
    approval_instance_id: null,
    operator_name: '管理员',
    remark: null,
    created_at: '2026-11-07T02:00:00Z',
  };
  assert.equal(
    changeSummary({ ...base, change_type: 'IDLE', from_status: 'IN_USE', to_status: 'IDLE', reason: '设备封存' }),
    '在役 → 闲置',
  );
  assert.equal(
    changeSummary({ ...base, change_type: 'ENABLE', from_status: 'IDLE', to_status: 'IN_USE', reason: '恢复使用' }),
    '闲置 → 在役',
  );
});

test('原因校验：普通原因非空，红冲原因 ≥ 2 字', () => {
  assert.ok(requireReasonError('   ', '转移原因'));
  assert.equal(requireReasonError('组织调整', '转移原因'), null);
  assert.ok(validateReverseReason('错'));
  assert.equal(validateReverseReason('录错'), null);
  assert.ok(validateReverseReason('x'.repeat(256)));
});

// ==================== ⑦ 分类 ====================

test('validateCategoryForm：编码可留空 / 月数 / 残值率 / 科目必填', () => {
  const empty = validateCategoryForm(emptyCategoryForm());
  // CODE-07：编码**留空交给后端取号**（AC-01 形态），前端不再拦「分类编码必填」
  assert.equal(empty.category_code, undefined);
  assert.ok(empty.category_name);
  assert.ok(empty.useful_life_months);
  assert.ok(empty.asset_account_id);
  assert.ok(empty.accum_account_id);
  assert.ok(empty.expense_account_id);

  const filled = {
    ...emptyCategoryForm(),
    category_code: 'ELEC',
    category_name: '电子设备',
    useful_life_months: 36,
    salvage_rate: '0.05',
    asset_account_id: 'a',
    accum_account_id: 'b',
    expense_account_id: 'c',
  };
  assert.deepEqual(validateCategoryForm(filled), {});
  // CODE-07：字符集改为「字母 / 数字 / - _ .」，统一大写后校验（后端 seq::validate_manual_code 同口径）：
  // 小写直接放行（落库前转大写），取号器自己的 `AC-01` 与企业自有码 `CAT-BUILDING` 都要能过。
  assert.equal(validateCategoryForm({ ...filled, category_code: 'elec' }).category_code, undefined);
  assert.equal(validateCategoryForm({ ...filled, category_code: 'AC-01' }).category_code, undefined);
  assert.equal(
    validateCategoryForm({ ...filled, category_code: 'CAT-BUILDING' }).category_code,
    undefined,
  );
  // 真不合法的仍拦：空格不在字符集里
  assert.ok(validateCategoryForm({ ...filled, category_code: 'CAT BUILDING' }).category_code);
  assert.ok(validateCategoryForm({ ...filled, useful_life_months: 601 }).useful_life_months);
  assert.ok(validateCategoryForm({ ...filled, salvage_rate: '1' }).salvage_rate);
});

test('分类逻辑：科目只列末级、回填表单、方法译名', () => {
  const accounts = [
    { id: 'a1', account_code: '1601', account_name: '固定资产', is_leaf: true, status: 1 },
    { id: 'a2', account_code: '1602', account_name: '累计折旧', is_leaf: false, status: 1 },
  ] as never[];
  const options = accountOptions(accounts);
  assert.equal(options.length, 1);
  assert.equal(options[0].label, '1601 固定资产');
  assert.equal(accountLabel(accounts, 'a1'), '1601 固定资产');
  assert.equal(accountLabel(accounts, null), '—');

  const form = categoryToForm(category());
  assert.equal(form.useful_life_months, 36);
  assert.equal(form.salvage_rate, '0.0500');
  assert.equal(form.remark, '');
  assert.equal(methodLabel('STRAIGHT_LINE'), '平均年限法');
  assert.equal(categoryStatusLabel(1), '启用');
  assert.equal(categoryStatusLabel(0), '停用');
});

// ==================== ⑧ 菜单 / 路由镜像（n08 的反向护栏） ====================

test('迁移里的三个菜单 component 都真实存在视图文件', () => {
  const migrations = [
    'backend/migrations/business/V0229__erp_asset_tables.up.sql',
    'backend/migrations/business/V0230__erp_depreciation_tables.up.sql',
  ]
    .map((relative) => resolve(FRONTEND_ROOT, '..', relative))
    .map((file) => {
      assert.ok(existsSync(file), `迁移文件不存在：${file}`);
      return readFileSync(file, 'utf8');
    })
    .join('\n');

  for (const component of [
    'business/erp/asset/index',
    'business/erp/asset-category/index',
    'business/erp/depreciation/index',
  ]) {
    assert.ok(migrations.includes(`'${component}'`), `迁移里没有菜单 component：${component}`);
    const view = resolve(FRONTEND_ROOT, 'src/views', `${component}.vue`);
    assert.ok(existsSync(view), `菜单 component 指向的视图文件不存在：${view}`);
  }
});

test('router/index.ts 注册了三条固定资产路由（路径按菜单，不改路由）', () => {
  const routerSource = readFileSync(resolve(FRONTEND_ROOT, 'src/router/index.ts'), 'utf8');
  for (const [path, view] of [
    ['business/erp/asset', '@/views/business/erp/asset/index.vue'],
    ['business/erp/asset-category', '@/views/business/erp/asset-category/index.vue'],
    ['business/erp/depreciation', '@/views/business/erp/depreciation/index.vue'],
  ]) {
    assert.ok(routerSource.includes(`path: '${path}'`), `缺少路由：${path}`);
    assert.ok(routerSource.includes(`component: () => import('${view}')`), `缺少组件映射：${view}`);
  }
});

// ==================== ⑨ 停用 / 启用接线（源码护栏） ====================

test('停用 / 启用接线：api 路径写全 /api（含 id 与 /status），页面给出两个 testid', () => {
  const apiSource = readFileSync(resolve(FRONTEND_ROOT, 'src/api/business/erp/asset.ts'), 'utf8');
  // 路径必须走 BASE（= /api/business/erp）；写死 '/business/...' 会命中 SPA fallback 返回 HTML
  assert.ok(apiSource.includes('/asset/${id}/status'), 'api 层缺少 /asset/{id}/status 调用');
  assert.match(apiSource, /target_status: 'IDLE' \| 'IN_USE'/);

  const viewSource = readFileSync(
    resolve(FRONTEND_ROOT, 'src/views/business/erp/asset/index.vue'),
    'utf8',
  );
  assert.ok(viewSource.includes('asset-btn-idle-'), '列表操作列缺少停用按钮 testid');
  assert.ok(viewSource.includes('asset-btn-enable-'), '列表操作列缺少启用按钮 testid');
  assert.ok(viewSource.includes('data-testid="asset-status-reason"'), '弹窗缺少原因字段');
  assert.ok(viewSource.includes('data-testid="asset-status-date"'), '弹窗缺少变动日期字段');
  // 权限码复用 asset:update（禁止新增 PERM.idle / PERM.enable 之类的新码）
  assert.ok(viewSource.includes("update: 'business:erp:asset:update'"), '权限码口径变了');
  assert.ok(!/PERM\.(idle|enable|status)/i.test(viewSource), '不许新增权限码');
  // 66046 只在 asset-logic 里，不硬编码在页面
  assert.ok(!viewSource.includes('66046'), '66046 不该硬编码在页面里（走 ASSET_ERROR_HINTS）');
});
