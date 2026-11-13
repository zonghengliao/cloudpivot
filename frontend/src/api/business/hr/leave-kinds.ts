/**
 * 请假单的出参契约与词表（ARCH-9 §5.2 #26~#31 · §6.1 / §6.2 · HR-28）。
 *
 * 纯逻辑文件：**不 import 请求层、不 import Vue**，`tsx --test` 里可以直接 import 做单测
 * （`tests/unit/hr-leave-page.test.ts` 会读 Rust 源码把两张词表对拍）。
 * 请求函数在 `./leave.ts`，那里 `export * from './leave-kinds'`，
 * 页面只认 `@/api/business/hr/leave` 一个入口。
 *
 * # 字段名逐条抄自后端，不缩写
 *
 * 出处：`backend/src/business/hr/leave_dto.rs`（HR-24 契约 + HR-25 详情/作废回执）。
 * `LeaveRes` 是列表 / 详情 / 作废回执**共用的同一形状**（详情用 `#[serde(flatten)]`
 * 平铺 + `approval` / `approval_history`，作废回执平铺 + `action` / `action_label`），
 * 所以前端一个 {@link LeaveListRow} 通吃三个接口。
 *
 * # 六处与「想当然」不一样的地方（都会让页面显示错值，先读）
 *
 * 1. **`paid` 是 `0/1` 数字，不是布尔**：DDL 是 `SMALLINT IN (0,1)`，且**不按假别推导**
 *    （设计 §3.1 ⑥ 列注释「请假类型与带薪与否解耦」）。事假也可能带薪（部分公司的
 *    全薪病假），所以表单给的是显式开关，不是「选事假自动置无薪」。
 * 2. **`duration_days` 是字符串**（`Decimal` → JSON `"1.50"`）。不要 `+`，要显示就过
 *    {@link durationText}（去尾零）。
 * 3. **`locked` 由后端给**（`APPROVED` / `CANCELLED`）：页面不自己 `match` 状态串决定
 *    按钮态，与 `state_machine::LEAVE_LOCKED` 保持同一份真相。
 * 4. **作废接口一条路径做三件事**（`DELETED` 软删 / `WITHDRAWN` 撤回 / `CANCELLED` 销假），
 *    由「当前状态」决定走哪条 —— 见 {@link cancelActionOf}，与后端
 *    `LeaveCancelAction::of_status` 逐值一致。
 * 5. **三条路都要求原因 ≥5 字**：后端 `leave_dto::cancel_of` 在**状态分流之前**统一校验
 *    `reason`，所以「删除草稿」也要填原因（不是只有销假要）。前端照做，否则点删除会收到
 *    400「作废原因（reason）必填」。已在卡内上报：如果想草稿删除免原因，属于后端契约变更。
 * 6. **「我的请假」不是另一个接口**：设计 §6.1 明确走 `GET /leave/page?mine=true`
 *    （`/leave/my` 随 HR-41 落）。本页两个 tab 共用 {@link fetchLeavePageFor} 的唯一入口，
 *    差别只是 `mine` —— 不要让「我的」tab 复用「全部」的缓存，那会把别人的单显示出来。
 *
 * # 姓名 / 部门的口径（`AGENTS.md` 约定 13）
 *
 * `employee_no` / `employee_name` 是**请假发生时的快照**（员工改名后这张单仍显示当时是谁）；
 * `dept_name` 是实时 join（部门改名后显示现在的归属）。页面直接显示后端下发的
 * `*_label` / `*_name`，本文件的 `*_LABELS` 只用于**下拉选项**与极端兜底。
 */

// ==================== 假别域（与 `ck_biz_leave_type` 逐字一致） ====================

/** 9 个假别（顺序 = 字典 `hr_leave_type` 的 `sort_order` = 后端 `LEAVE_TYPES`） */
export const LEAVE_TYPE_VALUES = [
  'ANNUAL',
  'SICK',
  'PERSONAL',
  'MARRIAGE',
  'MATERNITY',
  'PATERNITY',
  'BEREAVEMENT',
  'COMPENSATORY',
  'OTHER',
] as const;

/** 假别码值 */
export type LeaveType = (typeof LEAVE_TYPE_VALUES)[number];

/** 假别 → 中文（`leave_dto::leave_type_label` 逐字一致） */
export const LEAVE_TYPE_LABELS: Record<LeaveType, string> = {
  ANNUAL: '年假',
  SICK: '病假',
  PERSONAL: '事假',
  MARRIAGE: '婚假',
  MATERNITY: '产假',
  PATERNITY: '陪产假',
  BEREAVEMENT: '丧假',
  COMPENSATORY: '调休',
  OTHER: '其他',
};

/** 假别下拉选项（顺序与后端常量一致，筛选多选与建单单选共用） */
export const LEAVE_TYPE_OPTIONS = LEAVE_TYPE_VALUES.map((value) => ({
  value,
  label: LEAVE_TYPE_LABELS[value],
}));

/** 假别码值 → 中文，未知值回原值（不吞码值 —— 排障时看到 `NEW_TYPE` 比看到「未知」有用） */
export function leaveTypeLabel(code: string | null | undefined): string {
  if (!code) {
    return '';
  }
  return LEAVE_TYPE_LABELS[code as LeaveType] ?? code;
}

// ==================== 带薪域（`paid SMALLINT IN (0,1)`） ====================

/** 带薪（DDL 列默认值） */
export const PAID_YES = 1;
/** 无薪（进薪资扣款基线 `ATT_LEAVE_UNPAID_DAYS`） */
export const PAID_NO = 0;

/** `paid` → 中文（`leave_dto::paid_label` 逐字一致） */
export const PAID_LABELS: Record<number, string> = {
  [PAID_YES]: '带薪',
  [PAID_NO]: '无薪',
};

/** `paid` 中文名，未知值回原值 */
export function paidLabel(paid: number | null | undefined): string {
  if (paid === null || paid === undefined) {
    return '';
  }
  return PAID_LABELS[paid] ?? String(paid);
}

// ==================== 状态域（与 `ck_biz_leave_status` 逐字一致） ====================

/** 4 个状态（顺序 = 状态机正向顺序 = 后端 `STATUS_VALUES`） */
export const LEAVE_STATUS_VALUES = ['DRAFT', 'SUBMITTED', 'APPROVED', 'CANCELLED'] as const;

/** 请假状态码值 */
export type LeaveStatus = (typeof LEAVE_STATUS_VALUES)[number];

/** 状态 → 中文（`leave_dto::status_label` 逐字一致） */
export const LEAVE_STATUS_LABELS: Record<LeaveStatus, string> = {
  DRAFT: '草稿',
  SUBMITTED: '审批中',
  APPROVED: '已批准',
  CANCELLED: '已作废',
};

/**
 * 状态 → 标签配色。
 *
 * TDesign 只有 5 个语义色：草稿走中性灰（还没生效）、审批中走蓝（进行中）、
 * 已批准走绿（可进考勤/薪资）、已作废走红（会冲掉已建立的考勤口径）。
 */
export const LEAVE_STATUS_THEMES: Record<LeaveStatus, 'default' | 'primary' | 'success' | 'danger'> = {
  DRAFT: 'default',
  SUBMITTED: 'primary',
  APPROVED: 'success',
  CANCELLED: 'danger',
};

/** 状态 → 中文，未知值回原值 */
export function leaveStatusLabel(code: string | null | undefined): string {
  if (!code) {
    return '';
  }
  return LEAVE_STATUS_LABELS[code as LeaveStatus] ?? code;
}

/** 状态 → 标签配色，未知值走中性色 */
export function leaveStatusTheme(code: string | null | undefined): 'default' | 'primary' | 'success' | 'danger' {
  return LEAVE_STATUS_THEMES[code as LeaveStatus] ?? 'default';
}

/** 锁死态（与后端 `state_machine::LEAVE_LOCKED` 同域：已批准 / 已作废） */
export const LEAVE_LOCKED_STATUSES: readonly string[] = ['APPROVED', 'CANCELLED'];

/** 可编辑（只有草稿，后端 `EDITABLE_STATUSES`） */
export const LEAVE_EDITABLE_STATUSES: readonly string[] = ['DRAFT'];
/** 可提交（只有草稿，后端 `SUBMITTABLE_STATUSES`） */
export const LEAVE_SUBMITTABLE_STATUSES: readonly string[] = ['DRAFT'];
/** 可撤回（只有审批中，后端 `WITHDRAWABLE_STATUSES`） */
export const LEAVE_WITHDRAWABLE_STATUSES: readonly string[] = ['SUBMITTED'];
/** 可作废（审批中 / 已批准，后端 `CANCELLABLE_STATUSES`；草稿走软删不进这个清单） */
export const LEAVE_CANCELLABLE_STATUSES: readonly string[] = ['SUBMITTED', 'APPROVED'];

/** 状态下拉（筛选多选） */
export const LEAVE_STATUS_OPTIONS = LEAVE_STATUS_VALUES.map((value) => ({
  value,
  label: LEAVE_STATUS_LABELS[value],
}));

// ==================== 出参契约（逐字抄 `leave_dto.rs`） ====================

/**
 * 请假单出参（列表行 / 详情主体 / 作废回执主体**共用**）。
 *
 * 30 个键，全部来自 `LeaveRes`。`*_id` 都配了 `*_name`（`AGENTS.md` 约定 13）：
 * `employee_id → employee_no + employee_name`（快照）、`dept_id → dept_name`（实时 join）、
 * `submitted_by → submitted_by_name`、`cancelled_by → cancelled_by_name`。
 */
export interface LeaveListRow {
  id: string;
  /** 单号 `LV-yyyyMMdd-000001`（服务端取号） */
  leave_no: string;
  employee_id: string;
  /** 工号（快照） */
  employee_no: string;
  /** 姓名（快照：请假当时是谁） */
  employee_name: string;
  dept_id: string | null;
  /** 部门名（实时 join；未填 / 部门已删时为空串） */
  dept_name: string;
  leave_type: string;
  leave_type_label: string;
  /** 起（ISO8601 `+08:00`） */
  start_time: string;
  /** 止（ISO8601 `+08:00`） */
  end_time: string;
  /** 天数（`Decimal` → 字符串，如 `"1.50"`） */
  duration_days: string;
  /** 1 带薪 / 0 无薪 */
  paid: number;
  paid_label: string;
  status: string;
  status_label: string;
  /** 锁死态（后端算，前端不自己 match） */
  locked: boolean;
  reason: string | null;
  approval_instance_id: string | null;
  submitted_at: string | null;
  submitted_by: string | null;
  submitted_by_name: string | null;
  cancelled_at: string | null;
  cancelled_by: string | null;
  cancelled_by_name: string | null;
  cancel_reason: string | null;
  remark: string | null;
  created_at: string;
  updated_at: string;
}

/** 审批时间线一行（`LeaveApprovalTaskRes`） */
export interface LeaveApprovalTask {
  node_key: string;
  node_name: string;
  round: number;
  assignee_id: string;
  assignee_name: string;
  status: string;
  status_label: string;
  comment: string | null;
  is_self_approval: boolean;
  is_auto: boolean;
  created_at: string;
  completed_at: string | null;
}

/** 审批实例摘要 + 时间线（`LeaveApprovalRes`；草稿 / 实例已删时为 `null`） */
export interface LeaveApproval {
  instance_id: string;
  instance_no: string;
  flow_code: string;
  flow_name: string;
  status: string;
  status_label: string;
  initiator_name: string;
  started_at: string;
  ended_at: string | null;
  result_comment: string | null;
  timeline: LeaveApprovalTask[];
}

/** 详情接口（#27）回执：`LeaveRes` 平铺 + `approval` + `approval_history` */
export interface LeaveDetail extends LeaveListRow {
  approval: LeaveApproval | null;
  /** 历史上提交过几次（撤回重提会 > 1），审批轨迹 tab 的角标 */
  approval_history: number;
}

/** 作废接口（#31）回执：`LeaveRes` 平铺 + 「做的是哪件事」 */
export interface LeaveCancelResult extends LeaveListRow {
  /** `DELETED` / `WITHDRAWN` / `CANCELLED` */
  action: string;
  action_label: string;
  /** 审批实例终止后的状态（`null` = 本单没有在跑的实例） */
  instance_status: string | null;
}

// ==================== 入参 ====================

/** 列表查询串（`LeavePageQuery`；多选按**逗号分隔**，后端 `serde_urlencoded` 不收重复键） */
export interface LeavePageQuery {
  keyword?: string;
  employee_id?: string;
  dept_id?: string;
  /** 假别多选，逗号分隔 */
  leave_type?: string;
  /** 状态多选，逗号分隔 */
  status?: string;
  /** 起始日下界（含当天，`yyyy-MM-dd`） */
  date_from?: string;
  /** 起始日上界（含当天，`yyyy-MM-dd`） */
  date_to?: string;
  /** 只看本人（「我的请假」tab） */
  mine?: boolean;
  page?: number;
  page_size?: number;
}

/** 建单入参（`LeaveCreateReq`；`leave_no` / `status` / 员工快照都不收，服务端定） */
export interface LeaveCreatePayload {
  employee_id: string;
  leave_type: string;
  /** `yyyy-MM-dd HH:mm`（后端三种格式都收，见 `parse_datetime`） */
  start_time: string;
  end_time: string;
  duration_days: string;
  paid: number;
  reason?: string;
  remark?: string;
}

/** 改草稿入参（`LeaveUpdateReq`；**部分更新**，缺省字段保持库里的值，空串 = 清空） */
export interface LeaveUpdatePayload {
  leave_type?: string;
  start_time?: string;
  end_time?: string;
  duration_days?: string;
  paid?: number;
  reason?: string;
  remark?: string;
}

/** 提交审批入参（`LeaveSubmitReq`） */
export interface LeaveSubmitPayload {
  remark?: string;
}

/** 作废入参（`LeaveCancelReq`：`reason` 必填 ≥5 字，三条路都要） */
export interface LeaveCancelPayload {
  reason: string;
}

// ==================== 筛选行 ====================

/** 每页条数（后端默认也是 20） */
export const LEAVE_PAGE_SIZE = 20;

/** 筛选行（`useListFilterMemory` 会就地恢复 / 写回） */
export interface LeaveFilter {
  keyword: string;
  dept_id: string | undefined;
  /** 假别多选（空 = 全部） */
  leave_type: string[];
  /** 状态多选（空 = 全部） */
  status: string[];
  /** 请假起始日下界（`yyyy-MM-dd`） */
  date_from: string;
  /** 请假起始日上界（`yyyy-MM-dd`） */
  date_to: string;
}

/** 空筛选 */
export function emptyLeaveFilter(): LeaveFilter {
  return {
    keyword: '',
    dept_id: undefined,
    leave_type: [],
    status: [],
    date_from: '',
    date_to: '',
  };
}

/** 是否有任何筛选条件（决定空态文案：没数据 vs 没匹配） */
export function isLeaveFiltered(filter: LeaveFilter): boolean {
  return Boolean(
    (filter.keyword ?? '').trim() ||
      filter.dept_id ||
      (filter.leave_type ?? []).length > 0 ||
      (filter.status ?? []).length > 0 ||
      (filter.date_from ?? '').trim() ||
      (filter.date_to ?? '').trim(),
  );
}

/** 数组 → 逗号串（空数组 → `undefined`，即「不筛」） */
function joinCodes(values: readonly string[] | undefined): string | undefined {
  const list = (values ?? []).map((item) => item.trim()).filter((item) => item.length > 0);
  return list.length > 0 ? list.join(',') : undefined;
}

/**
 * 筛选行 → 查询串。
 *
 * - 空串 / 空数组一律丢掉（`status=[]` 表示「全部状态」，不该变成 `status=`，
 *   后端 `split(',')` 会得到 `[""]` 然后 400）；
 * - `mine` 只在 `true` 时出现（「全部请假」tab 不带这个键，让后端按数据权限裁）；
 * - 日期只到天（`date_from` / `date_to` 是 `NaiveDate`）。
 */
export function buildLeaveQuery(
  filter: LeaveFilter,
  page: number,
  pageSize: number = LEAVE_PAGE_SIZE,
  mine = false,
): LeavePageQuery {
  const query: LeavePageQuery = {
    page: page > 0 ? page : 1,
    page_size: pageSize > 0 ? pageSize : LEAVE_PAGE_SIZE,
  };
  const keyword = (filter.keyword ?? '').trim();
  if (keyword) {
    query.keyword = keyword;
  }
  if (filter.dept_id) {
    query.dept_id = filter.dept_id;
  }
  const leaveType = joinCodes(filter.leave_type);
  if (leaveType) {
    query.leave_type = leaveType;
  }
  const status = joinCodes(filter.status);
  if (status) {
    query.status = status;
  }
  const from = (filter.date_from ?? '').trim();
  if (from) {
    query.date_from = from;
  }
  const to = (filter.date_to ?? '').trim();
  if (to) {
    query.date_to = to;
  }
  if (mine) {
    query.mine = true;
  }
  return query;
}

// ==================== 表格列 ====================

/** 表格列（`colKey` 与后端出参同名；列表页与「我的请假」tab 共用） */
export const LEAVE_COLUMNS = [
  { colKey: 'leave_no', title: '单号', width: 170 },
  { colKey: 'employee', title: '员工', width: 150 },
  { colKey: 'dept_name', title: '部门', width: 130, ellipsis: true },
  { colKey: 'leave_type', title: '假别', width: 90 },
  { colKey: 'range', title: '请假区间', width: 250 },
  { colKey: 'duration_days', title: '天数', width: 80, align: 'right' as const },
  { colKey: 'paid', title: '带薪', width: 80 },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'op', title: '操作', width: 190, fixed: 'right' as const },
];

// ==================== 展示口径（纯函数） ====================

/** 取不到值时显示的空位（与员工页 `textOr` 同口径） */
export function textOr(value: string | null | undefined): string {
  const text = (value ?? '').trim();
  return text.length > 0 ? text : '-';
}

/**
 * ISO8601（`2026-11-10T09:00:00+08:00`）→ `2026-11-10 09:00`。
 *
 * 只做**字符串切分**，不 `new Date()`：后端已经按业务时区 `+08:00` 出参，
 * 再用本机时区转一次会让「09:00 的半天假」在非北京时区显示成别的时刻。
 * 格式不认识就原样返回（宁可显示后端原文，也不要显示 `Invalid Date`）。
 */
export function datetimeText(raw: string | null | undefined): string {
  const text = (raw ?? '').trim();
  if (!text) {
    return '';
  }
  const matched = /^(\d{4}-\d{2}-\d{2})[T ](\d{2}):(\d{2})/.exec(text);
  if (!matched) {
    return text;
  }
  return `${matched[1]} ${matched[2]}:${matched[3]}`;
}

/** ISO8601 → `2026-11-10`（列表里日期列用；同样只切字符串） */
export function dateText(raw: string | null | undefined): string {
  const text = (raw ?? '').trim();
  if (!text) {
    return '';
  }
  const matched = /^(\d{4}-\d{2}-\d{2})/.exec(text);
  return matched ? matched[1] : text;
}

/**
 * 天数去尾零：`"1.50"` → `"1.5"`、`"2.00"` → `"2"`。
 *
 * `Decimal` 出 JSON 恒带两位小数，直接显示会变成「请假 1.50 天」——
 * 读起来像金额。这里只做字符串裁尾（不 `Number()` 再格式化，避免精度漂移）。
 */
export function durationText(days: string | number | null | undefined): string {
  if (days === null || days === undefined) {
    return '';
  }
  const text = String(days).trim();
  if (!text) {
    return '';
  }
  if (!text.includes('.')) {
    return text;
  }
  return text.replace(/0+$/, '').replace(/\.$/, '');
}

/** 请假区间文案：`2026-11-10 09:00 ~ 2026-11-12 18:00`（同年同月同日时显示单日 + 时刻） */
export function rangeText(row: Pick<LeaveListRow, 'start_time' | 'end_time'>): string {
  const start = datetimeText(row.start_time);
  const end = datetimeText(row.end_time);
  if (!start || !end) {
    return textOr(start || end);
  }
  const [startDay, startClock] = start.split(' ');
  const [endDay, endClock] = end.split(' ');
  if (startDay === endDay) {
    return `${startDay} ${startClock} ~ ${endClock}`;
  }
  return `${start} ~ ${end}`;
}

/** 起止覆盖的自然日数（含首尾；解析不出来 → `null`） */
export function daySpanOf(start: string, end: string): number | null {
  const from = parseDayParts(start);
  const to = parseDayParts(end);
  if (!from || !to) {
    return null;
  }
  const span = (Date.UTC(to.y, to.m - 1, to.d) - Date.UTC(from.y, from.m - 1, from.d)) / 86_400_000;
  if (!Number.isFinite(span)) {
    return null;
  }
  return Math.round(span) + 1;
}

/** `yyyy-MM-dd`（或带时刻的串）→ 年月日数字；认不出返回 `null` */
function parseDayParts(raw: string): { y: number; m: number; d: number } | null {
  const matched = /^(\d{4})-(\d{2})-(\d{2})/.exec((raw ?? '').trim());
  if (!matched) {
    return null;
  }
  return { y: Number(matched[1]), m: Number(matched[2]), d: Number(matched[3]) };
}

/** `yyyy-MM-dd HH:mm` → 当天的分钟数（用于判断「半天」）；认不出返回 `null` */
export function minuteOfDay(raw: string): number | null {
  const matched = /^\d{4}-\d{2}-\d{2}[T ](\d{2}):(\d{2})/.exec((raw ?? '').trim());
  if (!matched) {
    return null;
  }
  return Number(matched[1]) * 60 + Number(matched[2]);
}

/**
 * 按起止**建议**天数（不是强制值，用户可改）。
 *
 * 口径（与后端「天数 ≤ 起止覆盖自然日数」的校验配合）：
 * - 同一天：结束时刻 ≤ 12:00 视为半天 → `0.5`，否则 `1`；
 * - 跨天：`跨度天数`（含首尾），结束时刻 ≤ 12:00 且跨度 > 1 时减 0.5
 *   （「到那天中午」= 少半天）。
 *
 * 只是省一次手算，**最终值以用户填的为准**（后端才是判据）。
 */
export function suggestDuration(start: string, end: string): string {
  const span = daySpanOf(start, end);
  if (span === null || span <= 0) {
    return '';
  }
  const endMinute = minuteOfDay(end);
  if (span === 1) {
    return endMinute !== null && endMinute <= 12 * 60 ? '0.5' : '1';
  }
  if (endMinute !== null && endMinute <= 12 * 60) {
    return String(span - 0.5);
  }
  return String(span);
}

// ==================== 动作可用性（与后端状态机同域） ====================

/** 作废接口一条路径承载的三件事（`LeaveCancelAction`） */
export type LeaveCancelActionCode = 'DELETED' | 'WITHDRAWN' | 'CANCELLED';

/**
 * 当前状态走哪条作废路（后端 `LeaveCancelAction::of_status` 逐值一致）。
 *
 * `null` = 没有作废路径（已作废的终态 / 未知脏数据）。
 */
export function cancelActionOf(status: string | null | undefined): LeaveCancelActionCode | null {
  switch ((status ?? '').trim().toUpperCase()) {
    case 'DRAFT':
      return 'DELETED';
    case 'SUBMITTED':
      return 'WITHDRAWN';
    case 'APPROVED':
      return 'CANCELLED';
    default:
      return null;
  }
}

/** 作废按钮文案（按状态说人话：草稿叫「删除」、审批中叫「撤回」、已批准叫「销假/作废」） */
export function cancelActionLabel(status: string | null | undefined): string {
  switch (cancelActionOf(status)) {
    case 'DELETED':
      return '删除';
    case 'WITHDRAWN':
      return '撤回';
    case 'CANCELLED':
      return '销假';
    default:
      return '';
  }
}

/** 作废弹窗里的原因标签（同一个接口，三种场景三种问法） */
export function cancelReasonLabel(status: string | null | undefined): string {
  switch (cancelActionOf(status)) {
    case 'DELETED':
      return '删除原因';
    case 'WITHDRAWN':
      return '撤回原因';
    case 'CANCELLED':
      return '销假原因';
    default:
      return '原因';
  }
}

/** 行内动作的可用性（纯由状态推出；权限码另由页面 `hasPermission` 拦） */
export interface LeaveActionState {
  /** 看详情（任何状态都可看） */
  detail: boolean;
  /** 改草稿 */
  edit: boolean;
  /** 提交审批 */
  submit: boolean;
  /** 作废类动作（删除 / 撤回 / 销假三选一） */
  cancel: boolean;
  /** 作废类动作的码值（`cancel=false` 时为 `null`） */
  cancelAction: LeaveCancelActionCode | null;
  /** 作废按钮文案 */
  cancelLabel: string;
  /** 作废是否必须填原因（三条路都要，见文件头第 5 条） */
  cancelNeedsReason: boolean;
}

/** 由状态算动作态（后端 `EDITABLE_STATUSES` / `SUBMITTABLE_STATUSES` / `CANCELLABLE_STATUSES` + 草稿软删） */
export function leaveActionsOf(status: string | null | undefined): LeaveActionState {
  const code = (status ?? '').trim().toUpperCase();
  const cancelAction = cancelActionOf(code);
  return {
    detail: true,
    edit: LEAVE_EDITABLE_STATUSES.includes(code),
    submit: LEAVE_SUBMITTABLE_STATUSES.includes(code),
    cancel: cancelAction !== null,
    cancelAction,
    cancelLabel: cancelActionLabel(code),
    cancelNeedsReason: cancelAction !== null,
  };
}

// ==================== 建单 / 改草稿表单 ====================

/** 表单模型（建单与改草稿共用；改草稿时 `employee_id` 只读展示） */
export interface LeaveForm {
  employee_id: string;
  leave_type: string;
  /** `yyyy-MM-dd HH:mm`（TDesign `t-date-picker` + `enable-time-picker` 的默认格式，后端收） */
  start_time: string;
  end_time: string;
  /**
   * 天数：`string | number`。
   *
   * **两者都要收**：`t-input-number` 的 `v-model` 写回的是 **number**（`3`），
   * 而 `formFromRow` 回填的是后端原样的字符串（`"2.00"`）。只用 string 会让
   * `.trim()` 在用户手输数字时抛 `TypeError`（校验静默失效、点保存毫无反应 ——
   * 真浏览器实测踩到过）；只用 number 又会丢掉 `2.00` 的精度表达。
   * 所以统一过 {@link textOf} 归一再判。
   */
  duration_days: string | number;
  /** 1 带薪 / 0 无薪 */
  paid: number;
  reason: string;
  remark: string;
}

/** 默认假别（草稿第一项，避免用户忘选后收到 400） */
export const LEAVE_DEFAULT_TYPE: LeaveType = 'ANNUAL';

/** 把 `string | number | null | undefined` 归一成去空白的字符串（表单字段的唯一归一入口） */
export function textOf(value: string | number | null | undefined): string {
  if (value === null || value === undefined) {
    return '';
  }
  return typeof value === 'string' ? value.trim() : String(value);
}

/** 空表单 */
export function emptyLeaveForm(employeeId = ''): LeaveForm {
  return {
    employee_id: employeeId,
    leave_type: LEAVE_DEFAULT_TYPE,
    start_time: '',
    end_time: '',
    duration_days: '',
    paid: PAID_YES,
    reason: '',
    remark: '',
  };
}

/**
 * 详情行 → 表单（改草稿用）。
 *
 * 时刻转成 `yyyy-MM-dd HH:mm` 交给 `t-date-picker`（它只认这个格式，喂 ISO 带
 * `+08:00` 会显示空）。天数原样带尾零，提交时后端也收 `"1.50"`。
 */
export function formFromRow(row: LeaveListRow): LeaveForm {
  return {
    employee_id: row.employee_id,
    leave_type: row.leave_type,
    start_time: datetimeText(row.start_time),
    end_time: datetimeText(row.end_time),
    duration_days: row.duration_days,
    paid: row.paid,
    reason: row.reason ?? '',
    remark: row.remark ?? '',
  };
}

/** 校验错误（`field` 用于表单定位，`message` 直接显示） */
export interface LeaveFormError {
  field: keyof LeaveForm;
  message: string;
}

/** 与后端 `leave_dto` 同值的上限（改后端要同步改这里，单测会对拍） */
export const LEAVE_MAX_REASON = 500;
export const LEAVE_MAX_REMARK = 1000;
export const LEAVE_MIN_CANCEL_REASON = 5;
export const LEAVE_MAX_CANCEL_REASON = 255;

/**
 * 表单校验（**纯函数**，与后端 `create_of` 的判据逐条对齐）。
 *
 * 前端先拦一遍不是为了替代后端，而是为了：
 * ① 报错能指到具体输入框（后端只回一句 400 报文）；
 * ② 「天数 vs 起止跨度」这种最容易填错的，现场就提示（否则单提交成功、
 *    审批通过后才在工资表上显形）。
 *
 * 顺序照后端：先逐字段，最后才判「天数 ≤ 跨度」（否则一个假别填错的单会先收到
 * 关于天数的报错，改完再改类型）。
 *
 * @param requireEmployee 建单要员工；改草稿时员工不可改，传 `false`
 */
export function validateLeaveForm(form: LeaveForm, requireEmployee = true): LeaveFormError[] {
  const errors: LeaveFormError[] = [];

  if (requireEmployee && !(form.employee_id ?? '').trim()) {
    errors.push({ field: 'employee_id', message: '请选择员工' });
  }
  if (!LEAVE_TYPE_VALUES.includes((form.leave_type ?? '') as LeaveType)) {
    errors.push({ field: 'leave_type', message: '请选择假别' });
  }

  const start = (form.start_time ?? '').trim();
  const end = (form.end_time ?? '').trim();
  if (!start) {
    errors.push({ field: 'start_time', message: '请选择请假开始时间' });
  }
  if (!end) {
    errors.push({ field: 'end_time', message: '请选择请假结束时间' });
  }
  const startMinute = parseStamp(start);
  const endMinute = parseStamp(end);
  if (start && end && startMinute !== null && endMinute !== null && endMinute <= startMinute) {
    errors.push({ field: 'end_time', message: '结束时间必须晚于开始时间（半天假用 12:00 切）' });
  }

  // `duration_days` 可能是 number（t-input-number 写回）或 string（后端回填 "2.00"）
  const durationTextValue = textOf(form.duration_days);
  const duration = Number(durationTextValue);
  if (!durationTextValue) {
    errors.push({ field: 'duration_days', message: '请填请假天数（半天填 0.5，一天填 1）' });
  } else if (!Number.isFinite(duration) || duration <= 0) {
    errors.push({ field: 'duration_days', message: '请假天数必须大于 0' });
  } else if (!isHalfStep(duration)) {
    errors.push({ field: 'duration_days', message: '请假天数只能以 0.5 天为步长（0.5 / 1 / 1.5 …）' });
  } else {
    const span = daySpanOf(start, end);
    if (span !== null && duration > span) {
      errors.push({
        field: 'duration_days',
        message: `请假天数 ${durationTextValue} 超过了起止覆盖的自然日数 ${span} 天：请假天数进薪资扣款，必须与起止对得上`,
      });
    }
  }

  if (form.paid !== PAID_YES && form.paid !== PAID_NO) {
    errors.push({ field: 'paid', message: '带薪与否只能选带薪或无薪' });
  }

  if ((form.reason ?? '').length > LEAVE_MAX_REASON) {
    errors.push({ field: 'reason', message: `请假事由最多 ${LEAVE_MAX_REASON} 字` });
  }
  if ((form.remark ?? '').length > LEAVE_MAX_REMARK) {
    errors.push({ field: 'remark', message: `备注最多 ${LEAVE_MAX_REMARK} 字` });
  }

  return errors;
}

/** 是否 0.5 的整数倍（用 `*2` 取整判，避免 `0.1+0.2` 这类浮点噪声） */
export function isHalfStep(days: number): boolean {
  if (!Number.isFinite(days)) {
    return false;
  }
  const doubled = days * 2;
  return Math.abs(doubled - Math.round(doubled)) < 1e-9;
}

/** `yyyy-MM-dd HH:mm` → 分钟序号（跨天可比大小）；认不出 `null` */
function parseStamp(raw: string): number | null {
  const parts = parseDayParts(raw);
  const minute = minuteOfDay(raw);
  if (!parts || minute === null) {
    return null;
  }
  return Date.UTC(parts.y, parts.m - 1, parts.d) / 60_000 + minute;
}

/** 表单 → 建单入参（空文本转 `undefined`：后端 `optional_text` 也是这么处理的） */
export function toCreatePayload(form: LeaveForm): LeaveCreatePayload {
  const payload: LeaveCreatePayload = {
    employee_id: form.employee_id.trim(),
    leave_type: form.leave_type,
    start_time: form.start_time.trim(),
    end_time: form.end_time.trim(),
    duration_days: textOf(form.duration_days),
    paid: form.paid,
  };
  const reason = (form.reason ?? '').trim();
  if (reason) {
    payload.reason = reason;
  }
  const remark = (form.remark ?? '').trim();
  if (remark) {
    payload.remark = remark;
  }
  return payload;
}

/**
 * 表单 → 改草稿入参。
 *
 * **部分更新**语义：只带变化的字段会让「用户清空事由」与「用户没动事由」无法区分
 * （后端「缺省 = 不改」「空串 = 清空」），所以这里**全量带**，清空传空串 ——
 * 页面在提交前已确认表单是从详情回填的，全量带不会误清。
 */
export function toUpdatePayload(form: LeaveForm): LeaveUpdatePayload {
  return {
    leave_type: form.leave_type,
    start_time: form.start_time.trim(),
    end_time: form.end_time.trim(),
    duration_days: textOf(form.duration_days),
    paid: form.paid,
    reason: (form.reason ?? '').trim(),
    remark: (form.remark ?? '').trim(),
  };
}

/** 作废原因校验（三条路都要求 ≥5 字，后端 `cancel_of`） */
export function validateCancelReason(reason: string): string {
  const text = (reason ?? '').trim();
  if (!text) {
    return `请填原因（至少 ${LEAVE_MIN_CANCEL_REASON} 个字：会改掉已建立的考勤口径，要能交代为什么）`;
  }
  if (text.length < LEAVE_MIN_CANCEL_REASON) {
    return `原因至少 ${LEAVE_MIN_CANCEL_REASON} 个字（当前 ${text.length} 个）`;
  }
  if (text.length > LEAVE_MAX_CANCEL_REASON) {
    return `原因最多 ${LEAVE_MAX_CANCEL_REASON} 字`;
  }
  return '';
}

// ==================== tab ====================

/** 页内 tab：全部请假（按数据权限） / 我的请假（`mine=true`） */
export const LEAVE_TABS = ['all', 'mine'] as const;
/** tab 码值 */
export type LeaveTab = (typeof LEAVE_TABS)[number];

/** 默认 tab（有人事权限的人先看全员；自助角色只看自己的，见 `defaultLeaveTab`） */
export function defaultLeaveTab(canListAll: boolean): LeaveTab {
  return canListAll ? 'all' : 'mine';
}

/** 本 tab 是否勾 `mine`（唯一的差别就在这里，不要复制两份请求逻辑） */
export function tabIsMine(tab: LeaveTab): boolean {
  return tab === 'mine';
}

// ==================== 审批时间线口径 ====================

/** 待办状态 → 中文（`leave_handler::task_status_label` 逐值一致；后端也会下发 `status_label`） */
export const APPROVAL_TASK_LABELS: Record<string, string> = {
  PENDING: '待审批',
  APPROVED: '已同意',
  REJECTED: '已驳回',
  SKIPPED: '已跳过',
  TRANSFERRED: '已转办',
  CANCELLED: '已终止',
  WITHDRAWN: '已撤回',
};

/** 待办状态 → 中文，未知值回原值（不吞码值） */
export function approvalTaskLabel(code: string | null | undefined): string {
  if (!code) {
    return '';
  }
  return APPROVAL_TASK_LABELS[code] ?? code;
}

/** 待办状态 → 时间线节点色：待审批蓝、同意绿、驳回红、跳过/终止中性 */
export function approvalTaskTheme(code: string | null | undefined): 'default' | 'primary' | 'success' | 'danger' {
  switch ((code ?? '').trim().toUpperCase()) {
    case 'APPROVED':
      return 'success';
    case 'REJECTED':
      return 'danger';
    case 'PENDING':
      return 'primary';
    default:
      return 'default';
  }
}

// ==================== 权限码（与 `V0387` + `V0401__hr_page_code_split` 逐字一致） ====================

/**
 * 本页权限码。
 *
 * # 页面码 ≠ 接口码（卡 t_0af7ec46 解耦后）
 *
 * `page` 是**页面显形码**（`sys_menu.menu_type=2` 那一行），只决定「侧边栏显不显示 /
 * 路由放不放行」；`list` 是**全量列表接口**的码（`GET /leave/page`），`detail` 是详情接口的码。
 * 员工自助只持有 `page` + `create` ⇒ 页能进、但 `/leave/page` 403，自己的请假走
 * `/leave/my`（登录即可）。
 */
export const LEAVE_PERMS = {
  /** 页面显形码：进页面（V0401 起由页面行承载，与接口码分开授予） */
  page: 'business:hr:leave:page',
  /** 全量请假列表接口（`GET /leave/page`，按数据权限出全员） */
  list: 'business:hr:leave:list',
  detail: 'business:hr:leave:detail',
  create: 'business:hr:leave:create',
  update: 'business:hr:leave:update',
  delete: 'business:hr:leave:delete',
  submit: 'business:hr:leave:submit',
  cancel: 'business:hr:leave:cancel',
} as const;

/**
 * 「我的请假」一行（`GET /leave/my` 的自助投影，后端 `self_service_dto::MyLeaveRes`）。
 *
 * 与 `LeaveListRow` 的差别就是「谁的信息」：这里**只有本人**，所以没有员工/部门快照，
 * 也没有审批实例 id 与 `locked`（改单编排态属 HR 视角）。
 */
export interface MyLeaveRow {
  id: string;
  /** 单号 `LV-yyyyMMdd-000001`（服务端取号） */
  leave_no: string;
  leave_type: string;
  leave_type_label: string;
  start_time: string;
  end_time: string;
  /** 天数（`Decimal` → 字符串，如 `"1.50"`） */
  duration_days: string;
  /** 是否带薪（后端是布尔，列表行用 0/1） */
  paid: boolean;
  status: string;
  status_label: string;
  reason: string | null;
  submitted_at: string | null;
  cancelled_at: string | null;
  cancel_reason: string | null;
}

/**
 * 「我的请假」一行（`GET /leave/my` 的自助投影）→ 列表行。
 *
 * # 为什么要这一层映射
 *
 * 解耦后没有 `leave:list` 的人（员工自助）不能再调 `/leave/page`（403），「我的请假」
 * tab 改调 `/leave/my`。自助投影**故意不带**员工/部门快照（只出本人，没有「别人的名字」
 * 可显示），所以这里把 `paid` 从布尔转成列表用的 0/1、把缺失的快照字段留空 ——
 * 界面在缺少 `leave:list` 时会隐藏「员工 / 部门」两列（见页面 `columns`）。
 */
export function myLeaveToListRow(row: MyLeaveRow): LeaveListRow {
  const paid = row.paid ? 1 : 0;
  return {
    id: row.id,
    leave_no: row.leave_no,
    employee_id: '',
    employee_no: '',
    employee_name: '',
    dept_id: null,
    dept_name: '',
    leave_type: row.leave_type,
    leave_type_label: row.leave_type_label,
    start_time: row.start_time,
    end_time: row.end_time,
    duration_days: String(row.duration_days),
    paid,
    paid_label: paidLabel(paid),
    status: row.status,
    status_label: row.status_label,
    // 自助接口不下发 locked（那是「谁改过」的编排态），本人自己的单按状态判动作即可
    locked: false,
    reason: row.reason,
    approval_instance_id: null,
    submitted_at: row.submitted_at,
    submitted_by: null,
    submitted_by_name: null,
    cancelled_at: row.cancelled_at,
    cancelled_by: null,
    cancelled_by_name: null,
    cancel_reason: row.cancel_reason,
    remark: null,
    created_at: row.submitted_at ?? '',
    updated_at: row.submitted_at ?? '',
  };
}
