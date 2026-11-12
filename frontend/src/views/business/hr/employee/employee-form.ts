/**
 * 员工建档表单的纯逻辑（本卡「员工档案新建 / 编辑 / 删除」）。
 *
 * 只放判定：表单状态、校验、请求体拼装、身份证校验、业务日。`.vue` 只负责画。
 * 纯函数 ⇒ `tests/unit/hr-employee-form.test.ts` 里不渲染组件也能全跑。
 *
 * # 三条与「想当然」不一样的口径（都踩过或会踩）
 *
 * 1. **编辑时身份证 / 工资卡号拿不到明文**：详情出参只有 `sensitive_masked.*_masked`
 *    （明文唯一出口是 `/sensitive`，每次调用写审计日志）。而 `PUT /employee/{id}` 的语义是
 *    「不传 = 保持原值、传空串 = 清空」—— 所以这两个字段**必须**在用户真的重填时才提交，
 *    否则「只改手机号」会把身份证和工资卡号清成 NULL。口径见 {@link buildUpdatePayload}。
 * 2. **编辑只提交改动过的字段**：其余字段不进请求体（后端保持原值）。这样即便某个字段
 *    未来被 `sys_field_mask` 打了码，也不会把 `138****5678` 这种打码值写回库里。
 * 3. **入职日期 ≤ 今天**（卡面要求）。后端不拦未来日期，但「入职日期」是考勤与薪资的
 *    起算点，先建一个下个月的空档会让当月工资单算错；要预登记请当天再建。
 *
 * # 身份证校验位
 *
 * 后端 `EmployeeCreateReq.id_card` 只做「≤18 字符」的长度校验（`MAX_ID_CARD`），
 * 拼错的身份证能一路进库，等报销 / 社保对接时才发现。所以 18 位 + GB 11643 校验位
 * 这条规则放在前端（{@link isValidIdCard}），只对证件类型 = 身份证 且填了值时才拦。
 */

import {
  EMPLOYEE_FIELD_MAX,
  EMPLOYEE_MIN_NAME,
  ID_CARD_TYPE_LABELS,
  type EmployeeCreateReq,
  type EmployeeDetail,
  type EmployeeUpdateReq,
} from '@/api/business/hr/employee-kinds';

/** 表单状态（控件值一律字符串；`gender` 是数字码值，性别单选没有「空串」这一档） */
export interface EmployeeFormState {
  /** 工号（新建留空 = 后端取号；编辑只读） */
  emp_no: string;
  /** 姓名 */
  name: string;
  /** 性别码值（1 男 / 2 女；`null` = 未选） */
  gender: number | null;
  /** 证件类型码值 */
  id_card_type: string;
  /** 身份证号（新建可填；编辑留空 = 不改） */
  id_card: string;
  /** 出生日期 */
  birth_date: string;
  /** 手机 */
  mobile: string;
  /** 邮箱 */
  email: string;
  /** 部门 id */
  dept_id: string;
  /** 岗位码值 */
  position_code: string;
  /** 职级码值 */
  job_level: string;
  /** 用工形式码值 */
  employment_type: string;
  /** 入职日期 */
  hire_date: string;
  /** 试用期截止 */
  probation_end_date: string;
  /** 学历码值 */
  education: string;
  /** 毕业院校 */
  school: string;
  /** 专业 */
  major: string;
  /** 毕业日期 */
  graduate_date: string;
  /** 籍贯 */
  native_place: string;
  /** 现住址 */
  address: string;
  /** 紧急联系人 */
  emergency_contact: string;
  /** 紧急联系电话 */
  emergency_phone: string;
  /** 与本人关系 */
  emergency_relation: string;
  /** 开户行 */
  bank_name: string;
  /** 开户支行 */
  bank_branch: string;
  /** 工资卡号（编辑留空 = 不改） */
  bank_account: string;
  /** 工资卡户名 */
  bank_account_name: string;
  /** 备注 */
  remark: string;
}

/** 表单错误（键 = 字段名，值 = 中文人话） */
export type EmployeeFormErrors = Partial<Record<keyof EmployeeFormState, string>>;

/** 表单模式：新建 / 编辑（编辑时身份证与工资卡号是「留空 = 不改」） */
export type EmployeeFormMode = 'create' | 'update';

/**
 * 业务日（`+08:00`）的 `YYYY-MM-DD`。
 *
 * 不用 `new Date().toISOString()`：那是 UTC，北京时间 08:00 之前会返回**前一天**，
 * 「入职日期 ≤ 今天」就会误拦当天入职。也不用 dayjs 的 tz 插件（本仓没装），
 * 直接给时刻加 8 小时再取日期。
 */
export function businessToday(now: Date = new Date()): string {
  const shifted = new Date(now.getTime() + 8 * 60 * 60 * 1000);
  return shifted.toISOString().slice(0, 10);
}

/** 空表单（新建用；`id_card_type` 默认身份证、`employment_type` 默认正式、`hire_date` 默认今天） */
export function emptyEmployeeForm(today: string = businessToday()): EmployeeFormState {
  return {
    emp_no: '',
    name: '',
    gender: null,
    id_card_type: 'ID_CARD',
    id_card: '',
    birth_date: '',
    mobile: '',
    email: '',
    dept_id: '',
    position_code: '',
    job_level: '',
    employment_type: 'FULL_TIME',
    hire_date: today,
    probation_end_date: '',
    education: '',
    school: '',
    major: '',
    graduate_date: '',
    native_place: '',
    address: '',
    emergency_contact: '',
    emergency_phone: '',
    emergency_relation: '',
    bank_name: '',
    bank_branch: '',
    bank_account: '',
    bank_account_name: '',
    remark: '',
  };
}

/**
 * 详情 → 表单（编辑预填）。
 *
 * 身份证 / 工资卡号**置空**：详情里只有打码值，把 `110***********1234` 填进输入框
 * 再保存就是把打码值写进库。这两个字段留空 = 不改（见文件头第 1 条）。
 */
export function formFromDetail(detail: EmployeeDetail): EmployeeFormState {
  const text = (value: string | null | undefined): string => (value ?? '').toString();
  return {
    emp_no: text(detail.emp_no),
    name: text(detail.name),
    // 老数据 `gender = 0`（未知）在单选里没有对应项 —— 留 `null` 让用户明确选一次；
    // 但那样「只改手机号」也会被性别必填拦住，所以 0 也当作已选（后端接受 0）。
    gender: detail.gender === 0 ? 0 : detail.gender,
    id_card_type: text(detail.id_card_type) || 'ID_CARD',
    id_card: '',
    birth_date: text(detail.birth_date),
    mobile: text(detail.mobile),
    email: text(detail.email),
    dept_id: text(detail.dept_id),
    position_code: text(detail.position_code),
    job_level: text(detail.job_level),
    employment_type: text(detail.employment_type) || 'FULL_TIME',
    hire_date: text(detail.hire_date),
    probation_end_date: text(detail.probation_end_date),
    education: text(detail.education),
    school: text(detail.school),
    major: text(detail.major),
    graduate_date: text(detail.graduate_date),
    native_place: text(detail.native_place),
    address: text(detail.address),
    emergency_contact: text(detail.emergency_contact),
    emergency_phone: text(detail.emergency_phone),
    emergency_relation: text(detail.emergency_relation),
    bank_name: text(detail.bank_name),
    bank_branch: text(detail.bank_branch),
    bank_account: '',
    bank_account_name: text(detail.bank_account_name),
    remark: text(detail.remark),
  };
}

/** 日期串是否是合法的 `YYYY-MM-DD`（含「2 月 30 日」这类假日期） */
export function isDateKey(value: string): boolean {
  const text = value.trim();
  if (!/^\d{4}-\d{2}-\d{2}$/.test(text)) {
    return false;
  }
  const [year, month, day] = text.split('-').map(Number);
  const date = new Date(Date.UTC(year, month - 1, day));
  return (
    date.getUTCFullYear() === year && date.getUTCMonth() === month - 1 && date.getUTCDate() === day
  );
}

/** 身份证加权因子（GB 11643 第 1-17 位） */
const ID_CARD_WEIGHTS = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2];
/** 校验位字符表（余数 → 字符） */
const ID_CARD_CHECK_CODES = ['1', '0', 'X', '9', '8', '7', '6', '5', '4', '3', '2'];

/**
 * 18 位身份证号校验（格式 + 出生日期 + GB 11643 校验位）。
 *
 * 允许末位小写 `x`（很多人手打），按 `X` 处理并接受。
 * 不接受 15 位老号：库列是 `VARCHAR(18)`、业务侧（社保 / 报销）也只认 18 位，
 * 收下 15 位等于把问题推到对账时。
 */
export function isValidIdCard(value: string): boolean {
  const text = value.trim().toUpperCase();
  if (!/^\d{17}[\dX]$/.test(text)) {
    return false;
  }
  const year = Number(text.slice(6, 10));
  const month = Number(text.slice(10, 12));
  const day = Number(text.slice(12, 14));
  if (!isDateKey(`${text.slice(6, 10)}-${text.slice(10, 12)}-${text.slice(12, 14)}`)) {
    return false;
  }
  if (year < 1900 || month < 1 || month > 12 || day < 1 || day > 31) {
    return false;
  }
  const sum = ID_CARD_WEIGHTS.reduce((acc, weight, index) => acc + Number(text[index]) * weight, 0);
  return ID_CARD_CHECK_CODES[sum % 11] === text[17];
}

/** 手机号（宽松：允许 `+86 138-0000-0000` 这类写法，但必须是 6-20 位数字/分隔符） */
export function isMobileLike(value: string): boolean {
  return new RegExp(`^[0-9+\\-() ]{6,${EMPLOYEE_FIELD_MAX.mobile}}$`).test(value.trim());
}

/** 邮箱（够用即可：本仓不追求 RFC 5322 全量） */
export function isEmailLike(value: string): boolean {
  return /^[^\s@]+@[^\s@]+\.[^\s@]{2,}$/.test(value.trim());
}

/**
 * 表单校验。返回空对象 = 通过。
 *
 * 文案与后端 400 的意思对齐（后端只说「name 不能为空」，这里说清「姓名要 2-50 字」），
 * 但**判定顺序按屏幕从上到下**：用户一眼看到的第一个红字就是最先要改的那个。
 */
export function validateEmployeeForm(
  form: EmployeeFormState,
  options: { mode: EmployeeFormMode; today?: string },
): EmployeeFormErrors {
  const today = options.today ?? businessToday();
  const errors: EmployeeFormErrors = {};
  const name = form.name.trim();

  if (!name) {
    errors.name = '请填姓名';
  } else if ([...name].length < EMPLOYEE_MIN_NAME) {
    errors.name = `姓名至少 ${EMPLOYEE_MIN_NAME} 个字`;
  } else if ([...name].length > EMPLOYEE_FIELD_MAX.name) {
    errors.name = `姓名最长 ${EMPLOYEE_FIELD_MAX.name} 个字（当前 ${[...name].length} 个）`;
  }

  // 性别：新建必须选；编辑时老数据可能是 0（未知），0 也放行（后端接受）
  if (form.gender !== 1 && form.gender !== 2 && !(options.mode === 'update' && form.gender === 0)) {
    errors.gender = '请选择性别';
  }

  if (!form.id_card_type.trim()) {
    errors.id_card_type = '请选择证件类型';
  }

  const idCard = form.id_card.trim();
  if (idCard) {
    if ([...idCard].length > EMPLOYEE_FIELD_MAX.id_card) {
      errors.id_card = `证件号最长 ${EMPLOYEE_FIELD_MAX.id_card} 个字符`;
    } else if (form.id_card_type === 'ID_CARD' && !isValidIdCard(idCard)) {
      errors.id_card = '身份证号不对：18 位数字（末位可为 X），且校验位要对得上';
    }
  }

  if (!form.dept_id.trim()) {
    errors.dept_id = '请选择人事归属部门（数据权限按部门裁剪，无部门档案谁都看不到）';
  }

  if (!form.employment_type.trim()) {
    errors.employment_type = '请选择用工形式';
  }

  const hireDate = form.hire_date.trim();
  if (!hireDate) {
    errors.hire_date = '请填入职日期';
  } else if (!isDateKey(hireDate)) {
    errors.hire_date = '入职日期格式是 YYYY-MM-DD';
  } else if (hireDate > today) {
    errors.hire_date = `入职日期不能晚于今天（${today}）`;
  }

  const probationEnd = form.probation_end_date.trim();
  if (probationEnd) {
    if (!isDateKey(probationEnd)) {
      errors.probation_end_date = '试用期截止日期格式是 YYYY-MM-DD';
    } else if (hireDate && isDateKey(hireDate) && probationEnd < hireDate) {
      errors.probation_end_date = '试用期截止不能早于入职日期';
    }
  }

  const birthDate = form.birth_date.trim();
  if (birthDate) {
    if (!isDateKey(birthDate)) {
      errors.birth_date = '出生日期格式是 YYYY-MM-DD';
    } else if (birthDate > today) {
      errors.birth_date = '出生日期不能晚于今天';
    } else if (birthDate < '1900-01-01') {
      errors.birth_date = '出生日期看着不对（1900 年以前）';
    }
  }

  const graduateDate = form.graduate_date.trim();
  if (graduateDate) {
    if (!isDateKey(graduateDate)) {
      errors.graduate_date = '毕业日期格式是 YYYY-MM-DD';
    } else if (graduateDate > today) {
      errors.graduate_date = '毕业日期不能晚于今天';
    }
  }

  if (form.mobile.trim() && !isMobileLike(form.mobile)) {
    errors.mobile = `手机号只能有数字 / 空格 / +-()，长度 6-${EMPLOYEE_FIELD_MAX.mobile}`;
  }
  if (form.email.trim() && !isEmailLike(form.email)) {
    errors.email = '邮箱格式不对（如 zhangsan@example.com）';
  }

  // 长度上限：与后端 `MAX_*` 逐条一致（超了后端 400，先在这里说清是哪个字段）
  const lengthRules: Array<[keyof EmployeeFormState, number, string]> = [
    ['emp_no', EMPLOYEE_FIELD_MAX.emp_no, '工号'],
    ['school', EMPLOYEE_FIELD_MAX.school, '毕业院校'],
    ['major', EMPLOYEE_FIELD_MAX.major, '专业'],
    ['native_place', EMPLOYEE_FIELD_MAX.native_place, '籍贯'],
    ['address', EMPLOYEE_FIELD_MAX.address, '现住址'],
    ['emergency_contact', EMPLOYEE_FIELD_MAX.emergency_contact, '紧急联系人'],
    ['emergency_phone', EMPLOYEE_FIELD_MAX.emergency_phone, '紧急联系电话'],
    ['emergency_relation', EMPLOYEE_FIELD_MAX.emergency_relation, '与本人关系'],
    ['bank_name', EMPLOYEE_FIELD_MAX.bank_name, '开户行'],
    ['bank_branch', EMPLOYEE_FIELD_MAX.bank_branch, '开户支行'],
    ['bank_account', EMPLOYEE_FIELD_MAX.bank_account, '工资卡号'],
    ['bank_account_name', EMPLOYEE_FIELD_MAX.bank_account_name, '工资卡户名'],
  ];
  for (const [key, max, label] of lengthRules) {
    const value = String(form[key] ?? '').trim();
    if ([...value].length > max && !errors[key]) {
      errors[key] = `${label}最长 ${max} 个字符（当前 ${[...value].length} 个）`;
    }
  }

  return errors;
}

/** 错误条数（保存按钮 / 提示条用） */
export function errorCount(errors: EmployeeFormErrors): number {
  return Object.keys(errors).length;
}

/** 去空白；空串 → `undefined`（请求体里不出现这个键） */
function trimmed(value: string): string {
  return value.trim();
}

/**
 * 表单 → 新建请求体。
 *
 * - 空串的可选字段**不进请求体**（后端把缺失当 `NULL`，等价且请求更短）；
 * - `emp_no` 留空 ⇒ 不带该键 ⇒ 后端取号（`EMP-0001`）；
 * - `gender` 传数字码值（后端 `Option<i16>`）。
 */
export function buildCreatePayload(form: EmployeeFormState): EmployeeCreateReq {
  const payload: EmployeeCreateReq = {
    name: trimmed(form.name),
    dept_id: trimmed(form.dept_id),
    hire_date: trimmed(form.hire_date),
    id_card_type: trimmed(form.id_card_type) || 'ID_CARD',
    employment_type: trimmed(form.employment_type) || 'FULL_TIME',
  };
  if (form.gender === 1 || form.gender === 2) {
    payload.gender = form.gender;
  }
  const optional: Array<[keyof EmployeeCreateReq, string]> = [
    ['emp_no', form.emp_no],
    ['id_card', form.id_card],
    ['birth_date', form.birth_date],
    ['mobile', form.mobile],
    ['email', form.email],
    ['position_code', form.position_code],
    ['job_level', form.job_level],
    ['probation_end_date', form.probation_end_date],
    ['education', form.education],
    ['school', form.school],
    ['major', form.major],
    ['graduate_date', form.graduate_date],
    ['native_place', form.native_place],
    ['address', form.address],
    ['emergency_contact', form.emergency_contact],
    ['emergency_phone', form.emergency_phone],
    ['emergency_relation', form.emergency_relation],
    ['bank_name', form.bank_name],
    ['bank_branch', form.bank_branch],
    ['bank_account', form.bank_account],
    ['bank_account_name', form.bank_account_name],
    ['remark', form.remark],
  ];
  for (const [key, value] of optional) {
    const text = trimmed(value);
    if (text) {
      (payload as unknown as Record<string, unknown>)[key] = text;
    }
  }
  return payload;
}

/**
 * 表单 → 修改请求体（**只带真的改过的字段**）。
 *
 * # 为什么按 diff 提交
 *
 * 1. 后端语义是「不传 = 保持原值」。整表单提交（含未改字段）虽然也等价，但一旦详情里
 *    某字段是打码值（`sys_field_mask` 命中），整表单就会把打码值写回库；
 *    diff 提交从结构上避免这件事。
 * 2. 用户清空某个可选字段（原值有、现在空）时**要传空串**（后端据此清列）——
 *    所以「改了」的判定是 `form[key] !== original[key]`，而不是「非空才带」。
 *
 * # 身份证 / 工资卡号（敏感、详情拿不到明文）
 *
 * 只在**用户真的填了**（trim 后非空）时才进请求体；留空 = 不改。
 * 见文件头第 1 条：无脑传空串会把这两列清成 NULL。
 *
 * 同时把 `emp_no` 摘掉：它不是可改列（后端 `UPDATE_SQL` 列白名单里没有它）。
 */
export function buildUpdatePayload(
  form: EmployeeFormState,
  original: EmployeeFormState,
): EmployeeUpdateReq {
  const payload: EmployeeUpdateReq = {};

  const diffKeys: Array<keyof EmployeeUpdateReq> = [
    'name',
    'id_card_type',
    'birth_date',
    'mobile',
    'email',
    'dept_id',
    'position_code',
    'job_level',
    'employment_type',
    'hire_date',
    'probation_end_date',
    'education',
    'school',
    'major',
    'graduate_date',
    'native_place',
    'address',
    'emergency_contact',
    'emergency_phone',
    'emergency_relation',
    'bank_name',
    'bank_branch',
    'bank_account_name',
    'remark',
  ];
  for (const key of diffKeys) {
    const next = trimmed(String(form[key as keyof EmployeeFormState] ?? ''));
    const prev = trimmed(String(original[key as keyof EmployeeFormState] ?? ''));
    if (next !== prev) {
      (payload as unknown as Record<string, unknown>)[key] = next;
    }
  }

  if (form.gender !== original.gender && (form.gender === 0 || form.gender === 1 || form.gender === 2)) {
    payload.gender = form.gender;
  }

  // 敏感字段：只认「用户填了值」，绝不因为「表单里是空」就传空串
  const idCard = trimmed(form.id_card);
  if (idCard) {
    payload.id_card = idCard;
  }
  const bankAccount = trimmed(form.bank_account);
  if (bankAccount) {
    payload.bank_account = bankAccount;
  }

  return payload;
}

/** 修改请求体是否为空（没有任何改动 —— 保存按钮直接提示「没有改动」而不是发一次空 PUT） */
export function isEmptyUpdate(payload: EmployeeUpdateReq): boolean {
  return Object.keys(payload).length === 0;
}

/**
 * 删除二次确认：输入的名字必须**逐字等于**档案姓名（去掉首尾空白）。
 *
 * 不做「输入 DELETE」这类确认：删除的是**某个具体的人**，输他的名字才真的看见了他。
 */
export function isDeleteConfirmed(name: string, input: string): boolean {
  const target = name.trim();
  return target.length > 0 && input.trim() === target;
}

/** 已离职档案是只读的（后端 `update_employee` 直接 409：历史薪资单的快照对不上） */
export function isReadonlyEmployee(status: string | null | undefined): boolean {
  return (status ?? '').toUpperCase() === 'RESIGNED';
}

/** 详情里两个敏感字段的打码值提示（编辑表单里显示「当前 XXX」用） */
export function maskedHint(detail: EmployeeDetail | null, field: 'id_card' | 'bank_account'): string {
  const masked =
    field === 'id_card' ? detail?.sensitive_masked?.id_card_masked : detail?.sensitive_masked?.bank_account_masked;
  return (masked ?? '').trim();
}

/** 证件类型中文（详情 / 表单提示用；未知码值原样回显） */
export function idCardTypeLabel(code: string | null | undefined): string {
  const key = (code ?? '').trim().toUpperCase();
  return ID_CARD_TYPE_LABELS[key] ?? key;
}
