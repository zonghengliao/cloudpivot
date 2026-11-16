/**
 * 现金流底稿 + 由底稿出表 的 API 层（后端 `cashflow_worksheet.rs` / `cashflow_statement.rs`）。
 *
 * 上游：`V0414` 建表 `biz_cashflow_worksheet`、`V0415` 底稿读/写两码、`V0416` 间接法调整项、
 * `V0417` 出表读/导出两码。本卡是 M1-18（前端页面：底稿录入 + 预览）。
 *
 * | 方法 | 路径 | 权限码 | 说明 |
 * |------|------|--------|------|
 * | GET  | `/api/business/erp/report/cash-flow/worksheet?period=yyyy-MM` | `business:erp:cashflow-worksheet:list`   | 读整张底稿（配置项 ∪ 已录行）+ 小计 + 勾稽 |
 * | PUT  | `/api/business/erp/report/cash-flow/worksheet`              | `business:erp:cashflow-worksheet:save`   | 录入（期初 / 明细 / 期末，事务内 upsert） |
 * | POST | `/api/business/erp/report/cash-flow/worksheet/check`        | `business:erp:cashflow-worksheet:list`   | 勾稽校验（**不落库**） |
 * | GET  | `/api/business/erp/report/cash-flow/statement?period=yyyy-MM` | `business:erp:cashflow-statement:list`  | 由底稿汇总的三栏报表（本期 / 上期） |
 * | GET  | `/api/business/erp/report/cash-flow/statement/export?period=yyyy-MM` | `business:erp:cashflow-statement:export` | 同一份报表导出 xlsx（未闭合 400 + 66087） |
 *
 * # 六个容易踩的点（逐条对着后端代码核过，不是猜的）
 *
 * 1. **路径必须写全 `/api`**（后端路由宏不含前缀，漏了会命中 SPA fallback 返回 HTML
 *    → 前端报「响应体格式不符合统一响应规范」）；
 * 2. **金额全是十进制字符串**（`NUMERIC(18,4)` → `rust_decimal` → JSON `"1200.0000"`）：
 *    不要 `Number()` 转换，报表上少一分钱是事故（格式化见 `worksheet-logic.ts`）；
 * 3. **保存「允许暂时不平」**：PUT 返回 200 + `check.balanced=false` 是**正常**的（工作稿），
 *    只有出表 / `require_balanced=true` 的校验才 400 + 66087。页面不要拿 `balanced` 拦保存；
 * 4. **`opening` / `closing` / `items` 传 `undefined` = 不改**（后端 `Option` / 空数组语义）：
 *    所以「保存」只发改动过的行，别把整表发一遍（会把没录过的配置项变成「已录 0」）；
 * 5. **`previous` 可能是 `null`**（上期没开账 / 上期没编这一行）：那是「没有数」，不是 0，
 *    页面必须显示占位符；
 * 6. **期间闸**：只有 `OPEN` 的期间可写（`editable`），`CLOSED` / `NOT_OPENED` 一律置灰，
 *    但 GET 照常 200 —— 历史底稿、已结账期间都要能看。
 */
import service, { get, post, put } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp/report/cash-flow';

/** 空过滤（后端把 `?period=` 当空串，会 400） */
function asParams(query: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === '') {
      continue;
    }
    out[key] = value;
  }
  return out;
}

// ==================== 底稿：出参 ====================

/** 底稿明细行（已录 / 未录同一形状，按 `entered` 区分） */
export interface WorksheetRow {
  /** 行类型：恒 `FLOW`（余额行在 `opening` / `closing` 里单列） */
  line_type: string;
  /** 项目编码 */
  item_code: string;
  /** 项目名称（已录行 = 底稿快照；未录行 = 当前配置名） */
  item_name: string;
  /** `OPERATING` / `INVESTING` / `FINANCING` */
  activity: string;
  /** `INFLOW` / `OUTFLOW`（方向由配置决定，前端不改） */
  direction: string;
  /** 金额（`0` = 未录入或本期为 0） */
  amount: string;
  /** 显示顺序（已录行取快照） */
  sort_order: number;
  /** 编制说明 */
  remark: string | null;
  /** 是否已有底稿行（`false` = 配置里有、本期还没录） */
  entered: boolean;
  /** 配置项是否仍启用（`false` = 已停用 / 已删，历史行仍显示） */
  item_active: boolean;
}

/** 余额行（期初 / 期末） */
export interface WorksheetBalance {
  /** `OPENING` / `CLOSING` */
  line_type: string;
  item_name: string;
  /** 余额（可负：透支 / 受限资金） */
  amount: string;
  remark: string | null;
}

/** 一个活动的小计 */
export interface WorksheetActivityBlock {
  code: string;
  /** `一、经营活动产生的现金流量` … */
  name: string;
  inflow: string;
  outflow: string;
  /** 流入 − 流出 */
  net: string;
  /** 本活动下已录入的项目行数 */
  entered_rows: number;
}

/** 合计区（流入 / 流出 / 净额） */
export interface WorksheetTotals {
  inflow: string;
  outflow: string;
  net: string;
}

/** 一条勾稽问题（机读码 + 人话） */
export interface WorksheetProblem {
  /** `OPENING_MISSING` / `CLOSING_MISSING` / `IMBALANCED` / `EMPTY` */
  code: string;
  message: string;
}

/** 勾稽结果（直接法：期初 + 流入 − 流出 = 期末） */
export interface WorksheetCheck {
  balanced: boolean;
  opening: string | null;
  inflow: string;
  outflow: string;
  expected_closing: string | null;
  closing: string | null;
  /** `expected_closing − closing`（缺任一端时 `null`） */
  diff: string | null;
  message: string;
  problems: WorksheetProblem[];
}

/** 整张底稿（GET 与 PUT 同一份返回体：保存后不用再拉一次） */
export interface WorksheetSheet {
  period: string;
  /** `OPEN` / `CLOSED` / `NOT_OPENED` */
  period_status: string;
  /** 是否可编辑（= 期间 `OPEN`） */
  editable: boolean;
  rows: WorksheetRow[];
  opening: WorksheetBalance | null;
  closing: WorksheetBalance | null;
  activities: WorksheetActivityBlock[];
  totals: WorksheetTotals;
  check: WorksheetCheck;
}

// ==================== 底稿：入参 ====================

/** 余额行录入 */
export interface BalanceIn {
  /** 可负（透支 / 受限资金） */
  amount: string;
  remark?: string | null;
}

/** 明细行录入（三个快照列由服务端从配置取） */
export interface WorksheetItemIn {
  item_code: string;
  /** 本期发生额，≥ 0，最多 4 位小数 */
  amount: string;
  remark?: string | null;
}

/** 录入请求（PUT）：`opening` / `closing` 省略 = 不改；`items` 空 = 不改 */
export interface SaveWorksheetReq {
  period: string;
  opening?: BalanceIn;
  closing?: BalanceIn;
  items: WorksheetItemIn[];
}

/** 校验请求（POST）：形状同录入，但一个字节都不写库 */
export interface CheckWorksheetReq {
  period: string;
  opening?: BalanceIn;
  closing?: BalanceIn;
  items: WorksheetItemIn[];
  /** `true` = 不平就 400 + 66087（出表前的复核闸；页面「试算」用 false） */
  require_balanced?: boolean;
}

// ==================== 报表：出参 ====================

/** 一栏「本期 / 上期」的金额对（`previous = null` = 这一列没有数，不是 0） */
export interface StatementAmount {
  current: string;
  previous: string | null;
}

/** 报表的一行 */
export interface StatementLine {
  item_code: string;
  item_name: string;
  activity: string;
  direction: string;
  sort_order: number;
  /** 本期金额（未录入 = 0） */
  current: string;
  /** 上期金额（`null` = 上期无底稿 / 上期没这一行） */
  previous: string | null;
  current_entered: boolean;
  previous_entered: boolean;
  item_active: boolean;
  remark: string | null;
}

/** 一个活动分区 */
export interface StatementSection {
  activity: string;
  activity_name: string;
  inflow: StatementLine[];
  inflow_total: StatementAmount;
  outflow: StatementLine[];
  outflow_total: StatementAmount;
  net: StatementAmount;
}

/** 余额行（期初 / 期末）：`null` 表示底稿里还没录，**不按 0 列示** */
export interface StatementBalance {
  line_type: string;
  item_name: string;
  current: string | null;
  previous: string | null;
}

/** 出表勾稽（两条等式 + 上期参考状态） */
export interface StatementCheck {
  /** 本期是否闭合（导出闸就看这一位） */
  balanced: boolean;
  /** 上期是否闭合（`null` = 没有可比的上期底稿；上期不闭合不阻塞本期） */
  previous_balanced: boolean | null;
  /** 报表自身加减关系：净增加额 + 期初 = 期末 */
  arithmetic_ok: boolean;
  /** `期初 + 净增加额 − 期末`（缺余额时 `null`） */
  diff: string | null;
  message: string;
  current: WorksheetCheck;
  previous: WorksheetCheck | null;
}

/** 现金流量表（三栏报表，由底稿汇总） */
export interface CashStatement {
  report_code: string;
  report_name: string;
  company_name: string;
  unit: string;
  period: string;
  /** `2026年11月` */
  period_name: string;
  period_status: string;
  period_start: string;
  period_end: string;
  /** 上期期间（`null` = 上期没开账） */
  previous_period: string | null;
  previous_period_status: string | null;
  current_label: string;
  previous_label: string;
  generated_at: string;
  /** 恒 `WORKSHEET` */
  source: string;
  source_note: string;
  sections: StatementSection[];
  net_increase: StatementAmount;
  opening: StatementBalance;
  closing: StatementBalance;
  check: StatementCheck;
  line_count: number;
  hidden_zero_lines: number;
  warnings: string[];
}

// ==================== 接口 ====================

/**
 * 读整张现金流底稿。
 *
 * # 异常情况
 * - 400 + `10001`：`period` 不是 `yyyy-MM`；
 * - 200 + `editable=false`：期间已结账 / 未开账（**不是**错误，只置灰）。
 */
export function fetchCashflowWorksheet(period?: string): Promise<WorksheetSheet> {
  return get<WorksheetSheet>(`${BASE}/worksheet`, asParams({ period }));
}

/**
 * 录入底稿（期初 / 明细 / 期末，同一事务 upsert）。
 *
 * 返回保存后的整张底稿 + 勾稽结果；`check.balanced=false` 是允许的（工作稿）。
 * 期间已结账 400 + `66085`、项目不存在 / 停用 400 + `66086`。
 */
export function saveCashflowWorksheet(payload: SaveWorksheetReq): Promise<WorksheetSheet> {
  return put<WorksheetSheet>(`${BASE}/worksheet`, payload);
}

/**
 * 勾稽校验（**不落库**）：把草稿叠加到已存底稿上算「期初 + 流入 − 流出 = 期末」。
 *
 * `require_balanced=true` 时不平回 400 + `66087`；页面「试算」用 `false`（要拿到差额）。
 */
export function checkCashflowWorksheet(payload: CheckWorksheetReq): Promise<WorksheetCheck> {
  return post<WorksheetCheck>(`${BASE}/worksheet/check`, payload);
}

/**
 * 由底稿汇总的现金流量表（本期 / 上期两栏）。
 *
 * 未闭合时仍 200（`check.balanced=false` + 逐条 `problems`）：页面要能打开去补底稿。
 */
export function fetchCashflowStatement(period: string, hideZero = false): Promise<CashStatement> {
  return get<CashStatement>(`${BASE}/statement`, asParams({ period, hide_zero: hideZero ? true : undefined }));
}

/**
 * 导出由底稿生成的现金流量表 xlsx（返回文件字节流，不走 `Res<T>`）。
 *
 * 出表闸：未闭合 400 + `66087`（错误体是 JSON 而不是 xlsx，调用方按 `ApiError` 处理）。
 *
 * # 为什么这里要自己解析错误体
 *
 * `responseType: 'blob'` 时后端 4xx 的 JSON 报文也变成了 Blob，全局拦截器
 * （`toApiError`）读不到 `msg`，只能给出「请求参数不正确」这类**通用**话术 —— 而导出恰恰
 * 最需要看原文（未闭合时要告诉用户差多少、去补哪一行）。这里把错误体读回文本再解析，
 * 拿不到就交回原错误由页面兜底（与 `cost-period.ts` 的 `blobError` 同一做法）。
 */
export async function exportCashflowStatement(period: string): Promise<Blob> {
  try {
    const response = await service.get<Blob>(`${BASE}/statement/export`, {
      params: asParams({ period }),
      responseType: 'blob',
      // 整表导出是一次性拉取（不受分页限制），默认 15s 对慢网太紧
      timeout: 60000,
    });
    return response.data;
  } catch (error) {
    throw await blobError(error);
  }
}

/** 把 blob 请求的失败还原成人话（优先后端 `msg`，它带差额与行名） */
async function blobError(error: unknown): Promise<Error> {
  const data = (error as { response?: { data?: unknown } }).response?.data;
  if (data instanceof Blob) {
    try {
      const text = await data.text();
      const body = JSON.parse(text) as { code?: number; msg?: string };
      if (body && typeof body.msg === 'string' && body.msg) {
        return new Error(body.msg);
      }
    } catch {
      // 不是 JSON（例如网关的 HTML 502）：保持原错误
    }
  }
  return error instanceof Error ? error : new Error(String(error));
}
