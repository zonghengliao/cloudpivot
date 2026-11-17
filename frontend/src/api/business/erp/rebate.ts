/**
 * 销售返利 API 层（M3-19 前端：政策配置 + 结算单）。
 *
 * # 两块内容、两个后端模块（字段名逐字抄，不是猜的）
 *
 * | 块 | 后端 | 权限码前缀 | 页面 |
 * |----|------|-----------|------|
 * | 返利政策 CRUD（含规则档位） | `erp/sales/rebate_handler.rs`（M3-15） | `business:erp:rebate-policy:*` | `views/business/erp/rebate`（政策 tab） |
 * | 结算单 试算/计算/跑批/重算/作废 | `erp/sales/rebate_settle_handler.rs`（M3-16） | `business:erp:rebate-settlement:*` | 同页（结算 tab） |
 *
 * # 逐字段对照（抄的是 DTO 与库列名）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 规则类型 | `rule_type`：`BY_QTY` / `BY_AMOUNT` / `TIERED` | `rebate_dto::RULE_TYPES` |
 * | 计价单位 | `rate_unit`：`PER_UNIT`（元/件）/ `PERCENT`（百分数） | `rebate_dto::RATE_UNITS` |
 * | 结算粒度 | `settle_period`：`MONTH` / `QUARTER` | `rebate_dto::SETTLE_PERIODS` |
 * | 政策状态 | `status`：`1` 启用 / `0` 停用（**数字**，不是字符串） | `rebate_dto::STATUS_ENABLED` |
 * | 档位区间 | `tier_from`（含）/ `tier_to`（不含，`null` = 无上限） | `biz_sales_rebate_rule` |
 * | 期间码 | 月度 `yyyy-MM`、季度 `yyyy-QN`（形状与粒度**互相钉死**） | `ck_biz_srs_period_shape` |
 * | 结算状态 | `status`：`DRAFT` / `SETTLED` / `CANCELLED` | `ck_biz_srs_status` |
 * | 净基数 | `base.net_amount` / `base.net_qty`（= 订单 − 退货，**可为负**） | `RebateBaseRes` |
 * | 红字发票 | `invoice_id` / `invoice_no` / `settled_at`（M3-17 写入） | `RebateSettleRes` |
 * | 开票 / 票面试算 | `POST …/{id}/invoice`、`POST …/{id}/invoice-preview`（**POST 不是 GET**） | `rebate_invoice_handler.rs` |
 *
 * # 五个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端路由宏不含前缀，漏了会命中 SPA fallback 返回 HTML
 *    → 前端报「响应体格式不符合」）；
 * 2. **金额 / 比率 / 数量全是字符串**（`NUMERIC` 的 JSON 形式，如 `"0.500000"`），
 *    `t-input-number` 只吃数字 —— 表单里存字符串、展示走本页 `moneyText()`，**别拿来做算术**；
 * 3. **`rules` 的顺序不重要**：后端按 `tier_from` 升序重排后重新编号（`seq = 1..N`），
 *    前端给的顺序与 `seq` 一律被忽略；但**形状**是硬约束（按量/按额恰好 1 档、
 *    阶梯 2~20 档首尾相接），错了返回 `400 + 66089` 并点名第几档；
 * 4. **编辑政策是整单覆盖**（`PUT` 不是 `PATCH`）：政策是条款，改了 `rule_type`
 *    却留着旧口径的档位会让库里的档位与类型互相解释不通 —— 弹窗必须提交全量字段；
 * 5. **期间必须被政策覆盖**：期间首日没有启用政策、或政策没覆盖整期 → `409 + 66091`
 *    （报文写清缺哪段日期）。这不是 bug，是「金额不猜」的取舍，页面要原样展示。
 */
import { del, get, post, put, type PageData } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const POLICY_BASE = '/api/business/erp/rebate-policy';
const SETTLE_BASE = '/api/business/erp/rebate-settlement';

/** 金额 / 比率 / 数量：后端 `NUMERIC` 在 JSON 里是字符串，`t-input-number` 给数字 */
export type DecimalText = string | number;

// ==================== 枚举（逐字抄后端常量与库层 CHECK） ====================

/** 规则类型：按量 / 按额 / 阶梯 */
export type RebateRuleType = 'BY_QTY' | 'BY_AMOUNT' | 'TIERED';

/** 计价单位：元/件 / 销售额百分比 */
export type RebateRateUnit = 'PER_UNIT' | 'PERCENT';

/** 结算粒度：月度 / 季度 */
export type RebateSettlePeriod = 'MONTH' | 'QUARTER';

/** 政策状态：`1` 启用（参与结算）/ `0` 停用（保留历史） */
export type RebatePolicyStatus = 0 | 1;

/** 结算单状态（与 `rebate_settle_dto::STATUSES` 逐字一致） */
export type RebateSettlementStatus = 'DRAFT' | 'SETTLED' | 'CANCELLED';

/** 跑批单客户结果（`CREATED` 新建 / `EXISTS` 已有跳过 / `FAILED` 失败） */
export type RebateRunOutcome = 'CREATED' | 'EXISTS' | 'FAILED';

// 状态域常量 / 期间形状判断放在页面逻辑层（`views/.../rebate/*-logic.ts`）：
// 本文件 import 了 `@/api/request`（`import.meta.env`），而单测跑在 node 下
// （`import.meta.env === undefined`）—— 页面逻辑若从本文件取**运行时值**，
// 整条测试链会在加载阶段就崩（本仓 `pnpm test:unit` 的既有约束）。

// ==================== 返利政策 ====================

/** 规则一行（字段名逐字抄 `biz_sales_rebate_rule` 列名 / `RebateRuleRes`） */
export interface RebateRuleRow {
  /** 规则行主键 */
  id: string;
  /** 档号（1 起；后端按 `tier_from` 升序重排后编号） */
  seq: number;
  /** 本档下限（含） */
  tier_from: DecimalText;
  /** 本档上限（不含）；`null` = 无上限（只有末档可空） */
  tier_to: DecimalText | null;
  /** 比率：`PER_UNIT` ⇒ 元/件；`PERCENT` ⇒ 百分数（10 = 10%） */
  rate: DecimalText;
  /** 本档备注 */
  remark: string | null;
}

/** 政策（详情与列表同形；`RebatePolicyRes`） */
export interface RebatePolicyRow {
  id: string;
  /** 政策编号 `BR-yyyyMMdd-000001` */
  policy_no: string;
  policy_name: string;
  customer_id: string;
  /** 客户编码（客户已软删时为 `null`） */
  customer_code: string | null;
  customer_name: string | null;
  rule_type: RebateRuleType;
  rate_unit: RebateRateUnit;
  settle_period: RebateSettlePeriod;
  /** 生效日（含，`yyyy-MM-dd`） */
  effective_from: string;
  /** 失效日（含；`null` = 长期有效） */
  effective_to: string | null;
  status: RebatePolicyStatus;
  remark: string | null;
  created_at: string;
  updated_at: string;
  /** 规则档位（按 `seq` 升序；列表也带，前端不必逐条再请求） */
  rules: RebateRuleRow[];
}

/** 一档规则（入参；`seq` 由后端重排，不填） */
export interface RebateRuleInput {
  /** 本档下限（含；按量/按额必须 0；省略 = 0） */
  tier_from?: DecimalText;
  /** 本档上限（不含）；省略 / `null` = 无上限 */
  tier_to?: DecimalText | null;
  /** 比率（必填，> 0；`PERCENT` ≤ 100） */
  rate?: DecimalText;
  remark?: string | null;
}

/** 新建 / 编辑政策（**整单覆盖**：政策头 + 规则整单替换） */
export interface RebatePolicyWriteReq {
  /** 政策名称（必填，≤ 200 字） */
  policy_name: string;
  /** 客户 id（必须存在、未软删且 `ACTIVE`） */
  customer_id: string;
  rule_type: RebateRuleType;
  /** 计价单位（按量/按额可省略 —— 后端按类型推；阶梯必填） */
  rate_unit?: RebateRateUnit;
  settle_period: RebateSettlePeriod;
  /** 生效日 `yyyy-MM-dd` */
  effective_from: string;
  /** 失效日 `yyyy-MM-dd`；省略 / 空 = 长期有效 */
  effective_to?: string | null;
  /** `1` 启用 / `0` 停用；省略 = 新建 1 / 编辑保留原值 */
  status?: RebatePolicyStatus;
  remark?: string | null;
  /** 规则档位（顺序无关；形状由后端 `checked_rules` 校验） */
  rules: RebateRuleInput[];
}

/** 政策列表筛选（空白值一律不发） */
export interface RebatePolicyFilter {
  /** 关键字：政策编号 / 政策名称 / 客户编码 / 客户名称 模糊 */
  keyword?: string;
  customer_id?: string;
  rule_type?: RebateRuleType | '';
  settle_period?: RebateSettlePeriod | '';
  /** `1` / `0` / `''`（全部） */
  status?: RebatePolicyStatus | '';
  /** 「在指定日期生效」筛选 `yyyy-MM-dd` */
  active_on?: string;
  page?: number;
  page_size?: number;
}

/** 去掉空串 / null / undefined（后端把 `?x=` 当「等于空串」过滤，会查出 0 行） */
function compact(query: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(query)) {
    if (value === null || value === undefined) {
      continue;
    }
    if (typeof value === 'string' && value.trim() === '') {
      continue;
    }
    if (Array.isArray(value) && value.length === 0) {
      continue;
    }
    out[key] = typeof value === 'string' ? value.trim() : value;
  }
  return out;
}

/** 政策分页（`GET /business/erp/rebate-policy/page`，权限 `…:list`） */
export function fetchRebatePolicyPage(
  query: RebatePolicyFilter = {},
): Promise<PageData<RebatePolicyRow>> {
  return get<PageData<RebatePolicyRow>>(`${POLICY_BASE}/page`, compact(query as Record<string, unknown>));
}

/** 政策详情（`GET /business/erp/rebate-policy/{id}`，同为 `…:list`） */
export function fetchRebatePolicy(id: string): Promise<RebatePolicyRow> {
  return get<RebatePolicyRow>(`${POLICY_BASE}/${id}`);
}

/** 新建政策（`POST /business/erp/rebate-policy`，权限 `…:create`；取号 `BR-yyyyMMdd-000001`） */
export function createRebatePolicy(payload: RebatePolicyWriteReq): Promise<RebatePolicyRow> {
  return post<RebatePolicyRow>(POLICY_BASE, payload);
}

/** 编辑政策（`PUT /business/erp/rebate-policy/{id}`，权限 `…:update`；规则整单替换） */
export function updateRebatePolicy(
  id: string,
  payload: RebatePolicyWriteReq,
): Promise<RebatePolicyRow> {
  return put<RebatePolicyRow>(`${POLICY_BASE}/${id}`, payload);
}

/** 软删政策（`DELETE /business/erp/rebate-policy/{id}`，权限 `…:delete`；历史结算单不受影响） */
export function deleteRebatePolicy(id: string): Promise<void> {
  return del<void>(`${POLICY_BASE}/${id}`);
}

// ==================== 返利结算 ====================

/** 政策档位快照（结算单里冻结的那一份） */
export interface RebatePolicyTierSnapshot {
  seq: number;
  tier_from: DecimalText;
  tier_to: DecimalText | null;
  rate: DecimalText;
  remark: string | null;
}

/** 基数（订单 / 退货 / 净额；列表、详情、试算同形） */
export interface RebateBaseNumbers {
  /** 计入的已确认销售订单张数 */
  order_count: number;
  /** 订单本位币含税金额合计 */
  order_amount: DecimalText;
  /** 订单明细数量合计 */
  order_qty: DecimalText;
  /** 计入的已过账退货张数 */
  return_count: number;
  /** 退货含税金额合计（折本位币） */
  return_amount: DecimalText;
  /** 退货明细数量合计 */
  return_qty: DecimalText;
  /** 净销售额 = 订单 − 退货（**可为负**：退货多于销售） */
  net_amount: DecimalText;
  /** 净销量 = 订单 − 退货 */
  net_qty: DecimalText;
}

/** 结算单（详情与列表同形；`RebateSettleRes`） */
export interface RebateSettlementRow {
  id: string;
  /** 结算单号 `RB-yyyyMMdd-000001` */
  settlement_no: string;
  customer_id: string;
  customer_code: string | null;
  customer_name: string | null;
  settle_period: RebateSettlePeriod;
  /** 粒度中文（月度 / 季度） */
  settle_period_label: string;
  /** 期间码 `yyyy-MM` / `yyyy-QN` */
  period_code: string;
  /** 期间首日（含） */
  period_from: string;
  /** 期间末日 +1（不含） */
  period_to: string;
  /** 期间末日的「含」展示值（= period_to − 1） */
  period_last_day: string;
  policy_id: string;
  policy_no: string;
  policy_name: string;
  rule_type: RebateRuleType;
  rule_type_label: string;
  rate_unit: RebateRateUnit;
  rate_unit_label: string;
  policy_effective_from: string;
  policy_effective_to: string | null;
  /** 政策全部档位快照 */
  policy_tiers: RebatePolicyTierSnapshot[];
  base: RebateBaseNumbers;
  /** 命中档号（未命中为 `null`） */
  tier_seq: number | null;
  tier_from: DecimalText;
  tier_to: DecimalText | null;
  /** 命中档比率快照 */
  rate: DecimalText;
  /** 应返金额（恒 ≥ 0） */
  rebate_amount: DecimalText;
  status: RebateSettlementStatus;
  status_label: string;
  remark: string | null;
  created_at: string;
  updated_at: string;
  /** 红字发票 id（M3-17；未开票为 `null`） */
  invoice_id: string | null;
  /** 红字发票号（M3-17） */
  invoice_no: string | null;
  /** 转结时间（置 `SETTLED` 的时刻） */
  settled_at: string | null;
}

/** 结算单摘要（试算里回显「这期已经算过」） */
export interface RebateSettleBrief {
  id: string;
  settlement_no: string;
  status: RebateSettlementStatus;
  status_label: string;
  rebate_amount: DecimalText;
}

/** 试算里的政策片段 */
export interface RebatePreviewPolicy {
  policy_id: string;
  policy_no: string;
  policy_name: string;
  rule_type: RebateRuleType;
  rate_unit: RebateRateUnit;
  effective_from: string;
  effective_to: string | null;
  tiers: RebatePolicyTierSnapshot[];
}

/** 试算出参（**不落库**；`POST /rebate-settlement/preview`） */
export interface RebatePreview {
  customer_id: string;
  customer_code: string | null;
  customer_name: string | null;
  settle_period: RebateSettlePeriod;
  settle_period_label: string;
  period_code: string;
  period_from: string;
  period_to: string;
  period_last_day: string;
  /** 适用政策（`null` = 该期间无覆盖整期的启用政策，此时 `rebate_amount = 0`） */
  policy: RebatePreviewPolicy | null;
  base: RebateBaseNumbers;
  tier_seq: number | null;
  tier_from: DecimalText;
  tier_to: DecimalText | null;
  rate: DecimalText;
  rebate_amount: DecimalText;
  /** 提示（无订单 / 负基数 / 无政策 …） */
  warnings: string[];
  /** 该客户该期间已有的结算单（未作废；`null` = 还没算过） */
  existing: RebateSettleBrief | null;
}

/** 跑批单客户结果 */
export interface RebateRunItem {
  customer_id: string;
  customer_code: string | null;
  customer_name: string | null;
  outcome: RebateRunOutcome;
  settlement_id: string | null;
  settlement_no: string | null;
  status: RebateSettlementStatus | null;
  rebate_amount: DecimalText | null;
  /** 失败原因（`FAILED` 时后端原文） */
  message: string | null;
}

/** 跑批出参 */
export interface RebateRunRes {
  settle_period: RebateSettlePeriod;
  settle_period_label: string;
  period_code: string;
  period_from: string;
  period_to: string;
  /** 命中政策数（= 参与跑批的客户数） */
  total: number;
  created: number;
  skipped: number;
  failed: number;
  items: RebateRunItem[];
}

/** 计算 / 试算 / 重算入参（试算忽略 `remark`） */
export interface RebateSettleReq {
  customer_id: string;
  settle_period: RebateSettlePeriod;
  /** 期间码：月度 `yyyy-MM`、季度 `yyyy-QN` */
  period_code: string;
  remark?: string | null;
}

/** 整期跑批入参（`customer_ids` 空 = 该期间有政策的全部客户） */
export interface RebateRunReq {
  settle_period: RebateSettlePeriod;
  period_code: string;
  customer_ids?: string[];
  remark?: string | null;
}

/** 结算单列表筛选（空白值一律不发） */
export interface RebateSettlementFilter {
  /** 关键字：结算单号 / 客户编码 / 客户名称 / 政策编号 模糊 */
  keyword?: string;
  customer_id?: string;
  settle_period?: RebateSettlePeriod | '';
  /** 期间码精确 */
  period_code?: string;
  status?: RebateSettlementStatus | '';
  policy_id?: string;
  page?: number;
  page_size?: number;
}

/** 试算（不落库；权限 `…:calc`） */
export function previewRebateSettlement(payload: RebateSettleReq): Promise<RebatePreview> {
  return post<RebatePreview>(`${SETTLE_BASE}/preview`, payload);
}

/** 计算并落库（`DRAFT` + 政策快照；权限 `…:calc`） */
export function createRebateSettlement(payload: RebateSettleReq): Promise<RebateSettlementRow> {
  return post<RebateSettlementRow>(SETTLE_BASE, payload);
}

/** 整期跑批（逐客户一个事务；已有则跳过；权限 `…:calc`） */
export function runRebateSettlement(payload: RebateRunReq): Promise<RebateRunRes> {
  return post<RebateRunRes>(`${SETTLE_BASE}/run`, payload);
}

/** 重算（仅 `DRAFT`；换政策 / 新增订单后刷新；权限 `…:calc`） */
export function recalcRebateSettlement(
  id: string,
  payload: RebateSettleReq,
): Promise<RebateSettlementRow> {
  return post<RebateSettlementRow>(`${SETTLE_BASE}/${id}/recalc`, payload);
}

/** 结算单分页（权限 `…:list`） */
export function fetchRebateSettlementPage(
  query: RebateSettlementFilter = {},
): Promise<PageData<RebateSettlementRow>> {
  return get<PageData<RebateSettlementRow>>(
    `${SETTLE_BASE}/page`,
    compact(query as Record<string, unknown>),
  );
}

/** 结算单详情（含政策全量档位快照；权限 `…:list`） */
export function fetchRebateSettlement(id: string): Promise<RebateSettlementRow> {
  return get<RebateSettlementRow>(`${SETTLE_BASE}/${id}`);
}

/**
 * 作废结算单（仅 `DRAFT`；行保留供审计，同期间可重开；权限 `…:delete`）。
 *
 * 后端这条是 `DELETE` + **query 参数**（`Query<RebateSettleReq>`，只用 `remark`），
 * 不是请求体 —— 写成 body 后端收不到原因。
 */
export function cancelRebateSettlement(id: string, remark?: string): Promise<void> {
  return del<void>(`${SETTLE_BASE}/${id}`, compact({ remark: remark ?? '' }));
}

// ==================== 返利 → 红字发票（M3-17 后端 / 本卡前端） ====================

/**
 * 开票入参（三个字段都可省：省了就是「今天 / 13% / 无备注」）。
 *
 * 字段逐字抄 `rebate_invoice_dto::RebateInvoiceReq`：
 * - `invoice_date`：`NaiveDate` ⇒ JSON 是 `"yyyy-MM-dd"` 字符串；
 * - `tax_rate`：**百分数**（13 = 13%，不是 0.13），`NUMERIC(7,4)` 最多 4 位小数、0~100；
 * - `remark`：≤ 200 字（空白会被后端归一成 `null`，然后用自动备注）。
 */
export interface RebateInvoiceReq {
  /** 开票日期 `yyyy-MM-dd`；省 = 服务端当天（CST） */
  invoice_date?: string | null;
  /** 税率（百分数 13 = 13%；省 = 13） */
  tax_rate?: DecimalText | null;
  /** 备注（省 = 后端自动生成「返利结算 RB-… 红冲」） */
  remark?: string | null;
}

/** 凭证分录预览行（`VoucherLineRes`；金额是**正数绝对值**） */
export interface RebateInvoiceVoucherLine {
  /** `DEBIT` 借 / `CREDIT` 贷 */
  side: 'DEBIT' | 'CREDIT' | string;
  /** 科目编码（如 `5001` / `2221.02` / `2241.02`） */
  account_code: string;
  /** 科目名称（后端 join 实时名） */
  account_name: string;
  /** 金额（绝对值） */
  amount: DecimalText;
  /** 摘要 */
  summary: string;
}

/**
 * 一张已开出的红字发票（`POST …/{id}/invoice` 的回执，也是预览里的 `existing_invoice`）。
 *
 * 字段逐字抄 `RebateInvoiceRes`。注意三个金额**全是负数**（红字票的库层
 * `ck_biz_invoice_sign` 要求 `amount < 0 && tax ≤ 0`），展示时别再加负号。
 */
export interface RebateInvoiceRow {
  settlement_id: string;
  settlement_no: string;
  /** 结算单状态（开票成功后恒 `SETTLED`） */
  settlement_status: RebateSettlementStatus;
  settlement_status_label: string;
  customer_id: string;
  customer_code: string | null;
  customer_name: string | null;
  settle_period: RebateSettlePeriod;
  period_code: string;
  /** 应返金额（含税，**正数**：结算单原值） */
  rebate_amount: DecimalText;
  /** 红字发票 id */
  invoice_id: string;
  /** 红字发票号 `IV-yyyyMMdd-000001` */
  invoice_no: string;
  /** 票种（恒 `OUT_RED`） */
  invoice_type: string;
  /** 票种中文「销项红字票」 */
  invoice_type_label: string;
  /** 开票日期 `yyyy-MM-dd` */
  invoice_date: string;
  /** 不含税金额（**负**） */
  invoice_amount: DecimalText;
  /** 税额（**≤ 0**） */
  invoice_tax: DecimalText;
  /** 价税合计（**负** = 冲减的含税金额） */
  invoice_total: DecimalText;
  /** 税率（百分数） */
  tax_rate: DecimalText;
  /** 本位币 */
  currency_code: string;
  /** 发票状态（`ISSUED`；后续可能被作废成 `VOID`） */
  invoice_status: string;
  /** 红字凭证 id */
  voucher_id: string | null;
  /** 红字凭证号 */
  voucher_no: string | null;
  /** 转结时间 */
  settled_at: string | null;
}

/**
 * 票面试算（`POST …/{id}/invoice-preview`，**不落库**）。
 *
 * 字段逐字抄 `RebateInvoicePreviewRes`。三件事一次给全：
 * ① 三列拆分（`invoice_amount` / `invoice_tax` / `invoice_total`，全为负）；
 * ② 凭证分录（`voucher_lines`，借方为正数绝对值）；
 * ③ **能不能开 + 拦在哪**（`can_generate` / `blockers` / `warnings`）。
 *
 * `can_generate === false` 时**不是接口报错**（HTTP 200）：预览的作用就是
 * 「点开就能看见为什么不能开」，所以这里照常渲染，把 `blockers` 原样列出。
 */
export interface RebateInvoicePreview {
  settlement_id: string;
  settlement_no: string;
  settlement_status: RebateSettlementStatus;
  settlement_status_label: string;
  customer_id: string;
  customer_code: string | null;
  customer_name: string | null;
  settle_period: RebateSettlePeriod;
  period_code: string;
  /** 应返金额（含税，正数） */
  rebate_amount: DecimalText;
  /** 将采用的税率（百分数） */
  tax_rate: DecimalText;
  /** 票种（恒 `OUT_RED`） */
  invoice_type: string;
  invoice_type_label: string;
  /** 将采用的开票日期 */
  invoice_date: string;
  currency_code: string;
  /** 不含税金额（负） */
  invoice_amount: DecimalText;
  /** 税额（≤ 0） */
  invoice_tax: DecimalText;
  /** 价税合计（负） */
  invoice_total: DecimalText;
  /** 能否开票（`false` 时看 `blockers`） */
  can_generate: boolean;
  /** 拦下来的原因（人话；每条都是一个可修的动作） */
  blockers: string[];
  /** 提醒（不拦，例如税率与默认 13% 不一致） */
  warnings: string[];
  /** 凭证分录预览 */
  voucher_lines: RebateInvoiceVoucherLine[];
  /** 已开出的发票（`SETTLED` 时回显；`DRAFT` / `CANCELLED` 为 `null`） */
  existing_invoice: RebateInvoiceRow | null;
}

/**
 * 票面试算（**不落库**；权限 `business:erp:rebate-settlement:invoice`）。
 *
 * ⚠️ 这是 **POST 不是 GET**：后端要吃 `invoice_date` / `tax_rate` 才能算出
 * 「改开票日会不会撞已结账期间」「换个税率拆出来多少」，路径参数只有 id 没法表达。
 * （`rebate_invoice_handler::rebate_invoice_preview_api` 是 `#[nik::post]`，
 * `rebate_invoice_dto` 顶部注释里写的 `GET` 是文档笔误。）
 */
export function previewRebateInvoice(
  id: string,
  payload: RebateInvoiceReq = {},
): Promise<RebateInvoicePreview> {
  return post<RebateInvoicePreview>(`${SETTLE_BASE}/${id}/invoice-preview`, payload);
}

/**
 * 生成红字发票（`DRAFT ──▶ SETTLED`；权限同上）。
 *
 * 票、凭证、结算单状态在一个事务里同生共死（后端 `generate_of`），所以：
 * - 成功 ⇒ 回执里 `settlement_status = 'SETTLED'` + `invoice_no` + `voucher_no`；
 * - 失败 ⇒ 结算单仍是 `DRAFT`，**改完可以重来**（不需要工程师改库）。
 *
 * 会撞的三道闸：`66093` 状态（已开过 / 已作废）、`66094` 应返 ≤ 0、
 * `66095` 开票日落在已结账期间。报文原样展示即可（`constants/error-messages.ts` 有翻译）。
 */
export function createRebateInvoice(
  id: string,
  payload: RebateInvoiceReq = {},
): Promise<RebateInvoiceRow> {
  return post<RebateInvoiceRow>(`${SETTLE_BASE}/${id}/invoice`, payload);
}

// ==================== 权限码（与 V0438 / V0439 / V0440 的 auth_code 逐字一致） ====================

/** 本页全部权限码（页面里 `v-privilege` / `hasPermission` 用的唯一一处定义） */
export const REBATE_PERMISSIONS = {
  policyList: 'business:erp:rebate-policy:list',
  policyCreate: 'business:erp:rebate-policy:create',
  policyUpdate: 'business:erp:rebate-policy:update',
  policyDelete: 'business:erp:rebate-policy:delete',
  settleList: 'business:erp:rebate-settlement:list',
  settleCalc: 'business:erp:rebate-settlement:calc',
  settleDelete: 'business:erp:rebate-settlement:delete',
  /** 票面试算 + 生成红字发票（M3-17；读写共用一个动作码，V0440 种） */
  settleInvoice: 'business:erp:rebate-settlement:invoice',
} as const;
