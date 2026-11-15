/**
 * 外币明细账 API 层（后端 `backend/app/src/business/erp/fx_ledger.rs` · M1-08 / 迁移 `V0413`）。
 *
 * # 为什么单独一个文件（不并进 `erp/index.ts`）
 *
 * 与 `fx-balance.ts` / `currency.ts` 同一口径：后端是独立模块（自己的取数文件 + 迁移），
 * 按模块边界单独成文件，页面只 `import '@/api/business/erp/fx-ledger'`，`index.ts` 一行不动。
 *
 * # 字段名逐字抄后端（不是猜的）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 报表编码 / 名称 | `report_code` = `FX_LEDGER` / `report_name` = 外币明细账 | `fx_ledger::{REPORT_CODE, REPORT_NAME}` |
 * | 账套本位币 | `base_currency` | `biz_currency WHERE is_base` |
 * | 请求币种 | `currency` / `currency_name` | 已规范化（大写）；名称来自 `biz_currency` |
 * | 区间 | `date_from` / `date_to`（`null` = 不限） | `period_from` / `period_to` 解析结果 |
 * | 科目过滤回显 | `account`：`{id, account_code, account_name, is_foreign_currency}` | 未过滤 = `null` |
 * | 合计 | `opening_fc` / `period_fc_debit` / `period_fc_credit` / `closing_fc` / `closing_side` | Σ 各科目 |
 * | 本位币发生额 | `period_book_debit` / `period_book_credit` / `period_book_net`（**不含调汇**） | 该币种分录的本位币金额 |
 * | 按科目汇总 | `accounts[]`（`entry_count` / 期初 / 本期 / 期末 / `closing_side`） | `FxLedgerAccount` |
 * | 明细行 | `rows[]`：`fc_debit` / `fc_credit` / `debit_amount` / `credit_amount` / `running_fc_balance` / `running_side` / `exchange_rate` / `is_red` | `FxLedgerRow` |
 * | 分页 | `total` / `page` / `page_size` | `scope::paginate_unscoped` |
 *
 * # 六个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端路由宏不含前缀，漏了会命中 SPA fallback 返回 HTML
 *    → 前端报「响应体格式不符合」）；
 * 2. **`currency` 必填**，且**不能是账套本位币**（后端 400 + 10001 并指路去总账）——
 *    本位币没有原币口径，不是「空表」而是「查错页面」；
 * 3. **金额全是字符串**（`NUMERIC(20,4)`）：不要 `Number()` 转换，报表上少一分钱是事故；
 * 4. **`running_fc_balance` 是「按科目分别累计」的余额**：跨科目的行不可连读，
 *    页面必须把科目列画出来，否则用户会把 A 科目的余额串到 B 科目上；
 * 5. **本位币三列不含历次调汇**：本表只列该币种的原币行，调汇凭证是本位币凭证，不在本表
 *    （含调汇的账面余额看「外币余额表」）；
 * 6. **`debit_amount` / `credit_amount` 是本位币、`fc_*` 是原币**：两套金额都存在，别混。
 */
import { get } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/** 余额方向（有符号余额的派生值） */
export type FxLedgerSide = 'DEBIT' | 'CREDIT' | 'ZERO';

/** 科目过滤回显（请求里给了科目时才有） */
export interface FxLedgerAccountEcho {
  id: string;
  account_code: string;
  account_name: string;
  /** 未标记「外币核算」时为 `false`（此时本表必然为空，`warnings` 里会点名） */
  is_foreign_currency: boolean;
}

/** 明细行（一条已过账分录） */
export interface FxLedgerRow {
  voucher_id: string;
  voucher_date: string;
  voucher_word: string;
  voucher_no: string;
  line_no: number;
  summary: string;
  account_id: string;
  account_code: string;
  account_name: string;
  /** 凭证币种（永远等于请求币种，回显用） */
  currency_code: string;
  /** 建单时锁定的汇率（`NUMERIC(18,8)`） */
  exchange_rate: string;
  /** 原币借方 */
  fc_debit: string;
  /** 原币贷方 */
  fc_credit: string;
  /** 本位币借方（该币种行的本位币金额，不含调汇） */
  debit_amount: string;
  /** 本位币贷方 */
  credit_amount: string;
  /** 逐行原币余额（含该科目期初，按科目分别累计；借方为正） */
  running_fc_balance: string;
  running_side: FxLedgerSide;
  /** 红字凭证（净额为负，余额直接累加） */
  is_red: boolean;
}

/** 按科目汇总（每个有该币种流水的科目一行） */
export interface FxLedgerAccount {
  account_id: string;
  account_code: string;
  account_name: string;
  /** `DEBIT` / `CREDIT`（科目主数据，不参与计算） */
  balance_direction: string;
  /** `ASSET` / `LIABILITY` / `EQUITY` / `COST` / `PROFIT_LOSS` */
  account_category: string;
  /** 本区间内该币种的分录条数 */
  entry_count: number;
  opening_fc: string;
  period_fc_debit: string;
  period_fc_credit: string;
  closing_fc: string;
  closing_side: FxLedgerSide;
  /** 本区间内该币种分录的本位币借方合计（不含调汇） */
  period_book_debit: string;
  period_book_credit: string;
  period_book_net: string;
}

/** 外币明细账响应（后端 `FxLedgerRes`） */
export interface FxLedgerReport {
  report_code: string;
  report_name: string;
  company_name: string;
  /** 账套本位币（如 `CNY`） */
  base_currency: string;
  /** 请求币种（已规范化大写） */
  currency: string;
  /** 币种名称（主数据缺失为 `null`） */
  currency_name: string | null;
  /** 起始日期（含；`null` = 不限下限） */
  date_from: string | null;
  /** 结束日期（含；`null` = 不限上限） */
  date_to: string | null;
  account: FxLedgerAccountEcho | null;
  opening_fc: string;
  period_fc_debit: string;
  period_fc_credit: string;
  closing_fc: string;
  closing_side: FxLedgerSide;
  period_book_debit: string;
  period_book_credit: string;
  /** 本区间内该币种分录的本位币净额（借 − 贷） */
  period_book_net: string;
  /** 本区间内该币种的分录条数（全量，跨页） */
  entry_count: number;
  accounts: FxLedgerAccount[];
  rows: FxLedgerRow[];
  total: number;
  page: number;
  page_size: number;
  generated_at: string;
  /** 需要人工处理的问题（外币分录落在未标记科目 / 原币金额缺失 / 筛了未标记科目） */
  warnings: string[];
  /** 口径提示 */
  hints: string[];
}

/** 明细账查询参数 */
export interface FxLedgerQuery {
  /** 币种（三位大写，**必填**；不能是账套本位币） */
  currency: string;
  /** 起始：`yyyy-MM`（取月首）或 `yyyy-MM-dd` */
  period_from?: string;
  /** 结束：`yyyy-MM`（取月末）或 `yyyy-MM-dd` */
  period_to?: string;
  /** 科目编码（可选；与 `account_id` 等价，页面用编码） */
  account_code?: string;
  page?: number;
  page_size?: number;
}

/**
 * 查外币明细账（`GET /erp/report/fx-ledger?currency=USD&period_from=2026-11&period_to=2026-11`）。
 *
 * - `currency` 必填；缺 / 非三位大写 / 等于账套本位币 ⇒ `400 + 10001`；
 * - `period_from` / `period_to` 缺省 = 不限；格式非法或起大于止 ⇒ `400 + 10001`；
 * - 科目编码查不到 ⇒ `404`；数据脏（未标记科目 / 原币缺失）**不报错**，进 `warnings`。
 */
export function fetchFxLedger(query: FxLedgerQuery): Promise<FxLedgerReport> {
  const params: Record<string, string | number> = { currency: query.currency };
  if (query.period_from) params.period_from = query.period_from;
  if (query.period_to) params.period_to = query.period_to;
  if (query.account_code) params.account_code = query.account_code;
  if (query.page) params.page = query.page;
  if (query.page_size) params.page_size = query.page_size;
  return get<FxLedgerReport>(`${BASE}/report/fx-ledger`, params);
}
