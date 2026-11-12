/**
 * 薪资单详情页的出参契约与词表（ARCH-9 §5.2 #37/#38/#41/#43/#44/#45 · §6.2.5 · HR-43）。
 *
 * 纯逻辑文件：**不 import 请求层、不 import Vue**，`tsx --test` 里可以直接 import 做单测
 * （`tests/unit/hr-salary-detail.test.ts` 会读 Rust 源码把词表 / 上限 / 权限码对拍）。
 * 请求函数在 `./salary.ts`，那里 `export * from './salary-kinds'`，
 * 页面只认 `@/api/business/hr/salary` 一个入口。
 *
 * # 字段名逐条抄自后端，不缩写
 *
 * | 块 | 后端出处 | 形状 |
 * |---|---|---|
 * | 单头全列 | `salary_sheet_dto.rs::SalarySheetHeaderRes` | {@link SalarySheetHeader} |
 * | 合计 / 部门汇总 / 对账 | `SalaryTotalsRes` · `SalaryDeptSummaryRes` · `SalaryMismatchRes` | {@link SalarySheetSummary} |
 * | 明细行（一人一行） | `salary_sheet_dto.rs::SalaryDetailRes` | {@link SalaryDetailRow} |
 * | 明细项（展开后 17 项） | `salary_sheet_dto.rs::SalaryItemLineRes` | {@link SalaryItemLine} |
 * | 提交 / 过账 / 红冲回执 | `SalarySheetCardRes` | {@link SalarySheetCard} |
 * | 改单项金额回执 | `salary_override_dto.rs::SalaryOverrideRes` | {@link SalaryOverrideResult} |
 *
 * # 六处与「想当然」不一样的地方（先读，都会让页面显示错值或点出错按钮）
 *
 * 1. **金额是字符串**（`Decimal` → JSON `"9154.03"`）：不许用 `+` 累加，一律过
 *    `money.ts::formatMoney` / `toCents`（整数分比较）。部门汇总合计与单头对拍必须走分，
 *    浮点比会在 `226.00000000000003` 这种地方假报警。
 * 2. **试算 / 删除 / 作废三个动作后端没有路由**（设计 §6.2.5 的按钮态表写了，但 §5.2 的
 *    46 条接口里没有）：薪资单的状态机只有 6 个值、**没有 `CANCELLED`**
 *    （`ck_biz_sal_sheet_status`），`POST /salary/calc` 是「按期间试算」不是「按单重算」。
 *    所以 {@link SALARY_ACTIONS_BY_STATUS} 只列**真接口**支撑的动作 —— 不画点了必 404 的按钮
 *    （与 `AGENTS.md`「不给点了打不开的入口」同一条红线）。本卡已在卡内上报。
 * 3. **`SUBMITTED` 只能去审批页撤回**：`SUBMITTED → CALCULATED` 是驳回 / 撤回边，
 *    由审批中心（flow engine）推进，薪资单这边没有任何「撤回」路由。详情页在
 *    `SUBMITTED` 下给只读 + 一句指向审批单的提示，不给假按钮。
 * 4. **明细能不能改问状态、不问 `locked` 字段**：后端 `state_machine::SALARY_SHEET_LOCKED`
 *    = `SUBMITTED / APPROVED / POSTED / REVERSED`，`ensure_writable` 只放 `DRAFT`/`CALCULATED`。
 *    出参没有 `locked` 列，所以 {@link isSalaryDetailEditable} 按同一份状态表算（单测与
 *    `state_machine.rs` 对拍）。
 * 5. **明细项条数用 `item_count`**：`with_items=false` 时 `items` 是空数组但 `item_count`
 *    仍是真实条数 —— 页面显示「15 项」要用 `item_count`，别数数组长度。
 * 6. **红冲原因 ≥5 字、改金额原因 ≥5 字**（两个 DTO 的 `MIN_REASON_CHARS` /
 *    `MIN_REVERSE_REASON_CHARS` 都是 5，上限一个 255 一个 500）。前端先手拦，用户不用等
 *    「事务已回滚」那种实现细节。
 *
 * # 姓名口径（`AGENTS.md` 约定 13）
 *
 * `employee_no` / `employee_name` / `dept_name` 是**算工资时的快照**（改名后历史工资条仍显示
 * 当时是谁、当时在哪个部门）。页面直接显示后端下发的快照字段，不再 join 主数据。
 */

import { formatMoney, toCents, toNumber, dash, shortDate } from '@/views/business/erp/shared/money';
import { voucherDocPathFor } from '@/views/business/erp/shared/voucher-trace';

// ==================== 权限码（与 V0387 的 sys_menu.auth_code 逐字一致） ====================

/**
 * 薪资单相关的 7 个权限码。
 *
 * 其中 `calc` 同时管「试算 / 建单 / 明细覆盖」三条后端路由
 * （`salary_sheet_handler.rs` 里 #39/#40 与 #41 共用这一个码）；`my` 属 HR-41 自助视图，
 * 本页不用，但列在这里是为了「一个模块的码在一处」。
 */
export const SALARY_PERMS = {
  /** 列表（`GET /business/hr/salary/page`） */
  list: 'business:hr:salary:list',
  /** 详情 / 明细分页（#37 / #38 共用） */
  detail: 'business:hr:salary:detail',
  /** 试算 / 建单 / 明细覆盖（#39 / #40 / #41） */
  calc: 'business:hr:salary:calc',
  /** 提交审批（#42） */
  submit: 'business:hr:salary:submit',
  /** 过账（#43） */
  post: 'business:hr:salary:post',
  /** 红冲（#44） */
  reverse: 'business:hr:salary:reverse',
  /** 导出 xlsx（#45） */
  export: 'business:hr:salary:export',
  /** 我的薪资（HR-41 自助，本页不用） */
  my: 'business:hr:salary:my',
} as const;

// ==================== 状态词表（与 ck_biz_sal_sheet_status 同域） ====================

/** 六档状态码（顺序 = 后端 `STATUSES` 顺序 = 状态机主链顺序）。 */
export const SALARY_STATUS_VALUES = [
  'DRAFT',
  'CALCULATED',
  'SUBMITTED',
  'APPROVED',
  'POSTED',
  'REVERSED',
] as const;

/** 薪资单状态码。 */
export type SalaryStatus = (typeof SALARY_STATUS_VALUES)[number];

/** 状态码 → 中文名（与后端 `salary_sheet_dto::STATUSES` 逐条一致）。 */
export const SALARY_STATUS_LABELS: Record<SalaryStatus, string> = {
  DRAFT: '草稿',
  CALCULATED: '已计算',
  SUBMITTED: '审批中',
  APPROVED: '已批准',
  POSTED: '已过账',
  REVERSED: '已红冲',
};

/** 状态 → 标签配色。 */
export const SALARY_STATUS_THEMES: Record<
  SalaryStatus,
  'default' | 'primary' | 'success' | 'warning' | 'danger'
> = {
  DRAFT: 'default',
  CALCULATED: 'primary',
  SUBMITTED: 'warning',
  APPROVED: 'success',
  POSTED: 'success',
  REVERSED: 'danger',
};

/** 状态下拉选项（筛选 / 展示共用，`{label,value}` 形状给 `t-select`）。 */
export const SALARY_STATUS_OPTIONS = SALARY_STATUS_VALUES.map((value) => ({
  value,
  label: SALARY_STATUS_LABELS[value],
}));

/** 状态码 → 中文名。未知码**原样返回**（宁可看到 `FOO` 也不要空白，与后端同口径）。 */
export function salaryStatusLabel(status: string | null | undefined): string {
  const code = (status ?? '').trim();
  if (!code) {
    return '—';
  }
  return isSalaryStatus(code) ? SALARY_STATUS_LABELS[code] : code;
}

/** 状态码 → 标签配色。未知码退回 `default`（不猜色）。 */
export function salaryStatusTheme(
  status: string | null | undefined,
): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  const code = (status ?? '').trim();
  return isSalaryStatus(code) ? SALARY_STATUS_THEMES[code] : 'default';
}

/** 是不是已知状态码（类型收窄）。 */
export function isSalaryStatus(status: string): status is SalaryStatus {
  return (SALARY_STATUS_VALUES as readonly string[]).includes(status);
}

/** 每个状态的「下一步会怎样」一句话（单头卡右侧的提示，避免用户对着标签猜）。 */
export const SALARY_STATUS_HINTS: Record<SalaryStatus, string> = {
  DRAFT: '草稿还没算过数：正常流程里建单直接是「已计算」，草稿只出现在异常中断的旧单上',
  CALCULATED: '数已算好、明细可改；核对无误后点「提交审批」，审批通过才能过账',
  SUBMITTED: '审批中：明细冻结（谁都不能改）。撤回 / 驳回请到审批中心，退回后回到「已计算」重算',
  APPROVED: '审批通过：点「过账」生成计提 + 发放两张凭证，过账后凭证号显示在本页凭证卡',
  POSTED: '已过账：凭证已进账。发现数错了只能「红冲」（红字凭证冲回），红冲是终态，要重发请新开一张',
  REVERSED: '已红冲：本单只读留痕，账已冲回。要再发一次工资请新建一张薪资单',
};

/** 状态提示（未知码给空串，页面不渲染那一行）。 */
export function salaryStatusHint(status: string | null | undefined): string {
  const code = (status ?? '').trim();
  return isSalaryStatus(code) ? SALARY_STATUS_HINTS[code] : '';
}

/**
 * 明细（含明细项）在当前状态能不能改 —— 与后端 `state_machine::SALARY_SHEET_LOCKED` 同域。
 *
 * `SUBMITTED / APPROVED / POSTED / REVERSED` 四个状态是锁死的（后端 `ensure_writable`
 * 会 409），只有 `DRAFT` / `CALCULATED` 可改；未知状态**一律不可改**（宁可不给按钮）。
 */
export function isSalaryDetailEditable(status: string | null | undefined): boolean {
  const code = (status ?? '').trim();
  return code === 'DRAFT' || code === 'CALCULATED';
}

// ==================== 操作栏（按钮态由状态算，页面不写 if） ====================

/** 详情页能做的动作（只含**真有后端路由**的四个）。 */
export type SalaryActionKey = 'submit' | 'post' | 'reverse' | 'export';

/** 一个动作的展示与权限定义。 */
export interface SalaryActionSpec {
  /** 动作键（页面 `switch` 用） */
  key: SalaryActionKey;
  /** 按钮文案 */
  label: string;
  /** 按钮配色 */
  theme: 'default' | 'primary' | 'success' | 'warning' | 'danger';
  /** 需要的权限码（`userStore.hasPermission` 判） */
  permission: string;
  /** 悬停提示（说清「点了会发生什么」） */
  title: string;
}

/** 四个动作的静态定义（顺序 = 操作栏里的出现顺序）。 */
export const SALARY_ACTIONS: Record<SalaryActionKey, SalaryActionSpec> = {
  submit: {
    key: 'submit',
    label: '提交审批',
    theme: 'primary',
    permission: SALARY_PERMS.submit,
    title: '冻结明细并发起审批（`POST /business/hr/salary/{id}/submit`）',
  },
  post: {
    key: 'post',
    label: '过账',
    theme: 'success',
    permission: SALARY_PERMS.post,
    title: '生成计提 + 发放两张凭证并进账（`POST /business/hr/salary/{id}/post`）',
  },
  reverse: {
    key: 'reverse',
    label: '红冲',
    theme: 'danger',
    permission: SALARY_PERMS.reverse,
    title: '生成两张红字凭证冲回本单（`POST /business/hr/salary/{id}/reverse`，原因必填）',
  },
  export: {
    key: 'export',
    label: '导出工资表',
    theme: 'default',
    permission: SALARY_PERMS.export,
    title: '导出 xlsx：一人一行、薪资项为列、按部门分组小计',
  },
};

/**
 * 状态 → 可用动作（对照设计 §6.2.5 的按钮态表，**去掉后端没有路由的三条**）。
 *
 * | 状态 | 设计表 | 本表（只认真路由） |
 * |---|---|---|
 * | `DRAFT` | 试算 / 删除 / 作废 | 无（三条都没路由，见模块文档 ②） |
 * | `CALCULATED` | 试算（重算）/ 提交 / 作废 | `submit`（`calc` 是按期间试算，不是按单重算） |
 * | `SUBMITTED` | 撤回（审批页）/ 只读 | 无（撤回在审批中心） |
 * | `APPROVED` | 过账 / 作废 | `post` |
 * | `POSTED` | 红冲 / 导出 / 打印 | `reverse`（导出是全局动作，见 {@link salaryActionsOf}） |
 * | `REVERSED` | 只读 + 导出 | 无 |
 *
 * 导出**不按状态拦**：后端 `export_bundle` 不看状态（草稿也有人要导出来核数），
 * 所以只要权限够就一直给。
 */
export const SALARY_ACTIONS_BY_STATUS: Record<SalaryStatus, SalaryActionKey[]> = {
  DRAFT: [],
  CALCULATED: ['submit'],
  SUBMITTED: [],
  APPROVED: ['post'],
  POSTED: ['reverse'],
  REVERSED: ['export'],
};

/**
 * 某个状态下该显示的动作（已把「导出」补齐到所有状态）。
 *
 * 权限过滤在页面里做（`userStore.hasPermission`）—— 本函数只回答「这个状态能不能点」，
 * 两件事分开才能单测（试权限要造 store，读状态只要一个字符串）。
 */
export function salaryActionsOf(status: string | null | undefined): SalaryActionSpec[] {
  const code = (status ?? '').trim();
  const keys: SalaryActionKey[] = isSalaryStatus(code)
    ? [...SALARY_ACTIONS_BY_STATUS[code]]
    : [];
  if (!keys.includes('export')) {
    keys.push('export');
  }
  return keys.map((key) => SALARY_ACTIONS[key]);
}

// ==================== 上限 / 校验（与后端 DTO 常量同尺） ====================

/** 红冲原因最少字数（后端 `MIN_REVERSE_REASON_CHARS`）。 */
export const MIN_REVERSE_REASON = 5;
/** 红冲原因最多字数（后端 `MAX_REVERSE_REASON_CHARS`）。 */
export const MAX_REVERSE_REASON = 255;
/** 改单项金额的原因最少字数（后端 `salary_override_dto::MIN_REASON_CHARS`）。 */
export const MIN_OVERRIDE_REASON = 5;
/** 改单项金额的原因最多字数（后端 `salary_override_dto::MAX_REASON_CHARS`）。 */
export const MAX_OVERRIDE_REASON = 500;
/** 明细行备注最多字数（后端 `salary_sheet_dto::MAX_REMARK_LEN`）。 */
export const MAX_DETAIL_REMARK = 500;
/** 明细分页默认条数（与后端 `DEFAULT_PAGE_SIZE` 一致）。 */
export const SALARY_DETAIL_PAGE_SIZE = 20;
/** 明细分页上限（后端 `MAX_PAGE_SIZE`，传大了会被 clamp）。 */
export const SALARY_DETAIL_MAX_PAGE_SIZE = 200;

/**
 * 原因文本校验（红冲 / 改金额共用）——`null` = 通过，否则是给用户看的那句话。
 *
 * 按**字符**计数（`[...text].length`）而不是 `String.length`：后端用 `chars().count()`，
 * 一个 emoji 在 JS 里算 2、在 Rust 里算 1，用 `length` 会出现「前端说 4 字、后端说 5 字」。
 */
export function reasonError(
  value: string | null | undefined,
  options: { min?: number; max?: number; label?: string } = {},
): string | null {
  const min = options.min ?? MIN_REVERSE_REASON;
  const max = options.max ?? MAX_REVERSE_REASON;
  const label = options.label ?? '原因';
  const text = (value ?? '').trim();
  if (!text) {
    return `${label}必填（至少 ${min} 个字，会落到单据与状态日志上）`;
  }
  const length = [...text].length;
  if (length < min) {
    return `${label}至少 ${min} 个字（现在 ${length} 个）`;
  }
  if (length > max) {
    return `${label}最多 ${max} 个字（现在 ${length} 个）`;
  }
  return null;
}

// ==================== 明细查询串 ====================

/** 明细筛选（页面状态）。 */
export interface SalaryDetailFilter {
  /** 按部门筛（`biz_salary_detail.dept_id`） */
  dept_id?: string;
  /** 工号 / 姓名模糊搜 */
  keyword?: string;
}

/** 空筛选（重置用；每次给新对象，别共享引用）。 */
export function emptyDetailFilter(): SalaryDetailFilter {
  return { dept_id: '', keyword: '' };
}

/**
 * 明细查询串（`GET /salary/{id}/detail/page`）。
 *
 * 三条口径：
 * - **`with_items` 恒为 `true`**：展开行直接读回执里的 `items`，不再一人一次请求（N+1 反模式）；
 * - 空筛选**不发空参数**（`dept_id=` 会让后端 `parse_uuid("")` 当成「传了但要解析」→ 400）；
 * - 页码 / 条数按后端 `normalize_page` 同口径兜底（`page ≥ 1`、`1 ≤ page_size ≤ 200`）。
 */
export function buildDetailQuery(
  page: number,
  pageSize: number,
  filter: SalaryDetailFilter = {},
): Record<string, unknown> {
  const query: Record<string, unknown> = {
    page: Math.max(1, Math.trunc(page) || 1),
    page_size: Math.min(
      SALARY_DETAIL_MAX_PAGE_SIZE,
      Math.max(1, Math.trunc(pageSize) || SALARY_DETAIL_PAGE_SIZE),
    ),
    with_items: true,
  };
  const deptId = (filter.dept_id ?? '').trim();
  if (deptId) {
    query.dept_id = deptId;
  }
  const keyword = (filter.keyword ?? '').trim();
  if (keyword) {
    query.keyword = keyword;
  }
  return query;
}

/** 筛选有没有生效（空态文案分「没有数据」/「没有匹配」）。 */
export function isDetailFiltered(filter: SalaryDetailFilter): boolean {
  return Boolean((filter.dept_id ?? '').trim() || (filter.keyword ?? '').trim());
}

// ==================== 出参形状 ====================

/** `rust_decimal::Decimal` 出 JSON 的两种可能（字符串为主）。 */
export type DecimalLike = number | string;

/** 单头合计（= `biz_salary_sheet` 的 8 个金额列 + 人数）。 */
export interface SalaryTotals {
  employee_count: number;
  earning_total: DecimalLike;
  pre_deduct_total: DecimalLike;
  gross_total: DecimalLike;
  withhold_total: DecimalLike;
  net_total: DecimalLike;
  si_company_total: DecimalLike;
  hf_company_total: DecimalLike;
  /** 计提总额 = 应发 + 单位社保 + 单位公积金 */
  accrual_total: DecimalLike;
}

/** 单头全列（`SalarySheetHeaderRes`）。 */
export interface SalarySheetHeader {
  id: string;
  sheet_no: string;
  period_code: string;
  period_start: string;
  period_end: string;
  status: string;
  status_label: string;
  attendance_period_id: string | null;
  totals: SalaryTotals;
  accrual_voucher_id: string | null;
  accrual_voucher_no: string | null;
  payment_voucher_id: string | null;
  payment_voucher_no: string | null;
  reversal_voucher_id: string | null;
  approval_instance_id: string | null;
  calculated_at: string | null;
  calculated_by: string | null;
  submitted_at: string | null;
  posted_at: string | null;
  reversed_at: string | null;
  reverse_reason: string | null;
  remark: string | null;
  created_at: string | null;
  updated_at: string | null;
}

/** 明细项按类型合计（`SalaryItemTypeTotalsRes`）。 */
export interface SalaryItemTypeTotals {
  earning_total: DecimalLike;
  pre_deduct_total: DecimalLike;
  withhold_total: DecimalLike;
  employer_total: DecimalLike;
  line_count: number;
}

/** 部门汇总一行（`SalaryDeptSummaryRes`）。 */
export interface SalaryDeptSummary {
  dept_id: string | null;
  dept_name: string;
  employee_count: number;
  earning_total: DecimalLike;
  pre_deduct_total: DecimalLike;
  gross_total: DecimalLike;
  withhold_total: DecimalLike;
  net_total: DecimalLike;
  si_company_total: DecimalLike;
  hf_company_total: DecimalLike;
  accrual_total: DecimalLike;
}

/** 三口径对账的一条不一致（`SalaryMismatchRes`）。 */
export interface SalaryMismatch {
  field: string;
  label: string;
  header: string;
  detail: string;
}

/** 单头汇总（`SalarySheetSummaryRes`）。 */
export interface SalarySheetSummary {
  sheet: SalarySheetHeader;
  detail_totals: SalaryTotals;
  item_totals: SalaryItemTypeTotals;
  consistent: boolean;
  mismatches: SalaryMismatch[];
  dept_summary: SalaryDeptSummary[];
}

/** 一条明细项（`SalaryItemLineRes`；`id` 只在落库后才有）。 */
export interface SalaryItemLine {
  id?: string;
  item_code: string;
  item_name: string;
  item_type: string;
  item_type_label: string;
  calc_type: string;
  calc_type_label: string;
  amount: DecimalLike;
  expr_snapshot: string | null;
  calc_note: string;
  line_no: number;
}

/** 一个人的工资条明细行（`SalaryDetailRes`）。 */
export interface SalaryDetailRow {
  id: string;
  sheet_id: string;
  employee_id: string;
  employee_no: string;
  employee_name: string;
  dept_id: string | null;
  dept_name: string | null;
  user_id: string | null;
  position_code: string | null;
  job_level: string | null;
  should_days: DecimalLike;
  actual_days: DecimalLike;
  leave_unpaid_days: DecimalLike;
  ot_hours: DecimalLike;
  earning_amount: DecimalLike;
  pre_deduct_amount: DecimalLike;
  gross_amount: DecimalLike;
  social_personal: DecimalLike;
  fund_personal: DecimalLike;
  iit_amount: DecimalLike;
  other_withhold: DecimalLike;
  withhold_amount: DecimalLike;
  net_amount: DecimalLike;
  si_company: DecimalLike;
  hf_company: DecimalLike;
  accrual_amount: DecimalLike;
  remark: string | null;
  items: SalaryItemLine[];
  /** 明细项条数（`with_items=false` 时 items 空，但它是真实条数） */
  item_count: number;
}

/** 提交 / 过账 / 红冲的回执（`SalarySheetCardRes`）。 */
export interface SalarySheetCard {
  id: string;
  sheet_no: string;
  period_code: string;
  period_start: string;
  period_end: string;
  status: string;
  status_label: string;
  employee_count: number;
  gross_total: DecimalLike;
  withhold_total: DecimalLike;
  net_total: DecimalLike;
  accrual_total: DecimalLike;
  accrual_voucher_no: string | null;
  payment_voucher_no: string | null;
  posted_at: string | null;
  remark: string | null;
}

/** 一条金额改动（`SalaryAmountChangeRes`）。 */
export interface SalaryAmountChange {
  item_code: string;
  item_name: string;
  before: DecimalLike;
  after: DecimalLike;
  delta: DecimalLike;
}

/** 改单项金额回执（`SalaryOverrideRes`）。 */
export interface SalaryOverrideResult {
  changed: boolean;
  sheet_id: string;
  sheet_no: string;
  sheet_status: string;
  sheet_status_label: string;
  detail_id: string;
  employee_no: string;
  employee_name: string;
  changes: SalaryAmountChange[];
  detail: SalaryDetailRow;
  sheet: SalarySheetHeader;
  log_id: string | null;
  reason: string;
}

/** 改单项金额的入参（`SalaryOverrideReq`）。 */
export interface SalaryOverridePayload {
  /** 覆盖原因（必填，≥5 字） */
  reason: string;
  /** 明细行备注（可选，写了才覆盖 `biz_salary_detail.remark`） */
  remark?: string;
  /** 要改的项（至少一条；同一编码不重复） */
  items: Array<{ item_code: string; amount: DecimalLike }>;
}

// ==================== 纯计算（页面直接调，单测直接打） ====================

/** 期间显示：`2026-11（11-01 ~ 11-30）`。 */
export function periodText(
  periodCode: string | null | undefined,
  start?: string | null,
  end?: string | null,
): string {
  const code = (periodCode ?? '').trim() || '—';
  const range = start || end ? `${shortDate(start)} ~ ${shortDate(end)}` : '';
  return range ? `${code}（${range}）` : code;
}

/** 人数 / 金额一行（空值给 `—`，不让界面出现 `NaN`）。 */
export function moneyText(value: DecimalLike | null | undefined): string {
  return formatMoney(value);
}

/** 天数（`Decimal` 字符串去尾零：`22.00` → `22`，`0.50` → `0.5`）。 */
export function daysText(value: DecimalLike | null | undefined): string {
  const num = toNumber(value);
  if (!Number.isFinite(num)) {
    return '—';
  }
  return String(Number(num.toFixed(2)));
}

/** 时间戳显示（`2026-11-13T01:22:33Z` → `2026-11-13 09:22`，按北京时间）。 */
export function datetimeText(value: string | null | undefined): string {
  const raw = (value ?? '').trim();
  if (!raw) {
    return '—';
  }
  const date = new Date(raw);
  if (Number.isNaN(date.getTime())) {
    return raw;
  }
  const shifted = new Date(date.getTime() + 8 * 60 * 60 * 1000);
  return `${shifted.toISOString().slice(0, 16).replace('T', ' ')}`;
}

/** 部门汇总的「合计行」：人数 + 8 个金额列（分累加，最后一次换元）。 */
export interface DeptTotals {
  employee_count: number;
  earning_total: number;
  pre_deduct_total: number;
  gross_total: number;
  withhold_total: number;
  net_total: number;
  si_company_total: number;
  hf_company_total: number;
  accrual_total: number;
}

/** 空合计（默认值；页面初始化用）。 */
export function emptyDeptTotals(): DeptTotals {
  return {
    employee_count: 0,
    earning_total: 0,
    pre_deduct_total: 0,
    gross_total: 0,
    withhold_total: 0,
    net_total: 0,
    si_company_total: 0,
    hf_company_total: 0,
    accrual_total: 0,
  };
}

/** 部门汇总求和（整数分累加后换回元，避免浮点漂移）。 */
export function sumDeptTotals(rows: readonly SalaryDeptSummary[] | null | undefined): DeptTotals {
  const cents = {
    earning_total: 0,
    pre_deduct_total: 0,
    gross_total: 0,
    withhold_total: 0,
    net_total: 0,
    si_company_total: 0,
    hf_company_total: 0,
    accrual_total: 0,
  };
  let employeeCount = 0;
  for (const row of rows ?? []) {
    employeeCount += Number(row.employee_count) || 0;
    for (const key of Object.keys(cents) as Array<keyof typeof cents>) {
      cents[key] += toCents(row[key]);
    }
  }
  return {
    employee_count: employeeCount,
    earning_total: cents.earning_total / 100,
    pre_deduct_total: cents.pre_deduct_total / 100,
    gross_total: cents.gross_total / 100,
    withhold_total: cents.withhold_total / 100,
    net_total: cents.net_total / 100,
    si_company_total: cents.si_company_total / 100,
    hf_company_total: cents.hf_company_total / 100,
    accrual_total: cents.accrual_total / 100,
  };
}

/**
 * 部门汇总合计是否等于单头（设计 §4.9 巡检 A7 的界面版）。
 *
 * 对拍 8 个金额列 + 人数：任何一项差 1 分都返回 `false`。
 * 与后端 `consistent` 是**两件事**（后端比的是单头 vs 明细 vs 明细项三份口径）；
 * 这里比的是「界面上这张部门表加起来是不是单头那个数」—— 两个都过才算这张单能放心看。
 */
export function deptTotalsMatchHeader(
  deptTotals: DeptTotals,
  header: SalaryTotals | null | undefined,
): boolean {
  if (!header) {
    return false;
  }
  if ((Number(header.employee_count) || 0) !== deptTotals.employee_count) {
    return false;
  }
  const pairs: Array<[number, DecimalLike | null | undefined]> = [
    [deptTotals.earning_total, header.earning_total],
    [deptTotals.pre_deduct_total, header.pre_deduct_total],
    [deptTotals.gross_total, header.gross_total],
    [deptTotals.withhold_total, header.withhold_total],
    [deptTotals.net_total, header.net_total],
    [deptTotals.si_company_total, header.si_company_total],
    [deptTotals.hf_company_total, header.hf_company_total],
    [deptTotals.accrual_total, header.accrual_total],
  ];
  return pairs.every(([sum, value]) => toCents(sum) === toCents(value));
}

/** 薪资项类型 → 标签配色（收入绿 / 应发前扣减橙 / 代扣红 / 单位成本蓝）。 */
export function itemTypeTheme(
  itemType: string | null | undefined,
): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  switch ((itemType ?? '').trim().toUpperCase()) {
    case 'EARNING':
      return 'success';
    case 'PRE_DEDUCT':
      return 'warning';
    case 'WITHHOLD':
      return 'danger';
    case 'EMPLOYER':
      return 'primary';
    default:
      return 'default';
  }
}

/**
 * 明细项的「为什么是这个数」一行。
 *
 * 优先用 `calc_note`（后端引擎写的代入值说明，如 `8000/21.75*2`），
 * 没有就退到公式快照（`expr_snapshot`），都没有给 `—`。
 */
export function itemExplainText(row: SalaryItemLine | null | undefined): string {
  if (!row) {
    return '—';
  }
  const note = (row.calc_note ?? '').trim();
  if (note) {
    return note;
  }
  const expr = (row.expr_snapshot ?? '').trim();
  return expr || '—';
}

/** 明细行「15 项」文案（用 `item_count`，不是数组长度）。 */
export function itemCountText(row: SalaryDetailRow | null | undefined): string {
  const count = Number(row?.item_count ?? 0);
  return `${count} 项`;
}

/** 明细行的考勤口径一行（设计 §4.3：应出勤 / 实际 / 无薪假 / 加班）。 */
export function attendanceText(row: SalaryDetailRow | null | undefined): string {
  if (!row) {
    return '—';
  }
  return [
    `应出勤 ${daysText(row.should_days)}`,
    `实际 ${daysText(row.actual_days)}`,
    `无薪假 ${daysText(row.leave_unpaid_days)}`,
    `加班 ${daysText(row.ot_hours)}h`,
  ].join(' · ');
}

// ==================== 凭证卡 ====================

/** 凭证卡的角色。 */
export type SalaryVoucherRole = 'accrual' | 'payment' | 'reversal';

/** 一张凭证卡（计提 / 发放 / 红冲）。 */
export interface SalaryVoucherCard {
  role: SalaryVoucherRole;
  /** 卡头中文名 */
  label: string;
  /** 凭证号（`null` = 还没生成） */
  no: string | null;
  /** 凭证主键（`null` = 还没生成） */
  id: string | null;
  /** 本卡对应的金额（计提 = 计提总额 / 发放 = 实发 / 红冲 = 计提总额） */
  amount: DecimalLike;
  /** 还没生成时显示的说明 */
  emptyText: string;
  /** 生成时机的说明（卡底那行灰字） */
  metaText: string;
  /** 凭证页深链（`null` = 没有凭证号，不给链接） */
  path: string | null;
}

/**
 * 单头 → 三张凭证卡。
 *
 * 为什么金额这么取：
 * - **计提**：借「费用/成本」= 应发 + 单位社保 + 单位公积金（`accrual_total`，设计 §4.6.1 ①）；
 * - **发放**：贷银行存款 = 实发（`net_total`，倒挤，设计 §4.6.1 ②）；
 * - **红冲**：红字冲回**两张**（计提 + 发放），金额沿用计提总额 —— 卡上只写计提那张的金额，
 *   卡底说明里讲清「两张都冲」。
 *
 * 红冲卡**没有单号**：单头只下发 `reversal_voucher_id`（不发 `reversal_voucher_no`），
 * 所以这张卡只显示「已红冲 / 未红冲」+ 主键尾号，**不给链接** —— 与其拼一个点了打不开的
 * 猜单号，不如让用户去凭证页按来源单号反查（后端 `GET /erp/voucher/by-source` 已支持）。
 *
 * 不按状态伪造：凭证号为空就是「未生成」（草稿 / 已计算 / 审批中 / 已批准都是这个状态），
 * 页面显示空态文案，不给点了打不开的链接（与 `voucher-trace.ts` 同一取向）。
 */
export function voucherCardsOf(sheet: SalarySheetHeader | null | undefined): SalaryVoucherCard[] {
  const totals = sheet?.totals;
  const accrualNo = (sheet?.accrual_voucher_no ?? '').trim() || null;
  const paymentNo = (sheet?.payment_voucher_no ?? '').trim() || null;
  const reversalId = (sheet?.reversal_voucher_id ?? '').trim() || null;
  const status = (sheet?.status ?? '').trim();
  const reversed = Boolean(reversalId) || status === 'REVERSED';

  return [
    {
      role: 'accrual',
      label: '计提凭证',
      no: accrualNo,
      id: (sheet?.accrual_voucher_id ?? '').trim() || null,
      amount: totals?.accrual_total ?? 0,
      emptyText: '过账后生成（借 费用/成本，贷 应付职工薪酬）',
      metaText: '借：费用 / 成本（按部门）· 贷：应付职工薪酬',
      path: voucherDocPathFor(accrualNo),
    },
    {
      role: 'payment',
      label: '发放凭证',
      no: paymentNo,
      id: (sheet?.payment_voucher_id ?? '').trim() || null,
      amount: totals?.net_total ?? 0,
      emptyText: '过账后生成（借 应付职工薪酬，贷 银行存款）',
      metaText: '借：应付职工薪酬 · 贷：银行存款（= 实发，倒挤）',
      path: voucherDocPathFor(paymentNo),
    },
    {
      role: 'reversal',
      label: '红冲凭证',
      no: reversed ? `红字 ×2（${reversalId ? `原单 ${reversalId.slice(0, 8)}…` : '已红冲'}）` : null,
      id: reversalId,
      amount: reversed ? (totals?.accrual_total ?? 0) : 0,
      emptyText: '红冲后生成（红字冲回计提 + 发放两张）',
      metaText: '红冲一次生成两张红字凭证：计提 + 发放都冲回',
      // 红冲凭证的单号没有随单头下发，所以不给深链（见上方文档）
      path: null,
    },
  ];
}

/** 已生成的凭证张数（只数计提 + 发放：红冲卡没有单号，不能算进「查看凭证 (N)」）。 */
export function generatedVoucherCount(sheet: SalarySheetHeader | null | undefined): number {
  return voucherCardsOf(sheet).filter(
    (card) => card.role !== 'reversal' && card.no !== null,
  ).length;
}

// ==================== 表格列（表头字典集中一处，页面不再手抄） ====================

/** 部门汇总列（设计 §6.2.5 汇总卡：人数 / 应发 / 单位社保 / 单位公积金 / 实发）。 */
export const DEPT_SUMMARY_COLUMNS = [
  { colKey: 'dept_name', title: '部门', width: 160, ellipsis: true },
  { colKey: 'employee_count', title: '人数', width: 80, align: 'right' as const },
  { colKey: 'gross_total', title: '应发合计', width: 140, align: 'right' as const },
  { colKey: 'si_company_total', title: '单位社保', width: 130, align: 'right' as const },
  { colKey: 'hf_company_total', title: '单位公积金', width: 130, align: 'right' as const },
  { colKey: 'net_total', title: '实发合计', width: 140, align: 'right' as const },
];

/** 明细表列（一人一行；金额列右对齐）。宽度合计 ≈1060，1440 视口下一屏放得下。
 */
export const DETAIL_COLUMNS = [
  { colKey: 'employee', title: '员工', width: 160 },
  { colKey: 'dept_name', title: '部门', width: 110, ellipsis: true },
  { colKey: 'attendance', title: '考勤口径', width: 210, ellipsis: true },
  { colKey: 'earning_amount', title: '收入合计', width: 120, align: 'right' as const },
  { colKey: 'gross_amount', title: '应发', width: 120, align: 'right' as const },
  { colKey: 'withhold_amount', title: '代扣', width: 120, align: 'right' as const },
  { colKey: 'net_amount', title: '实发', width: 120, align: 'right' as const },
  { colKey: 'item_count', title: '薪资项', width: 90, align: 'right' as const },
];

/** 明细项列（展开后的 17 行；`说明` 放引擎写的代入值）。 */
export const ITEM_COLUMNS = [
  { colKey: 'line_no', title: '#', width: 60, align: 'right' as const },
  { colKey: 'item_code', title: '编码', width: 130 },
  { colKey: 'item_name', title: '薪资项', width: 160, ellipsis: true },
  { colKey: 'item_type', title: '类型', width: 110 },
  { colKey: 'calc_type', title: '算法', width: 100 },
  { colKey: 'amount', title: '金额', width: 140, align: 'right' as const },
  { colKey: 'calc_note', title: '说明', minWidth: 240, ellipsis: true },
  { colKey: 'op', title: '操作', width: 90 },
];

/** 空值占位（`dash` 的再导出，页面少 import 一处）。 */
export { dash };
