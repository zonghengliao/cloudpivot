/**
 * 销售退货单 API 层（后端 `backend/src/business/erp/sales_return_handler.rs`，
 * SO-28 建表 / SO-29 接口 / SO-30 过账 / SO-31 前端页面消费）。
 *
 * # 为什么单独一个文件（不并进 `erp/index.ts`）
 *
 * 与 `shipment.ts` / `stock-out.ts` 同一口径：`index.ts` 已经是数千行的总契约，
 * 退货单是独立后端模块（自己的 handler / model / 迁移 V0303~V0305），按模块边界
 * 单独成文件，页面只 `import '@/api/business/erp/sales-return'`，`index.ts` 一行不动。
 *
 * # 字段名逐字抄后端（不是猜的）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 单号 | `return_no`（`SR-yyyyMMdd-000001`） | `ReturnRes::return_no` |
 * | 退货类型 | `return_type` + `return_type_label` | `sales_return_model::return_type_label` |
 * | 状态 | `status` + `status_label` | 后端给中文名，前端不再维护第二份译名（兜底在 query 层） |
 * | 价税合计 | `total_amount` / `total_tax` / `total_with_tax` | 恒等 `total_with_tax = total_amount + total_tax`（`ck_biz_sreturn_amount`） |
 * | 过账回填 | `stock_in_id` / `receivable_id` / `red_invoice_id` | SO-30 同一事务回填，未过账为 `null` |
 * | 可退明细 | `shipped` / `returned` / `remaining` | `ReturnableItemRes`（服务端现算，前端不重算） |
 *
 * # 四个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端路由宏不含前缀，漏了会命中 SPA fallback 返回 HTML
 *    → 前端报「响应体格式不符合」）；
 * 2. **列表行 `items` 恒为空数组**（明细只在 `GET /sales-return/{id}` 返回）；
 *    `customer_*` / `warehouse_*` / `so_no` / `shipment_no` 在列表里**是补齐的**
 *    （`page_return` 做了批量补齐，与发货单 `so_no` 那个缺口不同）；
 * 3. **`POSTED` 是真过账**（SO-30）：`PATCH /status` 的 POSTED 分支在同一事务里
 *    回库（三本账）+ 红字入库凭证 + 冲减应收 + 已开票则造红字发票；
 *    库存 / 应收相关的 409 要原样展示，不要在页面里编安慰话；
 * 4. **`returnable/{shipment_id}` 只对已过账发货单开放**（未过账 / 已作废 → 409），
 *    所以新建弹窗的发货单下拉必须先按 `status=POSTED` 筛。
 */
import { get, patch, post, type PageData } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/**
 * 退货单状态域（6 值，逐字抄 `sales_return_model::STATUSES` / V0303 的
 * `ck_biz_sreturn_status`）：
 *
 * `DRAFT → SUBMITTED → APPROVED → PENDING_POST → POSTED`，三个作废口
 * （`DRAFT|SUBMITTED|APPROVED → CANCELLED`）。
 */
export type ReturnStatus =
  | 'DRAFT'
  | 'SUBMITTED'
  | 'APPROVED'
  | 'PENDING_POST'
  | 'POSTED'
  | 'CANCELLED';

/** 退货类型域（3 值，逐字抄 `sales_return_model::RETURN_TYPES` / `ck_biz_sreturn_type`） */
export type ReturnType = 'QUALITY' | 'WRONG_ITEM' | 'OTHER';

/** 退货单明细行（逐字抄 `sales_return_handler::ReturnItemRes`） */
export interface ReturnItem {
  /** 主键 */
  id: string;
  /** 订单行号（1 起；无原单 / 订单行已删时为 0） */
  line_no: number;
  /** 对应的原销售订单行 id（无原单为 `null`） */
  so_item_id: string | null;
  /** 物料 id */
  material_id: string;
  /** 退货数量 */
  qty: number | string;
  /** 单价（不含税） */
  price: number | string;
  /** 行不含税金额 */
  amount: number | string;
  /** 税率百分数 */
  tax_rate: number | string;
  /** 行税额 */
  tax_amount: number | string;
  /** 退回库位（`null` = 整仓）；是 `biz_location.id` */
  location_id: string | null;
  /** 序列号（序列号物料才非空） */
  serial_nos: string[];
  /** 行备注 */
  remark: string | null;
}

/**
 * 销售退货单（逐字抄 `sales_return_handler::ReturnRes`；列表行与详情同构）。
 *
 * - **列表行** `items` 恒为空数组；
 * - `customer_*` / `warehouse_*` 在软删后为 `null`，页面回落「已删除」；
 * - `status_label` / `return_type_label` 后端给中文，前端不再自己译一遍。
 */
export interface SalesReturn {
  /** 主键 */
  id: string;
  /** 单号（`SR-yyyyMMdd-000001`） */
  return_no: string;
  /** 客户 id */
  customer_id: string;
  /** 客户编码（客户软删后 `null`） */
  customer_code: string | null;
  /** 客户名称（客户软删后 `null`） */
  customer_name: string | null;
  /** 原销售订单 id（可空） */
  so_id: string | null;
  /** 原销售订单号（列表也补齐；订单不存在 / 未挂订单时 `null`） */
  so_no: string | null;
  /** 原发货单 id（可空） */
  shipment_id: string | null;
  /** 原发货单号（列表也补齐） */
  shipment_no: string | null;
  /** 退货日期（`yyyy-MM-dd`） */
  return_date: string;
  /** 退货类型编码 */
  return_type: ReturnType;
  /** 退货类型中文名（后端给） */
  return_type_label: string;
  /** 退回入库仓库 id */
  warehouse_id: string;
  /** 仓库编码（仓库软删后 `null`） */
  warehouse_code: string | null;
  /** 仓库名称（仓库软删后 `null`） */
  warehouse_name: string | null;
  /** 状态编码 */
  status: ReturnStatus;
  /** 状态中文名（后端给） */
  status_label: string;
  /** 不含税金额合计 */
  total_amount: number | string;
  /** 税额合计 */
  total_tax: number | string;
  /** 价税合计 */
  total_with_tax: number | string;
  /** 过账生成的入库单 id（SO-30 回填；未过账 `null`） */
  stock_in_id: string | null;
  /** 冲减的应收单 id（SO-30 回填） */
  receivable_id: string | null;
  /** 红字发票 id（SO-30 回填） */
  red_invoice_id: string | null;
  /**
   * 审批实例 id（弱引用流程引擎；未提审为 `null`）。
   *
   * ⚠️ 现状（SO-31）：退货单**尚未接入流程引擎**（后端 `change_return_status` 不建实例，
   * 审批在状态机的 `SUBMITTED → APPROVED` 那条边上人工点）。所以本字段目前恒为 `null`，
   * 页面上的 `FlowProgress` 区块按共享口径降级为不渲染 —— 接上引擎后无需改前端。
   */
  approval_instance_id: string | null;
  /** 备注 */
  remark: string | null;
  /** 创建时间（RFC3339） */
  created_at: string;
  /** 更新时间（RFC3339） */
  updated_at: string;
  /** 明细（列表恒为空） */
  items: ReturnItem[];
}

/**
 * 一行「可退明细」（`GET /sales-return/returnable/{shipment_id}` 的出参）。
 *
 * `remaining = shipped - returned`（下限 0），**服务端现算**（含同一发货单上
 * 其他未作废退货单的占用），前端只在建单前做上限校验，不重算。
 */
export interface ReturnableItem {
  /** 订单行 id */
  so_item_id: string;
  /** 订单行号（1 起） */
  line_no: number;
  /** 物料 id */
  material_id: string;
  /** 已发货量 */
  shipped: number | string;
  /** 已退量 */
  returned: number | string;
  /** 可退量（下限 0） */
  remaining: number | string;
  /** 单价（不含税，取自订单行） */
  price: number | string;
  /** 税率（百分数，取自订单行） */
  tax_rate: number | string;
}

/** 一行明细入参（`sales_return_handler::ReturnItemReq`） */
export interface ReturnItemReq {
  /** 对应的原销售订单行 id（从发货单带出时必填） */
  so_item_id?: string;
  /** 物料（有 `so_item_id` 时**被服务端忽略**） */
  material_id?: string;
  /** 退货数量（留空 = 该行全部可退量） */
  qty?: string;
  /** 单价（不含税；留空 = 订单行单价） */
  price?: string;
  /** 税率百分数（留空 = 订单行税率） */
  tax_rate?: string;
  /** 退回库位（留空 = 整仓）；`biz_location.id` */
  location_id?: string;
  /** 序列号（条数必须等于数量） */
  serial_nos?: string[];
  /** 行备注 */
  remark?: string;
}

/** 新建销售退货单（`sales_return_handler::CreateReturnReq`） */
export interface CreateReturnReq {
  /** 原发货单（**推荐**：传了就按这张单的可退量校验） */
  shipment_id?: string;
  /** 原销售订单（可空；与 `shipment_id` 同时传时必须一致） */
  so_id?: string;
  /** 客户（可空：传了发货单 / 订单时由服务端带出并校验一致） */
  customer_id?: string;
  /** 退回入库仓库（必填） */
  warehouse_id: string;
  /** 退货日期（`yyyy-MM-dd`，留空 = 今天，北京时间） */
  return_date?: string;
  /** 退货类型：`QUALITY` / `WRONG_ITEM` / `OTHER`（必填） */
  return_type: ReturnType;
  /** 备注 */
  remark?: string;
  /** 明细（**留空 = 从原发货单带出全部可退明细**） */
  items?: ReturnItemReq[];
}

/** 状态流转（`ReturnStatusReq`）：`target_status` 必须是 7 条合法边之一 */
export interface ReturnStatusReq {
  /** 目标状态 */
  target_status: ReturnStatus;
  /** 原因（`SUBMITTED|APPROVED → CANCELLED` 必填，后端 400） */
  reason?: string;
}

/**
 * 退货单分页（`GET /business/erp/sales-return/page`，权限 `business:erp:sales-return:list`）。
 *
 * `return_no` 模糊，`status` / `so_id` / `customer_id` / `shipment_id` 精确；
 * 空串一律不发（后端把 `?return_no=` 当「等于空串」过滤，会查出 0 行）。
 */
export function fetchReturnPage(query: {
  return_no?: string;
  status?: ReturnStatus | '';
  so_id?: string;
  customer_id?: string;
  shipment_id?: string;
  page?: number;
  page_size?: number;
}): Promise<PageData<SalesReturn>> {
  return get<PageData<SalesReturn>>(`${BASE}/sales-return/page`, asParams(query));
}

/** 退货单详情（`GET /business/erp/sales-return/{id}`，`:list` 权限；含明细） */
export function fetchReturnDetail(id: string): Promise<SalesReturn> {
  return get<SalesReturn>(`${BASE}/sales-return/${id}`);
}

/**
 * 该发货单**可退明细**（`GET /sales-return/returnable/{shipment_id}`）。
 *
 * 这是「从原发货单生成退货单」对话框的唯一读数：只带 `remaining > 0` 的行，
 * 数量默认 `remaining`（与后端 `items` 留空时的自动带出同口径）。
 * 未过账 / 已作废的发货单 → **409**（货还没出门，没有可退的）。
 */
export function fetchReturnableItems(shipmentId: string): Promise<ReturnableItem[]> {
  return get<ReturnableItem[]>(`${BASE}/sales-return/returnable/${shipmentId}`);
}

/** 新建销售退货单（`POST /business/erp/sales-return`，权限 `:create`，落 `DRAFT`） */
export function createReturn(payload: CreateReturnReq): Promise<SalesReturn> {
  return post<SalesReturn>(`${BASE}/sales-return`, payload);
}

/**
 * 状态流转（`PATCH /business/erp/sales-return/{id}/status`，权限 `:status`）。
 *
 * - `DRAFT → SUBMITTED`：提交（同事务重跑超退闸）；`SUBMITTED → APPROVED`：批准；
 * - `APPROVED → PENDING_POST`：进入待过账；
 * - `PENDING_POST → POSTED`：**过账**（SO-30 同一事务：回库三本账 + 红字入库凭证 +
 *   冲减应收 + 已开票则造红字发票 + 回写订单已发量 + 释放信用占用）；库存 / 应收
 *   相关是 **409**，报文点名物料与金额；
 * - `DRAFT|SUBMITTED|APPROVED → CANCELLED`：作废（后两条必须给 `reason`）。
 */
export function changeReturnStatus(
  id: string,
  payload: ReturnStatusReq,
): Promise<SalesReturn> {
  return patch<SalesReturn>(`${BASE}/sales-return/${id}/status`, payload);
}

/** 丢掉空串 / null / undefined（后端把 `?x=` 当「等于空串」过滤，必须根本不发） */
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
