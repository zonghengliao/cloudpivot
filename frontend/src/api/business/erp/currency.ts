/**
 * 币种 / 汇率维护 API 层（后端 `backend/src/business/erp/currency_admin_handler.rs`，
 * SO-39 建表 / SO-40 建单锁定 / SO-42 维护页）。
 *
 * # 为什么单独一个文件（不并进 `erp/index.ts`）
 *
 * 与 `customer-price.ts` / `sales-return.ts` 同一口径：币种汇率是独立后端模块
 * （自己的 handler / model / 迁移 V0272 + V0316），按模块边界单独成文件，
 * 页面只 `import '@/api/business/erp/currency'`，`index.ts` 一行不动。
 *
 * # 字段名逐字抄后端（不是猜的）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 币种码 | `code`（**主键**，三位 ISO 4217 大写；小写会被后端转大写） | `CurrencyRes` |
 * | 名称 / 符号 | `name` / `symbol` | 同上 |
 * | 小数位 | `decimal_places`（0..4；本仓金额列是 4 位） | `CurrencyRes` |
 * | 本位币 | `is_base`（**全库唯一**，部分唯一索引兜底） | 同上 |
 * | 状态 | `status`：`ACTIVE` / `INACTIVE` | `ck_biz_currency_status` |
 * | 该币种汇率条数 | `rate_count`（列表直接用，不用二次请求） | `CurrencyRes` |
 * | 币对 | `from_currency` / `to_currency`（`1 from = rate to`） | `RateRes` |
 * | 汇率 | `rate`（**JSON 里是字符串**，`NUMERIC(18,8)`） | 全仓金额口径一致 |
 * | 类型 | `rate_type`：`SPOT` 即时 / `MONTH_END` 月末 | `ck_biz_exchange_rate_type` |
 * | 生效日 | `effective_date`（`YYYY-MM-DD`，**含当日**，无默认值） | `RateRes` |
 * | 预览来源 | `source`：`BASE` / `SPOT` / `MISSING` | `QuoteRes` |
 *
 * # 六个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端路由宏不含前缀，漏了会命中 SPA fallback 返回 HTML
 *    → 前端报「响应体格式不符合」）；
 * 2. **币种码是主键，改不了**：要换码只能新建 + 停用旧行（历史汇率/单据按码引用它）；
 * 3. **本位币不能停用**（后端 409）：全仓建单取本位币只认 `is_base AND ACTIVE`；
 * 4. **本位币单不需要汇率行**：`from <> to` 的 CHECK 会拒自反汇率，页面别给本位币录汇率；
 * 5. **汇率物理删**（表无软删列）：删了就是删了，`quote` 会立刻取不到 → 外币单建单 400；
 * 6. **`quote` 的 `exchange_rate = 0` 且 `source = 'MISSING'` 不是错误**：这是「该币种在
 *    该日期前没录过即时汇率」的合法业务结果，页面必须提示去维护，而不是显示 0 元汇率。
 */
import { del, get, post, put, type PageData } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/** 金额 / 汇率：后端 `NUMERIC` 在 JSON 里是字符串，但 `t-input-number` 给数字 */
export type DecimalText = string | number;

/** 币种状态（`ACTIVE` 新单据可选 / `INACTIVE` 停用但保留历史） */
export type CurrencyStatus = 'ACTIVE' | 'INACTIVE';

/** 汇率类型：`SPOT` 建单锁定用 / `MONTH_END` 月末重估用（SO-41） */
export type RateType = 'SPOT' | 'MONTH_END';

/** 汇率预览来源：本位币恒 1 / 取到牌价 / 外币但没录汇率 */
export type QuoteSource = 'BASE' | 'SPOT' | 'MISSING';

// ==================== 币种 ====================

/** 币种一行（列表 / 新建 / 编辑同构） */
export interface CurrencyRow {
  /** 币种码（主键，三位大写） */
  code: string;
  name: string;
  symbol: string;
  /** 小数位（0..4） */
  decimal_places: number;
  /** 是否本位币（全库唯一） */
  is_base: boolean;
  status: CurrencyStatus;
  remark: string | null;
  /** 该币种作为原币的汇率条数 */
  rate_count: number;
  created_at: string;
  updated_at: string;
}

/** 币种下拉选项（只含 `ACTIVE`） */
export interface CurrencyOption {
  code: string;
  /** 显示名（后端给 `人民币（CNY）`） */
  label: string;
  symbol: string;
  decimal_places: number;
  is_base: boolean;
}

/** 币种列表筛选（**不分页**：极小主数据，一次拉全） */
export interface CurrencyFilter {
  status?: CurrencyStatus;
  /** 码 / 名称模糊 */
  keyword?: string;
}

/** 新建币种请求 */
export interface CreateCurrencyPayload {
  /** 三位码（大小写不敏感，后端转大写） */
  code: string;
  name: string;
  symbol: string;
  /** 小数位（缺省 2） */
  decimal_places?: number;
  status?: CurrencyStatus;
  remark?: string | null;
  /** 顺手立为本位币（同一请求内接管；要求 ACTIVE） */
  set_base?: boolean;
}

/** 编辑币种请求（部分更新：不传 = 不改；`remark` 传空串 = 清空） */
export interface UpdateCurrencyPayload {
  name?: string;
  symbol?: string;
  decimal_places?: number;
  status?: CurrencyStatus;
  remark?: string | null;
}

/** 币种列表（`GET /api/business/erp/currency/list`，权限 `business:erp:currency:list`） */
export function fetchCurrencyList(filter: CurrencyFilter = {}): Promise<CurrencyRow[]> {
  return get<CurrencyRow[]>(`${BASE}/currency/list`, {
    status: filter.status,
    keyword: filter.keyword,
  });
}

/** 币种下拉（只 `ACTIVE`；开单页选币种用，与列表同一个权限码） */
export function fetchCurrencyOptions(): Promise<CurrencyOption[]> {
  return get<CurrencyOption[]>(`${BASE}/currency/options`);
}

/** 新建币种（权限 `business:erp:currency:create`） */
export function createCurrency(payload: CreateCurrencyPayload): Promise<CurrencyRow> {
  return post<CurrencyRow>(`${BASE}/currency`, payload);
}

/** 编辑币种（权限 `business:erp:currency:update`；本位币停用会 409） */
export function updateCurrency(
  code: string,
  payload: UpdateCurrencyPayload,
): Promise<CurrencyRow> {
  return put<CurrencyRow>(`${BASE}/currency/${encodeURIComponent(code)}`, payload);
}

/** 立为本位币（权限 `business:erp:currency:base`；全库唯一，库层部分唯一索引兜底） */
export function setBaseCurrency(code: string): Promise<CurrencyRow> {
  return post<CurrencyRow>(`${BASE}/currency/${encodeURIComponent(code)}/base`);
}

// ==================== 汇率 ====================

/** 汇率一行 */
export interface ExchangeRateRow {
  id: string;
  from_currency: string;
  to_currency: string;
  /** `1 原币 = rate 目标币` */
  rate: DecimalText;
  rate_type: RateType;
  /** 生效日（含） */
  effective_date: string;
  remark: string | null;
  created_at: string;
  updated_at: string;
}

/** 汇率分页筛选 */
export interface ExchangeRateFilter {
  from_currency?: string;
  to_currency?: string;
  rate_type?: RateType;
  /** 生效日 ≥ */
  date_from?: string;
  /** 生效日 ≤ */
  date_to?: string;
  page?: number;
  page_size?: number;
}

/** 新建汇率请求（`to_currency` 缺省 = 账套本位币） */
export interface CreateRatePayload {
  from_currency: string;
  to_currency?: string;
  rate: DecimalText;
  rate_type?: RateType;
  /** 缺省 = 今天（CST），**不会**按月末推算 */
  effective_date?: string;
  remark?: string | null;
}

/** 编辑汇率请求（部分更新） */
export interface UpdateRatePayload {
  from_currency?: string;
  to_currency?: string;
  rate?: DecimalText;
  rate_type?: RateType;
  effective_date?: string;
  remark?: string | null;
}

/** 月度批量录入的一行 */
export interface BatchRateItem {
  from_currency: string;
  rate: DecimalText;
  remark?: string | null;
}

/** 按月批量录入请求（生效日 = 该月月末；同键已存在则覆盖） */
export interface BatchMonthPayload {
  /** `YYYY-MM` */
  month: string;
  /** 缺省 `MONTH_END`（按月录入的天然语义） */
  rate_type?: RateType;
  remark?: string | null;
  rates: BatchRateItem[];
}

/** 批量录入结果的一行 */
export interface BatchRateSaved {
  from_currency: string;
  rate: DecimalText;
  /** `true` = 新插；`false` = 覆盖了同键既有行 */
  created: boolean;
}

/** 批量录入结果 */
export interface BatchMonthResult {
  month: string;
  /** 该月月末（生效日） */
  effective_date: string;
  rate_type: RateType;
  /** 目标币种（本位币） */
  to_currency: string;
  created_count: number;
  updated_count: number;
  saved: BatchRateSaved[];
}

/** 汇率预览（开单选币种时查；**只读**，与建单锁定同一把尺） */
export interface RateQuote {
  currency_code: string;
  base_currency: string;
  /** 本位币恒 1；`source = 'MISSING'` 时为 0（**不是**可用汇率） */
  exchange_rate: DecimalText;
  on_date: string;
  source: QuoteSource;
  /** 取到的那条牌价的生效日（`SPOT` 时有值） */
  effective_date: string | null;
}

/** 汇率分页（`GET /api/business/erp/exchange-rate/page`，权限 `business:erp:fx-rate:list`） */
export function fetchRatePage(
  filter: ExchangeRateFilter = {},
): Promise<PageData<ExchangeRateRow>> {
  return get<PageData<ExchangeRateRow>>(`${BASE}/exchange-rate/page`, {
    from_currency: filter.from_currency,
    to_currency: filter.to_currency,
    rate_type: filter.rate_type,
    date_from: filter.date_from,
    date_to: filter.date_to,
    page: filter.page,
    page_size: filter.page_size,
  });
}

/** 新建汇率（权限 `business:erp:fx-rate:create`；同键已存在 409） */
export function createRate(payload: CreateRatePayload): Promise<ExchangeRateRow> {
  return post<ExchangeRateRow>(`${BASE}/exchange-rate`, payload);
}

/** 编辑汇率（权限 `business:erp:fx-rate:update`） */
export function updateRate(
  id: string,
  payload: UpdateRatePayload,
): Promise<ExchangeRateRow> {
  return put<ExchangeRateRow>(`${BASE}/exchange-rate/${encodeURIComponent(id)}`, payload);
}

/** 删除汇率（权限 `business:erp:fx-rate:delete`；物删，不可恢复） */
export function deleteRate(id: string): Promise<void> {
  return del<void>(`${BASE}/exchange-rate/${encodeURIComponent(id)}`);
}

/** 按月批量录入（权限 `business:erp:fx-rate:batch`） */
export function batchMonthRates(payload: BatchMonthPayload): Promise<BatchMonthResult> {
  return post<BatchMonthResult>(`${BASE}/exchange-rate/batch-month`, payload);
}

/** 汇率预览（权限 `business:erp:fx-rate:quote`；只读，不落库） */
export function quoteRate(currencyCode?: string, date?: string): Promise<RateQuote> {
  return get<RateQuote>(`${BASE}/exchange-rate/quote`, {
    currency_code: currencyCode,
    date,
  });
}

// ==================== 域值常量（与后端逐字一致，页面不要另写字面量） ====================

/** 币种状态选项 */
export const CURRENCY_STATUS_OPTIONS: { label: string; value: CurrencyStatus }[] = [
  { label: '启用', value: 'ACTIVE' },
  { label: '停用', value: 'INACTIVE' },
];

/** 汇率类型选项 */
export const RATE_TYPE_OPTIONS: { label: string; value: RateType }[] = [
  { label: '即时（建单锁定）', value: 'SPOT' },
  { label: '月末（期末重估）', value: 'MONTH_END' },
];
