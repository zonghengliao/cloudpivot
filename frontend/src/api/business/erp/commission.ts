/**
 * 佣金 API 层（Sales Commission · ARCH-7 §15.4 / SO-36 后端 + SO-37 前端）。
 *
 * # 两块内容、两个后端模块
 *
 * | 块 | 后端 | 权限码前缀 | 页面 |
 * |----|------|-----------|------|
 * | 佣金规则 CRUD | `erp/commission_rule_handler.rs`（SO-36b） | `business:erp:commission-rule:*` | `views/business/erp/commission-rule` |
 * | 结算单 / 明细 | `erp/commission_handler.rs`（SO-36） | `business:erp:commission-settle:*` | `views/business/erp/commission` |
 *
 * # 字段名逐字抄后端（不是猜的）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 规则范围 | `scope_type`：`SALESPERSON` / `MATERIAL_CATEGORY` / `ALL` | `ck_biz_cmrule_scope` |
 * | 范围对象 | `scope_id`（`ALL` ⇒ `null`，其余必填） | `ck_biz_cmrule_scope_id` |
 * | 计算方式 | `calc_type`：`PERCENT`（百分比）/ `FIXED`（固定额） | `ck_biz_cmrule_calc` |
 * | 比率 | `rate`（`PERCENT` 是**百分数** 0–100，不是 0.05 这种小数） | `ck_biz_cmrule_rate` |
 * | 起始金额 | `min_amount`（基数低于此值不计佣） | `biz_commission_rule.min_amount` |
 * | 封顶 | `max_amount`（`null` = 不封顶） | `ck_biz_cmrule_amount` |
 * | 结算期间 | `period_code`（`yyyy-MM`） | `ck_biz_cmset_period` |
 * | 结算基数 | `total_sales` / 明细 `base_amount`（**有符号**：正计提 / 负冲回） | `commission_dto.rs` 头注释 |
 * | 结算状态 | `status`：`DRAFT` → `APPROVED` → `PAID`（无回退边） | `ck_biz_cmset_status` + `LEGAL_EDGES` |
 *
 * # 五个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端路由宏不含前缀，漏了会命中 SPA fallback 返回 HTML
 *    → 前端报「响应体格式不符合」）；
 * 2. **金额 / 比率都是字符串**（`NUMERIC` 的 JSON 形式），`t-input-number` 只吃数字，
 *    所以表单里存字符串、提交前转字符串，展示一律走本页 `decimalText()`，别拿来直接相加；
 * 3. **`rate` 的语义随 `calc_type` 变**：`PERCENT` 填 5 表示 5%，`FIXED` 填 5 表示 5 元/单，
 *    表单切换 `calc_type` 时**必须**清空 `rate`（否则 5% 会被当成 5 元佣金）；
 * 4. **结算单 `PATCH {id}/status` 只认 `APPROVED` / `PAID`**（`DRAFT` 是生成时的初始态），
 *    且必须按 `DRAFT→APPROVED→PAID` 走，跳步后端 409；
 * 5. **生成结算单是「幂等重算」**：同人同期间重复生成会覆盖旧草稿，但 `APPROVED` / `PAID`
 *    的结算单会 409 拒重算 —— 这不是错误，要在页面里原样展示。
 */
import { del, get, patch, post, put, type PageData } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/** 金额 / 比率：后端 `NUMERIC` 在 JSON 里是字符串，`t-input-number` 给数字 */
export type DecimalText = string | number;

// ==================== 枚举（逐字抄库层 CHECK） ====================

/** 规则适用范围：按销售员 / 按物料 / 全局 */
export type CommissionScopeType = 'SALESPERSON' | 'MATERIAL_CATEGORY' | 'ALL';

/** 计算方式：百分比 / 固定额 */
export type CommissionCalcType = 'PERCENT' | 'FIXED';

/** 规则状态：`INACTIVE` 只是不再参与匹配，历史明细仍引用它 */
export type CommissionRuleStatus = 'ACTIVE' | 'INACTIVE';

/** 结算单状态（与 `commission_dto::STATUSES` 逐字一致，顺序 = 状态机走向） */
export type CommissionSettlementStatus = 'DRAFT' | 'APPROVED' | 'PAID';

// 状态域常量 / 合法边表放在页面逻辑层（`views/.../commission/commission-logic.ts`）：
// 本文件 import 了 `@/api/request`（`import.meta.env`），而单测跑在 node 下
// （`import.meta.env === undefined`）—— 页面逻辑若从本文件取**运行时值**，
// 整条测试链会在加载阶段就崩（本仓 `pnpm test:unit` 的既有约束）。

// ==================== 佣金规则 ====================

/** 规则一行（字段名逐字抄 `biz_commission_rule` 列名） */
export interface CommissionRuleRow {
  id: string;
  /** 规则名（人读；`VARCHAR(100)`） */
  rule_name: string;
  scope_type: CommissionScopeType;
  /** 范围对象 id：`SALESPERSON` ⇒ `sys_user.id`，`MATERIAL_CATEGORY` ⇒ `biz_material.id`（基础版无分类表），`ALL` ⇒ `null` */
  scope_id: string | null;
  calc_type: CommissionCalcType;
  /** `PERCENT` ⇒ 百分数 0–100；`FIXED` ⇒ 金额（≥0） */
  rate: DecimalText;
  /** 起算金额（基数低于此值不计佣） */
  min_amount: DecimalText;
  /** 单笔封顶（`null` = 不封顶） */
  max_amount: DecimalText | null;
  /** 生效起始日（含，`yyyy-MM-dd`） */
  effective_from: string;
  /** 生效截止日（含；`null` = 长期有效） */
  effective_to: string | null;
  status: CommissionRuleStatus;
  remark: string | null;
  created_at: string;
  updated_at: string;
  /**
   * 范围对象的显示名（**可选**：后端若下发就用，没下发前端按 `scope_id` 前 8 位降级）。
   * `SALESPERSON` ⇒ 销售员姓名，`MATERIAL_CATEGORY` ⇒ 物料名称。
   */
  scope_name?: string | null;
}

/** 规则分页筛选（空白值一律不发：后端把 `?x=` 当「等于空串」过滤，会查出 0 行） */
export interface CommissionRuleFilter {
  keyword?: string;
  scope_type?: CommissionScopeType | '';
  status?: CommissionRuleStatus | '';
  page?: number;
  page_size?: number;
}

/** 新建规则入参 */
export interface CreateCommissionRuleReq {
  rule_name: string;
  scope_type: CommissionScopeType;
  /** `ALL` 时必须为 `null`（库层 `ck_biz_cmrule_scope_id` 强制） */
  scope_id: string | null;
  calc_type: CommissionCalcType;
  rate: DecimalText;
  min_amount: DecimalText;
  /** `null` = 不封顶 */
  max_amount: DecimalText | null;
  effective_from: string;
  /** `null` = 长期有效 */
  effective_to: string | null;
  status: CommissionRuleStatus;
  remark: string | null;
}

/**
 * 编辑规则入参：本页**总是发全量**（不做部分更新）。
 *
 * 不沿用「不传 = 不改」那套：规则表只有 11 个业务字段，全量覆盖的语义最清楚，
 * 也不会有「用户清空了备注但没传字段 → 备注改不掉」这类幽灵行为。
 */
export type UpdateCommissionRuleReq = CreateCommissionRuleReq;

/** 丢掉空串 / `null` / `undefined`（后端把 `?x=` 当「等于空串」过滤） */
function asParams<T extends object>(query: T): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === '') {
      continue;
    }
    out[key] = value;
  }
  return out;
}

/** 规则分页（`GET /business/erp/commission-rule/page`，权限 `business:erp:commission-rule:list`） */
export function fetchCommissionRulePage(
  params: CommissionRuleFilter,
): Promise<PageData<CommissionRuleRow>> {
  return get<PageData<CommissionRuleRow>>(`${BASE}/commission-rule/page`, asParams(params));
}

/** 新建规则（`POST /business/erp/commission-rule`，权限 `:create`） */
export function createCommissionRule(
  payload: CreateCommissionRuleReq,
): Promise<CommissionRuleRow> {
  return post<CommissionRuleRow>(`${BASE}/commission-rule`, payload);
}

/** 编辑规则（`PUT /business/erp/commission-rule/{id}`，权限 `:update`） */
export function updateCommissionRule(
  id: string,
  payload: UpdateCommissionRuleReq,
): Promise<CommissionRuleRow> {
  return put<CommissionRuleRow>(`${BASE}/commission-rule/${id}`, payload);
}

/**
 * 软删规则（`DELETE /business/erp/commission-rule/{id}`，权限 `:delete`）。
 *
 * **停用应该走编辑改 `status='INACTIVE'`**：软删只用于「录错了」，因为历史明细
 * 仍引用 `rule_id`。
 */
export function deleteCommissionRule(id: string): Promise<void> {
  return del<void>(`${BASE}/commission-rule/${id}`);
}

// ==================== 结算单 ====================

/** 结算单一行（逐字抄 `commission_dto::SettlementRes`） */
export interface CommissionSettlementRow {
  id: string;
  /** 结算单号 `CM-yyyyMM-000001` */
  settlement_no: string;
  salesperson_id: string;
  /** 销售员姓名（账号软删后空串） */
  salesperson_name: string;
  /** 结算期间 `yyyy-MM` */
  period_code: string;
  /** 期间佣金基数合计（已收款口径；负值按 0 兜底 —— 纯冲回期间记 0） */
  total_sales: DecimalText;
  /** 应发佣金合计（**可为负**：纯冲回期间） */
  total_commission: DecimalText;
  status: CommissionSettlementStatus;
  /** 发放时间（仅 `PAID` 有值） */
  paid_at: string | null;
  /** 计提凭证 id（审批通过后才有） */
  voucher_id: string | null;
  /** 计提凭证号（`PZ-...`） */
  voucher_no: string | null;
  /** 明细行数 */
  item_count: number;
  remark: string | null;
  created_at: string;
}

/** 佣金明细行（逐字抄 `commission_dto::CommissionItemRes`） */
export interface CommissionItemRow {
  id: string;
  /** 来源销售订单 id / 单号（弱引用，订单被删为 `null`） */
  so_id: string | null;
  so_no: string | null;
  /** 对应应收 id / 单号 */
  receivable_id: string | null;
  ar_no: string | null;
  /** 命中的规则 id / 规则名（快照） */
  rule_id: string | null;
  rule_name: string | null;
  /** 佣金基数（**有符号**：正计提 / 负冲回） */
  base_amount: DecimalText;
  /** 比率快照（`PERCENT` 的百分数 / `FIXED` 的金额） */
  rate: DecimalText;
  /** 本行佣金（可为负：退货 / 退款冲回） */
  commission: DecimalText;
  /** 行备注（如「退货 / 退款冲回」） */
  remark: string | null;
}

/** 结算单详情 = 单头（flatten）+ 明细 */
export interface CommissionSettlementDetail extends CommissionSettlementRow {
  /** 明细（后端按佣金倒序：计提在前、冲回在后） */
  items: CommissionItemRow[];
}

/** 结算单分页筛选 */
export interface CommissionSettlementFilter {
  period_code?: string;
  salesperson_id?: string;
  status?: CommissionSettlementStatus | '';
  /** 结算单号 / 销售员姓名 / 登录名 */
  keyword?: string;
  page?: number;
  page_size?: number;
}

/** 生成结算单入参 */
export interface SettleCommissionReq {
  /** 期间 `yyyy-MM`；缺省 = 当前月（北京时间） */
  period_code?: string;
  /** 只给这个销售员结算；缺省 = 所有有可计佣差额的销售员各生成一张 */
  salesperson_id?: string;
  remark?: string;
}

/** 生成结算单跳过的销售员（**不是错误**：没有可计佣的已收款 / 已有审批中的单） */
export interface CommissionSkippedRow {
  salesperson_id: string;
  salesperson_name: string;
  reason: string;
}

/** 生成结算单出参：逐销售员的处理结果 */
export interface SettleCommissionRes {
  period_code: string;
  /** 生成了的结算单（`DRAFT`） */
  settlements: CommissionSettlementRow[];
  /** 没有生成的原因（前端只提示，不报错） */
  skipped: CommissionSkippedRow[];
}

/** 状态迁移入参（只认 `APPROVED` / `PAID`） */
export interface CommissionStatusReq {
  status: CommissionSettlementStatus;
  /** 备注（发放说明） */
  reason?: string | null;
}

/** 生成结算单（`POST /business/erp/commission/settle`，权限 `business:erp:commission-settle:create`） */
export function settleCommission(payload: SettleCommissionReq): Promise<SettleCommissionRes> {
  return post<SettleCommissionRes>(`${BASE}/commission/settle`, payload);
}

/** 结算单分页（`GET /business/erp/commission/page`，权限 `:list`） */
export function fetchCommissionSettlementPage(
  params: CommissionSettlementFilter,
): Promise<PageData<CommissionSettlementRow>> {
  return get<PageData<CommissionSettlementRow>>(`${BASE}/commission/page`, asParams(params));
}

/** 结算单详情 + 明细（`GET /business/erp/commission/{id}`，权限 `:list`） */
export function fetchCommissionSettlement(id: string): Promise<CommissionSettlementDetail> {
  return get<CommissionSettlementDetail>(`${BASE}/commission/${id}`);
}

/** 审批 / 发放（`PATCH /business/erp/commission/{id}/status`，权限 `:status`；回包是完整详情） */
export function changeCommissionStatus(
  id: string,
  payload: CommissionStatusReq,
): Promise<CommissionSettlementDetail> {
  return patch<CommissionSettlementDetail>(`${BASE}/commission/${id}/status`, payload);
}
