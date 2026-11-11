/**
 * 员工档案页（HR-18）单测：全部跑在无 DOM 的 `tsx --test` 里。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 测什么
 *
 * 1. **词表镜像**：`employee-kinds.ts` 的 4 张「码值 → 中文」表逐条对齐后端
 *    `backend/src/business/hr/employee_dto.rs` 的常量（直接 `readFileSync` 读 Rust 源码）。
 *    后端改取值而前端没跟 → 这里先红，而不是线上看到 `PROBATION` 这种码值。
 * 2. **口径**：合同到期文案（≤30 天橙字 / 已过期 / 今天到期）、筛选序列化
 *    （数组 → 逗号串，后端 `Query` 是 `serde_urlencoded`，重复键会被拒）、
 *    薪资合计（整数分，不浮点累加）、明文解析（HR-13 三种候选形态）。
 * 3. **接线**：`index.vue` 是否真的用了这些纯函数（逻辑对、模板写错也要红）。
 *
 * 不渲染 `.vue`：本仓没有 happy-dom/jsdom（零新依赖），渲染层用源码断言兜住。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import {
  CONTRACT_STATUS_LABELS,
  CONTRACT_STATUS_THEMES,
  CONTRACT_TYPE_LABELS,
  CONTRACT_EXPIRE_WARN_DAYS,
  DEFAULT_STATUSES,
  EMPLOYEE_COLUMNS,
  EMPLOYMENT_TYPE_LABELS,
  ID_CARD_TYPE_LABELS,
  LEAVE_TYPE_LABELS,
  STATUS_LABELS,
  STATUS_THEMES,
  STATUS_VALUES,
  buildEmployeeQuery,
  contractExpireClass,
  contractExpireText,
  contractStatusTheme,
  currentSalaries,
  emptyEmployeeFilter,
  formatSalaryAmount,
  isContractUrgent,
  isEmployeeFiltered,
  labelOf,
  plainValueOf,
  salaryHistory,
  serializeStatuses,
  statusLabel,
  statusTheme,
  sumSalary,
  textOr,
  type EmployeeSalary,
} from '@/api/business/hr/employee-kinds';

/** 后端出参契约源码（词表唯一真源） */
const RUST_DTO = readFileSync(
  new URL('../../../backend/src/business/hr/employee_dto.rs', import.meta.url),
  'utf8',
);

/** 页面源码（接线断言用） */
const PAGE = readFileSync(
  new URL('../../src/views/business/hr/employee/index.vue', import.meta.url),
  'utf8',
);

/** 路由表源码（菜单 path 不漏注册） */
const ROUTER = readFileSync(new URL('../../src/router/index.ts', import.meta.url), 'utf8');

/**
 * 从 Rust 源码里解析一个 `pub const X: &[(&str, &str)] = &[("A","甲"), …];` 常量。
 *
 * 不用正则一次抓完：常量体里有注释与换行，逐条抓 `("key", "value")` 更稳，
 * 且能在断言失败时打印出真实条目。
 */
function rustPairs(constName: string): Array<[string, string]> {
  const start = RUST_DTO.indexOf(`pub const ${constName}`);
  assert.ok(start >= 0, `Rust 源码里找不到常量 ${constName}`);
  const end = RUST_DTO.indexOf('];', start);
  assert.ok(end > start, `常量 ${constName} 没有闭合的 \`];\``);
  const body = RUST_DTO.slice(start, end);
  const pairs: Array<[string, string]> = [];
  const pattern = /\("([A-Z_0-9]+)",\s*"([^"]+)"\)/g;
  let match: RegExpExecArray | null = pattern.exec(body);
  while (match) {
    pairs.push([match[1], match[2]]);
    match = pattern.exec(body);
  }
  return pairs;
}

// ==================== ① 词表镜像 ====================

test('status labels mirror employee_dto.rs::STATUS_LABELS', () => {
  const pairs = rustPairs('STATUS_LABELS');
  assert.equal(pairs.length, 4, 'DDL 的 ck_biz_employee_status 只有 4 个取值');
  for (const [code, label] of pairs) {
    assert.equal(STATUS_LABELS[code as keyof typeof STATUS_LABELS], label, `状态 ${code} 的中文名与后端不一致`);
  }
  // 取值域与顺序：下拉顺序由 STATUS_VALUES 决定，与后端 STATUS_VALUES 同序
  const rustValues = RUST_DTO.match(/pub const STATUS_VALUES: &\[&str\] = &\[([^\]]+)\]/);
  assert.ok(rustValues, 'Rust 源码里找不到 STATUS_VALUES');
  const values = rustValues[1]
    .split(',')
    .map((item) => item.trim().replace(/"/g, ''))
    .filter(Boolean);
  assert.deepEqual([...STATUS_VALUES], values, 'STATUS_VALUES 与后端逐字同序');
  assert.deepEqual([...DEFAULT_STATUSES], ['PROBATION', 'ACTIVE', 'SUSPENDED'], '列表默认不含离职（设计 §5.3）');
});

test('employment type / contract type / contract status / id card / leave type labels mirror backend', () => {
  const tables: Array<[string, Record<string, string>]> = [
    ['EMPLOYMENT_TYPE_LABELS', EMPLOYMENT_TYPE_LABELS],
    ['CONTRACT_TYPE_LABELS', CONTRACT_TYPE_LABELS],
    ['CONTRACT_STATUS_LABELS', CONTRACT_STATUS_LABELS],
    ['ID_CARD_TYPE_LABELS', ID_CARD_TYPE_LABELS],
    ['LEAVE_TYPE_LABELS', LEAVE_TYPE_LABELS],
  ];
  for (const [constName, table] of tables) {
    const pairs = rustPairs(constName);
    assert.ok(pairs.length > 0, `${constName} 没解析出条目`);
    assert.equal(Object.keys(table).length, pairs.length, `${constName} 条数与后端不一致`);
    for (const [code, label] of pairs) {
      assert.equal(table[code], label, `${constName} 的 ${code} 与后端不一致`);
    }
  }
});

test('contract expire threshold mirrors backend CONTRACT_EXPIRE_WARN_DAYS', () => {
  const match = RUST_DTO.match(/pub const CONTRACT_EXPIRE_WARN_DAYS: i64 = (\d+);/);
  assert.ok(match, 'Rust 源码里找不到 CONTRACT_EXPIRE_WARN_DAYS');
  assert.equal(CONTRACT_EXPIRE_WARN_DAYS, Number(match[1]), '合同「快到期」阈值必须与后端一致');
});

// ==================== ② 口径 ====================

test('status label / theme fall back to the raw code instead of blowing up', () => {
  assert.equal(statusLabel('PROBATION'), '试用期');
  assert.equal(statusLabel('ACTIVE'), '在职');
  assert.equal(statusLabel('NEW_CODE_NOT_IN_DDL'), 'NEW_CODE_NOT_IN_DDL', '未知码值原样显示（便于发现词表漏行）');
  assert.equal(statusLabel(null), '');

  assert.equal(statusTheme('ACTIVE'), 'success');
  assert.equal(statusTheme('PROBATION'), 'primary');
  assert.equal(statusTheme('SUSPENDED'), 'warning');
  assert.equal(statusTheme('RESIGNED'), 'default');
  assert.equal(statusTheme('WHAT'), 'default');
  // 主题表覆盖 DDL 的 4 个取值（新增状态必须补配色）
  assert.deepEqual(Object.keys(STATUS_THEMES).sort(), [...STATUS_VALUES].sort());
});

test('contract status themes cover the 4 CHECK values', () => {
  assert.deepEqual(Object.keys(CONTRACT_STATUS_THEMES).sort(), Object.keys(CONTRACT_STATUS_LABELS).sort());
  assert.equal(contractStatusTheme('active'), 'success', '码值大小写不敏感');
  assert.equal(contractStatusTheme('EXPIRED'), 'warning');
  assert.equal(contractStatusTheme(null), 'default');
});

test('contract expire text: 负数=已过期 / 0=今天到期 / 正数=还有 N 天', () => {
  assert.equal(contractExpireText(null), '');
  assert.equal(contractExpireText(undefined), '');
  assert.equal(contractExpireText(-3), '已过期 3 天');
  assert.equal(contractExpireText(0), '今天到期');
  assert.equal(contractExpireText(1), '还有 1 天到期');
  assert.equal(contractExpireText(30), '还有 30 天到期');
  assert.equal(contractExpireText(Number.NaN), '');
});

test('isContractUrgent: ≤30 天（含已过期）才算，31 天不算', () => {
  assert.equal(isContractUrgent(30), true, '阈值含 30');
  assert.equal(isContractUrgent(31), false);
  assert.equal(isContractUrgent(-1), true, '已过期也必须提醒（后端 contract_urgent 同口径）');
  assert.equal(isContractUrgent(null), false);
  assert.equal(contractExpireClass(30), 'is-urgent');
  assert.equal(contractExpireClass(31), '');
  assert.equal(contractExpireClass(null), '');
});

test('serializeStatuses: 数组 → 逗号串（后端 Query 是 serde_urlencoded，重复键会被拒）', () => {
  assert.equal(serializeStatuses([]), undefined, '空数组 = 后端默认三态');
  assert.equal(serializeStatuses(undefined), undefined);
  assert.equal(serializeStatuses(null), undefined);
  assert.equal(serializeStatuses(['ACTIVE']), 'ACTIVE');
  assert.equal(serializeStatuses(['PROBATION', 'ACTIVE']), 'PROBATION,ACTIVE');
  assert.equal(serializeStatuses([' ACTIVE ', '']), 'ACTIVE', '两端空白与被清空的选项要丢掉');
});

test('buildEmployeeQuery: 空筛选只带分页、含离职只在勾了才带', () => {
  const query = buildEmployeeQuery({}, 2, 50);
  assert.deepEqual(query, { page: 2, page_size: 50 }, '默认查询不能带 include_resigned=false 这类噪声');

  const full = buildEmployeeQuery(
    {
      keyword: '  张三  ',
      dept_id: 'dept-1',
      status: ['ACTIVE', 'PROBATION'],
      employment_type: 'FULL_TIME',
      include_resigned: true,
    },
    1,
    20,
  );
  assert.deepEqual(full, {
    page: 1,
    page_size: 20,
    keyword: '张三',
    dept_id: 'dept-1',
    status: ['ACTIVE', 'PROBATION'],
    employment_type: 'FULL_TIME',
    include_resigned: true,
  });

  // 非法页码兜底（页码为 0 / 负数时按第 1 页查，别把 400 甩给用户）
  assert.equal(buildEmployeeQuery({}, 0).page, 1);
  assert.equal(buildEmployeeQuery({}, 1, 0).page_size, 20);
});

test('isEmployeeFiltered: include_resigned=false 不算「筛选过」', () => {
  assert.equal(isEmployeeFiltered({}), false);
  assert.equal(isEmployeeFiltered({ keyword: '   ' }), false);
  assert.equal(isEmployeeFiltered({ status: [] }), false);
  assert.equal(isEmployeeFiltered({ include_resigned: false }), false);
  assert.equal(isEmployeeFiltered({ dept_id: 'd1' }), true);
  assert.equal(isEmployeeFiltered({ status: ['RESIGNED'] }), true);
  assert.equal(isEmployeeFiltered({ include_resigned: true }), true);
});

test('emptyEmployeeFilter resets every key (「重置」不能留下幽灵筛选)', () => {
  const empty = emptyEmployeeFilter();
  assert.deepEqual(Object.keys(empty).sort(), [
    'dept_id',
    'employment_type',
    'include_resigned',
    'keyword',
    'status',
  ]);
  assert.equal(empty.keyword, '');
  assert.equal(empty.dept_id, undefined);
  assert.deepEqual(empty.status, []);
  assert.equal(empty.employment_type, undefined);
  assert.equal(empty.include_resigned, false);
  assert.equal(isEmployeeFiltered(empty), false);
});

/** 一条薪资标准（`EmployeeSalary` 的最小构造） */
function salary(overrides: Partial<EmployeeSalary>): EmployeeSalary {
  return {
    item_code: 'BASE',
    item_name: '基本工资',
    amount: '8000.0000',
    effective_from: '2024-03-01',
    effective_to: null,
    current: true,
    remark: null,
    ...overrides,
  };
}

test('currentSalaries / salaryHistory split by the backend `current` flag', () => {
  const items = [
    salary({ item_code: 'BASE', current: true }),
    salary({ item_code: 'POST', item_name: '岗位津贴', amount: '500.5000', current: true }),
    salary({ item_code: 'BASE', amount: '7000.0000', effective_from: '2023-01-01', effective_to: '2024-02-29', current: false }),
  ];
  assert.deepEqual(
    currentSalaries(items).map((item) => item.item_code),
    ['BASE', 'POST'],
  );
  assert.equal(salaryHistory(items).length, 1);
  assert.equal(currentSalaries(null).length, 0, '没传子表时返回空数组，不能 undefined');
});

test('sumSalary: 整数分累加（8000 + 500.5 = 8,500.50，不出 8500.499999）', () => {
  assert.equal(sumSalary([]), '0.00');
  assert.equal(sumSalary([salary({}), salary({ item_code: 'POST', amount: '500.5000' })]), '8,500.50');
  assert.equal(sumSalary([salary({ amount: '0.1000' }), salary({ item_code: 'B', amount: '0.2000' })]), '0.30');
  assert.equal(sumSalary([salary({ amount: '-1200.0000' })]), '-1,200.00', '负数（考勤扣款）要保留符号');
  assert.equal(sumSalary([salary({ amount: 'not-a-number' })]), '0.00', '脏值不能让整页显示 NaN');
});

test('formatSalaryAmount keeps 2 decimals + thousands separators', () => {
  assert.equal(formatSalaryAmount('8000.0000'), '8,000.00');
  assert.equal(formatSalaryAmount(null), '0.00');
  assert.equal(formatSalaryAmount(''), '0.00');
});

test('textOr: null / 空白 都显示占位符（不显示 null）', () => {
  assert.equal(textOr(null), '—');
  assert.equal(textOr(undefined), '—');
  assert.equal(textOr('   '), '—');
  assert.equal(textOr('张三'), '张三');
  assert.equal(textOr(0), '0', '0 是真实值（不是空）');
  assert.equal(textOr(null, '未登记'), '未登记');
});

test('labelOf falls back to the code (与后端 label_of 同口径)', () => {
  assert.equal(labelOf({ A: '甲' }, 'A'), '甲');
  assert.equal(labelOf({ A: '甲' }, 'a'), '甲', '大小写不敏感');
  assert.equal(labelOf({ A: '甲' }, 'B'), 'B', '查不到就显示码值，藏起来更没法排查');
  assert.equal(labelOf({ A: '甲' }, ''), '');
});

test('plainValueOf 兜住 HR-13 的三种候选响应形态', () => {
  // 形态 ①：平铺
  assert.equal(plainValueOf({ id_card: '110101199001011234' }, 'id_card'), '110101199001011234');
  // 形态 ②：{ field, value }
  assert.equal(plainValueOf({ field: 'bank_account', value: '6222021234567890' }, 'bank_account'), '6222021234567890');
  // 形态 ③：包一层 data
  assert.equal(plainValueOf({ data: { id_card: 'x' } }, 'id_card'), 'x');
  // 纯字符串
  assert.equal(plainValueOf(' 6222 ', 'bank_account'), '6222');
  // 拿不到 → null（页面显示「未登记」而不是 "undefined"）
  assert.equal(plainValueOf({ field: 'id_card', value: 'x' }, 'bank_account'), null);
  assert.equal(plainValueOf(null, 'id_card'), null);
  assert.equal(plainValueOf({ id_card: '   ' }, 'id_card'), null);
});

test('EMPLOYEE_COLUMNS: 设计 §6.2.1 的列齐全，操作列在最后且右侧固定', () => {
  const keys = EMPLOYEE_COLUMNS.map((column) => column.colKey);
  assert.deepEqual(keys, [
    'emp_no',
    'name',
    'dept_name',
    'position_name',
    'status',
    'hire_date',
    'contract_end_date',
    'mobile',
    'op',
  ]);
  const last = EMPLOYEE_COLUMNS[EMPLOYEE_COLUMNS.length - 1];
  assert.equal(last.colKey, 'op');
  assert.equal(last.fixed, 'right');
});

// ==================== ③ 接线（源码断言） ====================

test('page wires the two real endpoints and the pure helpers', () => {
  assert.ok(PAGE.includes('fetchEmployeePage('), '列表必须调 /employee/page');
  assert.ok(PAGE.includes('fetchEmployeeDetail('), '详情必须调 /employee/{id}');
  assert.ok(PAGE.includes('buildEmployeeQuery(filter'), '查询条件必须过纯函数（别在页面里手拼参数）');
  assert.ok(PAGE.includes('contractExpireClass('), '合同到期橙字必须按纯函数判据');
  assert.ok(PAGE.includes('sumSalary(currentSalaryRows)'), '现行薪资合计必须过整数分累加');
  // 三条「别自己重算」的红线
  assert.ok(!PAGE.includes('new Date()'), '不要在页面里用本机时钟算到期天数（后端按 +08:00 算好了）');
  assert.ok(
    PAGE.includes("'business:hr:employee:sensitive'") && PAGE.includes('canViewPlain'),
    '「查看明文」按钮必须按 business:hr:employee:sensitive 权限显示',
  );
});

test('detail drawer has the three required blocks + attachment panel', () => {
  for (const tab of ['基本信息', '劳动合同', '薪资标准', '附件']) {
    assert.ok(PAGE.includes(`label="${tab}"`) || PAGE.includes(`\`劳动合同（`), `缺少「${tab}」区块`);
  }
  assert.ok(PAGE.includes('DOC_KIND.EMPLOYEE_CONTRACT'), '合同扫描件必须按 EMPLOYEE_CONTRACT 挂附件');
  assert.ok(PAGE.includes('DOC_KIND.EMPLOYEE'), '员工附件必须按 EMPLOYEE 挂附件');
  assert.ok(PAGE.includes('DocAttachmentPanel'), '附件面板必须复用共享组件');
  assert.ok(PAGE.includes('hr-emp-contract-timeline'), '合同时间线缺少 testid');
});

test('router registers /business/hr/employee with the menu permission code', () => {
  assert.ok(
    ROUTER.includes("path: 'business/hr/employee'"),
    '路由没注册 → 点菜单进 404（sys_menu 种子 V0387 已种这条 path）',
  );
  assert.ok(
    ROUTER.includes("permission: 'business:hr:employee:list'"),
    '路由 meta.permission 必须与 V0387 的页面权限码一致',
  );
});
