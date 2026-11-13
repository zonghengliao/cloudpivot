/**
 * 薪资管理列表页的契约与纯逻辑（ARCH-9 §5.2 #36/#39/#40/#46 · §6.1 · 卡 t_fc256a30）。
 *
 * 纯逻辑文件：**不 import 请求层、不 import Vue**，`tsx --test` 里可以直接 import 做单测
 * （`tests/unit/hr-salary-list.test.ts` 会读 Rust 源码把词表 / 上限 / 权限码对拍）。
 * 请求函数在 `./salary.ts`，那里 `export * from './salary-list-kinds'`，
 * 页面只认 `@/api/business/hr/salary` 一个入口。
 *
 * # 这一页接的四条接口（路径 / 权限码逐条来自后端 `ROUTE_CONTRACT`）
 *
 * | # | 方法 | 路径（后端注解不含 `/api`） | 权限码 | 页面用在哪 |
 * |:--:|---|---|---|---|
 * | 36 | GET  | `/business/hr/salary/page` | `salary:list` | 「薪资单」tab 列表（期间 / 状态 / 单号筛选 + 分页） |
 * | 39 | POST | `/business/hr/salary/calc` | `salary:calc` | 「试算」：只算不落库（`dry_run=true`） |
 * | 40 | POST | `/business/hr/salary` | `salary:calc` | 「建单并保存」：同一次计算 `dry_run=false` |
 * | 46 | GET  | `/business/hr/salary/my` | `salary:my` | 「我的薪资」tab（员工自助，只出本人已定稿的工资条） |
 *
 * 详情页（#37/#38/#41~#45）在 `./salary-kinds.ts` + `./salary.ts`（HR-43 已落）。
 *
 * # 五处与「想当然」不一样的地方（先读，都会让页面点出 403 / 假按钮）
 *
 * 1. **`calc` 一个码管三条路由**：试算 / 建单 / 改明细（#39/#40/#41）都是
 *    `business:hr:salary:calc` —— 能算工资的人才能改算出来的数。所以本页两个按钮
 *    共用同一个权限判据，不要给「建单」单列一个码（V0387 里没有这个码）。
 * 2. **`dry_run` 必须与接口身份一致**：试算接口传 `dry_run=false` 会被后端 400
 *    （`parse_calc_req` 拿它与 `CalcMode` 对拍）。所以报文一律由 {@link buildCalcPayload}
 *    按 `mode` 生成，页面不许手写 `dry_run`。
 * 3. **`status` 拼错是 400 不是「筛完还是全量」**：后端 `parse_list_query` 对
 *    `STATUSES` 白名单外的值直接拒。页面的状态下拉只从
 *    `SALARY_STATUS_OPTIONS`（六档，与 `ck_biz_sal_sheet_status` 同域）取。
 * 4. **期间码是 `yyyy-MM` 且月份必须 `01~12`**：库层 `ck_biz_sal_sheet_code` 是
 *    `^[0-9]{4}-(0[1-9]|1[0-2])$`，后端 `period_range` 还会真的算这个月的天数。
 *    前端先手拦（{@link periodCodeError}），用户不用等一次往返才知道 `2026-13` 不对。
 * 5. **「我的薪资」默认只出已定稿三档**（`APPROVED` / `POSTED` / `REVERSED`）：
 *    在途单（草稿 / 已计算 / 审批中）的金额还会变，让员工先看到中途版本 =
 *    「为什么少了 300」的客服成本（后端 `self_service_dto::VISIBLE_STATUSES`）。
 *    `include_pending=true` 只是给能算工资的账号自查用，本页把它挂在 `salary:calc` 权限后。
 *
 * # 「我的薪资」为什么是本页的一个 tab，而不是新菜单
 *
 * 设计 §6.1 定稿：一期不为自助单开页面（菜单会从 19 涨到 26）。`V0401` 把页面码
 * 改成 `business:hr:salary:page` 之后，员工**进得来这一页但没有 `salary:list`** ——
 * 全量列表对他 403 才是设计意图。所以本页的 tab 显隐按权限算：
 * 有 `salary:list` 才画「薪资单」tab，有 `salary:my` 才画「我的薪资」tab，
 * 两个都没有就只剩一句说明（不给点了必 403 的入口，`AGENTS.md` 红线）。
 */

import { formatMoney, toCents, toNumber, dash, shortDate } from '@/views/business/erp/shared/money';

import { SALARY_PERMS, SALARY_STATUS_OPTIONS, periodText, type SalarySheetCard } from './salary-kinds';

// ==================== 权限码（从 salary-kinds 派生，不重复定义） ====================

/**
 * 本页用到的 5 个权限码。
 *
 * **全部从 {@link SALARY_PERMS} 取**：薪资模块 8 个码只在一处定义（`salary-kinds.ts`），
 * 本页再抄一份就会出现「改了一边漏另一边 → 按钮永远不出现」的静默残废。
 * 单测会把它与 `V0387__hr_menu_perms.up.sql` 的 `sys_menu.auth_code` 逐字对拍。
 */
export const SALARY_LIST_PERMS = {
  /** 全量列表（#36；`V0401` 之后页面码 `salary:page` 不再等于它） */
  list: SALARY_PERMS.list,
  /** 详情页（#37/#38）+ 列表点单号跳转 */
  detail: SALARY_PERMS.detail,
  /** 试算 / 建单 / 改明细（#39/#40/#41 共用） */
  calc: SALARY_PERMS.calc,
  /** 导出工资表（#45，列表行操作里的「导出」） */
  export: SALARY_PERMS.export,
  /** 我的薪资（#46 自助） */
  my: SALARY_PERMS.my,
} as const;

// ==================== 分页（与后端 normalize_page 同尺） ====================

/** 默认每页条数（后端 `salary_sheet_dto::DEFAULT_PAGE_SIZE`）。 */
export const SALARY_LIST_PAGE_SIZE = 20;

/** 每页条数上限（后端 `MAX_PAGE_SIZE`，传大了会被 clamp）。 */
export const SALARY_LIST_MAX_PAGE_SIZE = 200;

/** 页脚可选条数（都 ≤ 上限）。 */
export const SALARY_LIST_PAGE_SIZE_OPTIONS = [10, 20, 50, 100];

/** 备注上限（后端 `salary_sheet_dto::MAX_REMARK_LEN`）。 */
export const MAX_CALC_REMARK = 500;

// ==================== 列表筛选 / 查询串 ====================

/** 「薪资单」tab 的筛选条件（页面状态）。 */
export interface SalaryListFilter {
  /** 薪资期间 `yyyy-MM`（空串 = 不限） */
  period_code: string;
  /** 状态码值（空串 = 不限） */
  status: string;
  /** 单号模糊搜 */
  keyword: string;
}

/** 空筛选（重置用；每次给新对象，别共享引用）。 */
export function emptyListFilter(): SalaryListFilter {
  return { period_code: '', status: '', keyword: '' };
}

/**
 * 期间码校验 —— `null` = 通过，否则是给用户看的那句话。
 *
 * 三条口径与后端逐条对齐（`ck_biz_sal_sheet_code` + `compute::period_range`）：
 * 1. 形状 `yyyy-MM`（四位年 + 两位月，多一段少一段都不行）；
 * 2. 月份 `01~12`（`2026-00` / `2026-13` 会过「两位数字」但过不了这条）；
 * 3. 年 `1900~2999`（后端 `NaiveDate::from_ymd_opt` 能建到那之外，但工资单落到
 *    `0000-01` 只可能是手滑，前端先拦）。
 *
 * 空串视为「没填」—— 是否必填由调用方决定（筛选可不填，试算/建单必须填）。
 */
export function periodCodeError(value: string | null | undefined): string | null {
  const text = (value ?? '').trim();
  if (!text) {
    return '薪资期间必填（形如 2026-11）';
  }
  const match = /^(\d{4})-(\d{2})$/.exec(text);
  if (!match) {
    return `薪资期间「${text}」不合法：格式必须是 yyyy-MM（例如 2026-11）`;
  }
  const year = Number(match[1]);
  const month = Number(match[2]);
  if (month < 1 || month > 12) {
    return `薪资期间「${text}」不合法：月份只能是 01~12`;
  }
  if (year < 1900 || year > 2999) {
    return `薪资期间「${text}」不合法：年份看着不对（1900~2999）`;
  }
  return null;
}

/**
 * 当前薪资期间（按北京时间取当月，`2026-11`）。
 *
 * 为什么不是 `toISOString().slice(0, 7)`：那是 UTC，每月 1 日 00:00~08:00
 * （北京时间）会算成上个月 —— 月初做工资的人正好撞上。
 */
export function currentPeriodCode(now: Date = new Date()): string {
  const shifted = new Date(now.getTime() + 8 * 60 * 60 * 1000);
  return shifted.toISOString().slice(0, 7);
}

/**
 * 列表查询串（`GET /salary/page`）。
 *
 * 空筛选**不发空参数**（`status=` 会让后端当成「传了但不在白名单」→ 400）；
 * 页码 / 条数按后端 `normalize_page` 同口径兜底（`page ≥ 1`、`1 ≤ page_size ≤ 200`）。
 */
export function buildListQuery(
  filter: SalaryListFilter,
  page: number,
  pageSize: number,
): Record<string, unknown> {
  const query: Record<string, unknown> = {
    page: Math.max(1, Math.trunc(page) || 1),
    page_size: Math.min(
      SALARY_LIST_MAX_PAGE_SIZE,
      Math.max(1, Math.trunc(pageSize) || SALARY_LIST_PAGE_SIZE),
    ),
  };
  const period = (filter.period_code ?? '').trim();
  if (period && !periodCodeError(period)) {
    query.period_code = period;
  }
  const status = (filter.status ?? '').trim();
  if (status) {
    query.status = status;
  }
  const keyword = (filter.keyword ?? '').trim();
  if (keyword) {
    query.keyword = keyword;
  }
  return query;
}

/** 筛选有没有生效（空态文案分「没有数据」/「没有匹配」）。 */
export function isListFiltered(filter: SalaryListFilter): boolean {
  return Boolean(
    (filter.period_code ?? '').trim() ||
      (filter.status ?? '').trim() ||
      (filter.keyword ?? '').trim(),
  );
}

/** 列表空态文案（两种空：库里没有 / 筛没了 —— 用户要知道该点哪个按钮）。 */
export function listEmptyText(filtered: boolean): string {
  return filtered
    ? '没有匹配的薪资单：换个期间 / 状态，或清空「单号」再试'
    : '还没有薪资单：点右上角「试算」核对数，确认后「建单」生成第一张';
}

/** 状态下拉选项（六档，与 `ck_biz_sal_sheet_status` 同域；直接复用详情页那份）。 */
export const SALARY_LIST_STATUS_OPTIONS = SALARY_STATUS_OPTIONS;

// ==================== 试算 / 建单 ====================

/** 计算模式：试算（不落库）/ 建单（落库）。与后端 `CalcMode` 一一对应。 */
export type SalaryCalcMode = 'preview' | 'persist';

/** 试算 / 建单的表单（页面状态）。 */
export interface SalaryCalcForm {
  /** 薪资期间 `yyyy-MM` */
  period_code: string;
  /** 单头备注（可选，≤500 字符） */
  remark: string;
}

/** 空表单（默认带当月 —— 九成五的建单就是「做这个月的工资」）。 */
export function emptyCalcForm(now: Date = new Date()): SalaryCalcForm {
  return { period_code: currentPeriodCode(now), remark: '' };
}

/** 表单校验：`null` = 通过，否则第一条错误（先期间后备注，一次只让用户改一处）。 */
export function calcFormError(form: SalaryCalcForm): string | null {
  const periodError = periodCodeError(form.period_code);
  if (periodError) {
    return periodError;
  }
  const remark = (form.remark ?? '').trim();
  if ([...remark].length > MAX_CALC_REMARK) {
    return `备注最多 ${MAX_CALC_REMARK} 个字（现在 ${[...remark].length} 个）`;
  }
  return null;
}

/**
 * 试算 / 建单报文（#39 / #40 同一形状）。
 *
 * `dry_run` **由 `mode` 决定**：后端 `parse_calc_req` 会拿它与接口身份对拍，
 * 试算接口收 `dry_run=false` 直接 400（反之亦然）。所以页面不许自己拼这个字段。
 *
 * 没有实现「人工录入项」（`manual`）：它要求同时知道 `employee_id` + 启用中的
 * `INPUT` 项编码，那是「调薪 / 补发」子场景（设计 §4.8.5），本页不画入口 ——
 * 补发目前走员工薪资标准，建完单在详情页改单项金额（#41）。
 */
export function buildCalcPayload(
  form: SalaryCalcForm,
  mode: SalaryCalcMode,
): { period_code: string; dry_run: boolean; remark?: string } {
  const payload: { period_code: string; dry_run: boolean; remark?: string } = {
    period_code: (form.period_code ?? '').trim(),
    dry_run: mode === 'preview',
  };
  const remark = (form.remark ?? '').trim();
  if (remark) {
    payload.remark = remark;
  }
  return payload;
}

/** 模式 → 按钮文案（试算 / 建单，提示语里要能说清「落不落库」）。 */
export const CALC_MODE_LABELS: Record<SalaryCalcMode, string> = {
  preview: '试算（不落库）',
  persist: '建单并保存',
};

/** 模式 → 一句话说明（抽屉顶部的解释，避免用户以为「试算」也生成单号）。 */
export const CALC_MODE_HINTS: Record<SalaryCalcMode, string> = {
  preview:
    '试算只算数：库里零行、单号不占，改完期间可以再算一次。核对无误后再「建单并保存」。',
  persist:
    '建单会真的落库并占一个单号（SAL-yyyyMM-000001），生成后进详情页核对、提交审批。',
};

/** 试算回执里的合计（`SalaryCalcRes.totals` 是本页唯一要显示的金额块）。 */
export interface SalaryCalcTotals {
  employee_count: number;
  earning_total: number | string;
  pre_deduct_total: number | string;
  gross_total: number | string;
  withhold_total: number | string;
  net_total: number | string;
  si_company_total: number | string;
  hf_company_total: number | string;
  accrual_total: number | string;
}

/** 一个人的试算结果（`SalarySlipRes` 的列表用子集）。 */
export interface SalarySlipRow {
  employee_id: string;
  employee_no: string;
  employee_name: string;
  dept_name: string | null;
  should_days: number | string;
  actual_days: number | string;
  leave_unpaid_days: number | string;
  ot_hours: number | string;
  earning_amount: number | string;
  pre_deduct_amount: number | string;
  gross_amount: number | string;
  withhold_amount: number | string;
  net_amount: number | string;
}

/** 落库后的单头引用（试算为 `null`）。 */
export interface SalarySheetRef {
  id: string;
  sheet_no: string;
  status: string;
  status_label: string;
}

/** 试算 / 建单回执（`salary_sheet_dto::SalaryCalcRes`）。 */
export interface SalaryCalcResult {
  period_code: string;
  period_start: string;
  period_end: string;
  attendance_period_id: string | null;
  dry_run: boolean;
  sheet: SalarySheetRef | null;
  totals: SalaryCalcTotals;
  item_count: number;
  missing_attendance: string[];
  employees: SalarySlipRow[];
}

/**
 * 缺考勤名单一句话（`missing_attendance` 是 `工号 姓名` 的数组）。
 *
 * 只列前 5 个 + 「还有 N 人」：一屏 60 人的名单塞进 alert 会盖住整页，
 * 而用户真正要做的是去考勤页补录 —— 名单给到能认人即可，全量在下面的逐人表里。
 */
export function missingAttendanceText(list: readonly string[] | null | undefined): string {
  const names = (list ?? []).map((item) => (item ?? '').trim()).filter(Boolean);
  if (!names.length) {
    return '';
  }
  const shown = names.slice(0, 5).join('、');
  const rest = names.length - Math.min(5, names.length);
  return rest > 0
    ? `缺当期考勤 ${names.length} 人：${shown} 等（还有 ${rest} 人，未计入应发）`
    : `缺当期考勤 ${names.length} 人：${shown}（未计入应发）`;
}

/** 试算回执的一句话结论（抽屉里那行大字）。 */
export function calcResultText(result: SalaryCalcResult | null | undefined): string {
  if (!result) {
    return '';
  }
  const period = periodText(result.period_code, result.period_start, result.period_end);
  const count = Number(result.totals?.employee_count ?? 0) || 0;
  const items = Number(result.item_count ?? 0) || 0;
  return `${period} · ${count} 人 · ${items} 个薪资项`;
}

/** 试算/建单回执的逐人表列（金额右对齐；只列核对工资要看的 8 列）。 */
export const SALARY_SLIP_COLUMNS = [
  { colKey: 'employee', title: '员工', width: 150 },
  { colKey: 'dept_name', title: '部门', width: 110, ellipsis: true },
  { colKey: 'attendance', title: '考勤口径', width: 200, ellipsis: true },
  { colKey: 'earning_amount', title: '收入合计', width: 120, align: 'right' as const },
  { colKey: 'gross_amount', title: '应发', width: 120, align: 'right' as const },
  { colKey: 'withhold_amount', title: '代扣', width: 120, align: 'right' as const },
  { colKey: 'net_amount', title: '实发', width: 120, align: 'right' as const },
];

/** 考勤口径的四个字段（试算逐人行与「我的薪资」行都有这四列）。 */
export interface AttendanceSnapshot {
  should_days: number | string;
  actual_days: number | string;
  leave_unpaid_days: number | string;
  ot_hours: number | string;
}

/** 一个人的考勤口径一行（与详情页 `attendanceText` 同文案，少打一次接口）。 */
export function slipAttendanceText(row: AttendanceSnapshot | null | undefined): string {
  if (!row) {
    return dash(null);
  }
  return [
    `应出勤 ${daysOf(row.should_days)}`,
    `实际 ${daysOf(row.actual_days)}`,
    `无薪假 ${daysOf(row.leave_unpaid_days)}`,
    `加班 ${daysOf(row.ot_hours)}h`,
  ].join(' · ');
}

/** 天数（`Decimal` 字符串去尾零：`22.00` → `22`，`0.50` → `0.5`；缺值给 `—` 不是 `0`）。 */
export function daysOf(value: number | string | null | undefined): string {
  // 缺值必须显示 `—`：`0 天` 与「没这个数」在工资条上是两件事（缺勤 0 天 vs 没算出来）
  if (value === null || value === undefined || value === '') {
    return dash(null);
  }
  const num = toNumber(value);
  if (!Number.isFinite(num)) {
    return dash(null);
  }
  return String(Number(num.toFixed(2)));
}

// ==================== 列表行展示 ====================

/** 列表行 = 单头卡（`SalarySheetCardRes`），一单一行的摘要。 */
export type SalarySheetRow = SalarySheetCard;

/** 一行的期间（`2026-11（11-01 ~ 11-30）`）。 */
export function sheetPeriodText(row: SalarySheetRow | null | undefined): string {
  if (!row) {
    return dash(null);
  }
  return periodText(row.period_code, row.period_start, row.period_end);
}

/**
 * 凭证列文案（数组，页面按行渲染）。
 *
 * 只认**真的拿到号**的两张（计提 / 发放）：未过账给空数组，页面显示「未过账」。
 * 红冲凭证号不在列表行里（单头只下发 `reversal_voucher_id`，见 `salary-kinds.ts`），
 * 所以不给猜出来的第三行。
 */
export function sheetVoucherLines(row: SalarySheetRow | null | undefined): string[] {
  const lines: string[] = [];
  const accrual = (row?.accrual_voucher_no ?? '').trim();
  const payment = (row?.payment_voucher_no ?? '').trim();
  if (accrual) {
    lines.push(`计提 ${accrual}`);
  }
  if (payment) {
    lines.push(`发放 ${payment}`);
  }
  return lines;
}

/**
 * 当前页合计（整数分累加后换回元，避免浮点漂移）。
 *
 * 只做「这一页加起来是多少」—— 全局合计要看期间汇总，那是详情页部门汇总卡的事；
 * 列表页画全局合计就得再打一次接口（后端 #36 只回分页行）。
 */
export interface SheetPageTotals {
  employee_count: number;
  gross_total: number;
  withhold_total: number;
  net_total: number;
  accrual_total: number;
}

/** 空合计。 */
export function emptySheetPageTotals(): SheetPageTotals {
  return { employee_count: 0, gross_total: 0, withhold_total: 0, net_total: 0, accrual_total: 0 };
}

/** 当前页合计。 */
export function sumSheetRows(rows: readonly SalarySheetRow[] | null | undefined): SheetPageTotals {
  const cents = { gross_total: 0, withhold_total: 0, net_total: 0, accrual_total: 0 };
  let employeeCount = 0;
  for (const row of rows ?? []) {
    employeeCount += Number(row.employee_count) || 0;
    cents.gross_total += toCents(row.gross_total);
    cents.withhold_total += toCents(row.withhold_total);
    cents.net_total += toCents(row.net_total);
    cents.accrual_total += toCents(row.accrual_total);
  }
  return {
    employee_count: employeeCount,
    gross_total: cents.gross_total / 100,
    withhold_total: cents.withhold_total / 100,
    net_total: cents.net_total / 100,
    accrual_total: cents.accrual_total / 100,
  };
}

/** 合计行文案（页面只显示「本页合计」，避免被误读成全局）。 */
export function pageTotalsText(totals: SheetPageTotals): string {
  return [
    `人数 ${totals.employee_count}`,
    `应发 ${formatMoney(totals.gross_total)}`,
    `代扣 ${formatMoney(totals.withhold_total)}`,
    `实发 ${formatMoney(totals.net_total)}`,
  ].join(' · ');
}

/** 详情页深链（`/business/hr/salary/:id`，HR-43 已注册）。 */
export function salaryDetailPath(id: string): string {
  return `/business/hr/salary/${id}`;
}

// ==================== 列表列 ====================

/**
 * 「薪资单」列表列（宽度合计 ≈1360，1440 视口下一屏放得下）。
 *
 * `period` / `voucher` / `op` 三个 `colKey` 不是后端字段，靠 `#period` / `#voucher` /
 * `#op` 具名插槽渲染（TDesign 里同名列插槽优先于取值）。
 */
export const SALARY_LIST_COLUMNS = [
  { colKey: 'sheet_no', title: '单号', width: 180 },
  { colKey: 'period', title: '薪资期间', width: 190 },
  { colKey: 'status', title: '状态', width: 96 },
  { colKey: 'employee_count', title: '人数', width: 80, align: 'right' as const },
  { colKey: 'gross_total', title: '应发合计', width: 130, align: 'right' as const },
  { colKey: 'withhold_total', title: '代扣合计', width: 130, align: 'right' as const },
  { colKey: 'net_total', title: '实发合计', width: 130, align: 'right' as const },
  { colKey: 'accrual_total', title: '计提合计', width: 130, align: 'right' as const },
  { colKey: 'voucher', title: '凭证', width: 190, ellipsis: true },
  { colKey: 'op', title: '操作', width: 150 },
];

// ==================== 我的薪资（#46 自助） ====================

/** 我的一条薪资项（`self_service_dto::MySalaryItemRes`；**不含**公式快照与计算说明）。 */
export interface MySalaryItemLine {
  item_code: string;
  item_name: string;
  item_type: string;
  item_type_label: string;
  amount: number | string;
}

/** 我的一期工资条（`self_service_dto::MySalaryRes`）。 */
export interface MySalaryRow {
  sheet_id: string;
  sheet_no: string;
  period_code: string;
  sheet_status: string;
  sheet_status_label: string;
  /** 是否已发放（`sheet_status == POSTED`） */
  paid: boolean;
  detail_id: string;
  employee_no: string;
  employee_name: string;
  dept_name: string | null;
  position_code: string | null;
  job_level: string | null;
  should_days: number | string;
  actual_days: number | string;
  leave_unpaid_days: number | string;
  ot_hours: number | string;
  earning_amount: number | string;
  pre_deduct_amount: number | string;
  gross_amount: number | string;
  social_personal: number | string;
  fund_personal: number | string;
  iit_amount: number | string;
  other_withhold: number | string;
  withhold_amount: number | string;
  net_amount: number | string;
  remark: string | null;
  items: MySalaryItemLine[];
}

/** 「我的薪资」筛选（期间 + 是否含在途，页面状态）。 */
export interface MySalaryFilter {
  period_code: string;
  include_pending: boolean;
}

/** 空筛选。 */
export function emptyMyFilter(): MySalaryFilter {
  return { period_code: '', include_pending: false };
}

/**
 * 「我的薪资」查询串（`GET /salary/my`）。
 *
 * `include_pending` **只在为真时才发**：后端默认只出已定稿三档，
 * 显式 `include_pending=false` 与不传等价，但少一个参数报文更好读。
 */
export function buildMyQuery(
  filter: MySalaryFilter,
  page: number,
  pageSize: number,
): Record<string, unknown> {
  const query: Record<string, unknown> = {
    page: Math.max(1, Math.trunc(page) || 1),
    page_size: Math.min(
      SALARY_LIST_MAX_PAGE_SIZE,
      Math.max(1, Math.trunc(pageSize) || SALARY_LIST_PAGE_SIZE),
    ),
  };
  const period = (filter.period_code ?? '').trim();
  if (period && !periodCodeError(period)) {
    query.period_code = period;
  }
  if (filter.include_pending) {
    query.include_pending = true;
  }
  return query;
}

/** 发放状态文案（员工最关心这一列：钱到没到账）。 */
export function myPaidText(row: MySalaryRow | null | undefined): string {
  if (!row) {
    return dash(null);
  }
  if (row.paid) {
    return '已发放';
  }
  switch ((row.sheet_status ?? '').trim()) {
    case 'APPROVED':
      return '已批准待发放';
    case 'REVERSED':
      return '已红冲';
    default:
      return row.sheet_status_label || '未定稿';
  }
}

/** 工资条的明细项一行（`BASE 8000.00 · OT_WD 320.00`）；没有明细给 `—`。 */
export function myItemsText(row: MySalaryRow | null | undefined): string {
  const items = row?.items ?? [];
  if (!items.length) {
    return dash(null);
  }
  return items
    .map((item) => `${item.item_name} ${formatMoney(item.amount)}`)
    .join(' · ');
}

/**
 * 工资条空态文案。
 *
 * 后端对「没绑账号 / 还没发过工资」都回 `200 + 空列表`（**不是 403**），
 * 所以这里要把两种原因都说到 —— 员工看到空白才知道该找谁。
 */
export function mySalaryEmptyText(filtered: boolean): string {
  return filtered
    ? '这一期没有我的工资条：清掉期间筛选看看历史'
    : '还没有可查看的工资条：工资单「已批准 / 已过账」后才会出现；若一直为空，请联系 HR 绑定员工档案';
}

/** 「我的薪资」列表列。 */
export const SALARY_MY_COLUMNS = [
  { colKey: 'period_code', title: '薪资期间', width: 120 },
  { colKey: 'sheet_no', title: '单号', width: 180 },
  { colKey: 'sheet_status', title: '状态', width: 100 },
  { colKey: 'paid', title: '发放', width: 120 },
  { colKey: 'attendance', title: '考勤口径', width: 210, ellipsis: true },
  { colKey: 'gross_amount', title: '应发', width: 120, align: 'right' as const },
  { colKey: 'withhold_amount', title: '代扣', width: 120, align: 'right' as const },
  { colKey: 'net_amount', title: '实发', width: 120, align: 'right' as const },
];

/** 工资条的日期区间文案（`2026-11`；`start`/`end` 后端不下发，只有 period_code）。 */
export function myPeriodText(row: MySalaryRow | null | undefined): string {
  const code = (row?.period_code ?? '').trim();
  return code || dash(null);
}

/** 明细项按类型分组的顺序（收入 → 应发前扣减 → 代扣；单位承担项员工不可见）。 */
export const MY_ITEM_TYPE_ORDER = ['EARNING', 'PRE_DEDUCT', 'WITHHOLD'] as const;

/** 员工可见的明细项（按 {@link MY_ITEM_TYPE_ORDER} 分组，组内保持后端 `line_no` 顺序）。 */
export function myItemsSorted(row: MySalaryRow | null | undefined): MySalaryItemLine[] {
  const items = [...(row?.items ?? [])];
  const rank = (type: string): number => {
    const index = (MY_ITEM_TYPE_ORDER as readonly string[]).indexOf((type ?? '').trim().toUpperCase());
    return index < 0 ? MY_ITEM_TYPE_ORDER.length : index;
  };
  return items.sort((left, right) => rank(left.item_type) - rank(right.item_type));
}

/** 明细项类型 → 标签配色（与详情页 `itemTypeTheme` 同色，员工页只显示名字所以只用来上色）。 */
export function myItemTheme(
  itemType: string | null | undefined,
): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  switch ((itemType ?? '').trim().toUpperCase()) {
    case 'EARNING':
      return 'success';
    case 'PRE_DEDUCT':
      return 'warning';
    case 'WITHHOLD':
      return 'danger';
    default:
      return 'default';
  }
}

/** 期间码在表格里的短显示（`2026-11` 而已；页面表头已经说明是薪资期间）。 */
export function periodShort(periodCode: string | null | undefined): string {
  const text = (periodCode ?? '').trim();
  return text || dash(null);
}

/** 日期短显示（列表里不用再拼区间时用）。 */
export { shortDate };
