/**
 * 批次管理页纯逻辑（COST-B1-12 / ARCH-9 §5.2「批次主数据」）。
 *
 * # 为什么单独一个 `.ts`
 *
 * 与 `cost-object-logic.ts` / `stock-out-logic.ts` 同理：用例跑在没有 DOM 的 node 里，
 * `.vue` 的 `setup()` 起不来；而这里有四件**错了会出事**的事：
 *
 * 1. **编辑只能发「真改过」的字段** —— 后端 PUT 是三态部分更新，整表回传会把
 *    没返回的字段当「清空」写掉（`remark` / 日期是典型的受害者）；
 * 2. **`material_id` 不能进编辑报文** —— 后端不认（业务键不可变），发过去只是噪音；
 * 3. **失效日期不得早于生产日期** —— 后端 `ck_biz_lot_dates` + dto 都拦，前端提前一道；
 * 4. **批号 / 物料必填** —— 空提交在唯一键检查前就是 400，白跑一次。
 *
 * 断言口径与后端逐字对齐：
 * - 值域：`ck_biz_lot_source` = `PURCHASE|PRODUCTION|OPENING|MANUAL`、`ck_biz_lot_qc` = `NA|PENDING|PASSED|REJECTED`；
 * - 状态：DDL `SMALLINT`，`1` 启用 / `0` 停用（**数字不是布尔**）；
 * - 长度：`lot_no VARCHAR(64)`、`source_doc_no VARCHAR(32)`；
 * - 重复码：`error_code::ERP_LOT_NO_DUPLICATE = 66056`。
 */

import type { LotPageQuery, LotQcStatus, LotRow, LotSourceType } from '@/api/business/erp/lot-kinds';
import {
  LOT_NO_DUPLICATE_CODE,
  LOT_NO_MAX,
  LOT_PAGE_SIZE,
  LOT_STATUS_DISABLED,
  LOT_STATUS_ENABLED,
  QC_STATUS_OPTIONS,
  SOURCE_TYPE_OPTIONS,
} from '@/api/business/erp/lot-kinds';

/** 列表筛选状态（`''` = 全部） */
export interface LotFilterState {
  /** 批号 / 物料编码 / 物料名称（模糊） */
  keyword: string;
  /** 物料（空 = 全部） */
  material_id: string;
  /** 来源类型（空 = 全部） */
  source_type: string;
  /** 质检状态（空 = 全部） */
  qc_status: string;
  /** 状态（`''` = 全部；`1` 启用 / `0` 停用） */
  status: string;
}

/** 空筛选 */
export function emptyLotFilter(): LotFilterState {
  return { keyword: '', material_id: '', source_type: '', qc_status: '', status: '' };
}

/** 筛选是否生效（空态文案要区分「还没有批」和「没有匹配」） */
export function isLotFiltered(filter: LotFilterState): boolean {
  return Boolean(
    filter.keyword.trim() || filter.material_id || filter.source_type || filter.qc_status || filter.status !== '',
  );
}

/** 筛选 → 查询参数（空串一律不发；`status` 要转数字，后端字段是 `Option<i16>`） */
export function buildLotQuery(
  filter: LotFilterState,
  page: number,
  pageSize: number = LOT_PAGE_SIZE,
): LotPageQuery {
  const query: LotPageQuery = { page, page_size: pageSize };
  if (filter.keyword.trim()) {
    query.keyword = filter.keyword.trim();
  }
  if (filter.material_id) {
    query.material_id = filter.material_id;
  }
  if (filter.source_type) {
    query.source_type = filter.source_type;
  }
  if (filter.qc_status) {
    query.qc_status = filter.qc_status;
  }
  if (filter.status !== '') {
    query.status = Number(filter.status);
  }
  return query;
}

/** 建 / 改表单（字符串字段空串 = 没填；提交时转 `null` / 省略） */
export interface LotFormState {
  /** 编辑态的行 id（`null` = 新建） */
  id: string | null;
  /** 批号 */
  lot_no: string;
  /** 物料（**编辑态只读**） */
  material_id: string;
  /** 来源类型 */
  source_type: LotSourceType;
  /** 生产 / 到货日期 `YYYY-MM-DD`（空串 = 没填） */
  prod_date: string;
  /** 失效日期 `YYYY-MM-DD`（空串 = 没填） */
  expire_date: string;
  /** 质检状态 */
  qc_status: LotQcStatus;
  /** 状态 1 / 0 */
  status: number;
  /** 备注 */
  remark: string;
}

/** 今天（CST，`YYYY-MM-DD`） */
export function todayString(now: Date = new Date()): string {
  const shifted = new Date(now.getTime() + 8 * 3600 * 1000);
  return shifted.toISOString().slice(0, 10);
}

/** 空表单（新建：来源缺省 `MANUAL`、质检 `NA`、启用） */
export function emptyLotForm(now: Date = new Date()): LotFormState {
  return {
    id: null,
    lot_no: '',
    material_id: '',
    source_type: 'MANUAL',
    prod_date: todayString(now),
    expire_date: '',
    qc_status: 'NA',
    status: LOT_STATUS_ENABLED,
    remark: '',
  };
}

/** 行 → 表单（编辑态；`material_id` 会带出来但界面只读） */
export function formFromRow(row: LotRow): LotFormState {
  return {
    id: row.id,
    lot_no: row.lot_no,
    material_id: row.material_id,
    source_type: (row.source_type as LotSourceType) || 'MANUAL',
    prod_date: row.prod_date ?? '',
    expire_date: row.expire_date ?? '',
    qc_status: (row.qc_status as LotQcStatus) || 'NA',
    status: row.status === LOT_STATUS_DISABLED ? LOT_STATUS_DISABLED : LOT_STATUS_ENABLED,
    remark: row.remark ?? '',
  };
}

/** 校验错误（键 = 字段名） */
export type LotFormErrors = Record<string, string>;

/** 表单校验（纯逻辑，不查库；唯一键冲突只能靠后端） */
export function validateLotForm(form: LotFormState): LotFormErrors {
  const errors: LotFormErrors = {};

  const lotNo = form.lot_no.trim();
  if (!lotNo) {
    errors.lot_no = '批号不能为空';
  } else if (lotNo.length > LOT_NO_MAX) {
    errors.lot_no = `批号最长 ${LOT_NO_MAX} 个字符（当前 ${lotNo.length} 个）`;
  }

  if (!form.material_id) {
    errors.material_id = '请选择物料（批号在物料下唯一，物料是业务键的一半）';
  }

  if (form.prod_date && form.expire_date && form.expire_date < form.prod_date) {
    errors.expire_date = `失效日期（${form.expire_date}）不能早于生产日期（${form.prod_date}）`;
  }

  if (form.remark.length > 1000) {
    errors.remark = `备注最长 1000 字（当前 ${form.remark.length} 字）`;
  }

  return errors;
}

/** 表单是否可提交 */
export function isLotFormValid(form: LotFormState): boolean {
  return Object.keys(validateLotForm(form)).length === 0;
}

/** 可空文本：空串 → `null` */
function nullableText(value: string): string | null {
  const text = value.trim();
  return text ? text : null;
}

/** 新建报文（只有 `lot_no` / `material_id` 必填） */
export function toCreatePayload(form: LotFormState) {
  return {
    lot_no: form.lot_no.trim(),
    material_id: form.material_id,
    source_type: form.source_type,
    prod_date: nullableText(form.prod_date),
    expire_date: nullableText(form.expire_date),
    qc_status: form.qc_status,
    status: form.status,
    remark: nullableText(form.remark),
  };
}

/**
 * 编辑报文：**只带真改过的字段**（后端三态部分更新）。
 *
 * 规则：
 * - 值没变 → 键不出现（后端「缺失 = 不改」）；
 * - 可空字段从「有值」改成「空」→ 发 `null`（后端「显式 null = 清空」）；
 * - `material_id` 永远不发（业务键不可变，后端入参里也没有）。
 *
 * 比较前对文本做 `trim`：用户在备注末尾多打一个空格不该算一次改动。
 */
export function toUpdatePayload(original: LotFormState, form: LotFormState) {
  const payload: Record<string, string | number | null> = {};

  if (form.lot_no.trim() !== original.lot_no.trim()) {
    payload.lot_no = form.lot_no.trim();
  }
  if (form.source_type !== original.source_type) {
    payload.source_type = form.source_type;
  }
  if (form.prod_date !== original.prod_date) {
    payload.prod_date = nullableText(form.prod_date);
  }
  if (form.expire_date !== original.expire_date) {
    payload.expire_date = nullableText(form.expire_date);
  }
  if (form.qc_status !== original.qc_status) {
    payload.qc_status = form.qc_status;
  }
  if (form.status !== original.status) {
    payload.status = form.status;
  }
  if (form.remark.trim() !== original.remark.trim()) {
    payload.remark = nullableText(form.remark);
  }

  return payload;
}

/** 编辑报文是否为空（没改任何东西 → 不必发请求） */
export function isEmptyUpdate(payload: Record<string, unknown>): boolean {
  return Object.keys(payload).length === 0;
}

/** 来源类型 → 中文名 */
export function sourceTypeLabel(value: LotSourceType | string | null | undefined): string {
  const hit = SOURCE_TYPE_OPTIONS.find((item) => item.value === value);
  return hit ? hit.label : String(value ?? '—');
}

/** 质检状态 → 中文名 */
export function qcStatusLabel(value: LotQcStatus | string | null | undefined): string {
  const hit = QC_STATUS_OPTIONS.find((item) => item.value === value);
  return hit ? hit.label : String(value ?? '—');
}

/** 质检状态 → TDesign 标签主题（不合格标红，待检标橙） */
export function qcStatusTheme(
  value: LotQcStatus | string | null | undefined,
): 'default' | 'primary' | 'warning' | 'success' | 'danger' {
  switch (value) {
    case 'PASSED':
      return 'success';
    case 'PENDING':
      return 'warning';
    case 'REJECTED':
      return 'danger';
    default:
      return 'default';
  }
}

/** 状态（数字）→ 中文名 */
export function lotStatusLabel(value: number | null | undefined): string {
  return value === LOT_STATUS_DISABLED ? '停用' : '启用';
}

/** 状态（数字）→ 标签主题 */
export function lotStatusTheme(value: number | null | undefined): 'success' | 'default' {
  return value === LOT_STATUS_DISABLED ? 'default' : 'success';
}

/** 空值占位 */
export function dash(value: string | null | undefined): string {
  return value && String(value).trim() ? String(value) : '—';
}

/** 短 id（物料名还没 join 出来时兜底，绝不整段 UUID 当名字显示） */
export function shortId(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return value.length > 8 ? `${value.slice(0, 8)}…` : value;
}

/** 接口错误的最小形状（不 import axios：单测能直接喂对象） */
export interface ErrorLike {
  /** 业务码（`66056` 批号重复 / `10001` 参数 …） */
  code?: number;
  /** 报文（**原样展示**：批号重复的「物料 + 批号」就在这句里） */
  message?: string;
}

/**
 * 错误 → 页面文案。
 *
 * 关键口径：**原样带后端 `msg`**。批号重复的报文长这样：
 * 「批号「LOT-A1」在物料「MAT-0001 钢轴」下已存在：同一物料不能有两个相同批号。…」
 * 只写「保存失败」等于把用户唯一的自救信息（换哪个批号 / 去改哪一行）丢掉。
 */
export function errorText(error: unknown): string {
  if (error && typeof error === 'object') {
    const like = error as ErrorLike;
    const message = (like.message ?? '').trim();
    if (message) {
      return typeof like.code === 'number' && like.code > 0 ? `${message}（错误码 ${like.code}）` : message;
    }
  }
  if (typeof error === 'string' && error.trim()) {
    return error;
  }
  return '操作失败：接口没有返回说明，请刷新后重试';
}

/** 报错是否是「批号重复」（页面要额外提示「去改已有那一行」） */
export function isDuplicateLotNoError(error: unknown): boolean {
  return Boolean(error && typeof error === 'object' && (error as ErrorLike).code === LOT_NO_DUPLICATE_CODE);
}
