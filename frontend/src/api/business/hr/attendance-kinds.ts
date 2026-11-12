/**
 * 考勤页的出参契约与词表（ARCH-9 §5.2 #18~#22 · §6.1 / §6.2 · HR-29）。
 *
 * 纯逻辑文件：**不 import 请求层、不 import Vue**，`tsx --test` 里可以直接 import 做单测
 * （`tests/unit/hr-attendance-page.test.ts` 会读 Rust 源码把词表与上限对拍）。
 * 请求函数在 `./attendance.ts`，那里 `export * from './attendance-kinds'`，
 * 页面只认 `@/api/business/hr/attendance` 一个入口。
 *
 * # 字段名逐条抄自后端，不缩写
 *
 * | 块 | 后端出处 | 形状 |
 * |---|---|---|
 * | 期间 | `attendance_period_dto.rs::AttendancePeriodRes`（HR-19） | {@link AttendancePeriodRow} |
 * | 台账行 | `attendance_record_dto.rs::AttendanceRecordRes`（HR-20） | {@link AttendanceRecordRow} |
 * | 单行修正回执 | `attendance_record_dto.rs::RecordPatchRes` | {@link RecordPatchRes} |
 * | 重算回执 | `attendance_period_dto.rs::PeriodRecalcRes` | {@link PeriodRecalcRes} |
 *
 * # 五处与「想当然」不一样的地方（先读，都会让页面显示错值）
 *
 * 1. **天数 / 小时是字符串**（`Decimal` → JSON `"22.00"`）：不要 `+`，要显示就过
 *    {@link decimalText}（去尾零），要编辑就过 {@link decimalValue}（转 number 给
 *    `t-input-number`）—— 传 `"7.25"` 回后端仍是精确小数，不会变成 `7.250000000001`。
 *    `late_count` / `early_leave_count` 是 `i32` → 出参是**数字**（两列不同口径）。
 * 2. **台账能不能改问 `editable`，不问 `period_status`**：后端按
 *    `state_machine::ATTENDANCE_PERIOD_LOCKED` 算好下发（`LOCKED` 才 false）。前端再
 *    `match` 一遍状态串就是第二份真相，早晚漂移。
 * 3. **期间「编辑 / 删除 / 重算」只看 `locked`**：`SUBMITTED` 故意不锁（客户第一轮导入
 *    几乎总有错行，设计 §4.2.3 允许提交后重跑导入），所以后端 `update_period` /
 *    `recalc_period` 只拦 `LOCKED`。状态边只有三条
 *    （`ATTENDANCE_PERIOD_EDGES`）：`DRAFT→SUBMITTED`（提交）、`SUBMITTED→LOCKED`（锁定）、
 *    `LOCKED→DRAFT`（解锁，**必须 ≥5 字原因**）。
 * 4. **台账期间下拉默认只列 `DRAFT`/`SUBMITTED`**（设计 §6.2.3）。但锁定期间的台账正是
 *    「发工资前该核对的那份」（后端 `attendance_record_handler` 模块文档明说必须能看），
 *    所以页面上给一个「含已锁定」开关，勾上后才把 `LOCKED` 也列进来（只读）。
 * 5. **导入不落 `POST /business/hr/attendance/import`**：该路由随 **HR-21**（本卡父卡
 *    t_8c17e9f5）只交了 impexp 导入定义，HR 专属路由尚未注册；本页走 impexp 通用上传
 *    `POST /business/impexp/import/upload?biz_type=hr.attendance`（已在卡内上报）。
 *    考勤月靠 Excel 的「考勤月」列，**不是**上传参数 —— 明细里的考勤月与所选期间不一致时
 *    后端预检会带 Excel 行号拦下。
 *
 * # 姓名 / 部门的口径（`AGENTS.md` 约定 13）
 *
 * `employee_no` / `employee_name` 是**考勤发生时的快照**（工资按它出表，改名后台账仍显示
 * 当时是谁）；`dept_name` 是实时 join（部门改名/撤并后显示现在的名字才对）。
 * 页面直接显示后端下发的 `*_label` / `*_name`，本文件的 `*_LABELS` 只用于**下拉选项**与兜底。
 */

// ==================== 权限码（与 `V0387__hr_menu_perms` 逐字一致） ====================

/**
 * 本页 6 个权限码。
 *
 * 页面外壳 + 期间列表用 `attendance:list`；期间维护（建 / 改 / 删 / 重算）用
 * `attendance:period`；状态流转（提交 / 锁定 / 解锁）用 `attendance:lock`
 * （设计 §4.2.3：三条边共用一个码，避免「能提交不能锁」把期间停在半路）；
 * 台账与单行修正用 `attendance:record`；导入用 `attendance:import`。
 */
export const ATTENDANCE_PERMS = {
  /** 页面外壳 + 期间列表 / 台账查询（`GET .../period/page`） */
  list: 'business:hr:attendance:list',
  /** 期间维护：建 / 改 / 删 / 汇总重算 */
  period: 'business:hr:attendance:period',
  /** 台账：列表 + 单行修正 */
  record: 'business:hr:attendance:record',
  /** 导入考勤（Excel） */
  import: 'business:hr:attendance:import',
  /** 导出考勤台账 xlsx（HR-30；含「锁定状态」列） */
  export: 'business:hr:attendance:export',
  /** 提交 / 锁定 / 解锁 */
  lock: 'business:hr:attendance:lock',
} as const;

// ==================== 状态域（与 `ck_biz_att_period_status` 逐字一致） ====================

/** 三个状态（顺序 = 状态机正向顺序 `ATTENDANCE_PERIOD_EDGES`） */
export const PERIOD_STATUS_VALUES = ['DRAFT', 'SUBMITTED', 'LOCKED'] as const;

/** 期间状态码值 */
export type PeriodStatus = (typeof PERIOD_STATUS_VALUES)[number];

/** 状态 → 中文（`attendance_period_dto::status_label` 逐字一致） */
export const PERIOD_STATUS_LABELS: Record<PeriodStatus, string> = {
  DRAFT: '草稿',
  SUBMITTED: '已提交',
  LOCKED: '已锁定',
};

/** 状态 → 标签主题（草稿灰、已提交蓝、已锁定红：锁了就不能改，要跳出来） */
export const PERIOD_STATUS_THEMES: Record<PeriodStatus, 'default' | 'primary' | 'danger'> = {
  DRAFT: 'default',
  SUBMITTED: 'primary',
  LOCKED: 'danger',
};

/** 状态筛选下拉（顺序与后端常量一致） */
export const PERIOD_STATUS_OPTIONS = PERIOD_STATUS_VALUES.map((value) => ({
  value,
  label: PERIOD_STATUS_LABELS[value],
}));

/**
 * 台账期间下拉的默认状态集（设计 §6.2.3「只列 `DRAFT`/`SUBMITTED`」）。
 *
 * 「含已锁定」开关打开时再追加 `LOCKED`（见文件头第 4 条）。
 */
export const LEDGER_PERIOD_STATUSES: string[] = ['DRAFT', 'SUBMITTED'];

/** 台账里可编辑的期间状态（反向 = 已锁定；与后端 `editable` 同域） */
export const RECORD_EDITABLE_STATUSES: string[] = ['DRAFT', 'SUBMITTED'];

/** 状态码 → 中文，未知值回原值（排障时看到码值比看到「未知」有用） */
export function periodStatusLabel(code: string | null | undefined): string {
  const text = (code ?? '').trim();
  if (!text) {
    return '-';
  }
  return PERIOD_STATUS_LABELS[text as PeriodStatus] ?? text;
}

/** 状态码 → 标签主题，未知值走默认灰 */
export function periodStatusTheme(code: string | null | undefined): 'default' | 'primary' | 'danger' {
  const text = (code ?? '').trim();
  return PERIOD_STATUS_THEMES[text as PeriodStatus] ?? 'default';
}

/** 台账行的来源（`ck_biz_att_rec_source`） */
export const SOURCE_VALUES = ['IMPORT', 'MANUAL', 'LEAVE_SYNC'] as const;

/** 来源码值 */
export type RecordSource = (typeof SOURCE_VALUES)[number];

/** 来源 → 中文（`attendance_record_dto::source_label` 逐字一致） */
export const SOURCE_LABELS: Record<RecordSource, string> = {
  IMPORT: '导入',
  MANUAL: '手工',
  LEAVE_SYNC: '请假同步',
};

/** 来源码 → 中文，未知值回原值 */
export function sourceLabel(code: string | null | undefined): string {
  const text = (code ?? '').trim();
  if (!text) {
    return '-';
  }
  return SOURCE_LABELS[text as RecordSource] ?? text;
}

// ==================== 长度 / 跨度上限（与后端 DTO 常量逐条对齐） ====================

/** `period_code` 长度（`yyyy-MM` 七个字符） */
export const PERIOD_CODE_LEN = 7;

/** `remark` 上限（`MAX_REMARK`） */
export const PERIOD_MAX_REMARK = 500;

/** 一个考勤周期的最大跨度（天，含首尾；`MAX_SPAN_DAYS`） */
export const MAX_SPAN_DAYS = 31;

/** 解锁原因最小字数（`MIN_REASON_CHARS`） */
export const MIN_UNLOCK_REASON = 5;

/** 台账每页条数（与后端 `DEFAULT_PAGE_SIZE` 同口径） */
export const ATTENDANCE_PAGE_SIZE = 20;

/** 台账每页上限（后端 `MAX_PAGE_SIZE`） */
export const ATTENDANCE_MAX_PAGE_SIZE = 200;

/** 导入时用到的业务编码（impexp 定义注册名） */
export const ATTENDANCE_BIZ_TYPE = 'hr.attendance';

/** 导入模板/定义的中文名（按钮与弹窗标题用） */
export const ATTENDANCE_BIZ_LABEL = '考勤台账';

// ==================== 导出（后端 `attendance_export.rs` 的镜像） ====================

/** 单次导出上限（后端 `EXPORT_MAX_ROWS`；超限时后端在响应头报截断） */
export const ATTENDANCE_EXPORT_MAX_ROWS = 5000;

/**
 * 导出表「锁定状态」列的两个值（后端 `LOCK_LABEL_LOCKED` / `LOCK_LABEL_UNLOCKED`）。
 *
 * 导出文件由后端生成，这里的词表只用于**页面提示**（「这份表是已锁定的工资基线」），
 * 与后端逐字一致才不至于「界面说未锁定、Excel 里写已锁定」。
 */
export const EXPORT_LOCK_LABELS = {
  locked: '已锁定',
  unlocked: '未锁定',
} as const;

/**
 * 导出的默认文件名：`考勤台账_2026-11_已锁定.xlsx`。
 *
 * 后端响应头里本来就有 `Content-Disposition`（用不上时也不影响），
 * 但 `saveBlob` 需要一个名字，而 `Axios` 的 blob 响应在跨域/代理下取响应头并不可靠 ——
 * 直接用「期间 + 锁定态」在本地拼，文件名一定对得上这份数据。
 */
export function attendanceExportFileName(periodCode: string, locked: boolean): string {
  const code = (periodCode || '').trim() || '未知期间';
  const state = locked ? EXPORT_LOCK_LABELS.locked : EXPORT_LOCK_LABELS.unlocked;
  return `考勤台账_${code}_${state}.xlsx`;
}

// ==================== 出参类型（逐字段抄后端） ====================

/** 期间头四个汇总快照（`PeriodTotals`） */
export interface PeriodTotals {
  /** 期间内记录条数（= 参与本期的员工数） */
  employee_count: number;
  /** 应出勤天数合计 */
  should_days_total: string;
  /** 实际出勤天数合计 */
  actual_days_total: string;
  /** 加班小时合计（平时 + 休息日 + 法定节假日） */
  ot_hours_total: string;
}

/** 一条考勤期间（`AttendancePeriodRes`） */
export interface AttendancePeriodRow {
  /** 主键 */
  id: string;
  /** 考勤月 `yyyy-MM` */
  period_code: string;
  /** 起（含）`yyyy-MM-dd` */
  period_start: string;
  /** 止（含） */
  period_end: string;
  /** 状态码值 */
  status: string;
  /** 状态中文名 */
  status_label: string;
  /** 是否已锁定（`status === 'LOCKED'`；前端禁按钮只认这个） */
  locked: boolean;
  /** 期间内记录条数 */
  employee_count: number;
  /** 应出勤天数合计 */
  should_days_total: string;
  /** 实际出勤天数合计 */
  actual_days_total: string;
  /** 加班小时合计 */
  ot_hours_total: string;
  /** 最近一次导入的 `sys_import_task.id` */
  import_task_id?: string | null;
  /** 最近一次提交时间 */
  submitted_at?: string | null;
  /** 最近一次提交人 */
  submitted_by?: string | null;
  /** 最近一次提交人姓名 */
  submitted_by_name?: string | null;
  /** 最近一次锁定时间（解锁后**保留**，不是「当前已锁定」的判据） */
  locked_at?: string | null;
  /** 最近一次锁定人 */
  locked_by?: string | null;
  /** 最近一次锁定人姓名 */
  locked_by_name?: string | null;
  /** 备注 */
  remark?: string | null;
  /** 创建时间 */
  created_at: string;
  /** 最后修改时间 */
  updated_at: string;
}

/** 台账行的 11 个数值列（`RecordNumbers`，摊平进 `AttendanceRecordRow`） */
export interface RecordNumbers {
  /** 应出勤天数 */
  should_days: string;
  /** 实际出勤天数 */
  actual_days: string;
  /** 带薪假天数 */
  leave_paid_days: string;
  /** 无薪假天数（→ 薪资扣款） */
  leave_unpaid_days: string;
  /** 病假天数 */
  sick_days: string;
  /** 旷工天数 */
  absent_days: string;
  /** 平时加班小时 */
  ot_hours_workday: string;
  /** 休息日加班小时 */
  ot_hours_weekend: string;
  /** 法定节假日加班小时 */
  ot_hours_holiday: string;
  /** 迟到次数（`i32` → 数字） */
  late_count: number;
  /** 早退次数（`i32` → 数字） */
  early_leave_count: number;
}

/** 台账一行（`AttendanceRecordRes`，一人一行） */
export interface AttendanceRecordRow extends RecordNumbers {
  /** 记录主键 */
  id: string;
  /** 所属期间 */
  period_id: string;
  /** 考勤月 */
  period_code: string;
  /** 期间状态码值 */
  period_status: string;
  /** 期间状态中文 */
  period_status_label: string;
  /** 这一行能不能改（`period_status !== 'LOCKED'`，后端算好下发） */
  editable: boolean;
  /** 员工 id */
  employee_id: string;
  /** 工号（**快照列**） */
  employee_no: string;
  /** 姓名（**快照列**） */
  employee_name: string;
  /** 部门 id */
  dept_id?: string | null;
  /** 部门名（实时 join） */
  dept_name?: string | null;
  /** 加班合计（三列之和） */
  ot_hours_total: string;
  /** 来源码值 */
  source: string;
  /** 来源中文 */
  source_label: string;
  /** 备注 */
  remark?: string | null;
  /** 最后修改时间 */
  updated_at: string;
  /** 最后修改人 */
  updated_by?: string | null;
  /** 最后修改人姓名 */
  updated_by_name?: string | null;
}

/** 单行修正回执（`RecordPatchRes`） */
export interface RecordPatchRes {
  /** 改后的这一行 */
  record: AttendanceRecordRow;
  /** 是否有实际变化（false = 值没动，库里没写也没留痕） */
  changed: boolean;
  /** 一致性警告（设计 §4.9 W1 只报不拦） */
  warnings: string[];
  /** 修正后**同事务重算**的期间汇总 */
  period_totals: PeriodTotals;
}

/** 汇总重算回执（`PeriodRecalcRes`） */
export interface PeriodRecalcRes {
  /** 本次参与汇总的记录条数 */
  scanned: number;
  /** 重算前 */
  before: PeriodTotals;
  /** 重算后 */
  after: PeriodTotals;
  /** 快照是否变了（false = 幂等） */
  changed: boolean;
  /** 重算后的期间全量 */
  period: AttendancePeriodRow;
}

/** 单行修正入参（部分更新：一次只发**一个**字段） */
export interface RecordPatchPayload {
  should_days?: string;
  actual_days?: string;
  leave_paid_days?: string;
  leave_unpaid_days?: string;
  sick_days?: string;
  absent_days?: string;
  ot_hours_workday?: string;
  ot_hours_weekend?: string;
  ot_hours_holiday?: string;
  late_count?: number;
  early_leave_count?: number;
  remark?: string | null;
}

// ==================== 可编辑数值列的元数据 ====================

/** 数值列的键（11 个，与 `RecordNumbers` 同名） */
export type NumberField =
  | 'should_days'
  | 'actual_days'
  | 'leave_paid_days'
  | 'leave_unpaid_days'
  | 'sick_days'
  | 'absent_days'
  | 'ot_hours_workday'
  | 'ot_hours_weekend'
  | 'ot_hours_holiday'
  | 'late_count'
  | 'early_leave_count';

/** 一列的元数据：标题、单位、小数位、上限 */
export interface NumberFieldMeta {
  /** 与后端出参同名 */
  key: NumberField;
  /** 表头 */
  label: string;
  /** 单位（tooltip 用） */
  unit: '天' | '小时' | '次';
  /** 小数位（`t-input-number` 的 `decimal-places`；次数为 0） */
  decimals: number;
  /** 上限（天数 31 / 小时 744 / 次数 62，与后端 `validate` 同口径） */
  max: number;
  /** 步长（天数 / 小时 0.5，次数 1） */
  step: number;
  /** 是否整数列（出参是数字而不是 Decimal 字符串） */
  integer: boolean;
}

/**
 * 11 个数值列（顺序 = 设计 §6.2.3 的表格列序）。
 *
 * 上限不是「合理性」判断而是「显然不可能」判断（与后端
 * `attendance_record_dto::RecordNumbers::validate` 同一份口径）：一个月最多 31 天、
 * 31 天 × 24 小时 = 744 小时、31 天 × 每天 2 次 = 62 次。
 */
export const NUMBER_FIELDS: NumberFieldMeta[] = [
  { key: 'should_days', label: '应出勤', unit: '天', decimals: 2, max: 31, step: 0.5, integer: false },
  { key: 'actual_days', label: '实际出勤', unit: '天', decimals: 2, max: 31, step: 0.5, integer: false },
  { key: 'leave_paid_days', label: '带薪假', unit: '天', decimals: 2, max: 31, step: 0.5, integer: false },
  { key: 'leave_unpaid_days', label: '无薪假', unit: '天', decimals: 2, max: 31, step: 0.5, integer: false },
  { key: 'sick_days', label: '病假', unit: '天', decimals: 2, max: 31, step: 0.5, integer: false },
  { key: 'absent_days', label: '旷工', unit: '天', decimals: 2, max: 31, step: 0.5, integer: false },
  { key: 'ot_hours_workday', label: '平时加班', unit: '小时', decimals: 2, max: 744, step: 0.5, integer: false },
  { key: 'ot_hours_weekend', label: '休息日加班', unit: '小时', decimals: 2, max: 744, step: 0.5, integer: false },
  { key: 'ot_hours_holiday', label: '节假日加班', unit: '小时', decimals: 2, max: 744, step: 0.5, integer: false },
  { key: 'late_count', label: '迟到', unit: '次', decimals: 0, max: 62, step: 1, integer: true },
  { key: 'early_leave_count', label: '早退', unit: '次', decimals: 0, max: 62, step: 1, integer: true },
];

/** 按 key 取列元数据（模板里逐列渲染用；未知 key 不返回） */
export function numberFieldMeta(key: NumberField): NumberFieldMeta {
  return NUMBER_FIELDS.find((item) => item.key === key) ?? NUMBER_FIELDS[0];
}

// ==================== 表格列定义 ====================

/** 期间列表列（`colKey` 与后端出参同名；`op` 是插槽列） */
export const ATTENDANCE_PERIOD_COLUMNS = [
  { colKey: 'period_code', title: '考勤月', width: 110 },
  { colKey: 'range', title: '考勤周期', width: 200 },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'employee_count', title: '人数', width: 70, align: 'right' as const },
  { colKey: 'should_days_total', title: '应出勤合计', width: 110, align: 'right' as const },
  { colKey: 'actual_days_total', title: '实际合计', width: 100, align: 'right' as const },
  { colKey: 'ot_hours_total', title: '加班合计', width: 100, align: 'right' as const },
  { colKey: 'audit', title: '提交 / 锁定', width: 190, ellipsis: true },
  { colKey: 'remark', title: '备注', width: 140, ellipsis: true },
  { colKey: 'op', title: '操作', width: 230, fixed: 'right' as const },
];

/** 台账列（前 4 列固定 + 11 个可编辑数值列 + 尾部） */
export const ATTENDANCE_RECORD_COLUMNS = [
  { colKey: 'employee_no', title: '工号', width: 110, fixed: 'left' as const },
  { colKey: 'employee_name', title: '姓名', width: 100, fixed: 'left' as const },
  { colKey: 'dept_name', title: '部门', width: 120, ellipsis: true },
  ...NUMBER_FIELDS.map((field) => ({
    colKey: field.key,
    title: field.label,
    width: field.unit === '次' ? 84 : 96,
    align: 'right' as const,
  })),
  { colKey: 'ot_hours_total', title: '加班合计', width: 96, align: 'right' as const },
  { colKey: 'source', title: '来源', width: 84 },
  { colKey: 'updated', title: '最后修改', width: 150, ellipsis: true },
];

// ==================== 数值格式化（纯函数） ====================

/**
 * Decimal 字符串 → 去尾零的展示文本（`"22.00"` → `"22"`、`"7.50"` → `"7.5"`）。
 *
 * 只做字符串处理，不 `Number()`：`"0.1" + "0.2"` 在浮点里是 `0.30000000000000004`，
 * 而台账的数字要能原样回到后端（金额/天数一分不差，设计 §5.1 的硬口径）。
 * 非数字文本原样返回（后端出参异常时宁可显示原文）。
 */
export function decimalText(value: string | number | null | undefined): string {
  if (value === null || value === undefined) {
    return '0';
  }
  const raw = String(value).trim();
  if (!raw) {
    return '0';
  }
  if (!/^-?\d+(\.\d+)?$/.test(raw)) {
    return raw;
  }
  const negative = raw.startsWith('-');
  const body = negative ? raw.slice(1) : raw;
  const [intPart, fracPart = ''] = body.split('.');
  const trimmedFrac = fracPart.replace(/0+$/, '');
  const normalizedInt = intPart.replace(/^0+(?=\d)/, '');
  const text = trimmedFrac ? `${normalizedInt}.${trimmedFrac}` : normalizedInt;
  if (text === '0') {
    return '0';
  }
  return negative ? `-${text}` : text;
}

/**
 * Decimal 字符串 → `t-input-number` 用的 number。
 *
 * 小数位按列元数据截断（库列是 `NUMERIC(6,2)` / `NUMERIC(8,2)`），避免输入框里出现
 * `7.250000000000001` 这类浮点尾巴。拿不到数字时回 0（输入框不接受 NaN）。
 */
export function decimalValue(value: string | number | null | undefined, decimals = 2): number {
  const text = decimalText(value);
  const parsed = Number(text);
  if (!Number.isFinite(parsed)) {
    return 0;
  }
  return Number(parsed.toFixed(decimals));
}

/** 台账行某一列的展示文本（次数列不走 decimalText：`3` 而不是 `3.00`） */
export function cellText(row: AttendanceRecordRow, key: NumberField): string {
  const meta = numberFieldMeta(key);
  const raw = row[key];
  return meta.integer ? String(Number(raw) || 0) : decimalText(raw);
}

/** ISO8601 → `2026-11-10 09:00`（只切字符串，不转时区；后端已按 `+08:00` 出参） */
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

/** 取不到值时显示的空位 */
export function textOr(value: string | null | undefined): string {
  const text = (value ?? '').trim();
  return text.length > 0 ? text : '-';
}

/** 期间周期文本：`2026-10-26 ~ 2026-11-25` */
export function periodRangeText(row: Pick<AttendancePeriodRow, 'period_start' | 'period_end'>): string {
  return `${row.period_start} ~ ${row.period_end}`;
}

/**
 * 期间下拉的选项文案：`2026-11（10-26 ~ 11-25）· 已锁定`。
 *
 * 下拉里必须带状态：`DRAFT` 与 `LOCKED` 的点进去一个是可编辑、一个是只读，
 * 只显示月份会让人以为「这个月怎么点都改不了」。
 */
export function periodOptionLabel(row: AttendancePeriodRow): string {
  const start = row.period_start.slice(5);
  const end = row.period_end.slice(5);
  return `${row.period_code}（${start} ~ ${end}）· ${row.status_label || periodStatusLabel(row.status)}`;
}

/** 期间选项（台账顶部下拉） */
export function periodOptions(rows: AttendancePeriodRow[]): { label: string; value: string }[] {
  return rows.map((row) => ({ label: periodOptionLabel(row), value: row.id }));
}

/** 提交 / 锁定审计文本：`提交 张三 11-02 09:10 · 锁定 —` */
export function periodAuditText(row: AttendancePeriodRow): string {
  const submitted = row.submitted_at
    ? `提交 ${row.submitted_by_name || '—'} ${datetimeText(row.submitted_at)}`
    : '提交 —';
  const locked = row.locked_at ? `锁定 ${row.locked_by_name || '—'} ${datetimeText(row.locked_at)}` : '锁定 —';
  return `${submitted} · ${locked}`;
}

// ==================== 期间动作态（与状态机三条边同域） ====================

/** 一个期间当前可做的动作 */
export interface PeriodActions {
  /** 编辑起止 / 备注（未锁定即可改） */
  edit: boolean;
  /** 删除（只有草稿能删，且期间下不能有记录 —— 后者由后端拦） */
  remove: boolean;
  /** 提交（`DRAFT → SUBMITTED`） */
  submit: boolean;
  /** 锁定（`SUBMITTED → LOCKED`） */
  lock: boolean;
  /** 解锁（`LOCKED → DRAFT`，要 ≥5 字原因） */
  unlock: boolean;
  /** 汇总重算（未锁定即可） */
  recalc: boolean;
  /** 看这一期的台账（任何状态都能看，锁定期只读） */
  ledger: boolean;
}

/**
 * 期间动作态（**唯一真相是后端的边表** `ATTENDANCE_PERIOD_EDGES`：
 * `DRAFT→SUBMITTED` / `SUBMITTED→LOCKED` / `LOCKED→DRAFT`）。
 *
 * 没有「`DRAFT → LOCKED` 一步到位」：锁定前必须先提交（设计 §4.2.3 的流转图），
 * 所以草稿行的「锁定」按钮不出现，而不是点了 400。
 */
export function periodActionsOf(status: string | null | undefined): PeriodActions {
  const code = (status ?? '').trim();
  const locked = code === 'LOCKED';
  return {
    edit: !locked,
    remove: code === 'DRAFT',
    submit: code === 'DRAFT',
    lock: code === 'SUBMITTED',
    unlock: locked,
    recalc: !locked,
    ledger: true,
  };
}

// ==================== 期间筛选 / 表单 ====================

/** 期间筛选（页面上的一行条件） */
export interface PeriodFilter {
  /** 关键字（考勤月模糊） */
  keyword: string;
  /** 状态多选 */
  status: string[];
  /** 年份 */
  year: string;
}

/** 空筛选 */
export function emptyPeriodFilter(): PeriodFilter {
  return { keyword: '', status: [], year: '' };
}

/** 筛选是否有条件（空态文案分「还没有期间」和「没有匹配」） */
export function isPeriodFiltered(filter: PeriodFilter): boolean {
  return Boolean(filter.keyword.trim()) || filter.status.length > 0 || Boolean(filter.year.trim());
}

/**
 * 期间筛选 → 查询串。
 *
 * `status` 用**逗号分隔**（后端 `PeriodPageQuery.status: Option<String>` 自己拆，
 * 重复键 `status=DRAFT&status=LOCKED` 会被 `serde_urlencoded` 只取最后一个）。
 */
export function buildPeriodQuery(
  filter: PeriodFilter,
  page: number,
  pageSize: number,
): Record<string, unknown> {
  const query: Record<string, unknown> = {
    page: Math.max(1, Math.floor(page)),
    page_size: Math.min(ATTENDANCE_MAX_PAGE_SIZE, Math.max(1, Math.floor(pageSize))),
  };
  const keyword = filter.keyword.trim();
  if (keyword) {
    query.keyword = keyword;
  }
  if (filter.status.length > 0) {
    query.status = filter.status.join(',');
  }
  const year = filter.year.trim();
  if (year) {
    query.year = Number(year);
  }
  return query;
}

/** 建期间 / 改期间的表单 */
export interface PeriodForm {
  /** 考勤月（`yyyy-MM`；改期间时只读） */
  period_code: string;
  /** 起（含） */
  period_start: string;
  /** 止（含） */
  period_end: string;
  /** 备注 */
  remark: string;
}

/** 空表单（默认带当月与自然月区间，省掉手打两次日期） */
export function emptyPeriodForm(today = new Date()): PeriodForm {
  const year = today.getFullYear();
  const month = today.getMonth() + 1;
  const code = `${year}-${String(month).padStart(2, '0')}`;
  const lastDay = new Date(year, month, 0).getDate();
  return {
    period_code: code,
    period_start: `${code}-01`,
    period_end: `${code}-${String(lastDay).padStart(2, '0')}`,
    remark: '',
  };
}

/** 行 → 表单（编辑抽屉） */
export function periodFormFromRow(row: AttendancePeriodRow): PeriodForm {
  return {
    period_code: row.period_code,
    period_start: row.period_start,
    period_end: row.period_end,
    remark: row.remark ?? '',
  };
}

/** 表单校验错误 */
export interface FormError {
  /** 字段名 */
  field: string;
  /** 人话原因 */
  message: string;
}

/** `yyyy-MM` 且月份 01~12（与后端 `check_period_code` 同口径） */
export function isPeriodCode(text: string): boolean {
  if (text.length !== PERIOD_CODE_LEN || text[4] !== '-') {
    return false;
  }
  const year = text.slice(0, 4);
  const month = text.slice(5);
  if (!/^\d{4}$/.test(year) || !/^\d{2}$/.test(month)) {
    return false;
  }
  const value = Number(month);
  return value >= 1 && value <= 12;
}

/** `yyyy-MM-dd` 且是真实日期（`2026-02-30` 不算） */
export function isDateText(text: string): boolean {
  const matched = /^(\d{4})-(\d{2})-(\d{2})$/.exec(text);
  if (!matched) {
    return false;
  }
  const year = Number(matched[1]);
  const month = Number(matched[2]);
  const day = Number(matched[3]);
  if (month < 1 || month > 12 || day < 1) {
    return false;
  }
  const daysInMonth = new Date(year, month, 0).getDate();
  return day <= daysInMonth;
}

/** 两个 `yyyy-MM-dd` 之间的天数（含首尾；非日期返回 0） */
export function spanDaysOf(start: string, end: string): number {
  if (!isDateText(start) || !isDateText(end)) {
    return 0;
  }
  const from = Date.UTC(Number(start.slice(0, 4)), Number(start.slice(5, 7)) - 1, Number(start.slice(8, 10)));
  const to = Date.UTC(Number(end.slice(0, 4)), Number(end.slice(5, 7)) - 1, Number(end.slice(8, 10)));
  const diff = Math.round((to - from) / 86_400_000);
  return diff < 0 ? 0 : diff + 1;
}

/**
 * 建期间表单校验（判据逐条对齐后端 `parse_create`：格式 / 区间 / 跨度 / 备注长度）。
 *
 * 前端先挡是为了即时反馈（不用等一次 400 往返）；后端仍会再校验一遍 —— 两层都留。
 */
export function validatePeriodForm(form: PeriodForm): FormError[] {
  const errors: FormError[] = [];
  const code = form.period_code.trim();
  if (!code) {
    errors.push({ field: 'period_code', message: '考勤月不能为空，格式 yyyy-MM（如 2026-11）' });
  } else if (!isPeriodCode(code)) {
    errors.push({ field: 'period_code', message: `考勤月必须是 yyyy-MM 且月份 01~12（收到「${code}」）` });
  }
  const start = form.period_start.trim();
  const end = form.period_end.trim();
  if (!start) {
    errors.push({ field: 'period_start', message: '起始日期不能为空' });
  } else if (!isDateText(start)) {
    errors.push({ field: 'period_start', message: `起始日期格式应为 yyyy-MM-dd（收到「${start}」）` });
  }
  if (!end) {
    errors.push({ field: 'period_end', message: '截止日期不能为空' });
  } else if (!isDateText(end)) {
    errors.push({ field: 'period_end', message: `截止日期格式应为 yyyy-MM-dd（收到「${end}」）` });
  }
  if (isDateText(start) && isDateText(end)) {
    if (end < start) {
      errors.push({ field: 'period_end', message: `截止日期（${end}）不能早于起始日期（${start}）` });
    } else {
      const span = spanDaysOf(start, end);
      if (span > MAX_SPAN_DAYS) {
        errors.push({
          field: 'period_end',
          message: `考勤周期跨度 ${span} 天：月度批次最长 ${MAX_SPAN_DAYS} 天（自然月或「26 日 ~ 次月 25 日」的计薪周期）`,
        });
      }
    }
  }
  if (form.remark.trim().length > PERIOD_MAX_REMARK) {
    errors.push({ field: 'remark', message: `备注最长 ${PERIOD_MAX_REMARK} 个字符` });
  }
  return errors;
}

/** 表单 → 建期间入参（`PeriodCreateReq`） */
export function toPeriodCreatePayload(form: PeriodForm): Record<string, unknown> {
  return {
    period_code: form.period_code.trim(),
    period_start: form.period_start.trim(),
    period_end: form.period_end.trim(),
    remark: form.remark.trim() || undefined,
  };
}

/**
 * 表单 → 改期间入参（`PeriodUpdateReq`，部分更新语义）。
 *
 * **不带 `period_code`**：后端不收（它是期间的人工标识，报表/薪资/巡检都按它对齐）。
 * `remark` 显式传空串 = 清空备注（后端用 `double_option` 区分「缺省」与「清空」）。
 */
export function toPeriodUpdatePayload(form: PeriodForm): Record<string, unknown> {
  return {
    period_start: form.period_start.trim(),
    period_end: form.period_end.trim(),
    remark: form.remark.trim(),
  };
}

/**
 * 状态流转入参（`PeriodStatusReq`）。
 *
 * 解锁（`LOCKED → DRAFT`）必须 ≥5 字原因；其余边不带原因（后端会忽略）。
 */
export function statusChangePayload(toStatus: PeriodStatus, reason = ''): Record<string, unknown> {
  const trimmed = reason.trim();
  if (toStatus === 'DRAFT') {
    return { to_status: toStatus, reason: trimmed };
  }
  return { to_status: toStatus };
}

/** 解锁原因校验（空 / <5 字都要拦，后端 `parse_status` 同口径） */
export function validateUnlockReason(reason: string): string {
  const trimmed = reason.trim();
  if (!trimmed) {
    return `解锁必须填原因（≥ ${MIN_UNLOCK_REASON} 字）：锁定期间是薪资试算的基线，解开它会让已经算过的工资与考勤对不上`;
  }
  if (trimmed.length < MIN_UNLOCK_REASON) {
    return `解锁原因至少 ${MIN_UNLOCK_REASON} 个字（现在 ${trimmed.length} 个）`;
  }
  return '';
}

// ==================== 台账筛选 / 查询 ====================

/** 台账顶部的一行条件 */
export interface RecordFilter {
  /** 期间 id（必填 —— 台账按期间查） */
  period_id: string;
  /** 部门 id（`biz_attendance_record.dept_id` 快照） */
  dept_id: string;
  /** 关键字（工号 / 姓名） */
  keyword: string;
}

/** 空台账筛选 */
export function emptyRecordFilter(): RecordFilter {
  return { period_id: '', dept_id: '', keyword: '' };
}

/** 台账筛选是否有条件（部门 + 关键字；期间是必填项不算「筛过」） */
export function isRecordFiltered(filter: RecordFilter): boolean {
  return Boolean(filter.dept_id.trim()) || Boolean(filter.keyword.trim());
}

/**
 * 台账查询串（`RecordPageQuery`）。
 *
 * `period_id` 缺失时**不抛错、返回 `null`**：页面在期间列表还没加载完时不该打一次注定
 * 400 的请求（后端对缺 `period_id` 回 400「考勤台账按期间查」）。
 */
export function buildRecordQuery(
  filter: RecordFilter,
  page: number,
  pageSize: number,
): Record<string, unknown> | null {
  const periodId = filter.period_id.trim();
  if (!periodId) {
    return null;
  }
  const query: Record<string, unknown> = {
    period_id: periodId,
    page: Math.max(1, Math.floor(page)),
    page_size: Math.min(ATTENDANCE_MAX_PAGE_SIZE, Math.max(1, Math.floor(pageSize))),
  };
  const deptId = filter.dept_id.trim();
  if (deptId) {
    query.dept_id = deptId;
  }
  const keyword = filter.keyword.trim();
  if (keyword) {
    query.keyword = keyword;
  }
  return query;
}

/**
 * 单元格失焦 → 修正入参（**只发改动的那一列**）。
 *
 * 返回 `null` 表示「值没变」：此时不发请求（后端也会回 `changed=false`，但白跑一次
 * 接口、白闪一次 loading 没必要）。
 *
 * 天数 / 小时发**字符串**（`Decimal`，避开 JS 浮点），次数发数字（库列是 `INT`）。
 */
export function recordPatchOf(
  row: AttendanceRecordRow,
  key: NumberField,
  nextValue: number | string | null | undefined,
): RecordPatchPayload | null {
  const meta = numberFieldMeta(key);
  if (nextValue === null || nextValue === undefined || nextValue === '') {
    // `t-input-number` 清空时会回 null：按 0 处理（库列 NOT NULL DEFAULT 0），
    // 而不是发 `null` 让后端按「缺省 = 不改」忽略掉，让用户以为清空成功了
    nextValue = 0;
  }
  const nextText = decimalText(String(nextValue));
  const currentText = cellText(row, key);
  if (nextText === currentText) {
    return null;
  }
  if (meta.integer) {
    return { [key]: Math.max(0, Math.trunc(Number(nextText) || 0)) } as RecordPatchPayload;
  }
  return { [key]: nextText } as RecordPatchPayload;
}

/** 期间汇总 → 台账底部合计行文案 */
export function totalsText(totals: PeriodTotals): string {
  return `共 ${totals.employee_count} 人 · 应出勤 ${decimalText(totals.should_days_total)} 天 · 实际 ${
    decimalText(totals.actual_days_total)
  } 天 · 加班 ${decimalText(totals.ot_hours_total)} 小时`;
}

/** 期间行 → 汇总（台账顶部「本期合计」；选中期间即得，不用二次请求） */
export function totalsOfPeriod(row: AttendancePeriodRow): PeriodTotals {
  return {
    employee_count: row.employee_count,
    should_days_total: row.should_days_total,
    actual_days_total: row.actual_days_total,
    ot_hours_total: row.ot_hours_total,
  };
}

/** 空汇总（还没选期间时用） */
export function emptyTotals(): PeriodTotals {
  return { employee_count: 0, should_days_total: '0', actual_days_total: '0', ot_hours_total: '0' };
}
