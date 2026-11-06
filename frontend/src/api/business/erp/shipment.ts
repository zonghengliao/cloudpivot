/**
 * 发货单 API 层（SO-09 后端 `backend/src/business/erp/shipment_{handler,model}.rs`，
 * SO-11 前端页面消费）。
 *
 * # 为什么单独一个文件（不并进 `erp/index.ts`）
 *
 * 与 `stock-out.ts` 同一口径：`index.ts` 已经是 4600+ 行的总契约，发货单是独立后端模块
 * （自己的 handler / model / 迁移 V0283~V0285），按模块边界单独成文件，页面只
 * `import '@/api/business/erp/shipment'`，`index.ts` 一行不动。
 *
 * # 字段名逐字抄后端（不是猜的）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 单号 | `ship_no` | `ShipmentRes::ship_no`（`SH-yyyyMMdd-000001`） |
 * | 状态 | `status` + `status_label` | 后端给中文名，前端不再维护第二份译名 |
 * | 关联出库单 | `stock_out_id` / **`stock_out_no`** | SO-10 过账回填；`stock_out_no` 只有详情补 |
 * | 明细数量合计 | `total_qty` | 建单时汇总，过账不改 |
 * | 可发明细 | `ordered` / `delivered` / `reserved` / `remaining` | `ShippableItemRes` |
 *
 * # 三个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端路由宏不含前缀，漏了会命中 SPA fallback 返回 HTML
 *    → 前端报「响应体格式不符合」）；
 * 2. **列表行 `items` 恒为空数组**（明细只在 `GET /shipment/{id}` 返回）；`stock_out_no`
 *    也只有详情补 —— 列表里那一列要么留空，要么点详情看；
 * 2b. **`so_no` 实测列表也不下发**（SO-11 实测：`page_shipment` 批量补了 customer_* /
 *    warehouse_*，唯独漏了 `so_no`，与 `ShipmentRes` 的文档注释不符；已开缺陷卡）。
 *    页面里这一列只能兜底显示「见详情」，详情接口的 `so_no` 是**有值**的；
 * 3. **发货明细的 `location_id` 是库位字典 `biz_location.id`**（不是库位账行 id，
 *    出库单那边才是账行 id）。两者混用 → 后端 400/409，前端类型上没有区别，靠注释挡。
 */
import { get, patch, post, type PageData } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/**
 * 发货单状态域（4 值，逐字抄 `shipment_model::STATUSES` / `V0283` 的 `ck_shipment_status`）。
 *
 * | 编码 | 中文 | 谁写 |
 * |------|------|------|
 * | `DRAFT` | 草稿 | 建单 |
 * | `PENDING_POST` | 待过账 | 人工提交（`DRAFT → PENDING_POST`） |
 * | `POSTED` | 已过账 | 人工过账（同事务生成销售出库单 + 扣三本账） |
 * | `CANCELLED` | 已作废 | 人工作废（`PENDING_POST → CANCELLED` 必须写原因） |
 *
 * 发货单**没有审批流**（`ShipmentRes` 里没有 `approval_instance_id`），所以页面上
 * 不挂 `FlowProgress`：审批链在销售订单（`so_approve`）那一层。
 */
export type ShipmentStatus = 'DRAFT' | 'PENDING_POST' | 'POSTED' | 'CANCELLED';

/** 发货单明细行（逐字抄 `shipment_handler::ShipmentItemRes`） */
export interface ShipmentItem {
  /** 主键 */
  id: string;
  /** 行号（1 起；订单行被物理删时为 0） */
  line_no: number;
  /** 对应的销售订单行 id */
  so_item_id: string;
  /** 物料 id（**服务端从订单行带出**，不信请求体） */
  material_id: string;
  /** 本次发货数量 */
  qty: number | string;
  /** 发货库位（`null` = 整仓 / 过账按 FIFO）；是 `biz_location.id` */
  location_id: string | null;
  /** 序列号（序列号物料才非空） */
  serial_nos: string[];
  /** 行备注 */
  remark: string | null;
}

/**
 * 发货单（逐字抄 `shipment_handler::ShipmentRes`；列表行与详情同构）。
 *
 * - **列表行** `items` 恒为空数组、`stock_out_no` 恒为 `null`（后端不批量补单号）；
 * - `customer_*` / `warehouse_*` 在软删后为 `null`，页面回落「已删除」；
 * - `status_label` 后端给中文，前端不再自己译一遍（兜底表在 `shipment-query.ts`）。
 */
export interface Shipment {
  /** 主键 */
  id: string;
  /** 单号（`SH-yyyyMMdd-000001`） */
  ship_no: string;
  /** 来源销售订单 id */
  so_id: string;
  /** 来源销售订单号（订单被物理删时为 `null`） */
  so_no: string | null;
  /** 客户 id */
  customer_id: string;
  /** 客户编码（客户软删后 `null`） */
  customer_code: string | null;
  /** 客户名称（客户软删后 `null`） */
  customer_name: string | null;
  /** 发货仓库 id */
  warehouse_id: string;
  /** 仓库编码（仓库软删后 `null`） */
  warehouse_code: string | null;
  /** 仓库名称（仓库软删后 `null`） */
  warehouse_name: string | null;
  /** 发货日期（`yyyy-MM-dd`） */
  ship_date: string;
  /** 状态编码 */
  status: ShipmentStatus;
  /** 状态中文名（后端给） */
  status_label: string;
  /** 明细数量合计 */
  total_qty: number | string;
  /** 过账生成的销售出库单 id（未过账 `null`） */
  stock_out_id: string | null;
  /** **过账生成的销售出库单号**（`OUT-yyyyMMdd-000001`；未过账 `null`） */
  stock_out_no: string | null;
  /** 备注 */
  remark: string | null;
  /** 创建时间（RFC3339） */
  created_at: string;
  /** 更新时间（RFC3339） */
  updated_at: string;
  /** 明细（列表恒为空） */
  items: ShipmentItem[];
}

/**
 * 一行「可发明细」（`GET /shipment/shippable/{so_id}` 的出参）。
 *
 * `remaining = ordered - delivered - reserved`（下限 0），与建单时的超发守卫
 * **同一个公式**（`shipment_model::shippable_items`），前端不再自己算一遍。
 */
export interface ShippableItem {
  /** 订单行 id */
  so_item_id: string;
  /** 订单行号（1 起） */
  line_no: number;
  /** 物料 id */
  material_id: string;
  /** 订单数量 */
  ordered: number | string;
  /** 已过账发货量 */
  delivered: number | string;
  /** 其他未过账发货单的占用 */
  reserved: number | string;
  /** 未发量（下限 0） */
  remaining: number | string;
}

/** 一行明细入参（**物料不发**：服务端从订单行带出） */
export interface ShipmentItemReq {
  /** 对应的销售订单行 id（必填） */
  so_item_id: string;
  /** 发货数量（留空 = 该行全部未发量） */
  qty?: string;
  /** 发货库位（留空 = 整仓 / FIFO）；`biz_location.id` */
  location_id?: string;
  /** 序列号（条数必须等于数量） */
  serial_nos?: string[];
  /** 行备注 */
  remark?: string;
}

/** 新建发货单（`shipment_handler::CreateShipmentReq`） */
export interface CreateShipmentReq {
  /** 来源销售订单（必须是 `APPROVED` / `PARTIAL_OUT`，否则 409） */
  so_id: string;
  /** 发货仓库 */
  warehouse_id: string;
  /** 发货日期（`yyyy-MM-dd`，留空 = 今天，北京时间） */
  ship_date?: string;
  /** 备注 */
  remark?: string | null;
  /** 明细（**留空 = 从订单带出全部未发完的行**） */
  items?: ShipmentItemReq[];
}

/** 状态流转（`ShipmentStatusReq`）：`target_status` 必须是 4 条合法边之一 */
export interface ShipmentStatusReq {
  /** 目标状态 */
  target_status: ShipmentStatus;
  /** 原因（`PENDING_POST → CANCELLED` 必填） */
  reason?: string;
}

/**
 * 发货单分页（`GET /business/erp/shipment/page`，权限 `business:erp:shipment:list`）。
 *
 * `ship_no` 模糊，`status` / `so_id` / `customer_id` 精确；空串一律不发
 * （后端把 `?ship_no=` 当「等于空串」过滤，会查出 0 行）。
 */
export function fetchShipmentPage(query: {
  ship_no?: string;
  status?: ShipmentStatus | '';
  so_id?: string;
  customer_id?: string;
  page?: number;
  page_size?: number;
}): Promise<PageData<Shipment>> {
  return get<PageData<Shipment>>(`${BASE}/shipment/page`, asParams(query));
}

/** 发货单详情（`GET /business/erp/shipment/{id}`，`:list` 权限；软删行 404） */
export function fetchShipmentDetail(id: string): Promise<Shipment> {
  return get<Shipment>(`${BASE}/shipment/${id}`);
}

/**
 * 该销售订单**未发完的行**（`GET /shipment/shippable/{so_id}`）。
 *
 * 这是「从销售订单生成发货单」对话框的唯一读数：只带 `remaining > 0` 的行，
 * 数量默认 `remaining`（与后端 `items` 留空时的自动带出同口径）。
 */
export function fetchShippableItems(soId: string): Promise<ShippableItem[]> {
  return get<ShippableItem[]>(`${BASE}/shipment/shippable/${soId}`);
}

/** 新建发货单（`POST /business/erp/shipment`，权限 `:create`） */
export function createShipment(payload: CreateShipmentReq): Promise<Shipment> {
  return post<Shipment>(`${BASE}/shipment`, payload);
}

/**
 * 状态流转（`PATCH /business/erp/shipment/{id}/status`，权限 `:status`）。
 *
 * - `DRAFT → PENDING_POST`：提交待过账；
 * - `PENDING_POST → POSTED`：**过账**（同事务生成销售出库单 + 扣三本账 + 回写订单已发量）；
 *   库存不足 / 并发超发都是 **409**，报文点名物料与数量；
 * - `DRAFT|PENDING_POST → CANCELLED`：作废（后者必须给 `reason`）。
 */
export function changeShipmentStatus(
  id: string,
  payload: ShipmentStatusReq,
): Promise<Shipment> {
  return patch<Shipment>(`${BASE}/shipment/${id}/status`, payload);
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
