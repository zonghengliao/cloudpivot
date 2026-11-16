/**
 * 安全库存设置页纯逻辑（M2-17）—— 筛选拼参、批量展开预览、表单校验、数值 / 状态格式化。
 *
 * # 为什么逻辑搬出 `.vue`
 *
 * 用例跑在没有 DOM 的 node 里（`tsx --test`，见 `tests/unit/m217-stock-policy-page.test.ts`），
 * 而下面几处错一个就会让仓管把水位设错，且**界面上看不出错**：
 *
 * * **批量展开**：`物料 × 仓库` 的笛卡尔积算错一格，就是漏设 / 多设一格水位；
 *   去重算错就会把「同一格传两次」发成两格（后端 upsert 会二次命中同一行 → 整批失败）。
 *   所以展开逻辑在这里是**纯函数**，与后端 `BatchSavePolicyReq::to_plan` 同一口径，
 *   用例逐条对拍（含 2 000 格上限、重复计数、顺序）。
 * * **校验**：`max_qty` 必须**严格大于** `min_qty`（相等 = 空区间）；`min_qty >= 0`。
 *   页面要在发请求前拦住并说清是哪个框，而不是让用户去读后端 400 的英文约束名。
 * * **数量**：后端 `Decimal` 出 JSON 是字符串（`"120.0000"`），直接 `+` 是字符串拼接；
 *   `formatQty` 是唯一出口，且要能把 `null`（未设最高水位）显示成 `—` 而不是 `0`。
 *
 * 纯函数、零副作用；字段名一律逐字抄后端（`stock_policy::dto`）。
 */
import type { StockPolicyBatchPayload, StockPolicyQuery, StockPolicyRow } from '@/api/business/erp/stock-policy';

// ==================== 常量 ====================

/** 列表每页条数（与其它 ERP 列表页同口径 20；后端上限 200） */
export const STOCK_POLICY_PAGE_SIZE = 20;

/**
 * 页面显形码（V0431 的菜单页面行 + 路由 `meta.permission`）。
 *
 * 它**只判前端**：侧栏显隐 + 路由守卫，后端源码零引用（登记在
 * `permission_audit_tests::ROUTE_LEVEL_ONLY`）。
 */
export const PERM_STOCK_POLICY_PAGE = 'business:erp:stock-policy:page';

/** 列表接口码（`GET …/stock-policy/page`，V0428 按钮行） */
export const PERM_STOCK_POLICY_LIST = 'business:erp:stock-policy:list';

/** 写入接口码（单格 upsert + 批量设置共用，V0428 按钮行） */
export const PERM_STOCK_POLICY_SAVE = 'business:erp:stock-policy:save';

/** 删除接口码（软删 = 该格回退物料级，V0428 按钮行） */
export const PERM_STOCK_POLICY_DELETE = 'business:erp:stock-policy:delete';

/**
 * 导入入口的业务编码（后端 `impexp::builtin::erp_setting::STOCK_POLICY_BIZ_TYPE`）。
 *
 * 与后端常量**逐字一致**：写错不会报错，只会让「下载模板」返回 404 `60011`「功能未注册」。
 */
export const STOCK_POLICY_IMPORT_BIZ_TYPE = 'business.erp.stock-policy';

/** 一次批量设置最多铺开多少格（与后端 `dto::MAX_BATCH_CELLS` 同值；页面提前拦住） */
export const MAX_BATCH_CELLS = 2000;

/** 状态枚举（落库 `1` / `0`，与 `ck_biz_msp_status` 一致） */
export const STOCK_POLICY_STATUS_OPTIONS = [
  { label: '启用', value: 1 },
  { label: '停用', value: 0 },
] as const;

/** 状态筛选下拉（值是**字符串**，与查询串同口径；空串 = 不筛） */
export const STOCK_POLICY_STATUS_FILTER_OPTIONS = [
  { label: '启用', value: '1' },
  { label: '停用', value: '0' },
] as const;

// ==================== 筛选 ====================

/** 列表筛选状态（`status` 是字符串：查询串口径） */
export interface StockPolicyFilterState {
  /** 关键字 */
  keyword: string;
  /** 物料 id（空 = 全部） */
  material_id: string;
  /** 仓库 id（空 = 全部） */
  warehouse_id: string;
  /** 状态（`''` / `'1'` / `'0'`） */
  status: string;
}

/** 空筛选（重置用） */
export function emptyStockPolicyFilter(): StockPolicyFilterState {
  return { keyword: '', material_id: '', warehouse_id: '', status: '' };
}

/** 是否处于筛选态（空态文案分「还没设过」与「没匹配」） */
export function isStockPolicyFiltered(state: StockPolicyFilterState): boolean {
  return Boolean(state.keyword.trim() || state.material_id || state.warehouse_id || state.status);
}

/**
 * 列表入参拼装：空串整键缺席（后端把 `?material_id=` 当非法 UUID → 400；
 * 把 `?keyword=` 当「匹配空串」→ 一条都不返回）。
 */
export function buildStockPolicyQuery(
  state: StockPolicyFilterState,
  page: number,
  pageSize: number,
): StockPolicyQuery {
  const query: StockPolicyQuery = {
    page: Math.max(1, Math.floor(page) || 1),
    page_size: Math.max(1, Math.floor(pageSize) || STOCK_POLICY_PAGE_SIZE),
  };
  const keyword = state.keyword.trim();
  if (keyword) {
    query.keyword = keyword;
  }
  if (state.material_id) {
    query.material_id = state.material_id;
  }
  if (state.warehouse_id) {
    query.warehouse_id = state.warehouse_id;
  }
  if (state.status) {
    query.status = state.status;
  }
  return query;
}

// ==================== 单格表单 ====================

/** 单格水位表单（字符串形态，直接绑 TDesign 输入框；`min_qty` 必填） */
export interface StockPolicyCellForm {
  /** 物料 id */
  material_id: string;
  /** 仓库 id */
  warehouse_id: string;
  /** 最低水位（字符串，可能是空串） */
  min_qty: string;
  /** 最高水位（字符串；空 = 未设） */
  max_qty: string;
  /** 状态（`1` / `0`） */
  status: number;
  /** 备注 */
  remark: string;
}

/** 新建表单的初值（物料 / 仓库由用户在抽屉里选） */
export function emptyStockPolicyCellForm(): StockPolicyCellForm {
  return { material_id: '', warehouse_id: '', min_qty: '', max_qty: '', status: 1, remark: '' };
}

/** 由列表行回填编辑表单（数量转字符串；`null` 最高水位 → 空串 = 未设） */
export function formFromStockPolicyRow(row: StockPolicyRow): StockPolicyCellForm {
  return {
    material_id: row.material_id,
    warehouse_id: row.warehouse_id,
    min_qty: qtyText(row.min_qty),
    max_qty: qtyText(row.max_qty),
    status: row.status === 0 ? 0 : 1,
    remark: row.remark ?? '',
  };
}

/** 一格的四列校验结果 */
export interface StockPolicyLevelError {
  /** 哪个字段错（`min_qty` / `max_qty`），整表单级问题为 `null` */
  field: 'min_qty' | 'max_qty' | null;
  /** 人话（页面直接显示在输入框下面 / 弹窗顶部） */
  message: string;
}

/**
 * 四列校验（纯逻辑，与后端 `checked_write` 逐条对应）。
 *
 * # 参数
 *
 * - `minText` / `maxText`：两个输入框原值（允许两端空白；空串 = 未填）；
 * - `status`：状态（缺省按启用）。
 *
 * # 返回值
 *
 * `null` = 通过；否则给出**哪个字段错**与一句人话。
 *
 * # 为什么不等后端 400
 *
 * 后端确实会 400，但它的文案是「min_qty 必须 >= 0」这类**字段名口径**，而用户在界面上
 * 看到的是「最低水位」四个字 —— 由页面指出是哪个框，比让他去对字段名快得多。
 */
export function validateStockPolicyLevels(
  minText: string,
  maxText: string,
  status: number,
): StockPolicyLevelError | null {
  if (status !== 0 && status !== 1) {
    return { field: null, message: '状态只能是「启用」或「停用」' };
  }
  const minTrim = minText.trim();
  if (!minTrim) {
    return { field: 'min_qty', message: '最低水位必填（填 0 = 这一格不预警）' };
  }
  const min = Number(minTrim);
  if (!Number.isFinite(min)) {
    return { field: 'min_qty', message: `最低水位应是数字（当前「${minTrim}」）` };
  }
  if (min < 0) {
    return { field: 'min_qty', message: '最低水位不能为负数（0 = 这一格不预警）' };
  }
  const maxTrim = maxText.trim();
  if (!maxTrim) {
    return null;
  }
  const max = Number(maxTrim);
  if (!Number.isFinite(max)) {
    return { field: 'max_qty', message: `最高水位应是数字（当前「${maxTrim}」）` };
  }
  if (max <= min) {
    return {
      field: 'max_qty',
      message: `最高水位（${maxTrim}）必须严格大于最低水位（${minTrim}）：相等就没有区间了；不想判超储请留空`,
    };
  }
  return null;
}

/** 四列校验通过后的提交载荷（数量按字符串发，后端 `Decimal` 直接吃） */
export function buildStockPolicySavePayload(
  form: StockPolicyCellForm,
): { material_id: string; warehouse_id: string; min_qty: string; max_qty: string | null; status: number; remark: string | null } {
  const maxText = form.max_qty.trim();
  const remark = form.remark.trim();
  return {
    material_id: form.material_id,
    warehouse_id: form.warehouse_id,
    min_qty: form.min_qty.trim(),
    max_qty: maxText ? maxText : null,
    status: form.status === 0 ? 0 : 1,
    remark: remark ? remark : null,
  };
}

// ==================== 批量设置 ====================

/** 批量设置表单（页面「批量设置」弹窗） */
export interface StockPolicyBatchForm {
  /** 选中的物料 id */
  material_ids: string[];
  /** 选中的仓库 id */
  warehouse_ids: string[];
  /** 最低水位 */
  min_qty: string;
  /** 最高水位（空 = 未设） */
  max_qty: string;
  /** 状态 */
  status: number;
  /** 备注（整批一份） */
  remark: string;
}

/** 批量表单初值 */
export function emptyStockPolicyBatchForm(): StockPolicyBatchForm {
  return { material_ids: [], warehouse_ids: [], min_qty: '', max_qty: '', status: 1, remark: '' };
}

/** 批量展开的预览结果 */
export interface StockPolicyBatchPreview {
  /** 去重后的物料 id（保持首次出现顺序） */
  materials: string[];
  /** 去重后的仓库 id */
  warehouses: string[];
  /** 铺开的格数 = 物料数 × 仓库数 */
  cells: number;
  /** 被去掉的重复物料数 */
  duplicateMaterials: number;
  /** 被去掉的重复仓库数 */
  duplicateWarehouses: number;
  /** 是否超过上限（超过时页面禁止提交） */
  overLimit: boolean;
}

/**
 * 批量展开预览（与后端 `BatchSavePolicyReq::to_plan` 同一口径）。
 *
 * # 为什么页面也要算一遍
 *
 * 用户点开「批量设置」时要**先看到**「将设置 12 格：3 个物料 × 4 个仓库」再确认 ——
 * 一批写错就是十几格水位同时错。算错也不要紧（后端仍会校验并 400），但如果页面算的数
 * 与后端不一致，用户就会看到「确认时说 12 格、结果说 10 格」，那才是真问题：
 * 两边口径由单测逐条对拍（重复、空、上限、顺序）。
 */
export function previewStockPolicyBatch(materialIds: string[], warehouseIds: string[]): StockPolicyBatchPreview {
  const materials: string[] = [];
  const seenMaterials = new Set<string>();
  for (const raw of materialIds) {
    const id = String(raw ?? '').trim();
    if (!id || seenMaterials.has(id)) {
      continue;
    }
    seenMaterials.add(id);
    materials.push(id);
  }
  const warehouses: string[] = [];
  const seenWarehouses = new Set<string>();
  for (const raw of warehouseIds) {
    const id = String(raw ?? '').trim();
    if (!id || seenWarehouses.has(id)) {
      continue;
    }
    seenWarehouses.add(id);
    warehouses.push(id);
  }
  const cells = materials.length * warehouses.length;
  return {
    materials,
    warehouses,
    cells,
    duplicateMaterials: materialIds.filter((raw) => String(raw ?? '').trim()).length - materials.length,
    duplicateWarehouses: warehouseIds.filter((raw) => String(raw ?? '').trim()).length - warehouses.length,
    overLimit: cells > MAX_BATCH_CELLS,
  };
}

/**
 * 批量设置的提交前校验（选品 + 四列 + 上限），返回人话错误或 `null`。
 *
 * # 参数
 *
 * - `form`：批量表单（可能一项都没选）；
 * - `hasSavePermission`：当前账号有没有 `…:stock-policy:save`（没有就别让他白填）。
 */
export function validateStockPolicyBatch(
  form: StockPolicyBatchForm,
  hasSavePermission = true,
): string | null {
  if (!hasSavePermission) {
    return `当前账号没有 ${PERM_STOCK_POLICY_SAVE} 权限，无法批量设置水位`;
  }
  if (!form.material_ids.length) {
    return '请至少选择一个物料';
  }
  if (!form.warehouse_ids.length) {
    return '请至少选择一个仓库';
  }
  const preview = previewStockPolicyBatch(form.material_ids, form.warehouse_ids);
  if (preview.overLimit) {
    return `一次最多批量设置 ${MAX_BATCH_CELLS} 格（当前 ${preview.materials.length} 物料 × ${preview.warehouses.length} 仓库 = ${preview.cells} 格）：请减少物料 / 仓库，或改用「导入」按 Excel 逐行设置`;
  }
  const levelError = validateStockPolicyLevels(form.min_qty, form.max_qty, form.status);
  if (levelError) {
    return levelError.message;
  }
  return null;
}

/** 批量提交载荷（去重后的 id 列表 + 四列） */
export function buildStockPolicyBatchPayload(form: StockPolicyBatchForm): StockPolicyBatchPayload {
  const preview = previewStockPolicyBatch(form.material_ids, form.warehouse_ids);
  const maxText = form.max_qty.trim();
  const remark = form.remark.trim();
  return {
    material_ids: preview.materials,
    warehouse_ids: preview.warehouses,
    min_qty: form.min_qty.trim(),
    max_qty: maxText ? maxText : null,
    status: form.status === 0 ? 0 : 1,
    remark: remark ? remark : null,
  };
}

/** 批量预览的一句话（弹窗里显示：将设置 N 格 / 去重提示 / 超限提示） */
export function batchPreviewText(preview: StockPolicyBatchPreview): string {
  if (!preview.cells) {
    return '请选择物料与仓库，这里会显示将设置多少格';
  }
  const base = `将设置 ${preview.cells} 格（${preview.materials.length} 个物料 × ${preview.warehouses.length} 个仓库）`;
  const dedupe: string[] = [];
  if (preview.duplicateMaterials > 0) {
    dedupe.push(`重复物料 ${preview.duplicateMaterials} 个`);
  }
  if (preview.duplicateWarehouses > 0) {
    dedupe.push(`重复仓库 ${preview.duplicateWarehouses} 个`);
  }
  const dedupeText = dedupe.length ? `；入参已去重（${dedupe.join('、')}）` : '';
  if (preview.overLimit) {
    return `${base}${dedupeText}；超过单次上限 ${MAX_BATCH_CELLS} 格，请减少后重试或改用「导入」`;
  }
  return `${base}${dedupeText}；已存在的格会被覆盖，不存在的格会新建`;
}

// ==================== 格式化 ====================

/** 数量出参（`Decimal` 可能是字符串）→ 数字；非法 / 空 → `0` */
export function toNumber(value: number | string | null | undefined): number {
  if (value === null || value === undefined || value === '') {
    return 0;
  }
  const parsed = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

/**
 * 数量的**可编辑文本**（回填表单用）：去掉尾零，但保留整数形态。
 *
 * `"100.0000"` → `"100"`、`"100.5000"` → `"100.5"`、`0` → `"0"`；
 * `null`（未设）→ `""`（表单里显示为空白 = 未设，而不是 `0` —— 这两件事语义不同）。
 */
export function qtyText(value: number | string | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '';
  }
  const text = String(value).trim();
  if (!text.includes('.')) {
    return text;
  }
  const trimmed = text.replace(/0+$/, '').replace(/\.$/, '');
  return trimmed === '' || trimmed === '-' ? '0' : trimmed;
}

/** 数量的展示文本：`null` / 空 → `—`（未设），否则千分位 + 去尾零 */
export function formatQty(value: number | string | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '—';
  }
  const parsed = toNumber(value);
  const text = qtyText(parsed);
  const [integer, fraction] = text.split('.');
  const grouped = integer.replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  return fraction ? `${grouped}.${fraction}` : grouped;
}

/** 最低水位展示：`0` 要显式写成「0（不预警）」—— 0 与「没设」在页面上必须能区分 */
export function minQtyText(value: number | string | null | undefined): string {
  if (toNumber(value) === 0) {
    return '0（不预警）';
  }
  return formatQty(value);
}

/** 状态文案 */
export function statusLabel(status: number): string {
  return status === 0 ? '停用' : '启用';
}

/** 状态主题（TDesign tag theme） */
export function statusTheme(status: number): 'success' | 'default' {
  return status === 0 ? 'default' : 'success';
}

/** 状态的语义说明（停用 = 保留数值但退回按物料级 safety_stock 判） */
export function statusHint(status: number): string {
  return status === 0
    ? '停用：保留设置数值，但这一格退回按物料级「安全库存」判水位'
    : '启用：这一格按本行水位判（0 = 明示不预警）';
}

/** 物料展示文本（`编码 名称`；主数据被删时给一句人话而不是空白） */
export function materialText(row: Pick<StockPolicyRow, 'material_code' | 'material_name'>): string {
  const code = row.material_code ?? '';
  const name = row.material_name ?? '';
  if (!code && !name) {
    return '物料已删除';
  }
  return [code, name].filter(Boolean).join(' ');
}

/** 仓库展示文本 */
export function warehouseText(row: Pick<StockPolicyRow, 'warehouse_code' | 'warehouse_name'>): string {
  const code = row.warehouse_code ?? '';
  const name = row.warehouse_name ?? '';
  if (!code && !name) {
    return '仓库已删除';
  }
  return [code, name].filter(Boolean).join(' ');
}

/** 行键（`物料 × 仓库` 一格一行；用设置行 id 更稳，因为软删后可重建） */
export function rowKey(row: Pick<StockPolicyRow, 'id'>): string {
  return row.id;
}

/** 列表空态文案（分「还没设过」与「筛选没匹配」） */
export function emptyText(filtered: boolean): string {
  return filtered
    ? '当前筛选条件下没有水位设置：清空筛选看看，或直接「新建水位」'
    : '还没有任何水位设置：可以「新建水位」逐格设置、「批量设置」一次铺开多格，或「导入」Excel';
}

/** 请求错误 → 一句人话（`ApiError` 已经带后端文案，其它错误给通用兜底 + 原文） */
export function requestErrorMessage(error: unknown): string {
  if (error instanceof Error && error.message) {
    return error.message;
  }
  return '请求失败：请稍后重试（若反复失败请把浏览器控制台的请求响应发给管理员）';
}

/** 导入入口的说明（页面提示条用） */
export function importHint(): string {
  return `支持 Excel 批量导入（biz_type=${STOCK_POLICY_IMPORT_BIZ_TYPE}）：先下载模板，按「物料编码 + 仓库编码 + 最低水位」逐行填写，第 2 行示例会被自动跳过；同一格重导是覆盖，不会产生第二行。`;
}
