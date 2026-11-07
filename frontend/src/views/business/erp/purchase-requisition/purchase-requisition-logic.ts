/**
 * 采购申请页面纯逻辑（P2-02）。
 *
 * # 为什么单独一个 ts 文件
 *
 * 页面（`index.vue`）只负责画画，**取数 / 组装 / 校验 / 合并口径提示**都放这里：
 * ① `vue-tsc` 对 ts 文件的检查比 .vue 更严，口径写错更容易当场红；
 * ② 这些函数没有 Vue 依赖，`tests/unit` 里能直接跑；
 * ③ 后端 `purchase_requisition_dto.rs` 的三条合并规则（同物料相加 / 税率必须一致 /
 *    供应商必须唯一）在前端**同口径预校验**，用户不用等 409 才知道哪两张单不能合。
 *
 * # 金额与数量一律字符串提交
 *
 * 后端是 `Decimal`（JSON 字符串）。`t-input-number` 给的是 number，
 * 所以数量/单价入参在 `build*Payload` 里统一 `String()` 转换，
 * **不做本地加减乘除**（金额只由服务端算）。
 */
import type {
  CreateRequisitionPayload,
  DecimalText,
  RequisitionDetail,
  RequisitionFilter,
  RequisitionStatus,
  ToPoPayload,
} from '@/api/business/erp/purchase-requisition';

/** 列表默认每页条数 */
export const PAGE_SIZE = 20;
/** 单次申请明细上限（与后端 `MAX_LINES` 一致） */
export const MAX_LINES = 200;
/** 备注上限（与后端 `MAX_REMARK_LEN` 一致） */
export const MAX_REMARK = 500;
/** 一次合并的申请单张数上限（与后端 `MAX_MERGE` 一致） */
export const MAX_MERGE = 50;
/** 默认税率（%） */
export const DEFAULT_TAX_PERCENT = 13;

/** `t-input-number` 的值 → number（`null` = 没填） */
export function toNumberOrNull(value: number | string | null | undefined): number | null {
  if (value === null || value === undefined) {
    return null;
  }
  if (typeof value === 'string' && value.trim() === '') {
    return null;
  }
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

/** 金额展示（`DecimalText` → 千分位以外原样；空值给 `—`） */
export function decimalText(value: DecimalText | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '—';
  }
  return String(value);
}

// ==================== 筛选 ====================

/** 列表筛选态 */
export interface FilterState {
  /** 关键字：申请单号 / 部门 / 申请人 / 备注 / PO 号 */
  keyword: string;
  /** 状态 */
  status: RequisitionStatus | '';
  /** 申请日期起 */
  date_from: string;
  /** 申请日期止 */
  date_to: string;
  /** 只看可转单 */
  convertible_only: boolean;
}

/** 空筛选 */
export function emptyFilter(): FilterState {
  return { keyword: '', status: '', date_from: '', date_to: '', convertible_only: false };
}

/** 是否处于筛选态（决定空列表提示文案） */
export function isFiltered(filter: FilterState): boolean {
  return Boolean(
    filter.keyword.trim() ||
      filter.status ||
      filter.date_from ||
      filter.date_to ||
      filter.convertible_only,
  );
}

/** 日期区间校验（起 > 止 → 文案；否则 null） */
export function dateRangeError(from: string, to: string): string | null {
  if (from && to && from > to) {
    return `申请日期起（${from}）不能晚于止（${to}）`;
  }
  return null;
}

/** 筛选态 → 分页入参 */
export function buildQuery(filter: FilterState, page: number, pageSize = PAGE_SIZE): RequisitionFilter {
  return {
    page,
    page_size: pageSize,
    keyword: filter.keyword.trim() || undefined,
    status: filter.status || undefined,
    date_from: filter.date_from || undefined,
    date_to: filter.date_to || undefined,
    convertible_only: filter.convertible_only ? true : undefined,
  };
}

// ==================== 新建 ====================

/** 一行明细草稿（数值字段用 `number | string`，与 `t-input-number` 的 modelValue 对齐） */
export interface CreateLine {
  /** 行内唯一键（表格 key，不用下标：删中间行时下标会串） */
  key: number;
  /** 物料 id */
  material_id: string;
  /** 数量 */
  qty: number | string;
  /** 预估不含税单价 */
  est_price: number | string;
  /** 税率（%） */
  tax_rate: number | string;
  /** 需求日期 */
  demand_date: string;
  /** 建议供应商 */
  suggest_supplier_id: string;
  /** 行备注 */
  remark: string;
}

/** 新建表单草稿 */
export interface CreateForm {
  /** 申请部门 */
  dept_id: string;
  /** 收货仓库 */
  warehouse_id: string;
  /** 申请日期 */
  req_date: string;
  /** 需求日期 */
  demand_date: string;
  /** 备注 */
  remark: string;
  /** 明细 */
  lines: CreateLine[];
}

let lineSeed = 1;

/** 新的一行明细（税率缺省 13%） */
export function newCreateLine(): CreateLine {
  lineSeed += 1;
  return {
    key: lineSeed,
    material_id: '',
    qty: 1,
    est_price: '',
    tax_rate: DEFAULT_TAX_PERCENT,
    demand_date: '',
    suggest_supplier_id: '',
    remark: '',
  };
}

/** 空新建表单（默认 1 行） */
export function emptyCreateForm(deptId = '', warehouseId = ''): CreateForm {
  return {
    dept_id: deptId,
    warehouse_id: warehouseId,
    req_date: '',
    demand_date: '',
    remark: '',
    lines: [newCreateLine()],
  };
}

/** 新建表单校验（返回第一条错误，`null` = 通过） */
export function validateCreateForm(form: CreateForm): string | null {
  if (!form.dept_id) {
    return '请选择申请部门';
  }
  if (!form.warehouse_id) {
    return '请选择收货仓库';
  }
  const lines = form.lines.filter(
    (line) => line.material_id || toNumberOrNull(line.qty) !== null || toNumberOrNull(line.est_price) !== null,
  );
  if (lines.length === 0) {
    return '至少填 1 行明细';
  }
  if (lines.length > MAX_LINES) {
    return `明细最多 ${MAX_LINES} 行（当前 ${lines.length} 行）`;
  }
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    const label = `第 ${index + 1} 行`;
    if (!line.material_id) {
      return `${label}：请选择物料`;
    }
    const qty = toNumberOrNull(line.qty);
    if (qty === null || qty <= 0) {
      return `${label}：数量必须大于 0`;
    }
    const price = toNumberOrNull(line.est_price);
    if (price === null || price <= 0) {
      return `${label}：预估单价必须大于 0（0 元等于不占预算）`;
    }
    const tax = toNumberOrNull(line.tax_rate);
    if (tax !== null && (tax < 0 || tax > 100)) {
      return `${label}：税率必须在 0–100 之间`;
    }
  }
  if (form.remark.length > MAX_REMARK) {
    return `备注最多 ${MAX_REMARK} 字`;
  }
  return null;
}

/** 新建表单 → 建单入参（数量/单价一律字符串） */
export function buildCreatePayload(form: CreateForm): CreateRequisitionPayload {
  const items = form.lines
    .filter((line) => line.material_id)
    .map((line) => ({
      material_id: line.material_id,
      qty: String(toNumberOrNull(line.qty) ?? 0),
      est_price: String(toNumberOrNull(line.est_price) ?? 0),
      tax_rate: String(toNumberOrNull(line.tax_rate) ?? DEFAULT_TAX_PERCENT),
      demand_date: line.demand_date || form.demand_date || null,
      suggest_supplier_id: line.suggest_supplier_id || null,
      remark: line.remark.trim() || null,
    }));
  return {
    dept_id: form.dept_id,
    warehouse_id: form.warehouse_id,
    req_date: form.req_date || null,
    demand_date: form.demand_date || null,
    remark: form.remark.trim() || null,
    items,
  };
}

// ==================== 合并转单 ====================

/** 转单表单草稿 */
export interface ToPoForm {
  /** 参与合并的申请单 id（含主单） */
  requisition_ids: string[];
  /** 供应商 */
  supplier_id: string;
  /** 收货仓库覆盖 */
  warehouse_id: string;
  /** 下单日期 */
  order_date: string;
  /** 备注 */
  remark: string;
}

/** 空转单表单 */
export function emptyToPoForm(requisitionIds: string[] = []): ToPoForm {
  return {
    requisition_ids: [...requisitionIds],
    supplier_id: '',
    warehouse_id: '',
    order_date: '',
    remark: '',
  };
}

/**
 * 合并预校验（与后端 `merge_by_material` / `resolve_merge_supplier` 同口径）：
 * ① 同物料税率必须一致；② 供应商必须唯一（要么全部建议同一家，要么显式指定）。
 */
export function validateMerge(details: RequisitionDetail[], supplierId: string): string | null {
  if (details.length === 0) {
    return '至少选 1 张申请单';
  }
  if (details.length > MAX_MERGE) {
    return `一次最多合并 ${MAX_MERGE} 张申请单（当前 ${details.length} 张）`;
  }
  const taxByMaterial = new Map<string, Set<string>>();
  const supplierIds = new Set<string>();
  details.forEach((detail) => {
    detail.items.forEach((item) => {
      const taxes = taxByMaterial.get(item.material_id) ?? new Set<string>();
      taxes.add(String(item.tax_rate));
      taxByMaterial.set(item.material_id, taxes);
      if (item.suggest_supplier_id) {
        supplierIds.add(item.suggest_supplier_id);
      }
    });
  });
  for (const [materialId, taxes] of taxByMaterial) {
    if (taxes.size > 1) {
      return `物料 ${materialId} 在所选申请单上的税率不一致（${[...taxes].join(' / ')}%）：请先改成一致再合并`;
    }
  }
  if (!supplierId && supplierIds.size === 0) {
    return '所选申请单都没有填建议供应商：请指定供应商';
  }
  if (!supplierId && supplierIds.size > 1) {
    return `所选申请单指定了 ${supplierIds.size} 家不同供应商：合并单只能是一个供应商，请选择`;
  }
  return null;
}

/** 转单表单 → 入参 */
export function buildToPoPayload(form: ToPoForm): ToPoPayload {
  return {
    requisition_ids: form.requisition_ids,
    supplier_id: form.supplier_id || null,
    warehouse_id: form.warehouse_id || null,
    order_date: form.order_date || null,
    remark: form.remark.trim() || null,
  };
}

// ==================== 展示口径 ====================

/** 预算判定摘要（列表「预算」列 / 详情用） */
export function budgetTextOf(row: {
  budget_checked: boolean;
  over_budget: boolean;
  over_amount: DecimalText;
  budget_reason: string | null;
}): string {
  if (!row.budget_checked) {
    return row.budget_reason ? `未计预算（${row.budget_reason.slice(0, 24)}…）` : '未计预算';
  }
  if (row.over_budget) {
    return `超预算 ${decimalText(row.over_amount)}（需财务特批）`;
  }
  return '预算内';
}

/** 可转单判定（前端按钮可点性；真正的闸在后端 CAS） */
export function canConvert(row: { status: string; po_id: string | null }): boolean {
  return row.status === 'APPROVED' && !row.po_id;
}

/** 可提交判定 */
export function canSubmit(row: { status: string }): boolean {
  return row.status === 'DRAFT';
}

/** 空列表文案 */
export function emptyListText(filtered: boolean): string {
  return filtered ? '没有符合条件的申请单' : '还没有采购申请：点右上角「新建申请」提第一张需求';
}
