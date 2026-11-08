/**
 * 固定资产三页的纯逻辑层（卡片 / 分类 / 折旧计提）。
 *
 * 为什么不写在 `.vue` 里：三个页面里真正会出错的都是「口径」——
 * 金额怎么比、月折旧额怎么预览、本月应提怎么估、处置弹窗上那三个数从哪来、
 * 哪些字段能改。这些用 `vue-tsc` 抓不到，但用 node 跑纯函数能逐条钉死
 * （`tests/unit/fin2-asset.test.ts`），页面只管渲染。
 *
 * # 金额口径（本文件最重要的一节）
 *
 * 后端所有金额都是 **numeric(18,4) → JSON 字符串**（`"2638.8889"`）。
 * 前端一律用「四位定点的 `bigint`（1 单位 = 0.0001）」处理：
 *
 * ```text
 * "2638.8889" ──parseMoney──→ 26388889n
 * ```
 *
 * 比较 / 加减全在 bigint 上做，只有最终展示才降到 2 位小数。
 * **不要** `Number(x) === Number(y)`：`0.1 + 0.2 !== 0.3` 这类误差在
 * 「应提 vs 已提」的对账行上就是一条假的差异。
 *
 * # 折旧预览与后端的一致性
 *
 * 建卡表单的「月折旧额 / 预计提完日期」是**展示口径**，必须与后端
 * `asset_compute::{residual_value, depreciable_base, monthly_depreciation}` 同算法：
 *
 * - 可折旧总额 = round(原值 × (1 − 残值率), 4)
 * - 月折旧额 = round(可折旧总额 / 使用月数, 4)（ROUND_HALF_UP）
 * - 起提日 = 购入日的**次月 1 日**（当月增加当月不提）
 * - 最后一个月取「可折旧总额 − 月折旧额 × (月数 − 1)」补齐尾差
 *
 * 建卡成功后页面一律重读接口返回的 `monthly_depreciation`，预览值不参与任何入账。
 */

import dayjs from 'dayjs';

import { masterCodeError } from '../shared/master-code';
import type {
  AssetChange,
  AssetDetail,
  AssetRow,
  AssetStatus,
  AssetCreatePayload,
  AssetDisposePayload,
  DisposalType,
  PayMode,
  AssetSourceType,
} from '@/api/business/erp/asset';
import {
  ASSET_STATUS_LABELS,
  DISPOSAL_TYPE_LABELS,
  ASSET_SOURCE_LABELS,
  CHANGE_TYPE_LABELS,
} from '@/api/business/erp/asset-kinds';
import type { DepBatch } from '@/api/business/erp/depreciation';
import { DEP_BATCH_STATUS_LABELS, DEP_BATCH_STATUS_THEMES } from '@/api/business/erp/asset-kinds';

// ==================== 四位定点金额 ====================

/** 金额标度：后端 numeric(18,4)，1 单位 = 0.0001。 */
export const MONEY_SCALE = 4;

/** 标度因子。 */
const MONEY_FACTOR = 10n ** BigInt(MONEY_SCALE);

/**
 * 把后端金额字符串解析成四位定点 bigint。
 *
 * 支持 `"2638.8889"`、`"-1.5"`、`"1,234.5"`（千分位）、数字、null / undefined（→ 0）。
 * 超过 4 位小数的部分按 ROUND_HALF_UP 进位（后端不会给，但手工输入会）。
 */
export function parseMoney(raw: string | number | null | undefined): bigint {
  if (raw === null || raw === undefined) {
    return 0n;
  }
  const text = String(raw).trim().replace(/,/g, '');
  if (!text) {
    return 0n;
  }
  const match = /^([+-]?)(\d*)(?:\.(\d*))?$/.exec(text);
  if (!match) {
    return 0n;
  }
  const sign = match[1] === '-' ? -1n : 1n;
  const intPart = match[2] || '0';
  const fracRaw = match[3] || '';
  const frac = fracRaw.slice(0, MONEY_SCALE).padEnd(MONEY_SCALE, '0');
  let units = BigInt(intPart) * MONEY_FACTOR + BigInt(frac);
  // 第 5 位及以后：≥5 进位（half-up，正负同规则）
  const rest = fracRaw.slice(MONEY_SCALE);
  if (rest && Number(rest[0]) >= 5) {
    units += 1n;
  }
  return sign * units;
}

/** 四位定点 bigint → 普通十进制字符串（`26388889n` → `"2638.8889"`）。 */
export function moneyToPlain(units: bigint): string {
  const negative = units < 0n;
  const abs = negative ? -units : units;
  const int = abs / MONEY_FACTOR;
  const frac = (abs % MONEY_FACTOR).toString().padStart(MONEY_SCALE, '0');
  return `${negative ? '-' : ''}${int.toString()}.${frac}`;
}

/** 加减比较（全部在 bigint 上，不经过 Number）。 */
export function addMoney(a: string | number | null | undefined, b: string | number | null | undefined): string {
  return moneyToPlain(parseMoney(a) + parseMoney(b));
}

/** 减。 */
export function subMoney(a: string | number | null | undefined, b: string | number | null | undefined): string {
  return moneyToPlain(parseMoney(a) - parseMoney(b));
}

/** 比较：a < b → -1，a = b → 0，a > b → 1。 */
export function cmpMoney(a: string | number | null | undefined, b: string | number | null | undefined): -1 | 0 | 1 {
  const left = parseMoney(a);
  const right = parseMoney(b);
  if (left === right) {
    return 0;
  }
  return left < right ? -1 : 1;
}

/** 是否为零（判「提足 / 差异为 0」用）。 */
export function isZeroMoney(raw: string | number | null | undefined): boolean {
  return parseMoney(raw) === 0n;
}

/** ROUND_HALF_UP 整除（正负同规则，远离零）。 */
export function roundDiv(numerator: bigint, denominator: bigint): bigint {
  if (denominator === 0n) {
    return 0n;
  }
  const negative = numerator < 0n !== denominator < 0n;
  const absNum = numerator < 0n ? -numerator : numerator;
  const absDen = denominator < 0n ? -denominator : denominator;
  const quotient = absNum / absDen;
  const remainder = absNum % absDen;
  const rounded = remainder * 2n >= absDen ? quotient + 1n : quotient;
  return negative ? -rounded : rounded;
}

/**
 * 展示用金额（千分位 + 固定 2 位小数，四舍五入）。
 *
 * 只在渲染时用；判定「差异是否为 0」请用 {@link isZeroMoney} / {@link cmpMoney}，
 * 别拿格式化后的字符串比较（`"0.00"` vs `"-0.00"`）。
 */
export function formatMoney(raw: string | number | null | undefined, digits = 2): string {
  const units = parseMoney(raw);
  const step = 10n ** BigInt(MONEY_SCALE - digits);
  const rounded = roundDiv(units, step); // 已按 digits 缩放
  const negative = rounded < 0n;
  const abs = negative ? -rounded : rounded;
  const int = (abs / 10n ** BigInt(digits)).toString();
  const frac = digits > 0 ? (abs % 10n ** BigInt(digits)).toString().padStart(digits, '0') : '';
  const grouped = int.replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  const body = frac ? `${grouped}.${frac}` : grouped;
  // 四舍五入到 0 时不显示 "-0.00"
  return negative && abs !== 0n ? `-${body}` : body;
}

/** 残值率（`"0.0500"`）→ 百分比文案（`"5.00%"`）。 */
export function formatRate(raw: string | number | null | undefined): string {
  const rate = parseMoney(raw);
  // 0.05 → 5.00% ；rate 单位是 0.0001，×100 后要再除 100 => 直接乘 100 个单位
  const percent = rate * 100n; // 仍是 1e-4 标度 ⇒ 500n = 5.0000%
  return `${formatMoney(moneyToPlain(percent), 2)}%`;
}

// ==================== 日期 / 期间 ====================

/** 今天（本地时区，`YYYY-MM-DD`）。 */
export function todayString(): string {
  return dayjs().format('YYYY-MM-DD');
}

/** 日期 → 期间 `yyyy-MM`。 */
export function periodOfDate(date: string): string {
  return dayjs(date).format('YYYY-MM');
}

/** 期间加减月（`"2026-09"` + 1 → `"2026-10"`）。 */
export function addMonthsToPeriod(period: string, delta: number): string {
  return dayjs(`${period}-01`).add(delta, 'month').format('YYYY-MM');
}

/** 期间首日。 */
export function periodStart(period: string): string {
  return `${period}-01`;
}

/** 期间末日。 */
export function periodEnd(period: string): string {
  return dayjs(`${period}-01`).endOf('month').format('YYYY-MM-DD');
}

/** 两个期间相差的月数（`to - from`）。 */
export function monthsBetween(fromPeriod: string, toPeriod: string): number {
  const from = dayjs(`${fromPeriod}-01`);
  const to = dayjs(`${toPeriod}-01`);
  return (to.year() - from.year()) * 12 + (to.month() - from.month());
}

/** 上一会计月（`run` 不传期间时后端也用它）。 */
export function previousPeriod(today = todayString()): string {
  return dayjs(today).subtract(1, 'month').format('YYYY-MM');
}

/** 当前会计月。 */
export function currentPeriod(today = todayString()): string {
  return dayjs(today).format('YYYY-MM');
}

/** 日期只留 `MM-DD`。 */
export function shortDate(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  const text = String(value);
  return text.length >= 10 ? text.slice(5, 10) : text;
}

/** ISO8601 → `YYYY-MM-DD HH:mm`（后端 `created_at` 是 UTC 的 RFC3339，dayjs 自动转本地）。 */
export function formatDateTime(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return dayjs(value).format('YYYY-MM-DD HH:mm');
}

// ==================== 折旧预览（建卡表单实时显示） ====================

/** 预览入参（分类带出年限与残值率）。 */
export interface DepreciationPreviewInput {
  /** 原值（字符串） */
  original_value: string | null | undefined;
  /** 净残值率（`"0.0500"`） */
  salvage_rate: string | number | null | undefined;
  /** 使用月数 */
  useful_life_months: number | null | undefined;
  /** 购入日期 `YYYY-MM-DD` */
  purchase_date: string | null | undefined;
}

/** 预览结果（全部为后端口径的字符串金额）。 */
export interface DepreciationPreview {
  /** 预计净残值 = round(原值 × 残值率, 4) */
  residual_value: string;
  /** 可折旧总额 = 原值 − 预计净残值 */
  depreciable_base: string;
  /** 月折旧额 = round(可折旧总额 / 月数, 4) */
  monthly_depreciation: string;
  /** 起提日 = 购入次月 1 日 */
  start_date: string;
  /** 最后计提期间 `yyyy-MM` */
  last_period: string;
  /** 预计提完日（最后计提期间的最后一天） */
  finish_date: string;
  /** 最后一个月的金额（尾差补齐，不等于月折旧额） */
  last_month_amount: string;
  /** 使用月数 */
  months: number;
}

/**
 * 计算建卡表单的折旧预览（与后端 `asset_compute` 同算法）。
 *
 * 入参不完整（原值 ≤ 0 / 月数非正 / 日期非法）→ 返回 `null`，页面显示「填完自动算」。
 */
export function previewDepreciation(input: DepreciationPreviewInput): DepreciationPreview | null {
  const original = parseMoney(input.original_value);
  const months = Math.trunc(Number(input.useful_life_months ?? 0));
  const rate = parseMoney(input.salvage_rate ?? '0');
  const purchase = String(input.purchase_date ?? '').trim();
  if (original <= 0n || months <= 0 || months > 600 || !purchase || !dayjs(purchase).isValid()) {
    return null;
  }
  if (rate < 0n || rate >= MONEY_FACTOR) {
    return null;
  }
  // 净残值 = round(原值 × 残值率, 4)
  const residual = roundDiv(original * rate, MONEY_FACTOR);
  const base = original - residual;
  const monthly = roundDiv(base, BigInt(months));
  const lastMonth = base - monthly * BigInt(months - 1);
  const start = dayjs(purchase).add(1, 'month').startOf('month');
  const lastPeriodDate = start.add(months - 1, 'month');
  return {
    residual_value: moneyToPlain(residual),
    depreciable_base: moneyToPlain(base),
    monthly_depreciation: moneyToPlain(monthly),
    start_date: start.format('YYYY-MM-DD'),
    last_period: lastPeriodDate.format('YYYY-MM'),
    finish_date: lastPeriodDate.endOf('month').format('YYYY-MM-DD'),
    last_month_amount: moneyToPlain(lastMonth),
    months,
  };
}

/** 以分类参数为口径的预览入口（表单里直接传分类下拉选中的那一条）。 */
export function previewFromCategory(
  form: Pick<AssetFormState, 'original_value' | 'purchase_date'>,
  category: { salvage_rate: string; useful_life_months: number } | null | undefined,
): DepreciationPreview | null {
  if (!category) {
    return null;
  }
  return previewDepreciation({
    original_value: form.original_value,
    salvage_rate: category.salvage_rate,
    useful_life_months: category.useful_life_months,
    purchase_date: form.purchase_date,
  });
}

// ==================== 建卡表单 ====================

/** 建卡表单状态（字符串字段，提交前才转成 payload）。 */
export interface AssetFormState {
  /** 名称 */
  asset_name: string;
  /** 分类 */
  category_id: string;
  /** 规格 */
  spec: string;
  /** 使用部门 */
  use_dept_id: string;
  /** 使用人 */
  custodian_id: string;
  /** 存放地点 */
  location: string;
  /** 原值 */
  original_value: string;
  /** 可抵扣进项税 */
  tax_amount: string;
  /** 购入日期 */
  purchase_date: string;
  /** 来源类型 */
  source_type: AssetSourceType;
  /** 来源单 id */
  source_id: string;
  /** 来源单号 */
  source_no: string;
  /** 供应商 */
  supplier_id: string;
  /** 付款方式 */
  pay_mode: PayMode;
  /** 备注 */
  remark: string;
}

/** 空建卡表单（购入日期默认今天）。 */
export function emptyAssetForm(): AssetFormState {
  return {
    asset_name: '',
    category_id: '',
    spec: '',
    use_dept_id: '',
    custodian_id: '',
    location: '',
    original_value: '',
    tax_amount: '',
    purchase_date: todayString(),
    source_type: 'MANUAL',
    source_id: '',
    source_no: '',
    supplier_id: '',
    pay_mode: 'CASH',
    remark: '',
  };
}

/** 收货单（只取预填需要的字段，避免页面层依赖整个 StockInRow）。 */
export interface StockInSourceLike {
  /** id */
  id: string;
  /** 单号 */
  in_no: string;
  /** 不含税金额（资产入账原值口径） */
  total_amount: string | number;
}

/**
 * 从收货单建卡：金额预填 + 锁定（交互细节②）。
 *
 * - 原值取收货单的**不含税金额**（`total_amount`）：`FROM_STOCK` 是从存货科目转出，
 *   进项税在收货时就抵扣过了，所以税额必须是 0（后端 `validate_create` 会拒绝）；
 * - 返回的 `locked` 由表单据此把「原值 / 税额」两个输入置为只读，提示
 *   「金额来自收货单 X，要改请回收货单作废重开」。
 */
export function applyStockInSource(
  form: AssetFormState,
  stockIn: StockInSourceLike,
): { form: AssetFormState; locked: true } {
  return {
    form: {
      ...form,
      source_type: 'STOCK_IN',
      source_id: stockIn.id,
      source_no: stockIn.in_no,
      original_value: moneyToPlain(parseMoney(stockIn.total_amount)),
      tax_amount: '0.0000',
    },
    locked: true,
  };
}

/** 清掉来源（切回手工录入时调用，否则后端会因「MANUAL 带来源单」回 400）。 */
export function clearSource(form: AssetFormState): AssetFormState {
  return {
    ...form,
    source_type: 'MANUAL',
    source_id: '',
    source_no: '',
  };
}

/** 金额是否被来源锁住（收货单转入 / 采购单转入都锁）。 */
export function isAmountLocked(form: Pick<AssetFormState, 'source_type'>): boolean {
  return form.source_type !== 'MANUAL';
}

/** 表单校验错误（键 = 字段名，值 = 中文提示）。 */
export type AssetFormErrors = Partial<Record<keyof AssetFormState, string>>;

/**
 * 建卡校验（镜像后端 `asset_dto::validate_create` 的 400 文案）。
 *
 * 前端先拦一道不是为了替代后端，而是为了「错在哪一格」当场可见 ——
 * 后端只回一条 message，用户还得自己找。
 */
export function validateAssetForm(form: AssetFormState): AssetFormErrors {
  const errors: AssetFormErrors = {};
  const name = form.asset_name.trim();
  if (!name) {
    errors.asset_name = '资产名称必填';
  } else if (name.length > 200) {
    errors.asset_name = '资产名称最长 200 字符';
  }
  if (!form.category_id) {
    errors.category_id = '资产分类必填：折旧年限与残值率都从分类带出';
  }
  if (!form.use_dept_id) {
    errors.use_dept_id = '使用部门必填：折旧费用要按部门核算';
  }
  const original = parseMoney(form.original_value);
  if (original <= 0n) {
    errors.original_value = '原值必须大于 0';
  }
  const tax = parseMoney(form.tax_amount);
  if (tax < 0n) {
    errors.tax_amount = '可抵扣进项税不能为负';
  } else if (original > 0n && tax >= original) {
    errors.tax_amount = '可抵扣进项税必须小于原值';
  }
  if (form.source_type === 'STOCK_IN' && tax !== 0n) {
    errors.tax_amount = '收货单转入不能带可抵扣进项税（进项税在收货时已抵扣）';
  }
  if (!form.purchase_date || !dayjs(form.purchase_date).isValid()) {
    errors.purchase_date = '购入 / 入账日期必填（它决定开始计提日）';
  }
  if (form.source_type !== 'MANUAL' && !form.source_id.trim()) {
    errors.source_id = `来源为${ASSET_SOURCE_LABELS[form.source_type]}时必须选择来源单`;
  }
  if (form.source_type === 'MANUAL' && form.source_id.trim()) {
    errors.source_id = '手工建卡没有来源单';
  }
  if (form.remark.length > 2000) {
    errors.remark = '备注最长 2000 字符';
  }
  return errors;
}

/** 表单 → 建卡 payload（空串一律不发，避免后端把 `""` 当值）。 */
export function toCreatePayload(form: AssetFormState): AssetCreatePayload {
  const payload: AssetCreatePayload = {
    asset_name: form.asset_name.trim(),
    category_id: form.category_id,
    use_dept_id: form.use_dept_id,
    original_value: moneyToPlain(parseMoney(form.original_value)),
    tax_amount: moneyToPlain(parseMoney(form.tax_amount || '0')),
    purchase_date: form.purchase_date,
    source_type: form.source_type,
    buy_mode: form.source_type === 'STOCK_IN' ? 'FROM_STOCK' : 'DIRECT_BUY',
    pay_mode: form.pay_mode,
  };
  const optional: Array<[keyof AssetCreatePayload, string]> = [
    ['spec', form.spec],
    ['custodian_id', form.custodian_id],
    ['location', form.location],
    ['source_id', form.source_id],
    ['source_no', form.source_no],
    ['supplier_id', form.supplier_id],
    ['remark', form.remark],
  ];
  for (const [key, value] of optional) {
    const text = value.trim();
    if (text) {
      (payload as Record<string, unknown>)[key] = text;
    }
  }
  return payload;
}

// ==================== 状态与动作 ====================

/** 卡片是否可改（只有未处置卡可改；金额口径永远不可改）。 */
export function canEditAsset(asset: Pick<AssetRow, 'status'>): boolean {
  return asset.status === 'IN_USE' || asset.status === 'IDLE';
}

/** 是否可转移 / 换使用人（在役或闲置）。 */
export function canTransferAsset(asset: Pick<AssetRow, 'status'>): boolean {
  return asset.status === 'IN_USE' || asset.status === 'IDLE';
}

/** 是否可提交处置（在役或闲置，且没有在跑的处置审批）。 */
export function canDisposeAsset(
  asset: Pick<AssetRow, 'status'>,
  pendingDisposal: string | null | undefined,
): boolean {
  return canTransferAsset(asset) && !pendingDisposal;
}

/** 是否可作废处置（已处置且存在处置审批 / 处置记录）。 */
export function canCancelDispose(asset: Pick<AssetRow, 'status'>): boolean {
  return asset.status === 'DISPOSED';
}

/**
 * 是否可停用（只有在役的卡）。
 *
 * 已处置的要先作废处置；闲置的重复停用会被后端 409 `66046` 挡住 —— 按钮层面直接不给。
 */
export function canIdleAsset(asset: Pick<AssetRow, 'status'>): boolean {
  return asset.status === 'IN_USE';
}

/** 是否可启用（只有闲置的卡）。 */
export function canEnableAsset(asset: Pick<AssetRow, 'status'>): boolean {
  return asset.status === 'IDLE';
}

/** 是否可删（未处置的卡才允许软删；已处置的卡是台账凭证，不能删）。 */
export function canDeleteAsset(asset: Pick<AssetRow, 'status'>): boolean {
  return canTransferAsset(asset);
}

/** 资产状态中文（后端没给 `status_label` 时兜底）。 */
export function assetStatusLabel(raw: string | null | undefined): string {
  if (!raw) {
    return '—';
  }
  return ASSET_STATUS_LABELS[raw as AssetStatus] ?? String(raw);
}

/** 资产状态 tag 主题。 */
export function assetStatusTheme(
  raw: string | null | undefined,
): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  switch (raw) {
    case 'IN_USE':
      return 'success';
    case 'IDLE':
      return 'warning';
    case 'DISPOSED':
      return 'default';
    case 'CANCELLED':
      return 'danger';
    default:
      return 'default';
  }
}

/** 变动类型中文（后端 `change_label` 的兜底）。 */
export function changeLabel(raw: string | null | undefined): string {
  if (!raw) {
    return '—';
  }
  return (CHANGE_TYPE_LABELS as Record<string, string>)[raw] ?? String(raw);
}

/** 处置类型中文。 */
export function disposalTypeLabel(raw: string | null | undefined): string {
  if (!raw) {
    return '—';
  }
  return (DISPOSAL_TYPE_LABELS as Record<string, string>)[raw] ?? String(raw);
}

/** 批次状态 tag 主题（卡片详情的折旧明细列用）。 */
export function depBatchStatusTheme(
  raw: string | null | undefined,
): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  if (!raw) {
    return 'default';
  }
  const themes = DEP_BATCH_STATUS_THEMES as Record<
    string,
    'default' | 'primary' | 'success' | 'warning' | 'danger'
  >;
  return themes[raw] ?? 'default';
}

/** 批次状态中文。 */
export function depBatchStatusLabel(raw: string | null | undefined): string {
  if (!raw) {
    return '—';
  }
  return (DEP_BATCH_STATUS_LABELS as Record<string, string>)[raw] ?? String(raw);
}

// ==================== 处置弹窗的三个数（交互细节④） ====================

/** 处置决策摘要（净值 / 已提月数 / 剩余可提，全部以接口返回为准）。 */
export interface DisposeSummary {
  /** 原值 */
  original_value: string;
  /** 累计折旧 */
  accumulated_depreciation: string;
  /** 净值 = 原值 − 累计折旧（接口的 `net_value`，不自己减） */
  net_value: string;
  /** 已提月数 */
  months_done: number;
  /** 使用月数 */
  useful_life_months: number;
  /** 剩余可提月数（详情顶层 `months_left`） */
  months_left: number;
  /** 剩余可提金额（详情顶层 `amount_left`） */
  amount_left: string;
}

/** 从详情取处置摘要（页面绝不自己算净值，直接用接口值）。 */
export function disposeSummaryOf(detail: AssetDetail): DisposeSummary {
  const asset = detail.asset;
  return {
    original_value: asset.original_value,
    accumulated_depreciation: asset.accumulated_depreciation,
    net_value: asset.net_value,
    months_done: asset.depreciation_months_done,
    useful_life_months: asset.useful_life_months,
    months_left: detail.months_left,
    amount_left: detail.amount_left,
  };
}

/**
 * 出售损益参考（**仅供决策展示**，不参与入账）。
 *
 * 口径：不含税售价 − 净值。后端真正入账的分录是
 * 借 1606（净值全部）/ 贷 1606（售价 + 销项税）/ 差额进 5301（资产处置损益），
 * 所以这里只回答「现在卖大概亏多少」。
 */
export function disposalGainPreview(netValue: string, salePrice: string): string {
  return subMoney(salePrice || '0', netValue);
}

/** 处置表单状态。 */
export interface DisposeFormState {
  /** 处置类型 */
  disposal_type: DisposalType;
  /** 处置日期 */
  disposal_date: string;
  /** 原因 */
  reason: string;
  /** 出售价（不含税） */
  sale_price: string;
  /** 销项税 */
  output_tax: string;
}

/** 空处置表单。 */
export function emptyDisposeForm(): DisposeFormState {
  return {
    disposal_type: 'SCRAP',
    disposal_date: todayString(),
    reason: '',
    sale_price: '',
    output_tax: '',
  };
}

/** 处置校验（镜像后端 `validate_dispose`）。 */
export function validateDisposeForm(form: DisposeFormState): Partial<Record<keyof DisposeFormState, string>> {
  const errors: Partial<Record<keyof DisposeFormState, string>> = {};
  if (!form.reason.trim()) {
    errors.reason = '处置原因必填（内控留痕要能回答「为什么处置」）';
  } else if (form.reason.trim().length > 255) {
    errors.reason = '处置原因最长 255 字符';
  }
  if (!form.disposal_date || !dayjs(form.disposal_date).isValid()) {
    errors.disposal_date = '处置日期必填';
  }
  const price = parseMoney(form.sale_price);
  const tax = parseMoney(form.output_tax);
  if (price < 0n) {
    errors.sale_price = '出售价不能为负';
  }
  if (tax < 0n) {
    errors.output_tax = '销项税不能为负';
  }
  if (form.disposal_type !== 'SALE' && (price !== 0n || tax !== 0n)) {
    errors.sale_price = `${DISPOSAL_TYPE_LABELS[form.disposal_type]}处置不能有出售价 / 销项税`;
  }
  if (form.disposal_type === 'SALE' && price === 0n && tax === 0n) {
    errors.sale_price = '出售处置必须给出售价（零价请走报废 / 盘亏）';
  }
  return errors;
}

/** 处置表单 → payload（非 SALE 不发送金额字段）。 */
export function toDisposePayload(form: DisposeFormState): AssetDisposePayload {
  const payload: AssetDisposePayload = {
    disposal_type: form.disposal_type,
    disposal_date: form.disposal_date,
    reason: form.reason.trim(),
  };
  if (form.disposal_type === 'SALE') {
    payload.sale_price = moneyToPlain(parseMoney(form.sale_price || '0'));
    payload.output_tax = moneyToPlain(parseMoney(form.output_tax || '0'));
  }
  return payload;
}

/** 变动 / 处置 / 红冲的原因校验（后端 `require_reason`：非空且 ≤ 255）。 */
export function requireReasonError(reason: string, field = '原因'): string | null {
  const text = reason.trim();
  if (!text) {
    return `${field}必填（留痕要能回答「为什么」）`;
  }
  if (text.length > 255) {
    return `${field}最长 255 字符`;
  }
  return null;
}

/** 红冲原因（后端 `reverse_batch` 要求 ≥ 2 字）。 */
export const MIN_REVERSE_REASON_CHARS = 2;

/** 红冲原因校验。 */
export function validateReverseReason(reason: string): string | null {
  const text = reason.trim();
  if (text.length < MIN_REVERSE_REASON_CHARS) {
    return `红冲原因至少 ${MIN_REVERSE_REASON_CHARS} 个字`;
  }
  if (text.length > 255) {
    return '红冲原因最长 255 字符';
  }
  return null;
}

// ==================== 本月应提 / 已提 / 差异（交互细节③） ====================

/**
 * 单台资产在某期间的应提额（**按月序数**推，与后端的差异见下）。
 *
 * - 第 n 个月（n = 起提期间到目标期间的月序，1 起）：`n < 月数` → 月折旧额；
 *   `n === 月数` → 可折旧总额 − 月折旧额 × (月数 − 1)（尾差补齐）；`n > 月数` → 0；
 * - 状态不是 `IN_USE / IDLE` → 0（处置后不再计提）；
 * - 处置日在目标期间**之前** → 0；在期间内 → 仍提整月（与后端 `disposal_date >= 期初` 一致）；
 * - 起提日晚于目标期间末 → 0（当月增加当月不提）。
 *
 * ⚠️ 这是**展示估算**：如果历史上有期间漏提（补提）或红冲后未重提，
 * 同口径下后端实际提的金额会和这里不同 —— 那种差异正是页面上「差异」行要暴露的。
 */
export function periodAmountOf(asset: AssetRow, periodCode: string): string {
  if (asset.status !== 'IN_USE' && asset.status !== 'IDLE') {
    return '0.0000';
  }
  if (!asset.start_depreciation_date) {
    return '0.0000';
  }
  const startPeriod = periodOfDate(asset.start_depreciation_date);
  if (monthsBetween(startPeriod, periodCode) < 0) {
    return '0.0000';
  }
  if (asset.disposal_date && periodOfDate(asset.disposal_date) < periodCode) {
    return '0.0000';
  }
  const months = Math.trunc(Number(asset.useful_life_months ?? 0));
  if (months <= 0) {
    return '0.0000';
  }
  const nth = monthsBetween(startPeriod, periodCode) + 1;
  if (nth > months) {
    return '0.0000';
  }
  const base = parseMoney(asset.depreciable_base);
  const monthly = parseMoney(asset.monthly_depreciation);
  const amount = nth === months ? base - monthly * BigInt(months - 1) : monthly;
  if (amount <= 0n) {
    return '0.0000';
  }
  // 尾差补齐不会超过可折旧总额
  return moneyToPlain(amount > base ? base : amount);
}

/** 三行汇总。 */
export interface DepreciationSummary {
  /** 期间 */
  period_code: string;
  /** 本月应提（按卡片参数逐台测算） */
  expected_amount: string;
  /** 参与计提的资产台数 */
  expected_count: number;
  /** 已提（该期间 POSTED 批次合计） */
  posted_amount: string;
  /** 已提台数 */
  posted_count: number;
  /** 差异 = 应提 − 已提（> 0 少提，< 0 多提） */
  diff_amount: string;
  /** 差异方向 */
  diff_level: 'OK' | 'UNDER' | 'OVER';
  /** 该期间 SKIPPED 批次的跳过原因 */
  skipped_reasons: string[];
  /** 该期间是否存在 POSTED 批次 */
  has_posted_batch: boolean;
}

/** 汇总入参。 */
export interface DepreciationSummaryInput {
  /** 资产台账（分页全量拉取后的行） */
  assets: AssetRow[];
  /** 该期间的批次（页面上只需该期间的；多传也只认同期间 POSTED） */
  batches: DepBatch[];
  /** 期间 `yyyy-MM` */
  period_code: string;
}

/** 计算「本月应提 / 已提 / 差异」。 */
export function summarizeDepreciation(input: DepreciationSummaryInput): DepreciationSummary {
  const period = input.period_code;
  let expected = 0n;
  let expectedCount = 0;
  for (const asset of input.assets) {
    const amount = parseMoney(periodAmountOf(asset, period));
    if (amount !== 0n) {
      expected += amount;
      expectedCount += 1;
    }
  }
  let posted = 0n;
  let postedCount = 0;
  let hasPosted = false;
  const skipped: string[] = [];
  for (const batch of input.batches) {
    if (batch.period_code !== period) {
      continue;
    }
    if (batch.status === 'POSTED') {
      posted += parseMoney(batch.total_amount);
      postedCount += Math.trunc(Number(batch.asset_count ?? 0));
      hasPosted = true;
    }
    if (batch.status === 'SKIPPED') {
      const reason = (batch.skip_reason ?? '').trim();
      if (reason) {
        skipped.push(reason);
      }
    }
  }
  const diff = expected - posted;
  return {
    period_code: period,
    expected_amount: moneyToPlain(expected),
    expected_count: expectedCount,
    posted_amount: moneyToPlain(posted),
    posted_count: postedCount,
    diff_amount: moneyToPlain(diff),
    diff_level: diff === 0n ? 'OK' : diff > 0n ? 'UNDER' : 'OVER',
    skipped_reasons: skipped,
    has_posted_batch: hasPosted,
  };
}

// ==================== 错误文案 ====================

/** 本模块涉及的后端错误码 → 前端补的「怎么办」。 */
export const ASSET_ERROR_HINTS: Readonly<Record<number, string>> = {
  66038: '该会计期间已结账，不能补提折旧：请先反结账，或改在未结账期间计提',
  66039: '批次当前状态不允许该操作：只有已过账（POSTED）的批次可以红冲',
  66040: '该资产已有在跑的处置审批：请先在流程中心处理完，或作废该审批',
  66046: '该资产当前状态不允许本次停用 / 启用：已处置的请先作废处置；重复点击不会重复通知（刷新即可）',
  65019: '发起人不能审批自己提交的处置：请换有审批权的账号（内控要求）',
};

/** 统一错误文案：后端 message（+错误码）为主，命中的话补一句「怎么办」。 */
export function errorText(error: unknown): string {
  let message = '';
  let code = 0;
  if (error && typeof error === 'object') {
    const like = error as { message?: string; code?: number };
    message = (like.message ?? '').trim();
    code = typeof like.code === 'number' ? like.code : 0;
  } else if (typeof error === 'string') {
    message = error.trim();
  }
  const hint = code ? (ASSET_ERROR_HINTS[code] ?? '') : '';
  if (message && hint) {
    return `${message}（${code}）—— ${hint}`;
  }
  if (message) {
    return code > 0 ? `${message}（错误码 ${code}）` : message;
  }
  if (hint) {
    return `错误码 ${code} —— ${hint}`;
  }
  return '操作失败：接口没有返回说明，请刷新后重试';
}

// ==================== 变动时间线文案 ====================

/** 变动记录 → 一句人话（时间线行）。 */
export function changeSummary(change: AssetChange): string {
  const parts: string[] = [];
  if (change.from_dept_name || change.to_dept_name) {
    parts.push(`${change.from_dept_name ?? '—'} → ${change.to_dept_name ?? '—'}`);
  }
  if (change.from_user_name || change.to_user_name) {
    parts.push(`${change.from_user_name ?? '—'} → ${change.to_user_name ?? '—'}`);
  }
  if (change.from_status || change.to_status) {
    parts.push(`${assetStatusLabel(change.from_status)} → ${assetStatusLabel(change.to_status)}`);
  }
  return parts.join('；') || change.reason;
}

/** 分类表单状态。 */
export interface CategoryFormState {
  /** 编码 */
  category_code: string;
  /** 名称 */
  category_name: string;
  /** 使用月数 */
  useful_life_months: number | undefined;
  /** 净残值率 */
  salvage_rate: string;
  /** 原值科目 */
  asset_account_id: string;
  /** 累计折旧科目 */
  accum_account_id: string;
  /** 折旧费用科目 */
  expense_account_id: string;
  /** 状态 */
  status: number;
  /** 排序 */
  sort_order: number;
  /** 备注 */
  remark: string;
}

/** 空分类表单。 */
export function emptyCategoryForm(): CategoryFormState {
  return {
    category_code: '',
    category_name: '',
    useful_life_months: undefined,
    salvage_rate: '0.0500',
    asset_account_id: '',
    accum_account_id: '',
    expense_account_id: '',
    status: 1,
    sort_order: 0,
    remark: '',
  };
}

/** 分类校验（镜像后端 `validate_category`）。 */
export function validateCategoryForm(
  form: CategoryFormState,
): Partial<Record<keyof CategoryFormState, string>> {
  const errors: Partial<Record<keyof CategoryFormState, string>> = {};
  // CODE-07：分类编码**可以留空** —— 留空由后端自动取号（`AC-01` 形态：
  // `biz_seq` 的 AC 号段，纯流水 2 位、不计日期、永不重置），「生成编码」按钮只是提前看号。
  // 填了则镜像后端字符集（字母 / 数字 / `-` `_` `.`，≤ 40，统一大写后校验）——
  // 别再用 `^[A-Z0-9_]+$`：那会把取号器自己产出的 `AC-01` 拦在门外。
  const codeError = masterCodeError(form.category_code);
  if (codeError) {
    errors.category_code = codeError;
  }
  if (!form.category_name.trim()) {
    errors.category_name = '分类名称必填';
  } else if (form.category_name.trim().length > 120) {
    errors.category_name = '分类名称最长 120 字符';
  }
  const months = Number(form.useful_life_months ?? 0);
  if (!Number.isInteger(months) || months < 1 || months > 600) {
    errors.useful_life_months = '使用月数必须是 1–600 的整数';
  }
  const rate = parseMoney(form.salvage_rate);
  if (rate < 0n || rate >= 10000n) {
    errors.salvage_rate = '净残值率必须满足 0 ≤ 残值率 < 1（如 0.05）';
  }
  if (!form.asset_account_id) {
    errors.asset_account_id = '原值科目必填';
  }
  if (!form.accum_account_id) {
    errors.accum_account_id = '累计折旧科目必填';
  }
  if (!form.expense_account_id) {
    errors.expense_account_id = '折旧费用科目必填';
  }
  return errors;
}
