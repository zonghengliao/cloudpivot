/**
 * 员工建档表单（本卡：新建 / 编辑 / 删除）单测 —— 全部跑在无 DOM 的 `tsx --test` 里。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 测什么
 *
 * 1. **与后端对拍**：字段长度上限（`MAX_*`）、证件类型码表（`ID_CARD_TYPES`）、
 *    性别码值（`ck_biz_employee_gender`）逐条读 Rust 源码比对 —— 后端改数值 / 改码值，
 *    前端先红，而不是等用户提交时才 400。
 * 2. **口径**：业务日按 `+08:00` 算（北京时间 08:00 前不能用 UTC 的「昨天」拦当天入职）、
 *    身份证 18 位 + GB 11643 校验位、入职日期 ≤ 今天、试用期截止 ≥ 入职日期。
 * 3. **请求体**（最容易出事的一层）：
 *    - 新建：空串字段不进请求体（后端当 NULL）、性别传数字码值、不带 status；
 *    - 编辑：只带**改过的**字段；身份证 / 工资卡号留空**绝不传空串**（否则一次「只改手机号」
 *      会把这两列清成 NULL）；`emp_no` 永远不进修改请求体。
 * 4. **接线**：`index.vue` 是否真的挂了三个按钮与权限码（逻辑对、模板写漏了也要红）。
 *
 * 不渲染 `.vue`：本仓没有 happy-dom/jsdom（零新依赖），渲染层用源码断言兜住。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import {
  EMPLOYEE_FIELD_MAX,
  EMPLOYEE_MIN_NAME,
  GENDER_LABELS,
  ID_CARD_TYPE_LABELS,
} from '@/api/business/hr/employee-kinds';
import {
  buildCreatePayload,
  buildUpdatePayload,
  businessToday,
  emptyEmployeeForm,
  errorCount,
  isDateKey,
  isDeleteConfirmed,
  isEmptyUpdate,
  isReadonlyEmployee,
  isValidIdCard,
  validateEmployeeForm,
  type EmployeeFormState,
} from '@/views/business/hr/employee/employee-form';
import type { EmployeeCreateReq, EmployeeUpdateReq } from '@/api/business/hr/employee-kinds';

/** 后端入参校验源码（长度上限 / 取值域的唯一真源） */
const RUST_HANDLER = readFileSync(
  new URL('../../../backend/src/business/hr/employee_handler.rs', import.meta.url),
  'utf8',
);
/** 后端出参词表源码（证件类型码表在 dto 里） */
const RUST_DTO = readFileSync(
  new URL('../../../backend/src/business/hr/employee_dto.rs', import.meta.url),
  'utf8',
);
/** 页面源码（接线断言用） */
const PAGE = readFileSync(new URL('../../src/views/business/hr/employee/index.vue', import.meta.url), 'utf8');
/** 建表迁移（性别码值的真源：`ck_biz_employee_gender`） */
const DDL = readFileSync(
  new URL('../../../backend/migrations/business/V0383__hr_tables.up.sql', import.meta.url),
  'utf8',
);

/** 今天的业务日（测试里统一用固定值，别让用例在跨零点时随机红） */
const TODAY = '2026-11-13';

/** 造一份通过校验的最小表单（各用例只改自己关心的字段） */
function validForm(patch: Partial<EmployeeFormState> = {}): EmployeeFormState {
  return {
    ...emptyEmployeeForm(TODAY),
    name: '张三',
    gender: 1,
    dept_id: '20000000-0000-4000-8000-0000000f0366',
    hire_date: '2026-11-01',
    ...patch,
  };
}

// ==================== ① 与后端对拍 ====================

test('字段长度上限逐条镜像 employee_handler.rs 的 MAX_*', () => {
  /** 前端键 → Rust 常量名 */
  const pairs: Array<[keyof typeof EMPLOYEE_FIELD_MAX, string]> = [
    ['emp_no', 'MAX_EMP_NO'],
    ['name', 'MAX_NAME'],
    ['id_card', 'MAX_ID_CARD'],
    ['mobile', 'MAX_MOBILE'],
    ['email', 'MAX_EMAIL'],
    ['position_code', 'MAX_POSITION'],
    ['job_level', 'MAX_JOB_LEVEL'],
    ['school', 'MAX_SCHOOL'],
    ['major', 'MAX_MAJOR'],
    ['native_place', 'MAX_PLACE'],
    ['address', 'MAX_ADDRESS'],
    ['emergency_contact', 'MAX_PERSON_NAME'],
    ['emergency_phone', 'MAX_PHONE'],
    ['emergency_relation', 'MAX_RELATION'],
    ['bank_name', 'MAX_BANK_NAME'],
    ['bank_branch', 'MAX_BRANCH'],
    ['bank_account', 'MAX_BANK_ACCOUNT'],
    ['bank_account_name', 'MAX_PERSON_NAME'],
  ];
  for (const [key, constName] of pairs) {
    const match = RUST_HANDLER.match(new RegExp(`const ${constName}: usize = (\\d+);`));
    assert.ok(match, `Rust 源码里找不到 ${constName}`);
    assert.equal(EMPLOYEE_FIELD_MAX[key], Number(match[1]), `${key} 的长度上限与后端 ${constName} 不一致`);
  }
});

test('证件类型码表镜像 employee_dto.rs::ID_CARD_TYPE_LABELS', () => {
  const start = RUST_DTO.indexOf('pub const ID_CARD_TYPE_LABELS');
  assert.ok(start >= 0, 'Rust 源码里找不到 ID_CARD_TYPE_LABELS');
  const body = RUST_DTO.slice(start, RUST_DTO.indexOf('];', start));
  const pairs: Array<[string, string]> = [];
  const pattern = /\("([A-Z_0-9]+)",\s*"([^"]+)"\)/g;
  let match: RegExpExecArray | null = pattern.exec(body);
  while (match) {
    pairs.push([match[1], match[2]]);
    match = pattern.exec(body);
  }
  assert.ok(pairs.length > 0, 'ID_CARD_TYPE_LABELS 没解析出条目');
  assert.equal(Object.keys(ID_CARD_TYPE_LABELS).length, pairs.length, '证件类型条数与后端不一致');
  for (const [code, label] of pairs) {
    assert.equal(ID_CARD_TYPE_LABELS[code], label, `证件类型 ${code} 的中文与后端不一致`);
  }
  // 表单默认值必须落在后端取值域里（否则新建直接被 `require_one_of` 400）
  assert.ok(pairs.some(([code]) => code === 'ID_CARD'), '默认证件类型 ID_CARD 必须是后端的合法取值');
});

test('性别码值与建表 CHECK 一致（1 男 / 2 女 / 0 未知）', () => {
  const check = DDL.match(/ck_biz_employee_gender[^;]*gender[^;]*/i);
  assert.ok(check, 'V0383 里找不到 ck_biz_employee_gender');
  const line = check[0];
  for (const value of [0, 1, 2]) {
    assert.ok(line.includes(String(value)), `性别码值 ${value} 不在 ck_biz_employee_gender 里`);
  }
  assert.deepEqual(Object.keys(GENDER_LABELS).map(Number).sort((a, b) => a - b), [0, 1, 2]);
  assert.equal(GENDER_LABELS[1], '男');
  assert.equal(GENDER_LABELS[2], '女');
});

// ==================== ② 业务日 / 身份证 ====================

test('businessToday 按 +08:00 算，不能用 UTC 的昨天', () => {
  // UTC 2026-11-12T16:30Z = 北京时间 2026-11-13 00:30（凌晨）
  assert.equal(businessToday(new Date('2026-11-12T16:30:00Z')), '2026-11-13');
  // UTC 2026-11-12T15:59Z = 北京时间 2026-11-12 23:59
  assert.equal(businessToday(new Date('2026-11-12T15:59:00Z')), '2026-11-12');
});

test('isDateKey 挡住假日期（2 月 30 日 / 13 月 / 斜杠格式）', () => {
  assert.equal(isDateKey('2026-02-28'), true);
  assert.equal(isDateKey('2026-02-30'), false);
  assert.equal(isDateKey('2026-13-01'), false);
  assert.equal(isDateKey('2026/02/28'), false);
  assert.equal(isDateKey(''), false);
});

test('isValidIdCard：18 位 + 校验位 + 出生日期', () => {
  // 加权和 167 % 11 = 2 → 校验位 X（GB 11643 标准算法）
  assert.equal(isValidIdCard('11010519491231002X'), true);
  assert.equal(isValidIdCard('11010519491231002x'), true, '末位小写 x 也认（手打常态）');
  assert.equal(isValidIdCard('110105194912310021'), false, '校验位错');
  assert.equal(isValidIdCard('11010519491231002'), false, '17 位');
  assert.equal(isValidIdCard('110105491231002'), false, '15 位老号不收');
  assert.equal(isValidIdCard('11010519491331002X'), false, '出生月份 13');
  assert.equal(isValidIdCard(''), false);
});

// ==================== ③ 校验 ====================

test('必填项缺失时逐条给出中文错误', () => {
  // 空表单（连入职日期也清掉 —— `emptyEmployeeForm` 默认填今天，所以这里显式给空串）
  const errors = validateEmployeeForm({ ...emptyEmployeeForm(TODAY), hire_date: '' }, {
    mode: 'create',
    today: TODAY,
  });
  for (const key of ['name', 'gender', 'dept_id', 'hire_date'] as const) {
    assert.ok(errors[key], `空表单应该拦下 ${key}`);
  }
  assert.equal(errorCount(errors) >= 4, true);
  // 默认值字段不该报错（证件类型 = 身份证、用工形式 = 正式）
  assert.equal(errors.id_card_type, undefined, '证件类型有默认值，不该红');
  assert.equal(errors.employment_type, undefined, '用工形式有默认值，不该红');
  // 反过来：默认表单只差「姓名 / 性别 / 部门」三项（入职日期已默认今天）
  const defaults = validateEmployeeForm(emptyEmployeeForm(TODAY), { mode: 'create', today: TODAY });
  assert.equal(defaults.hire_date, undefined, '入职日期默认今天，不该红');
  assert.equal(errorCount(defaults), 3);
});


test('姓名：2-50 字；1 个字与 51 个字都拦', () => {
  assert.ok(validateEmployeeForm(validForm({ name: '张' }), { mode: 'create', today: TODAY }).name);
  const errors = validateEmployeeForm(validForm({ name: '张'.repeat(EMPLOYEE_FIELD_MAX.name + 1) }), {
    mode: 'create',
    today: TODAY,
  });
  assert.ok(errors.name?.includes(String(EMPLOYEE_FIELD_MAX.name)));
  assert.equal(validateEmployeeForm(validForm({ name: '张三' }), { mode: 'create', today: TODAY }).name, undefined);
  assert.equal(EMPLOYEE_MIN_NAME, 2);
});

test('入职日期不得晚于今天；试用期截止不得早于入职日期', () => {
  const future = validateEmployeeForm(validForm({ hire_date: '2026-12-01' }), { mode: 'create', today: TODAY });
  assert.ok(future.hire_date?.includes('不能晚于今天'), future.hire_date);

  const probation = validateEmployeeForm(validForm({ probation_end_date: '2026-10-01' }), {
    mode: 'create',
    today: TODAY,
  });
  assert.ok(probation.probation_end_date?.includes('不能早于入职日期'), probation.probation_end_date);

  // 同一天是合法的（试用期 0 天是边界，但后端也放行：`end < hire_date` 才拒）
  assert.equal(
    validateEmployeeForm(validForm({ probation_end_date: '2026-11-01' }), { mode: 'create', today: TODAY })
      .probation_end_date,
    undefined,
  );
});

test('身份证只在「证件类型 = 身份证」时校验校验位（护照不按 18 位拦）', () => {
  const bad = validateEmployeeForm(validForm({ id_card_type: 'ID_CARD', id_card: '110105194912310021' }), {
    mode: 'create',
    today: TODAY,
  });
  assert.ok(bad.id_card, '身份证校验位错必须拦');

  const passport = validateEmployeeForm(validForm({ id_card_type: 'PASSPORT', id_card: 'E12345678' }), {
    mode: 'create',
    today: TODAY,
  });
  assert.equal(passport.id_card, undefined, '护照不该走 18 位规则');

  const ok = validateEmployeeForm(validForm({ id_card_type: 'ID_CARD', id_card: '11010519491231002X' }), {
    mode: 'create',
    today: TODAY,
  });
  assert.equal(ok.id_card, undefined);
});

test('手机 / 邮箱：空着不管，填了要像样', () => {
  assert.ok(validateEmployeeForm(validForm({ mobile: 'abc123' }), { mode: 'create', today: TODAY }).mobile);
  assert.ok(validateEmployeeForm(validForm({ email: 'zhangsan' }), { mode: 'create', today: TODAY }).email);
  const ok = validateEmployeeForm(validForm({ mobile: '+86 138-0000-1234', email: 'a@b.cn' }), {
    mode: 'create',
    today: TODAY,
  });
  assert.equal(ok.mobile, undefined);
  assert.equal(ok.email, undefined);
});

test('编辑模式放行老数据的 gender=0（未知），新建模式不放', () => {
  const form = validForm({ gender: 0 });
  assert.ok(validateEmployeeForm(form, { mode: 'create', today: TODAY }).gender, '新建必须明确男女');
  assert.equal(validateEmployeeForm(form, { mode: 'update', today: TODAY }).gender, undefined, '老档案 0 不该拦');
});

// ==================== ④ 请求体 ====================

test('新建请求体：空串字段不进请求体、性别是数字、不带 status', () => {
  const payload = buildCreatePayload(validForm({ mobile: '', email: 'zhangsan@example.com', remark: '   ' }));
  assert.equal(payload.name, '张三');
  assert.equal(payload.gender, 1);
  assert.equal(payload.employment_type, 'FULL_TIME');
  assert.equal(payload.id_card_type, 'ID_CARD');
  assert.equal(payload.email, 'zhangsan@example.com');
  assert.equal('mobile' in payload, false, '空手机号不该出现在请求体里');
  assert.equal('remark' in payload, false, '纯空白备注不该出现在请求体里');
  assert.equal('emp_no' in payload, false, '工号留空 = 后端取号，请求体里不该有键');
  assert.equal('status' in (payload as unknown as Record<string, unknown>), false, '新档状态由后端定（PROBATION）');
});

test('编辑请求体：只带改过的字段；清空要传空串；emp_no 永不上行', () => {
  const original = validForm({ mobile: '13800001234', email: '', remark: '老备注' });
  const form: EmployeeFormState = {
    ...original,
    mobile: '13900004321', // 改
    email: '', // 没改（原本就空）
    remark: '', // 清空（原本有值）
    emp_no: 'EMP-0001',
  };
  const payload = buildUpdatePayload(form, original);
  assert.equal(payload.mobile, '13900004321');
  assert.equal(payload.remark, '', '清空要显式传空串，后端据此置 NULL');
  assert.equal('email' in payload, false, '没改的字段不该上行');
  assert.equal('emp_no' in payload, false, '工号不是可改列');
  assert.equal('name' in payload, false);
});

test('编辑请求体：身份证 / 工资卡号留空绝不传（否则会清库）', () => {
  const original = validForm({ id_card: '', bank_account: '' }); // 预填时这两个就是空的
  const untouched = buildUpdatePayload({ ...original, mobile: '13900004321' }, original);
  assert.equal('id_card' in untouched, false, '留空 = 不改：绝不能传空串把身份证清成 NULL');
  assert.equal('bank_account' in untouched, false, '留空 = 不改：绝不能传空串把工资卡号清成 NULL');

  const filled = buildUpdatePayload({ ...original, id_card: '11010519491231002X' }, original);
  assert.equal(filled.id_card, '11010519491231002X', '重填了才提交');
});

test('buildUpdatePayload 没有任何改动时是空对象（不白跑一次 PUT）', () => {
  const original = validForm();
  assert.equal(isEmptyUpdate(buildUpdatePayload({ ...original }, original)), true);
});

test('请求体类型形状：必填三件套在 Create 里是必选（编译期约束）', () => {
  // 这不是运行时断言，而是把契约钉在类型上：删除任一必填键，typecheck 先红
  const payload: EmployeeCreateReq = {
    name: '张三',
    dept_id: 'd',
    hire_date: TODAY,
  };
  assert.equal(payload.name, '张三');
  const update: EmployeeUpdateReq = { mobile: '1' };
  assert.equal(update.mobile, '1');
});

// ==================== ⑤ 删除 / 只读 ====================

test('删除确认必须逐字等于姓名（去首尾空白）', () => {
  assert.equal(isDeleteConfirmed('张三', '张三'), true);
  assert.equal(isDeleteConfirmed('张三', ' 张三 '), true);
  assert.equal(isDeleteConfirmed('张三', '张'), false);
  assert.equal(isDeleteConfirmed('张三', '李三'), false);
  assert.equal(isDeleteConfirmed('', ''), false, '空姓名不该被判为「匹配」');
});

test('已离职档案只读', () => {
  assert.equal(isReadonlyEmployee('RESIGNED'), true);
  assert.equal(isReadonlyEmployee('resigned'), true);
  assert.equal(isReadonlyEmployee('ACTIVE'), false);
  assert.equal(isReadonlyEmployee(null), false);
});

// ==================== ⑥ 接线（index.vue 真的挂了按钮） ====================

test('工具栏「新建员工」按钮 + 权限码 + testid', () => {
  assert.ok(PAGE.includes('data-testid="hr-emp-create"'), '缺少「新建员工」按钮');
  assert.ok(PAGE.includes("create: 'business:hr:employee:create'"), '缺少 create 权限码');
  assert.ok(PAGE.includes('v-if="canCreate"'), '新建按钮必须按 canCreate 渲染（不是 disabled）');
  assert.ok(PAGE.includes('@click="openCreate"'), '新建按钮要能打开表单');
});

test('详情抽屉底部「编辑 / 删除」按钮 + 权限码', () => {
  assert.ok(PAGE.includes('data-testid="hr-emp-edit"'), '缺少「编辑」按钮');
  assert.ok(PAGE.includes('data-testid="hr-emp-delete"'), '缺少「删除」按钮');
  assert.ok(PAGE.includes("update: 'business:hr:employee:update'"), '缺少 update 权限码');
  assert.ok(PAGE.includes("delete: 'business:hr:employee:delete'"), '缺少 delete 权限码');
  assert.ok(PAGE.includes('v-if="canUpdate && !readonlyDetail"'), '编辑按钮要按 canUpdate 渲染，且已离职不给');
  assert.ok(PAGE.includes('v-if="canDelete && !readonlyDetail"'), '删除按钮要按 canDelete 渲染，且已离职不给');
});

test('表单是分组展示（5 段 + 备注），不是一屏长表单', () => {
  for (const section of ['basic', 'contact', 'edu', 'emergency', 'bank']) {
    assert.ok(PAGE.includes(`data-testid="hr-emp-form-section-${section}"`), `缺少「${section}」分组`);
  }
  // 必填控件必须带必填标记
  for (const field of ['姓名', '性别', '证件类型', '人事归属部门', '用工形式', '入职日期']) {
    const pattern = new RegExp(`label="${field}"[^>]*required-mark`);
    assert.match(PAGE, pattern, `「${field}」应标记为必填`);
  }
});

test('页面真的调了三个写接口，且业务日 / 校验走纯函数', () => {
  assert.ok(PAGE.includes('createEmployee(buildCreatePayload(form))'), '新建必须走 createEmployee + 纯函数拼 payload');
  assert.ok(PAGE.includes('updateEmployee(id, payload)'), '编辑必须走 updateEmployee');
  assert.ok(PAGE.includes('deleteEmployee(target.id)'), '删除必须走 deleteEmployee');
  assert.ok(PAGE.includes('validateEmployeeForm(form'), '提交前必须走纯函数校验');
  assert.ok(PAGE.includes('businessToday()'), '业务日必须走 businessToday（+08:00），不用本机时钟');
  assert.ok(!PAGE.includes('new Date()'), '不要在页面里用本机时钟（UTC 会让当天入职被当成未来）');
});
