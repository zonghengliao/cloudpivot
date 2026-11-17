/**
 * 客户信用「实时占用 / 可用额度」API（后端 `backend/app/src/business/erp/credit_handler.rs`，M3-10）。
 *
 * # 为什么单独一个文件（不并进 `erp/index.ts`）
 *
 * 与 `customer-price.ts` / `shipment.ts` 同一口径：信用快照是独立只读接口（下单前的
 * 额度胶囊），按接口边界单独成文件，`index.ts` 一行不动。
 *
 * # 字段名逐字抄后端 `CreditSnapshotRes`（不是猜的）
 *
 * | 语义 | 真字段名 | 口径 |
 * |------|----------|------|
 * | 额度 | `credit_limit` | `0` = **不控额度**（这时可用额度是 `null`，不是 0） |
 * | 已占 | `used` | 有效订单含税 − 已核销应收 − 已过账退货（下限 0），**现算** |
 * | 缓存列 | `cached_used` + `cache_drift` | 与 `used` 对拍；`true` = 每日重算该修而没修 |
 * | 在审 | `pending` | `SUBMITTED` 订单价税合计（不含本单） |
 * | 未清应收 | `open_receivable` | `OPEN`/`PARTIAL` 的未结金额，**对照口径，不重复计入** |
 * | 占用 | `occupied` | `used + pending` |
 * | 合计 | `total` | `occupied + order_amount` |
 * | 可用 | `available` / `available_after` | 额度 − 占用 / 额度 − 合计（下限 0；不控额度 → `null`） |
 * | 超限 | `over` / `is_over` | 本单导致的超出额 / 是否已超 |
 * | 判定 | `verdict` / `allowed` | `UNCONTROLLED`·`WITHIN`·`WARN`·`PASS`·`BLOCK`；`BLOCK` 或客户冻结 → `allowed=false` |
 *
 * # 三个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端路由宏不含前缀；漏了会命中 SPA fallback 返回 HTML
 *    → 前端报「响应体格式不符合」）；
 * 2. **金额在 JSON 里是字符串**（`NUMERIC(18,4)` → `"1000.0000"`），比较 / 累加先过
 *    `toCents`（见 `@/views/business/erp/shared/money`），别直接 `+`；
 * 3. **这不是「拦单」接口，是「早知道」**：它只回答「现在还能下多少、本单会超多少」，
 *    真正的拦截在提交时（`POST /sale-order/{id}/status` 或 `/submit-approval`）。
 *    所以任何失败都**不该**阻止用户继续填单（页面按「取不到就不显示胶囊」处理）。
 */
import { get } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp/credit';

/** 后端 `NUMERIC(18,4)` 出 JSON 是字符串（也兼容数字）。 */
export type DecimalText = number | string;

/** 信用判定（与后端的 `CreditVerdict` 变体一一对应）。 */
export type CreditVerdictLabel = 'UNCONTROLLED' | 'WITHIN' | 'WARN' | 'PASS' | 'BLOCK';

/** 超限策略（与 `ck_biz_customer_credit_policy` 逐字一致）。 */
export type CreditPolicy = 'BLOCK' | 'WARN' | 'PASS';

/** 客户信用实时快照（后端 `CreditSnapshotRes` 逐字段对应）。 */
export interface CreditSnapshot {
  /** 客户主键 */
  customer_id: string;
  /** 客户编码 */
  customer_code: string;
  /** 客户名称 */
  customer_name: string;
  /** 客户状态：`ACTIVE` 正常 / `FROZEN` 冻结（冻结客户不许下单，与额度无关） */
  customer_status: string;
  /** 信用额度（`0` = 不控额度） */
  credit_limit: DecimalText;
  /** 超限策略 */
  credit_policy: CreditPolicy | string;
  /** 已占额度（现算） */
  used: DecimalText;
  /** 缓存列 `biz_customer.credit_used`（对拍用） */
  cached_used: DecimalText;
  /** 缓存列与现算值是否漂移 */
  cache_drift: boolean;
  /** 在审占用（`SUBMITTED`，不含本单） */
  pending: DecimalText;
  /** 未清应收（对照口径，不重复计入占用） */
  open_receivable: DecimalText;
  /** 占用 = 已占 + 在审 */
  occupied: DecimalText;
  /** 本次订单价税合计（没试算时 `null`） */
  order_amount: DecimalText | null;
  /** 合计 = 占用 + 本次订单 */
  total: DecimalText;
  /** 可用额度（不控额度时 `null`） */
  available: DecimalText | null;
  /** 本单占完之后还剩（不控额度时 `null`） */
  available_after: DecimalText | null;
  /** 本单导致的超出额 */
  over: DecimalText;
  /** 合计是否已超额度 */
  is_over: boolean;
  /** 判定 */
  verdict: CreditVerdictLabel;
  /** 能否提交（`BLOCK` 或客户冻结 → false） */
  allowed: boolean;
  /** 口径说明（一句话，可直接进 tooltip） */
  basis: string;
}

/** 查询条件 */
export interface CreditSnapshotQuery {
  /** 本次订单价税合计（试算；不传 = 只看当前口径） */
  amount?: DecimalText | null;
  /** 把本单从「在审」里排除（编辑已提交单时用） */
  exclude_order_id?: string | null;
}

/**
 * 取客户信用实时快照。
 *
 * @param customerId 客户主键（UUID）
 * @param query      `amount` / `exclude_order_id` 都可空；**空值不要发空串** ——
 *                   后端把空串按「没传」处理，但少发一个参数报文更干净
 */
export function fetchCreditSnapshot(
  customerId: string,
  query: CreditSnapshotQuery = {},
): Promise<CreditSnapshot> {
  const params: Record<string, string> = {};
  if (query.amount !== null && query.amount !== undefined && `${query.amount}`.trim() !== '') {
    params.amount = `${query.amount}`;
  }
  if (query.exclude_order_id) {
    params.exclude_order_id = query.exclude_order_id;
  }
  return get<CreditSnapshot>(`${BASE}/${customerId}`, Object.keys(params).length ? params : undefined);
}
