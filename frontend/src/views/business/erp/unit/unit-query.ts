/**
 * 计量单位页纯逻辑（P4-A）——**所有可测的判断都放这里**，`.vue` 只负责渲染。
 *
 * # 字段口径（硬红线①：以 dto.rs 的 serde 为准）
 *
 * 出参逐字抄 `backend/src/business/erp/dto.rs::UnitRes`，并用真接口 curl 核对过：
 * `unit_code / unit_name / is_base / base_unit_id / rate / status / sort_order / remark
 *  / is_system / precision`。
 *
 * ERP-S1（迁移 V0187）给 `biz_unit` 补了两列，本页因此多了两列：
 * - `is_system`：系统预置（迁移种下的标准值）→ 列表打「系统」标签、**不给删**（只能停用）；
 * - `precision`：数量小数位数 0–6（个数 0 位、重量 / 长度 / 面积体积 3 位）。
 * V0187 之前这两列不存在，当时注释写的是「不做小数位列」——历史结论已作废。
 *
 * # 为什么测 `.ts` 不测 `.vue`
 *
 * 用例跑在没有 DOM 的 node 里。「折算率怎么格式化」「停用是不是灰 tag」「清空折算基准要不要发
 * `null`」「筛选空串到底带不带」这些一旦漂移，`vue-tsc` 抓不到、e2e 也只看得出「界面长这样」。
 */
import type { PrimaryTableCol } from 'tdesign-vue-next';
import type { CreateUnitReq, Unit, UpdateUnitReq } from '@/api/business/erp';

/** 每页条数（单位是主数据，量小；20 条够翻） */
export const UNIT_PAGE_SIZE = 20;

/** 状态筛选（`status` 是 `smallint`，1 启用 / 0 停用；空串 = 全部，别用 0 当「全部」） */
export const UNIT_STATUS_OPTIONS: { label: string; value: number }[] = [
  { label: '启用', value: 1 },
  { label: '停用', value: 0 },
];

/** 筛选行状态 */
export interface UnitFilterState {
  /** 关键词（编码 / 名称模糊） */
  keyword: string;
  /** 状态：1 / 0 / ''（全部） */
  status: number | '';
}

/** 空筛选（重置用；返回新对象，别让调用方共享同一个引用） */
export function emptyUnitFilter(): UnitFilterState {
  return { keyword: '', status: '' };
}

/** 是否处于筛选态（空态文案要分「没数据」和「没匹配」） */
export function isUnitFiltered(filter: UnitFilterState): boolean {
  return Boolean(filter.keyword.trim()) || filter.status !== '';
}

/** 分页查询参数（`GET /erp/unit/page` 的 query） */
export interface UnitQuery {
  /** 关键词；空串不发 */
  keyword?: string;
  /** 状态；'' 不发 */
  status?: number;
  /** 页码，从 1 开始 */
  page: number;
  /** 每页条数 */
  page_size: number;
}

/**
 * 拼 `/erp/unit/page` 的入参。
 *
 * 空串一律**丢掉**：后端把 `?keyword=` 当「等于空串」过滤，会查出 0 行（W4a 踩过的坑）。
 */
export function buildUnitQuery(
  filter: UnitFilterState,
  page: number,
  pageSize: number = UNIT_PAGE_SIZE,
): UnitQuery {
  const query: UnitQuery = { page, page_size: pageSize };
  const keyword = filter.keyword.trim();
  if (keyword) {
    query.keyword = keyword;
  }
  if (filter.status !== '') {
    query.status = filter.status;
  }
  return query;
}

/** 状态文案（`status` 是 smallint，不是布尔） */
export function unitStatusLabel(status: number): string {
  if (status === 1) {
    return '启用';
  }
  if (status === 0) {
    return '停用';
  }
  return `未知(${status})`;
}

/**
 * 状态 → t-tag 主题。
 *
 * 启用 = `success`（绿）、停用 = `default`（灰）；未知值也走灰，别把脏数据染成绿的。
 */
export function unitStatusTheme(status: number): 'success' | 'default' {
  return status === 1 ? 'success' : 'default';
}

/** 是否基础单位（`is_base` 后端下发的是 bool，不是 0/1） */
export function unitBaseLabel(isBase: boolean): string {
  return isBase ? '基础单位' : '折算单位';
}

/** 基础单位标记的 tag 主题：基础单位用 primary 顶出来，折算单位用 outline 灰 */
export function unitBaseTheme(isBase: boolean): 'primary' | 'default' {
  return isBase ? 'primary' : 'default';
}

/**
 * 来源文案（ERP-S1）：`is_system = true` → 「系统」（迁移预置的标准值）。
 *
 * 客户自建 → 「自建」。这两个字是验收项（老大要在一屏里分清「系统给的」和「我建的」）。
 */
export function unitSourceLabel(isSystem: boolean | undefined): string {
  return isSystem ? '系统' : '自建';
}

/** 来源标签主题：系统预置用 warning（醒目、但不至于像报错），自建用 default 灰 */
export function unitSourceTheme(isSystem: boolean | undefined): 'warning' | 'default' {
  return isSystem ? 'warning' : 'default';
}

/**
 * 能不能删这个单位（ERP-S1）：系统预置 → **不能删**（后端 409，只能停用）。
 *
 * 前端先按这个判断把「删除」入口隐藏，别让用户点了才吃 409。
 */
export function canRemoveUnit(row: Pick<Unit, 'is_system'>): boolean {
  return !row.is_system;
}

/** 小数位数文案：0 位显示「0（整数）」，其余原样（`null` / 缺失 → `—`，脏数据不假装是 0） */
export function precisionLabel(precision: number | null | undefined): string {
  if (precision === null || precision === undefined || !Number.isFinite(Number(precision))) {
    return '—';
  }
  const value = Number(precision);
  return value === 0 ? '0（整数）' : String(value);
}

/**
 * 折算率格式化：`"12.000000"` → `"12"`、`"1.500000"` → `"1.5"`、`null` → `"—"`。
 *
 * `Decimal` 后端以**字符串**下发（不走 f64，避免精度漂移），所以这里按字符串裁尾零，
 * 不要 `parseFloat` 一把梭（`0.000001` 会变成 `0.000001` 没问题，但 `1e21` 就变科学计数法了）。
 */
export function formatRate(rate: number | string | null | undefined): string {
  if (rate === null || rate === undefined || rate === '') {
    return '—';
  }
  const text = String(rate).trim();
  if (!text.includes('.')) {
    return text;
  }
  const trimmed = text.replace(/0+$/, '').replace(/\.$/, '');
  return trimmed === '' ? '0' : trimmed;
}

/** 折算率的展示文案：基础单位恒等 1，折算单位写成「1 箱 = 12 个」 */
export function rateLabel(row: Pick<Unit, 'is_base' | 'rate'>, baseName = ''): string {
  if (row.is_base) {
    return '1（基准）';
  }
  const value = formatRate(row.rate);
  return baseName ? `1 = ${value} ${baseName}` : `1 = ${value}`;
}

/**
 * 把 `base_unit_id` 译成单位名（列表里只有 id）。
 *
 * 找不到（单位已删 / 只读接口没拉到）时给 `—`，不要显示一串 uuid。
 */
export function unitBaseNameOf(row: Pick<Unit, 'base_unit_id'>, units: Pick<Unit, 'id' | 'unit_name'>[]): string {
  if (!row.base_unit_id) {
    return '';
  }
  const found = units.find((item) => item.id === row.base_unit_id);
  return found ? found.unit_name : '';
}

/** 折算基准下拉候选：只列**基础单位**，且剔除自己（后端 `ck_biz_unit_no_self` 会拦） */
export function baseUnitOptions(
  units: Pick<Unit, 'id' | 'unit_code' | 'unit_name' | 'is_base'>[],
  selfId = '',
): { label: string; value: string }[] {
  return units
    .filter((item) => item.is_base && item.id !== selfId)
    .map((item) => ({ label: `${item.unit_name}（${item.unit_code}）`, value: item.id }));
}

/** 空态判断：`rows.length === 0` 才显示空态（`loading` 期间不算） */
export function isUnitListEmpty(rows: Pick<Unit, 'id'>[] | null | undefined): boolean {
  return (rows?.length ?? 0) === 0;
}

/**
 * 表格列（`colKey` 与后端出参字段同名，插槽按 colKey 命中）。
 *
 * `op` 是唯一非后端字段列；其余 colKey 必须能在 `UnitRes` 里找到同名键 —— 单测守着这条。
 */
export const UNIT_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'unit_code', title: '单位编码', width: 150, fixed: 'left', ellipsis: true },
  { colKey: 'unit_name', title: '单位名称', minWidth: 160, ellipsis: true },
  { colKey: 'is_system', title: '来源', width: 90 },
  { colKey: 'is_base', title: '单位类型', width: 120 },
  { colKey: 'rate', title: '折算率', width: 180 },
  { colKey: 'precision', title: '小数位', width: 100 },
  { colKey: 'sort_order', title: '排序', width: 90 },
  { colKey: 'status', title: '状态', width: 100 },
  { colKey: 'op', title: '操作', width: 170, fixed: 'right' },
];

/** 新建 / 编辑表单（`base_unit_id` 用空串表示「基础单位」，提交时空串转 `null`） */
export interface UnitFormState {
  /** 单位编码（编辑态只读，后端 `UpdateUnitReq` 里根本没有这个字段） */
  unit_code: string;
  /** 单位名称 */
  unit_name: string;
  /** 是否基础单位 */
  is_base: boolean;
  /** 折算基准单位 id（`is_base=true` 时为空） */
  base_unit_id: string;
  /** 折算率（`is_base=true` 时恒 1，输入框置灰） */
  rate: number;
  /** 启用状态 1 / 0 */
  status: number;
  /** 排序值 */
  sort_order: number;
  /** 备注 */
  remark: string;
  /** 数量小数位数（0–6；ERP-S1 V0187） */
  precision: number;
}

/** 空表单（新建：默认基础单位，符合后端 `is_base` 缺省 true） */
export function emptyUnitForm(): UnitFormState {
  return {
    unit_code: '',
    unit_name: '',
    is_base: true,
    base_unit_id: '',
    rate: 1,
    status: 1,
    sort_order: 0,
    remark: '',
    precision: 0,
  };
}

/** 编辑回填 */
export function unitFormFromRow(row: Unit): UnitFormState {
  return {
    unit_code: row.unit_code,
    unit_name: row.unit_name,
    is_base: row.is_base,
    base_unit_id: row.base_unit_id ?? '',
    rate: Number(row.rate ?? 1),
    status: row.status === 0 ? 0 : 1,
    sort_order: Number(row.sort_order ?? 0),
    remark: row.remark ?? '',
    precision: Number(row.precision ?? 0),
  };
}

/** 表单字段错误（键与表单项 name 同名） */
export type UnitFieldErrors = Partial<
  Record<'unit_code' | 'unit_name' | 'base_unit_id' | 'rate' | 'precision', string>
>;

/** 是否有字段错误（有就拦住提交） */
export function hasUnitFieldError(errors: UnitFieldErrors): boolean {
  return Object.keys(errors).length > 0;
}

/**
 * 校验（口径照 `dto::CreateUnitReq::validate` / `UpdateUnitReq::validate`）。
 *
 * - 编码 ≤20、名称 ≤50（超了后端 400 + 10001，前端先拦能省一次往返）；
 * - 基础单位**不能**填折算基准、折算率必须 1；折算单位必须选基准且 `rate > 0`。
 *
 * @param isEdit 编辑态不校验编码（后端 PUT 不接受该字段）
 */
export function validateUnitForm(form: UnitFormState, isEdit: boolean): UnitFieldErrors {
  const errors: UnitFieldErrors = {};
  if (!isEdit) {
    const code = form.unit_code.trim();
    if (!code) {
      errors.unit_code = '单位编码必填';
    } else if (code.length > 20) {
      errors.unit_code = '单位编码最多 20 个字符';
    }
  }
  const name = form.unit_name.trim();
  if (!name) {
    errors.unit_name = '单位名称必填';
  } else if (name.length > 50) {
    errors.unit_name = '单位名称最多 50 个字符';
  }
  if (!form.is_base) {
    if (!form.base_unit_id) {
      errors.base_unit_id = '折算单位必须选一个基础单位';
    }
    if (!Number.isFinite(form.rate) || form.rate <= 0) {
      errors.rate = '折算率必须大于 0（1 个本单位等于几个基准单位）';
    }
  }
  // 小数位数：与后端 ck_biz_unit_precision 同口径（0–6 的整数）
  if (!Number.isInteger(form.precision) || form.precision < 0 || form.precision > 6) {
    errors.precision = '小数位数只能是 0–6 的整数';
  }
  return errors;
}

/** 新建请求体（基础单位不发 `base_unit_id` / `rate`，后端会拦「基础单位不能有基准」） */
export function buildUnitCreatePayload(form: UnitFormState): CreateUnitReq {
  const payload: CreateUnitReq = {
    unit_code: form.unit_code.trim(),
    unit_name: form.unit_name.trim(),
    is_base: form.is_base,
    status: form.status === 0 ? 0 : 1,
    sort_order: Number.isFinite(form.sort_order) ? form.sort_order : 0,
    remark: form.remark.trim() ? form.remark.trim() : null,
    precision: Number.isInteger(form.precision) ? form.precision : 0,
  };
  if (!form.is_base) {
    payload.base_unit_id = form.base_unit_id;
    payload.rate = form.rate;
  } else {
    payload.rate = 1;
  }
  return payload;
}

/**
 * 编辑请求体：**只发真改过的字段**（缺席 = 不改）。
 *
 * `base_unit_id` 从「有」改成「无」必须发 `null`（后端 `double_option`：`null` = 清空 → 变基础单位），
 * 发 `""` 会被当成非法 uuid → 400。
 */
export function buildUnitUpdatePayload(form: UnitFormState, original: Unit): UpdateUnitReq {
  const payload: UpdateUnitReq = {};
  const name = form.unit_name.trim();
  if (name && name !== original.unit_name) {
    payload.unit_name = name;
  }
  const status = form.status === 0 ? 0 : 1;
  if (status !== original.status) {
    payload.status = status;
  }
  if (Number(form.sort_order) !== Number(original.sort_order ?? 0)) {
    payload.sort_order = Number(form.sort_order);
  }
  const originalBase = original.base_unit_id ?? '';
  if (form.base_unit_id !== originalBase) {
    payload.base_unit_id = form.base_unit_id ? form.base_unit_id : null;
    // 变成基础单位时 rate 由后端归 1，前端不发，避免「rate != 1 + is_base」被 400 挡回
    if (form.base_unit_id && Number(form.rate) !== Number(original.rate ?? 1)) {
      payload.rate = Number(form.rate);
    }
  } else if (!form.is_base && Number(form.rate) !== Number(original.rate ?? 1)) {
    payload.rate = Number(form.rate);
  }
  const remark = form.remark.trim();
  const originalRemark = original.remark ?? '';
  if (remark !== originalRemark) {
    payload.remark = remark ? remark : null;
  }
  if (Number(form.precision) !== Number(original.precision ?? 0)) {
    payload.precision = Number(form.precision);
  }
  return payload;
}
