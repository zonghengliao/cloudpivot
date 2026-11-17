/**
 * 采购计划页面纯逻辑（M4-06：录入 + 执行率，无 Vue 依赖，可被 `tests/unit` 直跑）。
 *
 * # 为什么单独一个 ts 文件
 *
 * 与 `purchase-requisition-logic.ts` 同一套理由：`index.vue` 只负责画画，
 * 取数入参 / 表单校验 / 展示口径全放这里 —— `vue-tsc` 对 `.ts` 更严，口径写错当场红；
 * 而这里每条判据都能在 `pnpm test:unit` 里钉住（不用起浏览器、不用连库）。
 *
 * # 十条口径（每条都对得上后端一行代码，改一处必须两处一起改）
 *
 * ```text
 * ① 金额 / 数量一律**字符串**提交（后端 Decimal）；本地只做「预览值」，不与服务端争真相。
 * ② 类型 × 月份自洽：ANNUAL ⇒ plan_month 必须空；MONTHLY ⇒ 1–12。
 *    （后端 check_month_for_type；库层 ck_biz_pp_period 兜底）
 * ③ 月度计划的父单必须是**同年度**的**年度计划**（后端 check_parent_period）。
 * ④ 粒度 = 物料 × 仓库：一格里重复两行 → 请求层 400 并点出「第 3 行与第 1 行」，
 *    前端先拦一次，文案跟后端同口径（不让用户等一次 400 才知道哪两行撞了）。
 * ⑤ 计划量 > 0、单价 ≥ 0；行金额 = qty × price 4 位舍入（后端 line_amount）。
 * ⑥ 明细 1–500 行（后端 MAX_LINES = 500，比申请单的 200 宽）。
 * ⑦ 只有草稿能改 / 删 / 提交 / 转申请 —— 判据用后端给的 `editable` + `status`，
 *    前端不自己推状态机（状态机改了三处都跟着变）。
 * ⑧ 转申请「一仓一单」：本次转出量 ≤ 计划量 − 已执行量（后端按剩余量卡 400）。
 * ⑨ 计划行单价为 0 时，转申请必须逐行给 `est_price > 0`（0 元等于不占预算，后端拒）。
 * ⑩ 执行率**状态不要用 pct 反推**：后端允许 `exec_rate_pct = 100.00` 但状态 PARTIAL
 *    （率舍 2 位、状态用未舍入量判）。所以展示 pct、判状态读 `exec_state` / `over_plan`。
 * ```
 */
import type {
  ConvertLinePayload,
  ConvertedRequisition,
  DecimalText,
  ExecutionFilter,
  ExecState,
  PlanDetail,
  PlanItem,
  PlanItemPayload,
  PlanPayload,
  PlanExecRow,
  PlanFilter,
  PlanRow,
  PlanStatus,
  PlanType,
  ToRequisitionPayload,
} from '@/api/business/erp/purchase-plan';

/** 列表默认每页条数 */
export const PAGE_SIZE = 20;
/** 单张计划的明细上限（与后端 `purchase_plan_dto::MAX_LINES` 一致） */
export const MAX_LINES = 500;
/** 计划名称长度上限（与 `biz_purchase_plan.plan_name VARCHAR(128)` 一致） */
export const MAX_NAME = 128;
/** 备注长度上限（与后端 `MAX_REMARK_LEN` 一致） */
export const MAX_REMARK = 500;
/** 计划年度下界（与 `ck_biz_pp_year` 一致） */
export const YEAR_MIN = 1990;
/** 计划年度上界（与 `ck_biz_pp_year` 一致） */
export const YEAR_MAX = 2200;
/** 执行率下限筛选的合法上界（与后端 `MAX_MIN_RATE` 一致，单位百分比） */
export const MAX_MIN_RATE = 1000;

/** 月份下拉（1–12） */
export const MONTH_OPTIONS: { label: string; value: number }[] = Array.from({ length: 12 }, (_, index) => ({
  label: `${index + 1} 月`,
  value: index + 1,
}));

/** 执行率下限下拉（百分比；空 = 不限） */
export const MIN_RATE_OPTIONS: { label: string; value: number }[] = [
  { label: '不下限', value: 0 },
  { label: '≥ 50%', value: 50 },
  { label: '≥ 80%', value: 80 },
  { label: '≥ 100%（已下满 / 超计划）', value: 100 },
];

// ==================== 数值工具（只做展示 / 入参换算） ====================

/** `t-input-number` / 后端字符串 → number（`null` = 没填，非法也给 `null`） */
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

/**
 * 按位数取整的**整数标度**（4 位 = ×10000）。
 *
 * 用途：数量比较（`qty ≤ 剩余量`）与格式化成字符串提交。
 * 不用浮点直接比，是为了 `0.1 + 0.2` 那类尾差把「恰好等于剩余量」判成超量。
 */
export function scale(value: number | string, dp: number): number {
  const parsed = toNumberOrNull(value) ?? 0;
  return Math.round(parsed * 10 ** dp);
}

/** 标度整数 → 定点字符串（`1.5` + 4 位 → `"1.5000"`） */
export function fixedText(value: number | string | null | undefined, dp: number): string {
  const scaled = scale(value ?? 0, dp);
  return (scaled / 10 ** dp).toFixed(dp);
}

// ==================== 状态 / 标签 / 展示（唯一一份，页面与单测都用这里） ====================
//
// 为什么不放 `api/business/erp/purchase-plan.ts`：那个文件运行时 import `@/api/request`，
// 而 `request.ts` 顶层读 `import.meta.env` —— Node 里跑 `pnpm test:unit` 会直接炸
// （`Cannot read properties of undefined (reading 'VITE_APP_BASE_API')`）。
// 所以「展示件」全部留在本文件，api 层只放类型与取数函数。

/** 状态下拉（顺序与后端 `STATUSES` 一致） */
export const STATUS_OPTIONS: { value: PlanStatus; label: string }[] = [
  { value: 'DRAFT', label: '草稿' },
  { value: 'SUBMITTED', label: '审批中' },
  { value: 'APPROVED', label: '已批准' },
];

/** 类型下拉（顺序与后端 `PLAN_TYPES` 一致） */
export const PLAN_TYPE_OPTIONS: { value: PlanType; label: string }[] = [
  { value: 'ANNUAL', label: '年度计划' },
  { value: 'MONTHLY', label: '月度计划' },
];

/** 状态中文（未知值原样返回，不吞信息） */
export function statusLabel(status: string): string {
  return STATUS_OPTIONS.find((item) => item.value === status)?.label ?? status;
}

/** 状态主题（t-tag） */
export function statusTheme(status: string): 'default' | 'primary' | 'success' | 'warning' {
  switch (status) {
    case 'DRAFT':
      return 'default';
    case 'SUBMITTED':
      return 'warning';
    case 'APPROVED':
      return 'success';
    default:
      return 'default';
  }
}

/** 类型中文（未知值原样返回） */
export function planTypeLabel(planType: string): string {
  return PLAN_TYPE_OPTIONS.find((item) => item.value === planType)?.label ?? planType;
}

/** 执行率行状态主题（NOT_STARTED / PARTIAL / DONE / OVER；与 V0449 视图 `exec_state` 同集） */
export function execStateTheme(state: ExecState | string): 'default' | 'warning' | 'success' | 'danger' {
  switch (state) {
    case 'NOT_STARTED':
      return 'default';
    case 'PARTIAL':
      return 'warning';
    case 'DONE':
      return 'success';
    case 'OVER':
      return 'danger';
    default:
      return 'default';
  }
}

/**
 * 进度条状态色（TDesign `t-progress` 的 `status`，**不是** `theme`）。
 *
 * `theme` 是形状（line / plump / circle），颜色只能走 `status` 或 `color`。
 * 超计划红（error）、下满绿（success）、部分下单蓝（active）、未开工不给（默认灰）。
 */
export function progressStatus(state: ExecState | string): 'success' | 'error' | 'active' | undefined {
  switch (state) {
    case 'DONE':
      return 'success';
    case 'OVER':
      return 'error';
    case 'PARTIAL':
      return 'active';
    default:
      return undefined;
  }
}

/** 数量 / 金额展示（空值给 `—`，**不**做千分位：单据口径原样看） */
export function decimalText(value: DecimalText | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '—';
  }
  return String(value);
}

/** 行金额预览 = qty × price，4 位舍入（与后端 `line_amount` 同口径，仅用于输入时看数） */
export function lineAmountText(qty: number | string, price: number | string): string {
  const qtyScaled = scale(qty, 4);
  const priceScaled = scale(price, 6);
  // 4 位 × 6 位 = 10 位标度；再舍到 4 位（与 Decimal::round_dp(4) 同向）
  const amountScaled = Math.round((qtyScaled * priceScaled) / 10 ** 6);
  return (amountScaled / 10 ** 4).toFixed(4);
}

/** 期间文案：「2026 年」/「2026 年 11 月」 */
export function periodText(row: { plan_year: number; plan_month: number | null }): string {
  return row.plan_month ? `${row.plan_year} 年 ${row.plan_month} 月` : `${row.plan_year} 年`;
}

// ==================== 录入：筛选 ====================

/** 计划列表筛选态 */
export interface FilterState {
  /** 关键字：单号 / 名称 / 部门 / 备注 */
  keyword: string;
  /** 状态 */
  status: PlanStatus | '';
  /** 类型 */
  plan_type: PlanType | '';
  /** 年度（`null` = 全部） */
  plan_year: number | null;
  /** 编制部门 */
  dept_id: string;
}

/** 空筛选 */
export function emptyFilter(): FilterState {
  return { keyword: '', status: '', plan_type: '', plan_year: null, dept_id: '' };
}

/** 是否处于筛选态（决定空列表文案） */
export function isFiltered(filter: FilterState): boolean {
  return Boolean(
    filter.keyword.trim() || filter.status || filter.plan_type || filter.plan_year || filter.dept_id,
  );
}

/**
 * 筛选态 → 分页入参（空串 / null 一律**不出现在对象里**）。
 *
 * 不写 `keyword: undefined` 的原因有两个：后端把 `?keyword=` 当「等于空串」过滤会查出 0 行；
 * 而 `{ keyword: undefined }` 在 `deepEqual({...}, { page, page_size })` 里仍算一个键 ——
 * 单测钉不住「真的没传」。
 */
export function buildQuery(filter: FilterState, page: number, pageSize = PAGE_SIZE): PlanFilter {
  const query: PlanFilter = { page, page_size: pageSize };
  const keyword = filter.keyword.trim();
  if (keyword) {
    query.keyword = keyword;
  }
  if (filter.status) {
    query.status = filter.status;
  }
  if (filter.plan_type) {
    query.plan_type = filter.plan_type;
  }
  if (filter.plan_year !== null && filter.plan_year !== undefined) {
    query.plan_year = filter.plan_year;
  }
  if (filter.dept_id) {
    query.dept_id = filter.dept_id;
  }
  return query;
}

// ==================== 录入：表单 ====================

/** 明细行草稿（数值字段用 `number | string`，与 `t-input-number` 的 modelValue 对齐） */
export interface PlanFormLine {
  /** 行内唯一键（表格 key，不用下标：删中间行时下标会串） */
  key: number;
  /** 物料 */
  material_id: string;
  /** 仓库 */
  warehouse_id: string;
  /** 计划量 */
  planned_qty: number | string;
  /** 预估不含税单价 */
  planned_price: number | string;
  /** 行备注 */
  remark: string;
}

/** 建单 / 改单表单草稿 */
export interface PlanForm {
  /** 改单时的计划 id（新建为空串） */
  id: string;
  /** 计划名称 */
  plan_name: string;
  /** 类型 */
  plan_type: PlanType;
  /** 年度 */
  plan_year: number | string;
  /** 月份（年度计划为 `null`） */
  plan_month: number | null;
  /** 父年度计划 id（仅月度） */
  parent_plan_id: string;
  /** 编制部门 */
  dept_id: string;
  /** 编制人 */
  planner_id: string;
  /** 备注 */
  remark: string;
  /** 明细 */
  lines: PlanFormLine[];
}

let lineSeed = 1;

/** 新的一行明细（计划量缺省 1，单价空） */
export function newPlanLine(): PlanFormLine {
  lineSeed += 1;
  return { key: lineSeed, material_id: '', warehouse_id: '', planned_qty: 1, planned_price: '', remark: '' };
}

/**
 * 北京时间（CST）的「现在」。
 *
 * 后端所有「今天」都走 `today_cst()`（UTC+8）；浏览器可能在任何时区，
 * 直接 `new Date().getFullYear()` 在月末 23:00 之后会差一天 / 一年。
 */
export function nowCst(): Date {
  const now = new Date();
  return new Date(now.getTime() + (8 * 60 + now.getTimezoneOffset()) * 60_000);
}

/** 当前年度（北京时间；与后端 `today_cst().year()` 同口径） */
export function currentYear(): number {
  return nowCst().getFullYear();
}

/** 当前月份 1–12（北京时间） */
export function currentMonth(): number {
  return nowCst().getMonth() + 1;
}

/** 空表单（默认：年度计划 + 今年 + 一行明细） */
export function emptyPlanForm(year = currentYear()): PlanForm {
  return {
    id: '',
    plan_name: '',
    plan_type: 'ANNUAL',
    plan_year: year,
    plan_month: null,
    parent_plan_id: '',
    dept_id: '',
    planner_id: '',
    remark: '',
    lines: [newPlanLine()],
  };
}

/** 详情 → 表单（改单；只有草稿能进来，但这里不判状态） */
export function formFromDetail(detail: PlanDetail): PlanForm {
  return {
    id: detail.plan.id,
    plan_name: detail.plan.plan_name,
    plan_type: detail.plan.plan_type,
    plan_year: detail.plan.plan_year,
    plan_month: detail.plan.plan_month,
    parent_plan_id: detail.plan.parent_plan_id ?? '',
    dept_id: detail.plan.dept_id ?? '',
    planner_id: detail.plan.planner_id ?? '',
    remark: detail.plan.remark ?? '',
    lines: detail.items.map((item: PlanItem) => {
      lineSeed += 1;
      return {
        key: lineSeed,
        material_id: item.material_id,
        warehouse_id: item.warehouse_id,
        planned_qty: String(item.planned_qty),
        planned_price: String(item.planned_price),
        remark: item.remark ?? '',
      };
    }),
  };
}

/** 类型切换时把月份归位（年度 → `null`；月度 → 缺省当前月） */
export function monthForType(planType: PlanType, current: number | null): number | null {
  if (planType === 'ANNUAL') {
    return null;
  }
  if (current && current >= 1 && current <= 12) {
    return current;
  }
  return currentMonth();
}

/** 父亲候选（只列**同年度**的**年度**计划：跨年挂父单后端 400） */
export function parentCandidates<T extends { id: string; plan_type: string; plan_year: number }>(
  plans: readonly T[],
  planYear: number,
  selfId = '',
): T[] {
  return plans.filter(
    (plan) => plan.plan_type === 'ANNUAL' && plan.plan_year === planYear && plan.id !== selfId,
  );
}

/**
 * 表单校验（返回第一条错误，`null` = 通过）。
 *
 * `annualPlans` 给了就顺手校验「父单必须是同年度年度计划」（后端 `check_parent_period`）；
 * 不给（比如父单下拉还没加载）就跳过这一条 —— 后端仍然会拦。
 */
export function validatePlanForm(form: PlanForm, annualPlans?: readonly PlanRow[]): string | null {
  const name = form.plan_name.trim();
  if (!name) {
    return '计划名称不能为空';
  }
  if (name.length > MAX_NAME) {
    return `计划名称最长 ${MAX_NAME} 个字符（当前 ${name.length}）`;
  }

  const year = toNumberOrNull(form.plan_year);
  if (year === null || !Number.isInteger(year)) {
    return '计划年度必须是整数年份';
  }
  if (year < YEAR_MIN || year > YEAR_MAX) {
    return `计划年度 ${year} 超范围（${YEAR_MIN}–${YEAR_MAX}）`;
  }

  if (form.plan_type === 'ANNUAL') {
    if (form.plan_month !== null && form.plan_month !== undefined) {
      return `年度计划不能带月份（收到 ${form.plan_month}）：要编某个月请选「月度计划」`;
    }
  } else {
    const month = form.plan_month;
    if (month === null || month === undefined || !Number.isInteger(month) || month < 1 || month > 12) {
      return `月度计划必须选月份（1–12）`;
    }
  }

  if (form.parent_plan_id && form.plan_type !== 'MONTHLY') {
    return '只有月度计划能挂父年度计划';
  }
  if (form.parent_plan_id && annualPlans) {
    const parent = annualPlans.find((plan) => plan.id === form.parent_plan_id);
    if (!parent) {
      return '父年度计划不存在或已删除，请重新选择';
    }
    if (parent.plan_type !== 'ANNUAL') {
      return '父计划必须是**年度**计划（月度计划不能挂月度计划）';
    }
    if (parent.plan_year !== year) {
      return `月度计划的年度（${year}）与父年度计划（${parent.plan_year}）不一致`;
    }
  }

  if (form.remark.length > MAX_REMARK) {
    return `备注最长 ${MAX_REMARK} 个字符（当前 ${form.remark.length}）`;
  }

  if (form.lines.length === 0) {
    return '明细不能为空：计划至少要有 1 行物料';
  }
  if (form.lines.length > MAX_LINES) {
    return `明细最多 ${MAX_LINES} 行（当前 ${form.lines.length} 行），请拆成多张计划单`;
  }

  /** (material, warehouse) → 第一次出现的行号（与后端 `check_cells_unique` 同口径） */
  const seen = new Map<string, number>();
  for (const [index, line] of form.lines.entries()) {
    const lineNo = index + 1;
    if (!line.material_id) {
      return `第 ${lineNo} 行还没选物料`;
    }
    if (!line.warehouse_id) {
      return `第 ${lineNo} 行还没选仓库（粒度是「物料 × 仓库」，仓库必填）`;
    }
    const qty = toNumberOrNull(line.planned_qty);
    if (qty === null) {
      return `第 ${lineNo} 行的计划量不能为空`;
    }
    if (scale(qty, 4) <= 0) {
      return `第 ${lineNo} 行的计划量必须 > 0（收到 ${qty}）`;
    }
    const price = toNumberOrNull(line.planned_price) ?? 0;
    if (price < 0) {
      return `第 ${lineNo} 行的预估单价不能为负（收到 ${price}）`;
    }
    const cell = `${line.material_id}::${line.warehouse_id}`;
    const first = seen.get(cell);
    if (first !== undefined) {
      return `第 ${lineNo} 行与第 ${first} 行是同一个「物料 × 仓库」：同一张计划里一个物料在一个仓库只能有一行（要分仓库请拆成多行）`;
    }
    seen.set(cell, lineNo);
  }
  return null;
}

/** 表单 → 建单 / 改单入参（数值一律字符串，空值给 `null` 或不传） */
export function buildPlanPayload(form: PlanForm): PlanPayload {
  const items: PlanItemPayload[] = form.lines.map((line) => ({
    material_id: line.material_id,
    warehouse_id: line.warehouse_id,
    planned_qty: fixedText(line.planned_qty, 4),
    planned_price: fixedText(line.planned_price ?? 0, 6),
    remark: line.remark.trim() || null,
  }));
  return {
    plan_name: form.plan_name.trim(),
    plan_type: form.plan_type,
    plan_year: Number(form.plan_year),
    plan_month: form.plan_type === 'MONTHLY' ? form.plan_month : null,
    parent_plan_id: form.parent_plan_id || null,
    dept_id: form.dept_id || null,
    planner_id: form.planner_id || null,
    remark: form.remark.trim() || null,
    items,
  };
}

/** 表单预览合计（行数与金额；**只用于页面上显示预计金额**，服务端会重算） */
export function formTotals(form: PlanForm): { lines: number; qty: string; amount: string } {
  let qtyScaled = 0;
  let amountScaled = 0;
  form.lines.forEach((line) => {
    qtyScaled += scale(line.planned_qty, 4);
    amountScaled += Math.round((scale(line.planned_qty, 4) * scale(line.planned_price, 6)) / 10 ** 6);
  });
  return {
    lines: form.lines.length,
    qty: (qtyScaled / 10 ** 4).toFixed(4),
    amount: (amountScaled / 10 ** 4).toFixed(4),
  };
}

// ==================== 执行率 ====================

/** 执行率筛选态 */
export interface ExecFilterState {
  /** 关键字 */
  keyword: string;
  /** 计划状态 */
  status: PlanStatus | '';
  /** 类型 */
  plan_type: PlanType | '';
  /** 年度 */
  plan_year: number | null;
  /** 月份 */
  plan_month: number | null;
  /** 编制部门 */
  dept_id: string;
  /** 执行率下限（百分比；`null` = 不限） */
  min_rate: number | null;
}

/** 空执行率筛选 */
export function emptyExecFilter(): ExecFilterState {
  return { keyword: '', status: '', plan_type: '', plan_year: null, plan_month: null, dept_id: '', min_rate: null };
}

/** 是否处于筛选态 */
export function isExecFiltered(filter: ExecFilterState): boolean {
  return Boolean(
    filter.keyword.trim() ||
      filter.status ||
      filter.plan_type ||
      filter.plan_year ||
      filter.plan_month ||
      filter.dept_id ||
      filter.min_rate,
  );
}

/** 执行率筛选 → 入参（空串 / null 同样不出现在对象里；`min_rate = 0` 与「不限」等价） */
export function buildExecQuery(filter: ExecFilterState, page: number, pageSize = PAGE_SIZE): ExecutionFilter {
  const query: ExecutionFilter = { page, page_size: pageSize };
  const keyword = filter.keyword.trim();
  if (keyword) {
    query.keyword = keyword;
  }
  if (filter.status) {
    query.status = filter.status;
  }
  if (filter.plan_type) {
    query.plan_type = filter.plan_type;
  }
  if (filter.plan_year !== null && filter.plan_year !== undefined) {
    query.plan_year = filter.plan_year;
  }
  if (filter.plan_month !== null && filter.plan_month !== undefined) {
    query.plan_month = filter.plan_month;
  }
  if (filter.dept_id) {
    query.dept_id = filter.dept_id;
  }
  if (filter.min_rate) {
    query.min_rate = filter.min_rate;
  }
  return query;
}

/**
 * 执行率百分比文案（`40.00` → `40.00%`；`null` → `—`）。
 *
 * 后端已按 2 位舍入给 `exec_rate_pct`，这里**只加百分号**，不再四舍五入，
 * 免得「页面上 40.00%、后端记 0.3999996」这种两份口径。
 */
export function pctText(pct: DecimalText | null | undefined): string {
  if (pct === null || pct === undefined || pct === '') {
    return '—';
  }
  return `${pct}%`;
}

/**
 * 进度条百分比（0–100 的 number）。
 *
 * **超计划（OVER）也封顶 100**：`t-progress` 超过 100 会画出界；真实倍数在文案里
 * （`pctText` 给 150.00%），状态标签由 `exec_state` 给 —— 不用进度条表达「超了」。
 */
export function progressPercent(pct: DecimalText | null | undefined): number {
  const value = toNumberOrNull(pct);
  if (value === null || value <= 0) {
    return 0;
  }
  return Math.min(100, Math.round(value * 100) / 100);
}

/** 行状态计数文案（`未开工 3 · 部分 2 · 下满 1 · 超 0`） */
export function lineCountsText(row: {
  not_started_lines: number;
  partial_lines: number;
  done_lines: number;
  over_lines: number;
}): string {
  return `未开工 ${row.not_started_lines} · 部分 ${row.partial_lines} · 下满 ${row.done_lines} · 超 ${row.over_lines}`;
}

/**
 * 剩余量文案（**负数明确写成「超 X」**）。
 *
 * 后端 `remaining_qty` 刻意保留负数（超计划要能看出来）；直接显示 `-15` 容易被
 * 读成「倒欠」，所以负值一律给「超 15」。
 */
export function remainingText(value: DecimalText | null | undefined): string {
  const parsed = toNumberOrNull(value);
  if (parsed === null) {
    return '—';
  }
  if (parsed < 0) {
    return `超 ${Math.abs(parsed)}`;
  }
  return String(value);
}

/** 执行率总览行的「主键」：后端执行率出参叫 `plan_id`（不是 `id`） */
export function execRowKey(row: PlanExecRow): string {
  return row.plan_id;
}

// ==================== 转采购申请 ====================

/** 转申请的一行草稿 */
export interface ConvertLineDraft {
  /** 计划明细 id */
  plan_item_id: string;
  /** 行号（报文用） */
  line_no: number;
  /** 物料编码 */
  material_code: string;
  /** 物料名称 */
  material_name: string;
  /** 仓库名 */
  warehouse_name: string;
  /** 计划量（原样展示） */
  planned_qty: DecimalText;
  /** 已执行量（回写列） */
  executed_qty: DecimalText;
  /** 剩余可转量（4 位；后端口径 = 计划量 − 已执行量） */
  remaining: string;
  /** 是否勾选转出 */
  selected: boolean;
  /** 本次转出数量 */
  qty: number | string;
  /** 预估单价（计划行为 0 时必须在这里覆盖） */
  est_price: number | string;
}

/** 转申请表单 */
export interface ConvertForm {
  /** 计划 id */
  plan_id: string;
  /** 申请部门 */
  dept_id: string;
  /** 申请日期（空 = 今天） */
  req_date: string;
  /** 需求日期 */
  demand_date: string;
  /** 税率（0–100） */
  tax_rate: number | string;
  /** 备注 */
  remark: string;
  /** 明细 */
  lines: ConvertLineDraft[];
}

/** 计划行剩余可转量（4 位标度整数，负数按 0 —— 与后端 `ConvertibleLine::remaining` 同口径） */
export function remainingScaled(planned: DecimalText, booked: DecimalText): number {
  const remaining = scale(planned, 4) - scale(booked, 4);
  return remaining > 0 ? remaining : 0;
}

/** 详情 → 转申请表单（逐行缺省转剩余量；单价 0 的行留空等用户填） */
export function emptyConvertForm(detail: PlanDetail): ConvertForm {
  return {
    plan_id: detail.plan.id,
    dept_id: detail.plan.dept_id ?? '',
    req_date: '',
    demand_date: '',
    tax_rate: 0,
    remark: '',
    lines: detail.items.map((item) => {
      const remaining = remainingScaled(item.planned_qty, item.executed_qty);
      const price = toNumberOrNull(item.planned_price) ?? 0;
      return {
        plan_item_id: item.id,
        line_no: item.line_no,
        material_code: item.material_code,
        material_name: item.material_name,
        warehouse_name: item.warehouse_name,
        planned_qty: item.planned_qty,
        executed_qty: item.executed_qty,
        remaining: (remaining / 10 ** 4).toFixed(4),
        selected: remaining > 0,
        qty: (remaining / 10 ** 4).toFixed(4),
        est_price: price > 0 ? String(item.planned_price) : '',
      };
    }),
  };
}

/**
 * 转申请校验（`null` = 通过）。
 *
 * 三条硬口径（后端会再判一次，这里提前给人话）：
 * ① 公司级计划（没有编制部门）必须显式选申请部门；
 * ② 至少要有一行「勾选且数量 > 0」；
 * ③ 每行本次转出量 ≤ 剩余可转量；单价必须 > 0。
 */
export function validateConvert(form: ConvertForm, companyLevel: boolean): string | null {
  if (companyLevel && !form.dept_id) {
    return '这张计划是公司级（没有编制部门），转申请必须选一个申请部门';
  }
  const tax = toNumberOrNull(form.tax_rate);
  if (tax === null || tax < 0 || tax > 100) {
    return `税率必须在 0–100 之间（收到 ${form.tax_rate}）`;
  }
  if (form.remark.length > MAX_REMARK) {
    return `备注最长 ${MAX_REMARK} 个字符（当前 ${form.remark.length}）`;
  }

  const picked = form.lines.filter((line) => line.selected);
  if (picked.length === 0) {
    return '至少勾选一行要转出的明细';
  }
  for (const line of picked) {
    const qty = scale(line.qty, 4);
    if (qty <= 0) {
      return `第 ${line.line_no} 行的转出数量必须 > 0`;
    }
    const remaining = scale(line.remaining, 4);
    if (qty > remaining) {
      return `第 ${line.line_no} 行本次转出 ${(qty / 10 ** 4).toFixed(4)} 超过剩余可转量 ${
        line.remaining
      }（临时加量请直接新建采购申请单，计划是基准）`;
    }
    const price = scale(line.est_price, 6);
    if (price <= 0) {
      return `第 ${line.line_no} 行的预估单价必须 > 0（0 元等于不占预算，后端会拒）`;
    }
  }
  return null;
}

/** 转申请入参（只送勾选的行；数量 / 单价按 4 / 6 位定点字符串） */
export function buildConvertPayload(form: ConvertForm): ToRequisitionPayload {
  const items: ConvertLinePayload[] = form.lines
    .filter((line) => line.selected)
    .map((line) => ({
      plan_item_id: line.plan_item_id,
      qty: fixedText(line.qty, 4),
      est_price: fixedText(line.est_price, 6),
    }));
  return {
    dept_id: form.dept_id || null,
    req_date: form.req_date || null,
    demand_date: form.demand_date || null,
    tax_rate: fixedText(form.tax_rate ?? 0, 4),
    remark: form.remark.trim() || null,
    items,
  };
}

/** 转申请预览合计（行数 / 数量 / 金额；金额 = Σ qty × price 4 位舍入） */
export function convertTotals(form: ConvertForm): { lines: number; qty: string; amount: string } {
  let lines = 0;
  let qtyScaled = 0;
  let amountScaled = 0;
  form.lines
    .filter((line) => line.selected)
    .forEach((line) => {
      lines += 1;
      qtyScaled += scale(line.qty, 4);
      amountScaled += Math.round((scale(line.qty, 4) * scale(line.est_price, 6)) / 10 ** 6);
    });
  return {
    lines,
    qty: (qtyScaled / 10 ** 4).toFixed(4),
    amount: (amountScaled / 10 ** 4).toFixed(4),
  };
}

/** 转申请结果的一句话（N 张申请单：单号 + 仓库） */
export function convertedSummaryText(requisitions: readonly ConvertedRequisition[]): string {
  if (requisitions.length === 0) {
    return '没有生成申请单';
  }
  return requisitions.map((req) => `${req.req_no}（${req.warehouse_name}）`).join(' / ');
}

/** 空列表文案（区分「没数据」与「筛掉了」） */
export function emptyListText(filtered: boolean): string {
  return filtered
    ? '没有符合筛选条件的采购计划：清一下筛选条件再试'
    : '还没有采购计划：点右上角「新建计划」按年度 / 月度编物料需求';
}
