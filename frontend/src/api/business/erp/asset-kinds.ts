/**
 * 固定资产 / 折旧的**词表**（纯常量，零依赖）。
 *
 * # 为什么要单独一个文件
 *
 * 词表要在两个地方用：API 层（`asset.ts` / `depreciation.ts`，带 axios）和
 * 纯逻辑层（`asset-logic.ts`，要被 `tsx --test` 在 node 里直接跑）。
 * 如果逻辑层从 `asset.ts` 取常量，就会顺着 import 链把 `@/api/request`
 * 拉进 node —— 那里顶层读 `import.meta.env`，tsx 下直接 `TypeError`。
 * 所以词表独立成「零 import」的模块：谁都能引，谁都不带副作用。
 *
 * # 唯一真相
 *
 * 这些取值逐字抄后端 DDL 的 `CHECK` 与 Rust 常量：
 *
 * | 轴 | 后端出处 |
 * |----|----------|
 * | 资产状态 4 值 | `ck_biz_fa_status` / `asset_dto::STATUS_*` |
 * | 变动类型 5 值 | `ck_biz_fac_type` / `asset_dto::CHANGE_*` |
 * | 处置类型 3 值 | `ck_biz_fa_disposal` / `asset_compute::DisposalKind` |
 * | 购入口径 2 值 | `ck_biz_fa_buy_mode` |
 * | 卡片来源 3 值 | `ck_biz_fa_source` |
 * | 折旧批次 4 值 | `depreciation_model::STATUS_*` |
 * | 计提结果 4 值 | `asset_dto::DepRunKind` |
 *
 * 加值必须先改后端（`biz_type_domain` 巡检会拦越界的值），前端只是镜像。
 */

// ==================== 资产 ====================

/** 资产状态（`ck_biz_fa_status`，4 值）。 */
export const ASSET_STATUSES = ['IN_USE', 'IDLE', 'DISPOSED', 'CANCELLED'] as const;

/** 资产状态。 */
export type AssetStatus = (typeof ASSET_STATUSES)[number];

/** 资产状态 → 中文（与 Rust `asset_dto::status_label` 逐字一致）。 */
export const ASSET_STATUS_LABELS: Readonly<Record<AssetStatus, string>> = {
  IN_USE: '在役',
  IDLE: '闲置',
  DISPOSED: '已处置',
  CANCELLED: '已作废',
};

/** 变动类型（`ck_biz_fac_type`，5 值；`biz_fixed_asset_change` 只增不改）。 */
export const CHANGE_TYPES = ['TRANSFER', 'CUSTODIAN', 'DISPOSAL', 'ENABLE', 'IDLE'] as const;

/** 变动类型。 */
export type ChangeType = (typeof CHANGE_TYPES)[number];

/** 变动类型 → 中文（与 Rust `asset_dto::change_label` 逐字一致）。 */
export const CHANGE_TYPE_LABELS: Readonly<Record<ChangeType, string>> = {
  TRANSFER: '部门转移',
  CUSTODIAN: '使用人变更',
  DISPOSAL: '处置',
  ENABLE: '启用',
  IDLE: '停用',
};

/** 处置类型（`ck_biz_fa_disposal`，3 值）。 */
export const DISPOSAL_TYPES = ['SALE', 'SCRAP', 'LOSS'] as const;

/** 处置类型。 */
export type DisposalType = (typeof DISPOSAL_TYPES)[number];

/** 处置类型 → 中文。 */
export const DISPOSAL_TYPE_LABELS: Readonly<Record<DisposalType, string>> = {
  SALE: '出售',
  SCRAP: '报废',
  LOSS: '盘亏',
};

/** 购入口径（`ck_biz_fa_buy_mode`）：直接购入 / 由收货单转入。 */
export const BUY_MODES = ['DIRECT_BUY', 'FROM_STOCK'] as const;

/** 购入口径。 */
export type BuyMode = (typeof BUY_MODES)[number];

/** 卡片来源（`ck_biz_fa_source`）。 */
export const ASSET_SOURCE_TYPES = ['MANUAL', 'PURCHASE_ORDER', 'STOCK_IN'] as const;

/** 卡片来源。 */
export type AssetSourceType = (typeof ASSET_SOURCE_TYPES)[number];

/** 卡片来源 → 中文。 */
export const ASSET_SOURCE_LABELS: Readonly<Record<AssetSourceType, string>> = {
  MANUAL: '手工录入',
  PURCHASE_ORDER: '采购单',
  STOCK_IN: '收货单',
};

/** 付款方式：赊购贷 2202 / 现付贷 1002。 */
export const PAY_MODES = ['CREDIT', 'CASH'] as const;

/** 付款方式。 */
export type PayMode = (typeof PAY_MODES)[number];

/** 付款方式 → 中文。 */
export const PAY_MODE_LABELS: Readonly<Record<PayMode, string>> = {
  CREDIT: '赊购（贷应付账款）',
  CASH: '现付（贷银行存款）',
};

/** 折旧方法（本期只有平均年限法，`ck_biz_asset_cat_method` 只放一个值）。 */
export const DEPRECIATION_METHODS = ['STRAIGHT_LINE'] as const;

/** 折旧方法。 */
export type DepreciationMethod = (typeof DEPRECIATION_METHODS)[number];

/** 折旧方法 → 中文。 */
export const DEPRECIATION_METHOD_LABELS: Readonly<Record<DepreciationMethod, string>> = {
  STRAIGHT_LINE: '平均年限法',
};

// ==================== 折旧批次 ====================

/** 折旧批次状态（4 值，与 `depreciation_model::STATUS_*` 逐字一致）。 */
export const DEP_BATCH_STATUSES = ['CALCULATED', 'POSTED', 'REVERSED', 'SKIPPED'] as const;

/** 折旧批次状态。 */
export type DepBatchStatus = (typeof DEP_BATCH_STATUSES)[number];

/** 批次状态 → 中文。 */
export const DEP_BATCH_STATUS_LABELS: Readonly<Record<DepBatchStatus, string>> = {
  CALCULATED: '已计算',
  POSTED: '已过账',
  REVERSED: '已红冲',
  SKIPPED: '已跳过',
};

/** 批次状态主题（表格 tag 用）。 */
export const DEP_BATCH_STATUS_THEMES: Readonly<
  Record<DepBatchStatus, 'default' | 'primary' | 'success' | 'warning' | 'danger'>
> = {
  CALCULATED: 'warning',
  POSTED: 'success',
  REVERSED: 'danger',
  SKIPPED: 'default',
};

/** 计提结果类型（`RunDepreciationRes.outcome` 的四个取值）。 */
export const DEP_RUN_OUTCOMES = ['POSTED', 'EXISTS', 'SKIPPED', 'REVERSED_RERUN'] as const;

/** 计提结果类型。 */
export type DepRunOutcomeKind = (typeof DEP_RUN_OUTCOMES)[number];

/** 计提结果 → 中文（页面 toast / 结果条用）。 */
export const DEP_RUN_OUTCOME_LABELS: Readonly<Record<DepRunOutcomeKind, string>> = {
  POSTED: '计提成功',
  EXISTS: '本期已计提（幂等，不重提）',
  SKIPPED: '本期无应提折旧',
  REVERSED_RERUN: '红冲后重提（复用同一批次行）',
};
