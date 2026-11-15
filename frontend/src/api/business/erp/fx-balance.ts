/**
 * 外币余额表 API 层（后端 `backend/app/src/business/erp/fx_balance.rs` · M1-07 / 迁移 `V0412`）。
 *
 * # 为什么单独一个文件（不并进 `erp/index.ts`）
 *
 * 与 `currency.ts` / `customer-price.ts` 同一口径：后端是独立模块（自己的取数文件 + 迁移），
 * 按模块边界单独成文件，页面只 `import '@/api/business/erp/fx-balance'`，`index.ts` 一行不动。
 *
 * # 字段名逐字抄后端（不是猜的）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 报表编码 / 名称 | `report_code` = `FX_BALANCE` / `report_name` | `fx_balance::{REPORT_CODE, REPORT_NAME}` |
 * | 账套本位币 | `base_currency` | `biz_currency WHERE is_base` |
 * | 期间窗口 | `period` / `period_start` / `period_end` | 期间表；未建账时按自然月推算 |
 * | 期间状态 | `period_status`：`OPEN` / `CLOSED` / `null`（未建账） | `biz_accounting_period.status` |
 * | 原币三列 | `opening_fc` / `period_fc_debit` / `period_fc_credit` / `closing_fc` | `biz_voucher_entry.fc_*` |
 * | 账面本位币四列 | `opening_book` / `period_book_debit` / `period_book_credit` / `closing_book` | `debit_amount` / `credit_amount` |
 * | 折算 | `close_rate` / `converted_base` / `reval_diff` | 期末即期汇率；缺牌价三列全 `null` |
 * | 账面口径 | `book_basis`：`ACCOUNT` / `CURRENCY` | 多币种科目退化，见模块文档 ② |
 * | 按币种汇总 | `currencies[]`（`currency_code` / `account_count` / `closing_fc` / ...） | `FxCurrencySummary` |
 * | 合计 | `total_closing_book` / `total_converted_base` / `total_reval_diff` | 折算合计在缺牌价时是 `null` |
 *
 * # 五个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端路由宏不含前缀，漏了会命中 SPA fallback 返回 HTML
 *    → 前端报「响应体格式不符合」）；
 * 2. **金额全是字符串**（`NUMERIC(20,4)`）：不要 `Number()` 转换，报表上少一分钱是事故；
 * 3. **余额是「借方为正」的有符号值**：负数 = 贷方余额，页面必须按符号显示借 / 贷，
 *    不能取绝对值（资产与负债共用一套公式，见后端模块文档）；
 * 4. **`converted_base` / `reval_diff` 可能是 `null`**（缺期末牌价）：这不是错误，
 *    后端**没有**按汇率 1 折算；页面必须显示「—」并提示去补牌价，不能显示 0；
 * 5. **`period_status = null` 不是异常**：该月未建账，后端按自然月推算期间窗口。
 */
import { get } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/** 账面口径：科目合计（单币种科目） */
export type FxBookBasis = 'ACCOUNT' | 'CURRENCY';

/** 余额方向（有符号余额的派生值） */
export type FxBalanceSide = 'DEBIT' | 'CREDIT' | 'ZERO';

/** 期间状态（未建账为 `null`） */
export type FxPeriodStatus = 'OPEN' | 'CLOSED';

/** 明细行：一个 `(科目, 币种)` 一行 */
export interface FxBalanceRow {
  account_id: string;
  account_code: string;
  account_name: string;
  /** `ASSET` / `LIABILITY` / `EQUITY` / `COST` / `PROFIT_LOSS` */
  account_category: string;
  /** `DEBIT` / `CREDIT`（科目主数据，不参与计算） */
  balance_direction: string;
  currency_code: string;
  currency_name: string | null;
  /** 期初原币余额（借方为正） */
  opening_fc: string;
  period_fc_debit: string;
  period_fc_credit: string;
  closing_fc: string;
  /** 期初账面本位币余额 */
  opening_book: string;
  period_book_debit: string;
  period_book_credit: string;
  closing_book: string;
  book_basis: FxBookBasis;
  /** 期末即期汇率（缺牌价 `null`） */
  close_rate: string | null;
  /** 折算本位币余额（缺牌价 `null`） */
  converted_base: string | null;
  /** 汇兑差额 = 折算 − 账面（缺牌价 `null`） */
  reval_diff: string | null;
  rate_missing: boolean;
}

/** 按币种汇总 */
export interface FxCurrencySummary {
  currency_code: string;
  currency_name: string | null;
  account_count: number;
  closing_fc: string;
  closing_book: string;
  close_rate: string | null;
  converted_base: string | null;
  reval_diff: string | null;
  rate_missing: boolean;
}

/** 外币余额表响应（后端 `FxBalanceRes`） */
export interface FxBalanceReport {
  report_code: string;
  report_name: string;
  company_name: string;
  /** 账套本位币（如 `CNY`） */
  base_currency: string;
  period: string;
  period_start: string;
  period_end: string;
  period_status: FxPeriodStatus | null;
  /** 实际生效的币种过滤（已规范化；未过滤为 `null`） */
  currency_filter: string | null;
  generated_at: string;
  rows: FxBalanceRow[];
  currencies: FxCurrencySummary[];
  total_closing_book: string;
  /** 任一币种缺牌价 ⇒ `null`（半套折算的合计不给） */
  total_converted_base: string | null;
  total_reval_diff: string | null;
  /** 缺期末牌价的币种（升序） */
  missing_rates: string[];
  /** 需要人工处理的问题（多币种科目退化 / 外币分录落在未标记科目上） */
  warnings: string[];
  /** 口径提示 */
  hints: string[];
}

/**
 * 编外币余额表（`GET /erp/report/fx-balance?period=yyyy-MM&currency=USD`）。
 *
 * - `period` 必填（`yyyy-MM`）；不传或月份非法 ⇒ `400 + 10001`；
 * - `currency` 可选（三位大写，后端会转大写）；非法码 ⇒ `400 + 10001`（不静默忽略过滤）；
 * - **不会**因为缺期末牌价而报错，也不会因为该月未开账而报错（见文件头 ④⑤）。
 */
export function fetchFxBalance(query: { period: string; currency?: string }): Promise<FxBalanceReport> {
  const params: Record<string, string> = { period: query.period };
  if (query.currency) {
    params.currency = query.currency;
  }
  return get<FxBalanceReport>(`${BASE}/report/fx-balance`, params);
}
