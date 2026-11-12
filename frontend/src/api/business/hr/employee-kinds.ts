/**
 * 员工档案的出参契约与词表（ARCH-9 §5.2 #1/#2/#7 · HR-18）。
 *
 * 纯逻辑文件：**不 import 请求层**，`tsx --test` 里可以直接 import 做单测。
 * 请求函数在 `./employee.ts`，那里 `export * from './employee-kinds'`，
 * 页面只认 `@/api/business/hr/employee` 一个入口。
 *
 * # 字段名逐条抄自后端，不缩写
 *
 * 出处：`backend/src/business/hr/employee_dto.rs`（HR-11 定稿出参契约）
 * —— `EmployeeListRes` 21 个键 / `EmployeeDetailRes` 49 个键 /
 * `EmployeeContractRes` / `EmployeeSalaryRes` / `SensitiveMasked`。
 * 猜字段名的代价是运行时 `undefined`（列空白、金额显示 `NaN`），编译期抓不到；
 * 这一层宁可长也不缩写。
 *
 * # 三处与「直觉」不一样的地方（都会让页面白屏 / 显示错值，先读）
 *
 * 1. **敏感字段只出 `sensitive_masked.{id_card_masked, bank_account_masked}`**。
 *    设计 §5.4 的示例把 `id_card_masked` 平铺在顶层，但 §5.2 #2 的字段表写的是
 *    `sensitive_masked` —— 接口清单是契约，HR-11 按 §5.2 落地并用 census 单测钉死。
 *    明文唯一出口是 `GET /employee/{id}/sensitive`（HR-13）。
 * 2. **金额是字符串**：`Decimal` 出 JSON 为 `"8000.0000"`，加减一律先换整数分
 *    （`erp/shared/money.ts::toCents`），不要直接 `+`。
 * 3. **到期天数是服务端按 `+08:00` 业务日算的**（`contract_expire_in_days`）。
 *    前端不要用本机时钟重算 —— 跨时区 / 用户改系统时间都会飘。
 *
 * # 状态的中文名也从后端来
 *
 * `status_label` / `employment_type_label` / `contract_type_label` 等都由后端下发，
 * 页面**直接显示 label**。本文件的 `*_LABELS` 只用于两处：
 * - 筛选下拉的选项（后端不提供「全部取值」接口）；
 * - 后端没给 label 的极端情况（老实例 / 新码值）的兜底。
 * `tests/unit/hr-employee-page.test.ts` 会读 Rust 源码把两张表对拍。
 */

// ==================== 取值域（与 DDL 的 CHECK / 后端常量逐字一致） ====================

/** 员工状态（`ck_biz_employee_status`） */
export const STATUS_VALUES = ['PROBATION', 'ACTIVE', 'SUSPENDED', 'RESIGNED'] as const;
/** 员工状态码值 */
export type EmployeeStatus = (typeof STATUS_VALUES)[number];

/** 列表默认显示的状态（设计 §5.3：不含离职） */
export const DEFAULT_STATUSES: EmployeeStatus[] = ['PROBATION', 'ACTIVE', 'SUSPENDED'];

/** 员工状态 → 中文（`employee_dto::STATUS_LABELS` 逐字一致） */
export const STATUS_LABELS: Record<EmployeeStatus, string> = {
  PROBATION: '试用期',
  ACTIVE: '在职',
  SUSPENDED: '停职',
  RESIGNED: '离职',
};

/** 员工状态 → 标签配色（设计 §6.2.1：试用蓝、在职绿、停职灰、离职深灰） */
export const STATUS_THEMES: Record<EmployeeStatus, 'primary' | 'success' | 'warning' | 'default'> = {
  // 设计写「停薪灰」，但 TDesign 只有 5 个语义色；用 warning 让「非正常在职」
  // 在列表里一眼可辨（灰与「离职」的 default 撞色，反而不易区分）。
  PROBATION: 'primary',
  ACTIVE: 'success',
  SUSPENDED: 'warning',
  RESIGNED: 'default',
};

/** 用工形式（`ck_biz_employee_emptype` 的 6 个取值 → `EMPLOYMENT_TYPE_LABELS`） */
export const EMPLOYMENT_TYPE_LABELS: Record<string, string> = {
  FULL_TIME: '正式',
  PROBATION: '试用',
  INTERN: '实习',
  OUTSOURCE: '外包',
  PART_TIME: '兼职',
  DISPATCH: '劳务派遣',
};

/** 用工形式下拉选项（顺序与后端常量一致） */
export const EMPLOYMENT_TYPE_OPTIONS = Object.entries(EMPLOYMENT_TYPE_LABELS).map(([value, label]) => ({
  value,
  label,
}));

/** 合同类型（`ck_biz_emp_contract_type` 的 5 个取值） */
export const CONTRACT_TYPE_LABELS: Record<string, string> = {
  FIXED_TERM: '固定期限',
  UNFIXED_TERM: '无固定期限',
  INTERN: '实习协议',
  LABOR_SERVICE: '劳务合同',
  DISPATCH: '派遣协议',
};

/** 合同状态（`ck_biz_emp_contract_status` 的 4 个取值） */
export const CONTRACT_STATUS_LABELS: Record<string, string> = {
  ACTIVE: '生效中',
  EXPIRED: '已到期',
  TERMINATED: '已解除',
  RENEWED: '已续签',
};

/** 合同状态 → 时间线配色（生效中绿 / 已到期橙 / 已解除灰 / 已续签蓝） */
export const CONTRACT_STATUS_THEMES: Record<string, 'success' | 'warning' | 'default' | 'primary'> = {
  ACTIVE: 'success',
  EXPIRED: 'warning',
  TERMINATED: 'default',
  RENEWED: 'primary',
};

/** 证件类型（`ck_biz_employee_idtype` 的 4 个取值） */
export const ID_CARD_TYPE_LABELS: Record<string, string> = {
  ID_CARD: '身份证',
  PASSPORT: '护照',
  HK_MO_TW: '港澳台居民证件',
  FOREIGN: '外籍证件',
};

/** 证件类型下拉选项（顺序与后端 `ID_CARD_TYPES` 常量一致：身份证在最前） */
export const ID_CARD_TYPE_OPTIONS = Object.entries(ID_CARD_TYPE_LABELS).map(([value, label]) => ({
  value,
  label,
}));

/** 性别（`biz_employee.gender`：0 未知 / 1 男 / 2 女，与 DDL 的 `ck_biz_employee_gender` 一致） */
export const GENDER_LABELS: Record<number, string> = {
  0: '未知',
  1: '男',
  2: '女',
};

/** 性别单选（**不含 0**：新建档案必须明确男女，0 只是老数据的兜底） */
export const GENDER_OPTIONS = [
  { value: 1, label: GENDER_LABELS[1] },
  { value: 2, label: GENDER_LABELS[2] },
];

/**
 * 表单字段长度上限（逐条抄自 `employee_handler.rs` 的 `MAX_*` 常量）。
 *
 * 前端 `maxlength` + 提交前校验都用它：不设上限时用户能敲进 200 字姓名，
 * 提交到后端才 400「name 最长 50 个字符」，白填一屏。
 * `tests/unit/hr-employee-form.test.ts` 会读 Rust 源码逐条对拍。
 */
export const EMPLOYEE_FIELD_MAX = {
  emp_no: 32,
  name: 50,
  id_card: 18,
  mobile: 20,
  email: 100,
  position_code: 32,
  job_level: 20,
  school: 100,
  major: 100,
  native_place: 100,
  address: 200,
  emergency_contact: 50,
  emergency_phone: 20,
  emergency_relation: 20,
  bank_name: 100,
  bank_branch: 200,
  bank_account: 40,
  bank_account_name: 50,
} as const;

/**
 * 姓名最短字数（**前端比后端更严**）。
 *
 * 后端 `require_text` 只要求非空；但一个字的姓名在列表里和「张三」混排时几乎认不出，
 * 人事建档也几乎不会出现 —— 卡面要求 2-50 字，这里按卡面拦（后端仍兜 ≤50）。
 */
export const EMPLOYEE_MIN_NAME = 2;

/** 建档表单用到的字典编码（V0386 已种；`DictSelect` 直接吃字典编码） */
export const EMPLOYEE_DICT_CODES = {
  position: 'hr_position',
  job_level: 'hr_job_level',
  education: 'hr_education',
} as const;

/** 离职类型（`ck_biz_employee_leavetype` 的 5 个取值） */
export const LEAVE_TYPE_LABELS: Record<string, string> = {
  RESIGN: '辞职',
  DISMISS: '辞退',
  RETIRE: '退休',
  CONTRACT_END: '合同到期',
  DEATH: '身故',
};

/** 合同「快到期」阈值（天）：设计 §6.2 —— `≤ 30` 出橙字（含已过期） */
export const CONTRACT_EXPIRE_WARN_DAYS = 30;

/** 员工列表每页条数（列表默认 20；42 人以上才有翻页，与设计 §6.3 的「42 人」口径一致） */
export const EMPLOYEE_PAGE_SIZE = 20;

// ==================== 出参类型（逐字段对齐 employee_dto.rs） ====================

/** 员工列表行（`GET /business/hr/employee/page` 的一项） */
export interface EmployeeListRow {
  /** 主键 */
  id: string;
  /** 工号 `EMP-0001` */
  emp_no: string;
  /** 姓名 */
  name: string;
  /** 性别码值（0 未知 / 1 男 / 2 女） */
  gender: number;
  /** 性别中文 */
  gender_label: string;
  /** 手机号（`sys_field_mask` 有规则时才遮） */
  mobile: string | null;
  /** 部门 id */
  dept_id: string;
  /** 部门名（join 实时取；已删 → `（部门已删除）`） */
  dept_name: string;
  /** 岗位码值 */
  position_code: string | null;
  /** 岗位名（与码值成对） */
  position_name: string | null;
  /** 职级码值 */
  job_level: string | null;
  /** 职级名 */
  job_level_name: string | null;
  /** 用工形式码值 */
  employment_type: string;
  /** 用工形式中文 */
  employment_type_label: string;
  /** 入职日期 `YYYY-MM-DD` */
  hire_date: string;
  /** 状态码值 */
  status: string;
  /** 状态中文 */
  status_label: string;
  /** 关联账号 id */
  user_id: string | null;
  /** 账号用户名（与 `user_id` 成对） */
  username: string | null;
  /** 现行合同到期日 */
  contract_end_date: string | null;
  /** 距到期天数（负数 = 已过期；`≤ 30` 出橙字） */
  contract_expire_in_days: number | null;
}

/** 劳动合同（详情里的 `contracts[]` / `current_contract`） */
export interface EmployeeContract {
  /** 合同主键 */
  id: string;
  /** 合同编号 */
  contract_no: string;
  /** 合同类型码值 */
  contract_type: string;
  /** 合同类型中文 */
  contract_type_label: string;
  /** 签订日 */
  sign_date: string;
  /** 起始日 */
  start_date: string;
  /** 终止日（无固定期限为 null） */
  end_date: string | null;
  /** 试用期（月） */
  probation_months: number;
  /** 状态码值 */
  status: string;
  /** 状态中文 */
  status_label: string;
  /** 解除日 */
  terminated_date: string | null;
  /** 解除原因 */
  terminate_reason: string | null;
  /** 续签来源合同 id */
  renewed_from_id: string | null;
  /** 续签来源合同编号（与 id 成对） */
  renewed_from_no: string | null;
  /** 距到期天数（仅 ACTIVE 且有终止日时给） */
  expire_in_days: number | null;
  /** 备注 */
  remark: string | null;
}

/** 一条员工薪资标准（`salary_items[]`；`current=true` 是业务日生效的那批） */
export interface EmployeeSalary {
  /** 薪资项编码 */
  item_code: string;
  /** 薪资项名（join 不到时回退编码） */
  item_name: string;
  /** 月标准金额（字符串，如 `"8000.0000"`） */
  amount: string;
  /** 生效起（含） */
  effective_from: string;
  /** 生效止（含）；null = 至今 */
  effective_to: string | null;
  /** 业务日是否落在生效区间内 */
  current: boolean;
  /** 备注 */
  remark: string | null;
}

/** 敏感字段的脱敏值（`null` = 该字段没登记，不是「遮掉了」） */
export interface EmployeeSensitiveMasked {
  id_card_masked: string | null;
  bank_account_masked: string | null;
}

/** 员工详情（`GET /business/hr/employee/{id}`，49 个键） */
export interface EmployeeDetail {
  id: string;
  emp_no: string;
  name: string;
  gender: number;
  gender_label: string;
  birth_date: string | null;
  id_card_type: string;
  id_card_type_label: string;
  mobile: string | null;
  email: string | null;
  dept_id: string;
  dept_name: string;
  position_code: string | null;
  position_name: string | null;
  job_level: string | null;
  job_level_name: string | null;
  employment_type: string;
  employment_type_label: string;
  hire_date: string;
  probation_end_date: string | null;
  leave_date: string | null;
  leave_type: string | null;
  leave_type_label: string | null;
  status: string;
  status_label: string;
  education: string | null;
  education_name: string | null;
  school: string | null;
  major: string | null;
  graduate_date: string | null;
  native_place: string | null;
  address: string | null;
  emergency_contact: string | null;
  emergency_phone: string | null;
  emergency_relation: string | null;
  bank_name: string | null;
  bank_branch: string | null;
  bank_account_name: string | null;
  user_id: string | null;
  username: string | null;
  remark: string | null;
  /** 创建时间（ISO8601 `+08:00`） */
  created_at: string;
  /** 最后修改时间 */
  updated_at: string;
  create_by_name: string | null;
  update_by_name: string | null;
  /** 敏感字段脱敏值（明文唯一出口是 `GET /employee/{id}/sensitive`） */
  sensitive_masked: EmployeeSensitiveMasked;
  /** 全部劳动合同（`start_date DESC`） */
  contracts: EmployeeContract[];
  /** 现行合同（ACTIVE 里最新一份；没有则最近一份历史合同） */
  current_contract: EmployeeContract | null;
  /** 全部薪资标准（按 `effective_from DESC`，现行那批标 `current`） */
  salary_items: EmployeeSalary[];
}

/** 列表筛选（设计 §5.2 #1 的入参） */
export interface EmployeePageQuery {
  /** 关键字：工号 / 姓名 / 手机 */
  keyword?: string;
  /** 部门（精确匹配本部门，不含子树） */
  dept_id?: string;
  /** 状态多选（空 = 后端默认三态，不含离职） */
  status?: string[];
  /** 用工形式 */
  employment_type?: string;
  /** 含离职（默认 false） */
  include_resigned?: boolean;
  /** 页码（1 起） */
  page?: number;
  /** 每页条数 */
  page_size?: number;
}

// ==================== 入参 / 回执类型（逐字段对齐 employee_handler.rs） ====================

/**
 * 新建员工入参（`POST /business/hr/employee`，权限 `business:hr:employee:create`）。
 *
 * 后端 `EmployeeCreateReq` 的每个字段都是 `Option`：缺省由 handler 给默认值
 * （`id_card_type` → `ID_CARD`、`employment_type` → `FULL_TIME`），
 * 必填只有 `name` / `dept_id` / `hire_date` 三条。
 *
 * `emp_no` 留空 ⇒ 后端在**同一个事务**里取号（`EMP-0001` 形态，HR-14）：
 * 取号失败 / 插入失败一起回滚，不留空号 —— 所以前端不要自己拼工号。
 */
export interface EmployeeCreateReq {
  /** 工号（留空自动取号；企业自有工号体系可直填） */
  emp_no?: string;
  /** 姓名（必填，2-50 字） */
  name: string;
  /** 性别（1 男 / 2 女；0 未知只出现在老数据） */
  gender?: number;
  /** 出生日期 `YYYY-MM-DD` */
  birth_date?: string;
  /** 身份证号（明文入参，出参只给打码值） */
  id_card?: string;
  /** 证件类型（缺省 `ID_CARD`） */
  id_card_type?: string;
  /** 手机号 */
  mobile?: string;
  /** 邮箱 */
  email?: string;
  /** 人事归属部门（必填；数据权限按它裁剪） */
  dept_id: string;
  /** 岗位码值（字典 `hr_position`） */
  position_code?: string;
  /** 职级码值（字典 `hr_job_level`） */
  job_level?: string;
  /** 用工形式（缺省 `FULL_TIME`） */
  employment_type?: string;
  /** 入职日期（必填，`YYYY-MM-DD`，不得晚于今天） */
  hire_date: string;
  /** 试用期截止（不得早于入职日期） */
  probation_end_date?: string;
  /** 学历码值（字典 `hr_education`） */
  education?: string;
  /** 毕业院校 */
  school?: string;
  /** 专业 */
  major?: string;
  /** 毕业日期 */
  graduate_date?: string;
  /** 籍贯 */
  native_place?: string;
  /** 现住址 */
  address?: string;
  /** 紧急联系人 */
  emergency_contact?: string;
  /** 紧急联系电话 */
  emergency_phone?: string;
  /** 与本人关系 */
  emergency_relation?: string;
  /** 工资卡开户行 */
  bank_name?: string;
  /** 开户支行 */
  bank_branch?: string;
  /** 工资卡号（明文入参） */
  bank_account?: string;
  /** 工资卡户名 */
  bank_account_name?: string;
  /** 关联登录账号（可空；必须是未删除的 HUMAN 账号） */
  user_id?: string;
  /** 备注 */
  remark?: string;
}

/**
 * 修改员工入参（`PUT /business/hr/employee/{id}`，权限 `business:hr:employee:update`）。
 *
 * # 缺省语义（这三条决定「保存后数据会不会被清空」）
 *
 * | 传什么 | 后端行为 |
 * |---|---|
 * | 字段**不传** / `undefined` | 保持库里当前值（handler `merged_write` 从锁定行回填） |
 * | 字段传**空串** | 清空该列（`NULL`） |
 * | 字段有值 | 覆盖 |
 *
 * 所以：**详情里拿不到明文的两个字段（`id_card` / `bank_account`）绝不能无脑提交空串**，
 * 否则一次「只改手机号」的保存会把身份证和工资卡号清掉。口径见
 * `views/business/hr/employee/employee-form.ts::buildUpdatePayload`。
 *
 * `emp_no` / `org_id` / `status` / `user_id` 不在可改列里（工号是身份、组织是隔离维度、
 * 状态只能走状态机、账号走 `bind-user`）。
 */
export interface EmployeeUpdateReq {
  name?: string;
  gender?: number;
  birth_date?: string;
  id_card?: string;
  id_card_type?: string;
  mobile?: string;
  email?: string;
  dept_id?: string;
  position_code?: string;
  job_level?: string;
  employment_type?: string;
  hire_date?: string;
  probation_end_date?: string;
  education?: string;
  school?: string;
  major?: string;
  graduate_date?: string;
  native_place?: string;
  address?: string;
  emergency_contact?: string;
  emergency_phone?: string;
  emergency_relation?: string;
  bank_name?: string;
  bank_branch?: string;
  bank_account?: string;
  bank_account_name?: string;
  remark?: string;
}

/** 新建回执（`{ id, emp_no }`；`emp_no` 是后端取号/回显的真实工号，回显给用户看） */
export interface EmployeeCreatedRes {
  /** 主键 */
  id: string;
  /** 工号（`EMP-0001` 或用户自填值） */
  emp_no: string;
}

/** 修改 / 删除回执（后端 `EmployeeIdRes`） */
export interface EmployeeIdRes {
  /** 主键 */
  id: string;
}

/**
 * 敏感字段明文出参。
 *
 * HR-13 尚未落地（卡在 todo），响应形状未定稿，故这里用「宽容形状」：
 * 三种候选形态都由 {@link plainValueOf} 兜住 ——
 * `{ id_card, bank_account }` / `{ field, value }` / 纯字符串。
 * 等 HR-13 落地后把这里收紧成确定类型即可（页面只经 `plainValueOf` 读值）。
 */
export interface EmployeeSensitivePlain {
  id_card?: string | null;
  bank_account?: string | null;
  field?: string;
  value?: string | null;
  [key: string]: unknown;
}

// ==================== 纯函数 ====================

/**
 * 在一张「码值 → 中文」表里查标签；查不到时**原样返回码值**。
 *
 * 与后端 `employee_dto::label_of` 同口径：显示码值比显示「未知」更有排查价值。
 */
export function labelOf(table: Record<string, string>, code: string | null | undefined): string {
  const key = (code ?? '').trim();
  if (!key) {
    return '';
  }
  return table[key] ?? table[key.toUpperCase()] ?? key;
}

/** 员工状态中文（后端 `status_label` 缺失时的兜底） */
export function statusLabel(code: string | null | undefined): string {
  return labelOf(STATUS_LABELS, code);
}

/** 员工状态标签配色（未知码值 → `default`，不 panic） */
export function statusTheme(code: string | null | undefined): 'primary' | 'success' | 'warning' | 'default' {
  const key = (code ?? '').toUpperCase() as EmployeeStatus;
  return STATUS_THEMES[key] ?? 'default';
}

/** 合同状态标签配色 */
export function contractStatusTheme(
  code: string | null | undefined,
): 'success' | 'warning' | 'default' | 'primary' {
  return CONTRACT_STATUS_THEMES[(code ?? '').toUpperCase()] ?? 'default';
}

/**
 * 合同到期提示文案（设计 §6.2.1：`≤ 30` 天橙字 + tooltip「还有 N 天到期」）。
 *
 * - `null` → 空串（无合同 / 无终止日，调用方不渲染）；
 * - 负数 → `已过期 N 天`；
 * - `0` → `今天到期`；
 * - 正数 → `还有 N 天到期`。
 */
export function contractExpireText(days: number | null | undefined): string {
  if (days === null || days === undefined || !Number.isFinite(days)) {
    return '';
  }
  if (days < 0) {
    return `已过期 ${Math.abs(days)} 天`;
  }
  if (days === 0) {
    return '今天到期';
  }
  return `还有 ${days} 天到期`;
}

/** 是否需要「合同快到期」提醒：`≤ 30` 天（含已过期；与后端 `contract_urgent()` 同判据） */
export function isContractUrgent(days: number | null | undefined): boolean {
  return typeof days === 'number' && Number.isFinite(days) && days <= CONTRACT_EXPIRE_WARN_DAYS;
}

/** 列表「合同到期」单元格的 class（橙字由 CSS `.is-urgent` 给） */
export function contractExpireClass(days: number | null | undefined): string {
  return isContractUrgent(days) ? 'is-urgent' : '';
}

/**
 * 筛选状态 → 查询串里的 `status`。
 *
 * 后端是 `Query` 提取（`serde_urlencoded` 不支持重复键 → Vec），本仓的既有约定是
 * **逗号分隔**（`announcement/handler.rs:713` 的 `query.status.split(',')`）。
 * 空数组 → `undefined`（= 后端默认三态，不含离职）。
 */
export function serializeStatuses(statuses: readonly string[] | null | undefined): string | undefined {
  const list = (statuses ?? []).map((item) => item.trim()).filter(Boolean);
  return list.length > 0 ? list.join(',') : undefined;
}

/**
 * 列表筛选 → 请求参数（丢掉空值）。
 *
 * `include_resigned` 与 `status` 的语义关系：勾了「含离职」但没选状态时，
 * 后端按 `include_resigned=true` 放开离职；一旦显式选了状态，就以选中的为准
 * （`RESIGNED` 没选就看不到离职 —— 这是后端 `EmployeeFilter` 的既有语义）。
 */
export function buildEmployeeQuery(
  filter: {
    keyword?: string;
    dept_id?: string | null;
    status?: string[];
    employment_type?: string | null;
    include_resigned?: boolean;
  },
  page: number,
  pageSize: number = EMPLOYEE_PAGE_SIZE,
): EmployeePageQuery {
  const query: EmployeePageQuery = {
    page: page > 0 ? page : 1,
    page_size: pageSize > 0 ? pageSize : EMPLOYEE_PAGE_SIZE,
  };
  const keyword = (filter.keyword ?? '').trim();
  if (keyword) {
    query.keyword = keyword;
  }
  if (filter.dept_id) {
    query.dept_id = filter.dept_id;
  }
  const status = serializeStatuses(filter.status);
  if (status) {
    query.status = status.split(',');
  }
  if (filter.employment_type) {
    query.employment_type = filter.employment_type;
  }
  if (filter.include_resigned) {
    query.include_resigned = true;
  }
  return query;
}

/**
 * 是否有任何筛选条件生效（空态文案「没有匹配的员工 / 还没有员工」靠它区分）。
 *
 * `include_resigned=false`（默认）不算条件 —— 它是默认值，不是用户的选择。
 */
export function isEmployeeFiltered(filter: {
  keyword?: string;
  dept_id?: string | null;
  status?: string[];
  employment_type?: string | null;
  include_resigned?: boolean;
}): boolean {
  return Boolean(
    (filter.keyword ?? '').trim() ||
      filter.dept_id ||
      (filter.status ?? []).length > 0 ||
      filter.employment_type ||
      filter.include_resigned,
  );
}

/** 空筛选（「重置」用；`include_resigned` 一并回默认 false） */
export function emptyEmployeeFilter(): {
  keyword: string;
  dept_id: string | undefined;
  status: string[];
  employment_type: string | undefined;
  include_resigned: boolean;
} {
  return {
    keyword: '',
    dept_id: undefined,
    status: [],
    employment_type: undefined,
    include_resigned: false,
  };
}

/** 现行薪资标准（`current=true` 那批；顺序保持后端给的 `effective_from DESC`） */
export function currentSalaries(items: readonly EmployeeSalary[] | null | undefined): EmployeeSalary[] {
  return (items ?? []).filter((item) => item.current);
}

/** 薪资历史（非现行那批） */
export function salaryHistory(items: readonly EmployeeSalary[] | null | undefined): EmployeeSalary[] {
  return (items ?? []).filter((item) => !item.current);
}

/**
 * 金额（后端 `Decimal` 字符串）→ 显示文本（千分位 + 2 位小数）。
 *
 * 本文件不 import 请求层，也就不能 import `erp/shared/money.ts` 之外的东西 ——
 * 那个模块是纯函数，可以引（它没有 axios 依赖）。加总仍用整数分。
 */
export function formatSalaryAmount(value: string | number | null | undefined): string {
  const raw = value === null || value === undefined || value === '' ? 0 : Number(value);
  const num = Number.isFinite(raw) ? raw : 0;
  return num.toLocaleString('zh-CN', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}

/** 现行薪资合计（整数分累加后换回元，避免浮点误差） */
export function sumSalary(items: readonly EmployeeSalary[] | null | undefined): string {
  const cents = (items ?? []).reduce((acc, item) => {
    const num = Number(item.amount);
    if (!Number.isFinite(num)) {
      return acc;
    }
    // 负数先取绝对值再补号，与后端 `Decimal::round_dp` 的 half-away-from-zero 一致
    const sign = num < 0 ? -1 : 1;
    return acc + sign * Math.round(Math.abs(num) * 100);
  }, 0);
  return formatSalaryAmount(cents / 100);
}

/** 详情页「基本信息」用的空值占位（不显示 `null` / `undefined`） */
export function textOr(value: string | number | null | undefined, fallback = '—'): string {
  if (value === null || value === undefined) {
    return fallback;
  }
  const text = String(value).trim();
  return text ? text : fallback;
}

/**
 * 从 `/employee/{id}/sensitive` 的响应里取某个字段的明文。
 *
 * HR-13 未落地前响应形状未定稿，三种候选形态都兜住（见 {@link EmployeeSensitivePlain}）；
 * 都取不到 → `null`（页面显示「未返回」而不是 `undefined` 字符串）。
 */
export function plainValueOf(
  response: EmployeeSensitivePlain | string | null | undefined,
  field: 'id_card' | 'bank_account',
): string | null {
  if (response === null || response === undefined) {
    return null;
  }
  if (typeof response === 'string') {
    return response.trim() || null;
  }
  // 形态 ①（最可能）：{ id_card, bank_account }
  const direct = response[field];
  if (typeof direct === 'string' && direct.trim()) {
    return direct.trim();
  }
  // 形态 ②：{ field, value }
  if (response.field === field && typeof response.value === 'string' && response.value.trim()) {
    return response.value.trim();
  }
  // 形态 ③：{ data: { ... } }（包了一层的实现）
  const nested = response.data;
  if (nested && typeof nested === 'object') {
    const inner = (nested as Record<string, unknown>)[field];
    if (typeof inner === 'string' && inner.trim()) {
      return inner.trim();
    }
  }
  return null;
}

/** 表格列（`colKey` 与后端出参同名；操作列最后） */
export const EMPLOYEE_COLUMNS = [
  { colKey: 'emp_no', title: '工号', width: 120 },
  { colKey: 'name', title: '姓名', width: 100 },
  { colKey: 'dept_name', title: '部门', width: 140, ellipsis: true },
  { colKey: 'position_name', title: '岗位', width: 120, ellipsis: true },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'hire_date', title: '入职日期', width: 120 },
  { colKey: 'contract_end_date', title: '合同到期', width: 170 },
  { colKey: 'mobile', title: '手机', width: 140 },
  { colKey: 'op', title: '操作', width: 90, fixed: 'right' as const },
];
