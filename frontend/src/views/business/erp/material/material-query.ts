/**
 * 物料列表页的纯逻辑（W4b-1）。
 *
 * 为什么单独一个 `.ts` 而不是全塞 `.vue`：单测跑的是 `tsx --test`（没有 DOM、没有
 * `@vue/test-utils`），`.vue` 里的判断一条都测不到。所以「筛选参数怎么组装 / 一物一码
 * 显示成什么 / 分页参数怎么算 / 新建编辑请求体带哪些字段」全放这里，`.vue` 只渲染。
 *
 * # 字段口径（照 `backend/src/business/erp/dto.rs` 的 serde 出参，不是卡面草图）
 *
 * | 卡面草图 | 真字段 | 出处 |
 * |---|---|---|
 * | 分类 `category_id` | `material_type` ∈ `RAW/SEMI/FINISHED/SERVICE` | `dto::MATERIAL_TYPES`；`handler.rs` 没有 `/erp/category` 路由 |
 * | 单位 `unit_name` | `unit_id` + `/erp/unit/page` 本地译名 | `MaterialRes` 只有 id |
 * | 是否启用 `is_active` | `status` = 1 / 0 | `MaterialRes::status: i16` |
 * | `serial_required` | `serial_required: bool` | V0138 列；W2b 补进接口 |
 */
import type {
  CreateMaterialReq,
  MaterialRow,
  MaterialType,
  UnitRow,
  UpdateMaterialReq,
} from '@/api/business/erp';

/** 默认每页条数（服务端分页） */
export const MATERIAL_PAGE_SIZE = 10;

/** 「一物一码」筛选的三态值（`t-select` 只吃字符串；`'all'` = 全部 → 请求里整键缺席） */
export type SerialFilterValue = 'all' | 'true' | 'false';

/** 筛选行状态 */
export interface MaterialFilterState {
  /** 关键词（编码 / 名称 / 规格模糊） */
  keyword: string;
  /** 分类 = 物料类型；`''` = 全部 */
  material_type: MaterialType | '';
  /** 一物一码：`'all'` 全部 / `'true'` 是 / `'false'` 否 */
  serial_required: SerialFilterValue;
}

/** 物料类型下拉项（中文文案与 `MATERIAL_TYPES` 顺序一致） */
export const MATERIAL_TYPE_OPTIONS: ReadonlyArray<{ label: string; value: MaterialType }> = [
  { label: '原材料', value: 'RAW' },
  { label: '半成品', value: 'SEMI' },
  { label: '产成品', value: 'FINISHED' },
  { label: '包装物', value: 'PACKAGING' },
  { label: '低值易耗品', value: 'CONSUMABLE' },
  { label: '办公用品', value: 'OFFICE' },
  { label: '服务', value: 'SERVICE' },
];

/** 一物一码下拉项：★ 标在「是」上，和列表里的标记呼应 */
export const SERIAL_FILTER_OPTIONS: ReadonlyArray<{ label: string; value: SerialFilterValue }> = [
  { label: '全部', value: 'all' },
  { label: '★ 是（一物一码）', value: 'true' },
  { label: '否', value: 'false' },
];

/**
 * 成本法（计价方式）下拉项 —— **只有一项**。
 *
 * 收敛依据：`backend/src/business/erp/dto.rs::COSTING_METHODS`（V0201 起 = `["MOVING_AVG"]`）。
 * 后端 `stockin_write.rs` / `transfer_settle.rs` / `serial_model.rs` 全文不读 `costing_method`，
 * 成本一律按 `avg_cost`（移动加权）算 —— FIFO / STANDARD 从来没有实现，以前选了只是
 * **静默按移动加权算**。所以这里把下拉砍成一项 + 灰字说明，不再让用户以为自己能选。
 *
 * 放开的时机：后端放开 `COSTING_METHODS` 与 `ck_biz_material_costing` 时，这份常量同步加项。
 */
export const COSTING_METHOD_OPTIONS: ReadonlyArray<{ label: string; value: string }> = [
  { label: '移动加权平均（MOVING_AVG）', value: 'MOVING_AVG' },
];

/** 成本法灰字说明（与后端 400 文案同口径，改一边就要改另一边） */
export const COSTING_METHOD_HINT =
  '本版本仅支持移动加权平均（MOVING_AVG）；FIFO / 标准成本（STANDARD）需要批次成本层，属路线图能力，暂不可选。';

/**
 * 成本法下拉项（含历史值的兜底展示）。
 *
 * 主数据里若还留着收敛前的 `FIFO` / `STANDARD`（老数据 / 别的环境），下拉不能只显示空白
 * —— 那样用户看不出这行到底是什么，会以为丢了数据。这里把它作为**禁用的**历史项插到最前，
 * 文案点明「本版本不支持」（后端已被本次收敛回填为 MOVING_AVG，能选到这种值的只剩没跑
 * V0201 的环境）。
 *
 * @param current 当前表单 / 行上的取值
 */
export function costingMethodOptions(current: string | null | undefined): ReadonlyArray<{
  label: string;
  value: string;
  disabled?: boolean;
}> {
  const options: { label: string; value: string; disabled?: boolean }[] = COSTING_METHOD_OPTIONS.map(
    (item) => ({ ...item }),
  );
  const value = (current ?? '').trim();
  if (value && !options.some((item) => item.value === value)) {
    options.unshift({ label: `${value}（历史值，本版本不支持）`, value, disabled: true });
  }
  return options;
}

/** 成本法中文名（下拉标签；未知值原样透出，不吞数据） */
export function costingMethodLabel(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return COSTING_METHOD_OPTIONS.find((item) => item.value === value)?.label ?? value;
}

/** 类型 → 中文（未知值原样回显，不吞数据） */
const MATERIAL_TYPE_LABELS: Record<string, string> = MATERIAL_TYPE_OPTIONS.reduce<Record<string, string>>(
  (acc, item) => {
    acc[item.value] = item.label;
    return acc;
  },
  {},
);

/**
 * 物料类型中文名。
 *
 * @param type 后端 `material_type`
 */
export function materialTypeLabel(type: string | null | undefined): string {
  if (!type) {
    return '—';
  }
  return MATERIAL_TYPE_LABELS[type] ?? type;
}

/**
 * 是否一物一码（**严格判定**：只有 true / 1 / 'true' 算是）。
 *
 * 后端出参是 `bool`，但历史脏数据 / 缓存里的 `0` 不能被 `Boolean()` 误判成 true ——
 * 一物一码是会触发序列号生成的开关，宁可少判也不多判。
 */
export function isSerialRequired(value: unknown): boolean {
  return value === true || value === 1 || value === 'true';
}

/**
 * 列表「一物一码」列的文案。
 *
 * `true` → `「★是」`（★ 让一物一码物料在一屏里能一眼捞出来）；
 * 其它 → `「否」`。
 */
export function serialRequiredLabel(value: unknown): string {
  return isSerialRequired(value) ? '★是' : '否';
}

/** 三态筛选值 → 后端 `serial_required`（`''` = 全部 = **不带**该参数） */
export function serialFilterFlag(value: SerialFilterValue): boolean | undefined {
  if (value === 'true') {
    return true;
  }
  if (value === 'false') {
    return false;
  }
  return undefined;
}

/** 空的筛选状态 */
export function emptyMaterialFilter(): MaterialFilterState {
  return { keyword: '', material_type: '', serial_required: 'all' };
}

/** `/erp/material/page` 的查询参数（`undefined` 的键由 api 层 `asParams` 丢掉） */
export interface MaterialPageQuery {
  page: number;
  page_size: number;
  keyword?: string;
  material_type?: MaterialType;
  serial_required?: boolean;
}

/**
 * 组装分页查询参数。
 *
 * 三条硬口径：
 * 1. `keyword` 先 `trim`，空串**不进**参数（后端把 `?keyword=` 当等于空串过滤 → 0 行）；
 * 2. `serial_required` 只在三态选了「是 / 否」时出现，`''`（全部）时**整键缺席**；
 * 3. `page` / `page_size` 一律正整数（0 / 负数 / 小数夹到合法值）。
 */
export function buildMaterialQuery(
  state: MaterialFilterState,
  page: number = 1,
  pageSize: number = MATERIAL_PAGE_SIZE,
): MaterialPageQuery {
  const query: MaterialPageQuery = {
    page: normalizePositiveInt(page, 1),
    page_size: normalizePositiveInt(pageSize, MATERIAL_PAGE_SIZE),
  };
  const keyword = state.keyword.trim();
  if (keyword) {
    query.keyword = keyword;
  }
  if (state.material_type) {
    query.material_type = state.material_type;
  }
  const flag = serialFilterFlag(state.serial_required);
  if (flag !== undefined) {
    query.serial_required = flag;
  }
  return query;
}

/** 正整数夹紧 */
function normalizePositiveInt(value: number, fallback: number): number {
  if (!Number.isFinite(value)) {
    return fallback;
  }
  const int = Math.trunc(value);
  return int >= 1 ? int : fallback;
}

/**
 * 页码夹紧：筛选变窄 / 删到当前页空时，别停在一个不存在的页码上。
 *
 * @param page 想去的页码
 * @param total 筛选后的总条数
 * @param pageSize 每页条数
 */
export function clampPage(page: number, total: number, pageSize: number = MATERIAL_PAGE_SIZE): number {
  const size = normalizePositiveInt(pageSize, MATERIAL_PAGE_SIZE);
  const maxPage = Math.max(1, Math.ceil(Math.max(0, total) / size));
  return Math.min(Math.max(1, normalizePositiveInt(page, 1)), maxPage);
}

/** 单位 id → 单位名（`unit_name（unit_code）` 太长，列表只要名字） */
export function unitNameOf(unitId: string | null | undefined, units: ReadonlyArray<UnitRow>): string {
  if (!unitId) {
    return '—';
  }
  const hit = units.find((unit) => unit.id === unitId);
  return hit ? hit.unit_name : '—';
}

/** 单位下拉项（`单位名（编码）`，编码在仓库里唯一、肉眼好核对） */
export function unitOptions(units: ReadonlyArray<UnitRow>): ReadonlyArray<{ label: string; value: string }> {
  return units.map((unit) => ({ label: `${unit.unit_name}（${unit.unit_code}）`, value: unit.id }));
}

/** 启用状态文案 */
export function materialStatusLabel(status: number | null | undefined): string {
  return status === 1 ? '启用' : '停用';
}

/** 新建 / 编辑弹窗的表单状态（全字符串，方便 `t-input` 直接绑） */
export interface MaterialFormState {
  /** 编码（编辑态只读） */
  material_code: string;
  /** 名称 */
  material_name: string;
  /** 分类 = 类型 */
  material_type: MaterialType;
  /** 基础单位 id */
  unit_id: string;
  /** 规格 */
  spec: string;
  /** 成本法（本版本锁定 MOVING_AVG；字段留在表单里是为了回填 + 提交时显式带上） */
  costing_method: string;
  /** ★ 是否一物一码 */
  serial_required: boolean;
  /** 1 启用 / 0 停用（模板里用开关桥接） */
  status: number;
  /** 备注 */
  remark: string;
}

/** 表单校验错误（键 = 字段名） */
export type MaterialFieldErrors = Partial<Record<'material_code' | 'material_name' | 'unit_id', string>>;

/** 空表单（新建） */
export function emptyMaterialForm(defaultType: MaterialType = 'RAW'): MaterialFormState {
  return {
    material_code: '',
    material_name: '',
    material_type: defaultType,
    unit_id: '',
    spec: '',
    costing_method: 'MOVING_AVG',
    serial_required: false,
    status: 1,
    remark: '',
  };
}

/** 列表行 → 表单（编辑回填） */
export function materialFormFromRow(row: MaterialRow): MaterialFormState {
  return {
    material_code: row.material_code,
    material_name: row.material_name,
    material_type: row.material_type,
    unit_id: row.unit_id,
    spec: row.spec ?? '',
    costing_method: row.costing_method ?? 'MOVING_AVG',
    serial_required: isSerialRequired(row.serial_required),
    status: row.status,
    remark: row.remark ?? '',
  };
}

/**
 * 表单校验（与后端 `CreateMaterialReq::validate` 同口径）。
 *
 * 编码 ≤40、名称 ≤120（`require_text`）、单位必填；编码只在新建时校验（编辑不改码）。
 */
export function validateMaterialForm(form: MaterialFormState, isEdit: boolean): MaterialFieldErrors {
  const errors: MaterialFieldErrors = {};
  if (!isEdit) {
    const code = form.material_code.trim();
    if (!code) {
      errors.material_code = '物料编码必填';
    } else if (code.length > 40) {
      errors.material_code = '物料编码最多 40 个字符';
    }
  }
  const name = form.material_name.trim();
  if (!name) {
    errors.material_name = '物料名称必填';
  } else if (name.length > 120) {
    errors.material_name = '物料名称最多 120 个字符';
  }
  if (!form.unit_id) {
    errors.unit_id = '请选择计量单位（后端 unit_id 必填）';
  }
  return errors;
}

/** 错误表是否为空 */
export function hasFieldError(errors: MaterialFieldErrors): boolean {
  return Object.keys(errors).length > 0;
}

/** 空串 → `null`（后端 `Option<String>` 用 null 表示清空 / 不填） */
function nullableText(value: string): string | null {
  const text = value.trim();
  return text ? text : null;
}

/**
 * 新建请求体（`CreateMaterialReq`）。
 *
 * **不含** `material_code` 以外的身份字段；`status` 走 1/0；`serial_required` 一定显式带上
 * （新建时它是这个物料最重要的一位，不能靠后端缺省 false 蒙对）。
 */
export function buildMaterialCreatePayload(form: MaterialFormState): CreateMaterialReq {
  return {
    material_code: form.material_code.trim(),
    material_name: form.material_name.trim(),
    material_type: form.material_type,
    unit_id: form.unit_id,
    spec: nullableText(form.spec),
    // 成本法显式带上：本版本只有 MOVING_AVG（V0201 收敛），不靠后端缺省蒙对
    costing_method: form.costing_method || 'MOVING_AVG',
    serial_required: form.serial_required === true,
    status: form.status === 1 ? 1 : 0,
    remark: nullableText(form.remark),
  };
}

/**
 * 编辑请求体（`UpdateMaterialReq`）。
 *
 * 两条硬口径：
 * 1. **不带 `material_code`** —— 后端 `UpdateMaterialReq` 里没有这个字段（编码是身份）；
 * 2. **不带 `serial_required`** —— 页面编辑态把它置灰（一物一码语义一旦创建就不能随便翻），
 *    所以它永远等于原值，发了也只是噪音。真值不同才发（防御将来放开置灰）。
 * 3. **不带 `costing_method`** —— 同理：本版本只有 MOVING_AVG（V0201 收敛），下拉锁定，
 *    值永远等于原值；真发一个不是 MOVING_AVG 的历史值只会换来后端 400。
 */
export function buildMaterialUpdatePayload(form: MaterialFormState, row: MaterialRow): UpdateMaterialReq {
  const payload: UpdateMaterialReq = {
    material_name: form.material_name.trim(),
    material_type: form.material_type,
    unit_id: form.unit_id,
    spec: nullableText(form.spec),
    status: form.status === 1 ? 1 : 0,
    remark: nullableText(form.remark),
  };
  if ((form.serial_required === true) !== isSerialRequired(row.serial_required)) {
    payload.serial_required = form.serial_required === true;
  }
  return payload;
}
