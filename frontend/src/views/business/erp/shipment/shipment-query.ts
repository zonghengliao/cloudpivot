/**
 * 发货单页纯逻辑（SO-11）—— 所有可测的判断都在这里，`.vue` 只负责渲染。
 *
 * # 字段口径
 *
 * 出参 / 入参逐字抄 `backend/src/business/erp/shipment_handler.rs`
 * （`ShipmentRes` / `ShipmentItemRes` / `ShippableItemRes` / `CreateShipmentReq` /
 * `ShipmentItemReq`），前端类型在 `@/api/business/erp/shipment`。
 *
 * **别照出库单抄**：出库单是 `out_no` / `out_type`，发货单是 `ship_no` / 无类型；
 * 出库单明细的 `location_id` 是**库位账行 id**，发货单是**库位字典 id**。
 *
 * # 状态机（唯一真相：`state_machine::SHIPMENT_EDGES`）
 *
 * 4 条边、4 个状态，**没有审批流**（`ShipmentRes` 没有 `approval_instance_id`）：
 *
 * ```text
 * DRAFT ──提交──> PENDING_POST ──过账──> POSTED
 *   │                 │
 *   └──作废──> CANCELLED <──作废（必须写原因）──┘
 * ```
 *
 * - `POSTED` 是终态：已过账的作废要红冲出库单 + 回滚三本账（另一张卡），不是一条边；
 * - `PENDING_POST → DRAFT` 不存在：要改就先作废重建；
 * - 只有 `PENDING_POST → CANCELLED` 需要 `reason`（`SHIPMENT_REASON_EDGES` 只有这一条）。
 *
 * # 数量
 *
 * 「未发量」一律用服务端 `ShippableItemRes.remaining`（= 订单 − 已发 − 其他未过账占用），
 * 前端只在提交前用它做**上限校验**（超发后端 400，别等那一下）。
 */
import type { PrimaryTableCol } from 'tdesign-vue-next';
import type { Shipment, ShipmentStatus, ShippableItem } from '@/api/business/erp/shipment';

/** 每页条数（与销售订单 / 出库单列表同口径；后端上限 200、`0` → 20） */
export const SHIPMENT_PAGE_SIZE = 20;

/** 备注上限（`V0283` 的 `remark` 是 `VARCHAR(500)`） */
export const SHIPMENT_REMARK_MAX = 500;

/** 明细行数上限（表上没有硬限制，纯防手滑） */
export const SHIPMENT_MAX_LINES = 200;

/** 状态中文名（与后端 `shipment_model::status_label` 逐字一致；后端也给，这是兜底） */
export const SHIPMENT_STATUS_LABELS: Record<ShipmentStatus, string> = {
  DRAFT: '草稿',
  PENDING_POST: '待过账',
  POSTED: '已过账',
  // ⚠️ 后端 `shipment_model::status_label` 对 CANCELLED 的中文是「已取消」（不是「已作废」）。
  // 动作叫「作废」、状态叫「已取消」，两套词各有出处：徽标一律显示后端给的
  // `status_label`，这张表只是兜底 —— 兜底也必须逐字对齐后端，否则同一张单
  // 在两个页面上叫两个名字。
  CANCELLED: '已取消',
};

/** 状态标签配色：草稿灰、待过账蓝、已过账绿、已取消（作废）红 */
export const SHIPMENT_STATUS_THEMES: Record<
  ShipmentStatus,
  'default' | 'primary' | 'success' | 'warning' | 'danger'
> = {
  DRAFT: 'default',
  PENDING_POST: 'primary',
  POSTED: 'success',
  CANCELLED: 'danger',
};

/** 状态域（后端顺序，用于兜底判断） */
export const SHIPMENT_STATUSES: readonly ShipmentStatus[] = [
  'DRAFT',
  'PENDING_POST',
  'POSTED',
  'CANCELLED',
];

/** 状态下拉候选（顺序 = 状态机走向；`''` = 全部） */
export const SHIPMENT_STATUS_OPTIONS: ReadonlyArray<{
  label: string;
  value: '' | ShipmentStatus;
}> = [
  { label: '全部状态', value: '' },
  { label: '草稿', value: 'DRAFT' },
  { label: '待过账', value: 'PENDING_POST' },
  { label: '已过账', value: 'POSTED' },
  { label: '已取消', value: 'CANCELLED' },
];

/**
 * 合法边表（逐条抄 `state_machine::SHIPMENT_EDGES`，顺序也一致）。
 *
 * 前端的按钮显隐 / 目标态下拉**只读这一份**，不写第二套状态机。
 */
export const SHIPMENT_EDGES: ReadonlyArray<readonly [ShipmentStatus, ShipmentStatus]> = [
  ['DRAFT', 'PENDING_POST'],
  ['PENDING_POST', 'POSTED'],
  ['DRAFT', 'CANCELLED'],
  ['PENDING_POST', 'CANCELLED'],
];

/** 需要原因的状态边（逐条抄 `SHIPMENT_REASON_EDGES`：只有 `PENDING_POST → CANCELLED`） */
export const SHIPMENT_REASON_EDGES: ReadonlyArray<readonly [ShipmentStatus, ShipmentStatus]> = [
  ['PENDING_POST', 'CANCELLED'],
];

// ==================== 状态判定 ====================

/** 状态中文名：认不出编码时回落编码本身（别显示空白） */
export function shipmentStatusLabel(status: string | null | undefined): string {
  if (!status) {
    return '—';
  }
  return SHIPMENT_STATUS_LABELS[status as ShipmentStatus] ?? status;
}

/** 状态标签配色：未知状态回落 `default`（不猜颜色） */
export function shipmentStatusTheme(
  status: string | null | undefined,
): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  if (!status) {
    return 'default';
  }
  return SHIPMENT_STATUS_THEMES[status as ShipmentStatus] ?? 'default';
}

/** 这条迁移在不在边表里（后端 `model::can_transition` 同口径） */
export function canTransition(
  from: string | null | undefined,
  to: string | null | undefined,
): boolean {
  if (!from || !to) {
    return false;
  }
  return SHIPMENT_EDGES.some(([edgeFrom, edgeTo]) => edgeFrom === from && edgeTo === to);
}

/** 从当前态出发的合法目标态（后端 `allowed_targets` 同口径，顺序按边表） */
export function allowedTargets(status: string | null | undefined): ShipmentStatus[] {
  if (!status) {
    return [];
  }
  return SHIPMENT_EDGES.filter(([from]) => from === status).map(([, to]) => to);
}

/** 提交待过账按钮：只有 `DRAFT`（`DRAFT → PENDING_POST`） */
export function canSubmit(status: string | null | undefined): boolean {
  return status === 'DRAFT';
}

/**
 * **过账**按钮：只有 `PENDING_POST`（`PENDING_POST → POSTED`）。
 *
 * `DRAFT` 不能直接过账 —— 后端边表里没有 `DRAFT → POSTED`，前端先「提交待过账」。
 */
export function canPost(status: string | null | undefined): boolean {
  return status === 'PENDING_POST';
}

/** 作废按钮：`{DRAFT, PENDING_POST} → CANCELLED` */
export function canCancel(status: string | null | undefined): boolean {
  return status === 'DRAFT' || status === 'PENDING_POST';
}

/** 终态（已过账 / 已作废）：出了就不再动 */
export function isTerminal(status: string | null | undefined): boolean {
  return status === 'POSTED' || status === 'CANCELLED';
}

/** 这条边要不要原因（与 `SHIPMENT_REASON_EDGES` 逐条一致） */
export function statusNeedsReason(
  from: string | null | undefined,
  to: ShipmentStatus | null | undefined,
): boolean {
  if (!from || !to) {
    return false;
  }
  return SHIPMENT_REASON_EDGES.some(([edgeFrom, edgeTo]) => edgeFrom === from && edgeTo === to);
}

/** 列表行 / 详情操作条的显隐（一次算清，模板里不再写条件） */
export function rowActions(row: {
  status: string | null | undefined;
}): {
  submit: boolean;
  post: boolean;
  cancel: boolean;
} {
  const status = row.status;
  return {
    submit: canSubmit(status),
    post: canPost(status),
    cancel: canCancel(status),
  };
}

// ==================== 列表查询 ====================

/** 列表筛选（`ShipmentFilter`：`ship_no` 模糊，`status` / `customer_id` 精确） */
export interface ShipmentFilter {
  /** 单号（模糊） */
  ship_no: string;
  /** 状态（`''` = 全部） */
  status: '' | ShipmentStatus;
  /** 客户 id（`''` = 全部） */
  customer_id: string;
}

/** 空筛选（每次重置都造一份新的，别共享引用） */
export function emptyShipmentFilter(): ShipmentFilter {
  return { ship_no: '', status: '', customer_id: '' };
}

/** 是否处于筛选态（重置按钮 / 「已筛选」提示用） */
export function isShipmentFiltered(filter: ShipmentFilter): boolean {
  return Boolean(filter.ship_no.trim()) || Boolean(filter.status) || Boolean(filter.customer_id);
}

/** 筛选 + 分页 → 请求参数（空串根本不发，见 `shipment.ts::asParams`） */
export function buildShipmentQuery(
  filter: ShipmentFilter,
  page: number,
  pageSize: number = SHIPMENT_PAGE_SIZE,
): {
  ship_no?: string;
  status?: ShipmentStatus;
  customer_id?: string;
  page: number;
  page_size: number;
} {
  const query: {
    ship_no?: string;
    status?: ShipmentStatus;
    customer_id?: string;
    page: number;
    page_size: number;
  } = { page, page_size: pageSize };
  const shipNo = filter.ship_no.trim();
  if (shipNo) {
    query.ship_no = shipNo;
  }
  if (filter.status) {
    query.status = filter.status;
  }
  if (filter.customer_id) {
    query.customer_id = filter.customer_id;
  }
  return query;
}

/**
 * 列表列（单号 / 订单 / 客户 / 仓库 / 日期 / 数量 / 状态 / 出库单 / 操作）。
 *
 * 宽度按「1600 视口、侧栏展开时 `t-table__content` ≈1288px」预算：合计 1230，
 * 留 58px 余量，固定「操作」列不会盖住「出库单号」列。
 */
export const SHIPMENT_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'ship_no', title: '发货单号', width: 175, fixed: 'left' },
  { colKey: 'so_no', title: '销售订单', width: 165 },
  { colKey: 'customer_name', title: '客户', width: 160, ellipsis: true },
  { colKey: 'warehouse_name', title: '发货仓库', width: 110, ellipsis: true },
  { colKey: 'ship_date', title: '发货日期', width: 100 },
  { colKey: 'total_qty', title: '数量', width: 85, align: 'right' },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'stock_out_no', title: '关联出库单', width: 165 },
  { colKey: 'op', title: '操作', width: 180, fixed: 'right' },
];

/** 详情明细列（行号 / 物料 / 数量 / 库位 / 序列号 / 备注） */
export const SHIPMENT_ITEM_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '行号', width: 66 },
  { colKey: 'material_id', title: '物料', minWidth: 200, ellipsis: true },
  { colKey: 'qty', title: '发货数量', width: 110, align: 'right' },
  { colKey: 'location_id', title: '发货库位', width: 130 },
  { colKey: 'serial_nos', title: '序列号', minWidth: 150, ellipsis: true },
  { colKey: 'remark', title: '备注', minWidth: 120, ellipsis: true },
];

// ==================== 从订单带出明细（表单） ====================

/** 一行「从订单带出」的明细草稿 */
export interface ShipmentLineDraft {
  /** 前端行 id（不是后端主键；只用于 `key`） */
  key: string;
  /** 销售订单行 id（提交时必填） */
  so_item_id: string;
  /** 物料 id（只用于显示，提交不发） */
  material_id: string;
  /** 订单行号 */
  line_no: number;
  /** 订单数量 */
  ordered: string;
  /** 已发数量 */
  delivered: string;
  /** 其他未过账占用 */
  reserved: string;
  /** 未发量（= 本次可发上限） */
  remaining: string;
  /** 本次发货数量（`number | string` 两态都收） */
  qty: number | string;
  /** 发货库位（`''` = 整仓 / FIFO） */
  location_id: string;
  /** 行备注 */
  remark: string;
}

let lineSeq = 0;

/** 数字/字符串 → 4 位定标的字符串（后端 `numeric(18,4)` 口径；非法 → `'0.0000'` 的简写 `'0'`） */
export function qtyText(value: number | string | null | undefined): string {
  const num = toFinite(value);
  const rounded = Math.round(num * 10000) / 10000;
  return String(rounded);
}

function toFinite(value: number | string | null | undefined): number {
  if (value === null || value === undefined || value === '') {
    return 0;
  }
  const parsed = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

/**
 * 可发明细 → 表单行（**只带 `remaining > 0` 的行**，数量默认 `remaining`）。
 *
 * 与后端「`items` 留空 = 从订单带出全部未发完的行」同一个取数口径
 * （`shipment_handler::resolve_items` 的 ① 分支），所以「自动带出」与
 * 「前端显式带出后提交」在建单结果上等价。
 */
export function linesFromShippable(items: ReadonlyArray<ShippableItem>): ShipmentLineDraft[] {
  return items
    .filter((item) => toFinite(item.remaining) > 0)
    .slice()
    .sort((a, b) => a.line_no - b.line_no)
    .map((item) => {
      lineSeq += 1;
      const remaining = qtyText(item.remaining);
      return {
        key: `line-${Date.now()}-${lineSeq}`,
        so_item_id: item.so_item_id,
        material_id: item.material_id,
        line_no: item.line_no,
        ordered: qtyText(item.ordered),
        delivered: qtyText(item.delivered),
        reserved: qtyText(item.reserved),
        remaining,
        qty: remaining,
        location_id: '',
        remark: '',
      };
    });
}

/** 表单合计（预览；服务端建单后会重算 `total_qty`） */
export function totalQtyOf(lines: ReadonlyArray<{ qty: number | string }>): number {
  const sum = lines.reduce((acc, line) => acc + toFinite(line.qty), 0);
  return Math.round(sum * 10000) / 10000;
}

/** 新建发货单的表单态 */
export interface ShipmentFormDraft {
  /** 来源销售订单 id（`''` = 还没选） */
  so_id: string;
  /** 发货仓库 id（`''` = 还没选） */
  warehouse_id: string;
  /** 发货日期（`yyyy-MM-dd`） */
  ship_date: string;
  /** 备注 */
  remark: string;
  /** 明细（选订单后从 `shippable` 带出） */
  items: ShipmentLineDraft[];
}

/** 空表单（`today` 由调用方给，便于测试与「北京时间今天」口径） */
export function emptyShipmentForm(today: string): ShipmentFormDraft {
  return { so_id: '', warehouse_id: '', ship_date: today, remark: '', items: [] };
}

/**
 * 表单校验（提交前的前端闸；服务端还会校一遍，前端只是省一次 400）。
 *
 * 规则逐条对齐后端 `create_shipment` / `resolve_items`：
 * 订单必选、仓库必选、日期 `yyyy-MM-dd`、至少 1 行、每行数量 > 0 且 ≤ 未发量。
 */
export function validateShipmentForm(form: ShipmentFormDraft): string | null {
  if (!form.so_id) {
    return '请选择销售订单（只有已审批 / 部分出库的订单能发货）';
  }
  if (!form.warehouse_id) {
    return '请选择发货仓库';
  }
  if (!isDateString(form.ship_date)) {
    return '发货日期格式应为 yyyy-MM-dd';
  }
  if (form.remark.length > SHIPMENT_REMARK_MAX) {
    return `备注不能超过 ${SHIPMENT_REMARK_MAX} 字`;
  }
  if (form.items.length === 0) {
    return '这张订单没有可发货的明细（未发量都是 0，或已被其他未过账发货单占满）';
  }
  if (form.items.length > SHIPMENT_MAX_LINES) {
    return `明细最多 ${SHIPMENT_MAX_LINES} 行`;
  }
  for (const [index, line] of form.items.entries()) {
    const at = `第 ${index + 1} 行（订单行 ${line.line_no}）`;
    const qty = toFinite(line.qty);
    if (!Number.isFinite(Number(line.qty)) || qty <= 0) {
      return `${at}：发货数量必须大于 0`;
    }
    const remaining = toFinite(line.remaining);
    if (qty > remaining) {
      return `${at}：发货数量 ${qty} 超过未发量 ${remaining}（超发后端会 400）`;
    }
  }
  if (totalQtyOf(form.items) <= 0) {
    return '发货数量合计必须大于 0';
  }
  return null;
}

/** 表单 → 建单入参（物料不发；`qty` / `location_id` 空值一律不带） */
export function buildCreatePayload(form: ShipmentFormDraft): {
  so_id: string;
  warehouse_id: string;
  ship_date: string;
  remark?: string;
  items: Array<{ so_item_id: string; qty?: string; location_id?: string; remark?: string }>;
} {
  const payload: {
    so_id: string;
    warehouse_id: string;
    ship_date: string;
    remark?: string;
    items: Array<{ so_item_id: string; qty?: string; location_id?: string; remark?: string }>;
  } = {
    so_id: form.so_id,
    warehouse_id: form.warehouse_id,
    ship_date: form.ship_date,
    items: [],
  };
  const remark = form.remark.trim();
  if (remark) {
    payload.remark = remark;
  }
  for (const line of form.items) {
    const item: { so_item_id: string; qty?: string; location_id?: string; remark?: string } = {
      so_item_id: line.so_item_id,
    };
    // 留空 = 「该行全部未发量」由服务端决定：这里只在用户真的填了数量时发
    if (line.qty !== '' && line.qty !== null && line.qty !== undefined) {
      item.qty = qtyText(line.qty);
    }
    if (line.location_id) {
      item.location_id = line.location_id;
    }
    const lineRemark = line.remark.trim();
    if (lineRemark) {
      item.remark = lineRemark;
    }
    payload.items.push(item);
  }
  return payload;
}

/** 表单里是否有「改过数量」的行（提交按钮提示用：全默认 = 整单全发） */
export function isFullShipment(form: ShipmentFormDraft): boolean {
  return form.items.every((line) => toFinite(line.qty) === toFinite(line.remaining));
}

/** `yyyy-MM-dd` 且是真日期（`2026-13-01` / `2026-02-30` 都要拒） */
export function isDateString(value: string | null | undefined): boolean {
  if (!value || !/^\d{4}-\d{2}-\d{2}$/.test(value)) {
    return false;
  }
  const [year, month, day] = value.split('-').map(Number) as [number, number, number];
  if (month < 1 || month > 12 || day < 1) {
    return false;
  }
  const daysInMonth = new Date(Date.UTC(year, month, 0)).getUTCDate();
  return day <= daysInMonth;
}

/** 列表空态文案（区分「没数据」与「筛出来没有」） */
export function emptyListText(filter: ShipmentFilter): string {
  return isShipmentFiltered(filter)
    ? '当前筛选条件下没有发货单，试试放宽条件或重置'
    : '还没有发货单，点右上角「新建发货单」从销售订单带出未发完的明细';
}

// ==================== 详情 / 过账文案 ====================

/**
 * 过账确认弹窗的正文（**只陈述后端真实效果**，见 `shipment_posting::post_effects`）。
 *
 * 三个效果一次说清：生成销售出库单 / 扣三本账 + 成本结转凭证 / 回写订单已发量与状态。
 */
export function postDialogText(shipment: Pick<Shipment, 'ship_no' | 'total_qty'> | null): string {
  const no = shipment?.ship_no ?? '本单';
  const qty = shipment ? qtyText(shipment.total_qty) : '';
  return (
    `过账后会给 ${no}（合计 ${qty}）生成一张销售出库单：` +
    '按仓库可用量扣减三本账（成本账 / 库位账 / 流水）、按移动加权单价生成成本结转凭证，' +
    '并回写来源销售订单的已发数量与状态（部分出库 / 已出库）。' +
    '过账不可撤销（要退回只能走红冲），确认继续？'
  );
}

/** 过账后详情里「关联出库单」的显示值 */
export function stockOutText(shipment: {
  status: string | null | undefined;
  stock_out_no: string | null;
}): string {
  if (shipment.stock_out_no) {
    return shipment.stock_out_no;
  }
  return shipment.status === 'POSTED' ? '出库单已删除' : '未过账';
}
