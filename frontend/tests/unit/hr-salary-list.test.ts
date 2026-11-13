/**
 * 薪资管理列表页（t_fc256a30）单测：全部跑在无 DOM 的 `tsx --test` 里。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 测什么
 *
 * 1. **词表镜像**：状态六档（码值 + 中文名）与后端 `salary_sheet_dto.rs::STATUSES`
 *    逐条对拍；分页 20/200、备注 500 与后端常量逐条对拍 —— 后端改取值而前端没跟，
 *    这里先红，而不是线上「筛完还是全量」。
 * 2. **权限码镜像**：本页 5 个码必须逐字出现在 `V0387__hr_menu_perms.up.sql` 的
 *    `sys_menu.auth_code` 里，并且与 `handler.rs::ROUTE_CONTRACT` 的四条路由对得上
 *    （`salary/page` / `salary/calc` / `salary` / `salary/my`）—— 「按钮点下去 403」是
 *    这类页面最常见的坑。
 * 3. **期间码**：`2026-11` 过；`2026-13` / `2026-00` / `202611` / `2026-1` / `26-11` 全拒，
 *    正则与库层 `ck_biz_sal_sheet_code` 同一把尺子（直接读迁移文件对拍）。
 * 4. **报文形状**：试算 `dry_run=true`、建单 `dry_run=false`（后端会与接口身份对拍，
 *    传错 400）；空筛选不发空参数（后端把 `status=` 当非法值 → 400）。
 * 5. **口径**：当前页合计按整数分累加（浮点会在 `0.1+0.2` 处假报警）；
 *    「我的薪资」明细项按 收入 → 应发前扣减 → 代扣 分组。
 * 6. **接线**：`index.vue` 真的走这四条接口、真按权限显隐两个 tab；
 *    `router/index.ts` 注册了 `/business/hr/salary` 且 component 与
 *    `sys_menu.component`（`business/hr/salary/index`）指向同一个视图文件 ——
 *    这一条正是 `system::menu::tests::n08` 的判据。
 *
 * 不渲染 `.vue`：本仓没有 happy-dom/jsdom（零新依赖），渲染层用源码断言兜住。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  CALC_MODE_HINTS,
  CALC_MODE_LABELS,
  MAX_CALC_REMARK,
  MY_ITEM_TYPE_ORDER,
  SALARY_LIST_COLUMNS,
  SALARY_LIST_MAX_PAGE_SIZE,
  SALARY_LIST_PAGE_SIZE,
  SALARY_LIST_PERMS,
  SALARY_LIST_STATUS_OPTIONS,
  SALARY_MY_COLUMNS,
  SALARY_SLIP_COLUMNS,
  buildCalcPayload,
  buildListQuery,
  buildMyQuery,
  calcFormError,
  calcResultText,
  currentPeriodCode,
  daysOf,
  emptyCalcForm,
  emptyListFilter,
  emptyMyFilter,
  emptySheetPageTotals,
  isListFiltered,
  listEmptyText,
  missingAttendanceText,
  myItemsSorted,
  myPaidText,
  mySalaryEmptyText,
  pageTotalsText,
  periodCodeError,
  salaryDetailPath,
  sheetPeriodText,
  sheetVoucherLines,
  slipAttendanceText,
  sumSheetRows,
  type MySalaryRow,
  type SalaryCalcResult,
  type SalarySheetRow,
} from '../../src/api/business/hr/salary-list-kinds.ts';
import { SALARY_PERMS, SALARY_STATUS_LABELS, SALARY_STATUS_VALUES } from '../../src/api/business/hr/salary-kinds.ts';

const here = dirname(fileURLToPath(import.meta.url));
const backend = resolve(here, '../../../backend/src/business/hr');
const migrations = resolve(here, '../../../backend/migrations/business');
const readBackend = (file: string): string => readFileSync(resolve(backend, file), 'utf8');
const readMigration = (file: string): string => readFileSync(resolve(migrations, file), 'utf8');
const readFrontend = (file: string): string =>
  readFileSync(resolve(here, '../../src', file), 'utf8');

const DTO = readBackend('salary_sheet_dto.rs');
const HANDLER = readBackend('handler.rs');
const SELF_SERVICE_DTO = readBackend('self_service_dto.rs');
const V0387 = readMigration('V0387__hr_menu_perms.up.sql');
const V0383 = readMigration('V0383__hr_tables.up.sql');
const API = readFrontend('api/business/hr/salary.ts');
const PAGE = readFrontend('views/business/hr/salary/index.vue');
const ROUTER = readFrontend('router/index.ts');

/** 造一行列表数据（只填用到的字段） */
function sheetRow(patch: Partial<SalarySheetRow> = {}): SalarySheetRow {
  return {
    id: patch.id ?? '00000000-0000-4000-8000-000000000001',
    sheet_no: 'SAL-202611-000001',
    period_code: '2026-11',
    period_start: '2026-11-01',
    period_end: '2026-11-30',
    status: 'CALCULATED',
    status_label: '已计算',
    employee_count: 3,
    gross_total: '30000.00',
    withhold_total: '5000.00',
    net_total: '25000.00',
    accrual_total: '34000.00',
    accrual_voucher_no: null,
    payment_voucher_no: null,
    posted_at: null,
    remark: null,
    ...patch,
  };
}

/** 造一期「我的薪资」 */
function myRow(patch: Partial<MySalaryRow> = {}): MySalaryRow {
  return {
    sheet_id: '00000000-0000-4000-8000-0000000000aa',
    sheet_no: 'SAL-202611-000001',
    period_code: '2026-11',
    sheet_status: 'POSTED',
    sheet_status_label: '已过账',
    paid: true,
    detail_id: '00000000-0000-4000-8000-0000000000bb',
    employee_no: 'E001',
    employee_name: '张三',
    dept_name: '研发部',
    position_code: 'DEV',
    job_level: 'P5',
    should_days: '22.00',
    actual_days: '22.00',
    leave_unpaid_days: '0.00',
    ot_hours: '4.00',
    earning_amount: '8320.00',
    pre_deduct_amount: '0.00',
    gross_amount: '8320.00',
    social_personal: '600.00',
    fund_personal: '400.00',
    iit_amount: '120.00',
    other_withhold: '0.00',
    withhold_amount: '1120.00',
    net_amount: '7200.00',
    remark: null,
    items: [
      { item_code: 'SI_PERSONAL', item_name: '社保（个人）', item_type: 'WITHHOLD', item_type_label: '代扣项', amount: '600.00' },
      { item_code: 'BASE', item_name: '基本工资', item_type: 'EARNING', item_type_label: '收入项', amount: '8000.00' },
      { item_code: 'OT_WD', item_name: '平时加班', item_type: 'EARNING', item_type_label: '收入项', amount: '320.00' },
    ],
    ...patch,
  };
}

// ==================== ① 词表镜像（后端是唯一真相） ====================

test('状态六档：码值与中文名逐条等于后端 STATUSES', () => {
  // 后端表：pub const STATUSES: [(&str, &str); 6] = [ (STATUS_DRAFT, "草稿"), … ];
  const pairs = [...DTO.matchAll(/\((STATUS_[A-Z]+),\s*"([^"]+)"\)/g)].map(
    (match) => [match[1], match[2]] as const,
  );
  assert.equal(pairs.length, 6, '没解析到后端 STATUSES 的 6 条（解析逻辑要跟着改）');
  const codes = SALARY_LIST_STATUS_OPTIONS.map((option) => option.value);
  const labels = SALARY_LIST_STATUS_OPTIONS.map((option) => option.label);
  assert.deepEqual(codes, SALARY_STATUS_VALUES, '状态码顺序 / 取值与后端不一致');
  assert.deepEqual(
    labels,
    pairs.map(([, label]) => label),
    '状态中文名与后端 STATUSES 不一致',
  );
  // 每一档都要有中文名（页面 tag 直接读它）
  for (const code of SALARY_STATUS_VALUES) {
    assert.ok(SALARY_STATUS_LABELS[code], `${code} 缺中文名`);
  }
});

test('分页与备注上限：20 / 200 / 500 与后端常量同值', () => {
  const def = /pub const DEFAULT_PAGE_SIZE: u64 = (\d+);/.exec(DTO);
  const max = /pub const MAX_PAGE_SIZE: u64 = (\d+);/.exec(DTO);
  const remark = /pub const MAX_REMARK_LEN: usize = (\d+);/.exec(DTO);
  assert.ok(def && max && remark, '没解析到后端分页 / 备注常量');
  assert.equal(SALARY_LIST_PAGE_SIZE, Number(def[1]), '默认每页条数与后端不一致');
  assert.equal(SALARY_LIST_MAX_PAGE_SIZE, Number(max[1]), '每页条数上限与后端不一致');
  assert.equal(MAX_CALC_REMARK, Number(remark[1]), '备注上限与后端不一致');
  assert.ok(SALARY_LIST_PAGE_SIZE <= SALARY_LIST_MAX_PAGE_SIZE);
});

// ==================== ② 权限码镜像 ====================

test('本页 5 个权限码都在 V0387 的 sys_menu.auth_code 里', () => {
  const wanted: string[] = Object.values(SALARY_LIST_PERMS);
  assert.equal(wanted.length, 5);
  for (const code of wanted) {
    assert.ok(V0387.includes(`'${code}'`), `${code} 在 V0387 里没有落菜单行（N10 会红）`);
  }
});

test('权限码从 salary-kinds 派生，不重复定义一套', () => {
  // 同一个码在仓库里只能有一个 JS 字面量来源：本页的 5 个全部来自 SALARY_PERMS
  assert.equal(SALARY_LIST_PERMS.list, SALARY_PERMS.list);
  assert.equal(SALARY_LIST_PERMS.detail, SALARY_PERMS.detail);
  assert.equal(SALARY_LIST_PERMS.calc, SALARY_PERMS.calc);
  assert.equal(SALARY_LIST_PERMS.export, SALARY_PERMS.export);
  assert.equal(SALARY_LIST_PERMS.my, SALARY_PERMS.my);
  const kinds = readFrontend('api/business/hr/salary-list-kinds.ts');
  assert.ok(
    !/['"]business:hr:salary:list['"]/.test(kinds),
    'salary-list-kinds.ts 里出现了硬编码的权限码字符串：必须从 SALARY_PERMS 派生',
  );
});

test('四条路由的 METHOD + 路径 + 权限码与后端 ROUTE_CONTRACT 一致', () => {
  const wanted: Array<[string, string, string]> = [
    ['GET', '/business/hr/salary/page', 'business:hr:salary:list'],
    ['POST', '/business/hr/salary/calc', 'business:hr:salary:calc'],
    ['POST', '/business/hr/salary', 'business:hr:salary:calc'],
    ['GET', '/business/hr/salary/my', 'business:hr:salary:my'],
  ];
  for (const [method, path, permission] of wanted) {
    const at = HANDLER.indexOf(`path: "${path}",`);
    assert.ok(at >= 0, `后端 ROUTE_CONTRACT 里没有 ${path}`);
    // RouteSpec 的字面量顺序是 method → path → permission：各取一段，避免串到相邻块
    const before = HANDLER.slice(Math.max(0, at - 200), at);
    const after = HANDLER.slice(at, at + 200);
    assert.ok(before.includes(`method: "${method}"`), `${path} 的后端方法是 ${method}`);
    assert.ok(
      after.includes(`permission: Some("${permission}")`),
      `${path} 的权限码应为 ${permission}`,
    );
  }
});

// ==================== ③ 期间码（与库层正则同一把尺子） ====================

test('期间正则与 ck_biz_sal_sheet_code 同域', () => {
  const check = /CONSTRAINT ck_biz_sal_sheet_code\s+CHECK \(period_code ~ '([^']+)'\)/.exec(V0383);
  assert.ok(check, '没解析到 ck_biz_sal_sheet_code 的正则');
  // 库层：^[0-9]{4}-(0[1-9]|1[0-2])$ —— 前端必须同样严（多一位/少一位都要拦下）
  const dbPattern = new RegExp(check[1]);
  for (const value of ['2026-11', '2026-01', '2026-12', '1999-07', '2999-01']) {
    assert.equal(periodCodeError(value), null, `${value} 应该合法`);
    assert.ok(dbPattern.test(value), `${value} 库层正则也应该过`);
  }
  for (const value of ['2026-13', '2026-00', '2026-1', '26-11', '202611', '2026-11-01', 'abc']) {
    assert.ok(periodCodeError(value), `${value} 应该被前端拦下`);
    assert.ok(!dbPattern.test(value), `${value} 库层正则也应该拒`);
  }
  assert.ok(periodCodeError(''), '空期间必须报「必填」');
  assert.ok(periodCodeError(null), 'null 期间必须报「必填」');
  assert.ok(periodCodeError('1899-12'), '年份过小要拦（手滑打成 1899）');
  assert.ok(periodCodeError('3000-01'), '年份过大要拦');
});

test('默认期间取北京时间当月（不是 UTC）', () => {
  // 2026-10-31T20:00:00Z = 北京时间 11-01 04:00 → 必须是 2026-11
  assert.equal(currentPeriodCode(new Date('2026-10-31T20:00:00Z')), '2026-11');
  assert.equal(currentPeriodCode(new Date('2026-11-30T16:00:00Z')), '2026-12');
  assert.equal(emptyCalcForm(new Date('2026-03-15T00:00:00Z')).period_code, '2026-03');
});

// ==================== ④ 报文形状 ====================

test('列表查询串：空筛选不发空参数、页码按后端 clamp', () => {
  assert.deepEqual(buildListQuery(emptyListFilter(), 1, 20), { page: 1, page_size: 20 });
  // `status=` 会让后端 parse_list_query 当「传了但不在白名单」→ 400，所以空串不许发
  assert.ok(!('status' in buildListQuery({ ...emptyListFilter(), status: '' }, 1, 20)));
  assert.deepEqual(
    buildListQuery({ period_code: ' 2026-11 ', status: 'POSTED', keyword: ' SAL-2026 ' }, 2, 50),
    { page: 2, page_size: 50, period_code: '2026-11', status: 'POSTED', keyword: 'SAL-2026' },
  );
  // 非法期间不发出（后端会 400；用户已经在页面上看到红字）
  assert.ok(!('period_code' in buildListQuery({ ...emptyListFilter(), period_code: '2026-13' }, 1, 20)));
  const clamped = buildListQuery(emptyListFilter(), 0, 9999);
  assert.equal(clamped.page, 1);
  assert.equal(clamped.page_size, SALARY_LIST_MAX_PAGE_SIZE);
});

test('试算 / 建单报文：dry_run 由模式决定，不许手写', () => {
  const form = { period_code: ' 2026-11 ', remark: '  11 月工资  ' };
  assert.deepEqual(buildCalcPayload(form, 'preview'), {
    period_code: '2026-11',
    dry_run: true,
    remark: '11 月工资',
  });
  assert.deepEqual(buildCalcPayload({ period_code: '2026-11', remark: '   ' }, 'persist'), {
    period_code: '2026-11',
    dry_run: false,
  });
  // 后端确实拿 dry_run 与接口身份对拍（传错 → 400）：解析逻辑变了这里会红
  assert.ok(
    DTO.includes('dry_run={flag}') || DTO.includes('与「{}」接口不符'),
    '后端的 dry_run 一致性校验不见了：前端可以放宽，但要让本用例知道',
  );
});

test('表单校验：先期间后备注，一次只说一处', () => {
  assert.equal(calcFormError({ period_code: '2026-11', remark: '' }), null);
  const badPeriod = calcFormError({ period_code: '2026-13', remark: 'x'.repeat(MAX_CALC_REMARK + 1) });
  assert.ok(badPeriod?.includes('2026-13'), '期间错了要先说期间');
  const longRemark = calcFormError({ period_code: '2026-11', remark: '补'.repeat(MAX_CALC_REMARK + 1) });
  assert.ok(longRemark?.includes(String(MAX_CALC_REMARK)), '备注超长要报出上限');
  // 按字符数（不是 String.length）判：emoji 在 Rust 里是 1 个字
  assert.ok(calcFormError({ period_code: '2026-11', remark: '💰'.repeat(MAX_CALC_REMARK + 1) }));
  assert.equal(calcFormError({ period_code: '2026-11', remark: '💰'.repeat(MAX_CALC_REMARK) }), null);
});

test('两种模式的文案都写清「落不落库」', () => {
  assert.ok(CALC_MODE_LABELS.preview.includes('不落库'));
  assert.ok(CALC_MODE_LABELS.persist.includes('建单'));
  assert.ok(CALC_MODE_HINTS.preview.includes('零行') || CALC_MODE_HINTS.preview.includes('不占'));
  assert.ok(CALC_MODE_HINTS.persist.includes('单号'));
});

// ==================== ⑤ 展示口径 ====================

test('空态文案分「没有数据」与「筛没了」', () => {
  const empty = listEmptyText(false);
  const filtered = listEmptyText(true);
  assert.notEqual(empty, filtered);
  assert.ok(empty.includes('试算') && empty.includes('建单'), '空库要告诉用户下一步点哪里');
  assert.ok(filtered.includes('期间') || filtered.includes('清空'), '筛没了要说换条件');
  assert.ok(mySalaryEmptyText(false).includes('HR'), '员工空态要说「联系 HR 绑定档案」');
  assert.ok(isListFiltered(emptyListFilter()) === false);
  assert.ok(isListFiltered({ period_code: '', status: 'DRAFT', keyword: '' }) === true);
});

test('本页合计按整数分累加（浮点会在 0.1+0.2 处假报警）', () => {
  const rows = [
    sheetRow({ gross_total: '0.10', withhold_total: '0.20', net_total: '-0.10', accrual_total: '0.30', employee_count: 1 }),
    sheetRow({ gross_total: '0.20', withhold_total: '0.10', net_total: '0.10', accrual_total: '0.30', employee_count: 2 }),
  ];
  const totals = sumSheetRows(rows);
  assert.equal(totals.gross_total, 0.3);
  assert.equal(totals.withhold_total, 0.3);
  assert.equal(totals.net_total, 0);
  assert.equal(totals.accrual_total, 0.6);
  assert.equal(totals.employee_count, 3);
  assert.deepEqual(sumSheetRows([]), emptySheetPageTotals());
  assert.ok(pageTotalsText(totals).includes('实发'));
  // 后端金额是 Decimal 字符串：不许 `+` 拼串
  assert.ok(pageTotalsText({ ...totals, gross_total: 30000 }).includes('30,000.00'));
});

test('凭证列只认真拿到号的两张，未过账给空数组', () => {
  assert.deepEqual(sheetVoucherLines(sheetRow()), []);
  const posted = sheetRow({
    status: 'POSTED',
    accrual_voucher_no: 'PZ-20261130-000012',
    payment_voucher_no: 'PZ-20261205-000003',
  });
  assert.deepEqual(sheetVoucherLines(posted), [
    '计提 PZ-20261130-000012',
    '发放 PZ-20261205-000003',
  ]);
  // 只有计提一张（极端：过账半途）也只显示那一条，不猜第二张
  assert.deepEqual(sheetVoucherLines(sheetRow({ accrual_voucher_no: 'PZ-1' })), ['计提 PZ-1']);
});

test('期间 / 考勤口径 / 天数文案', () => {
  assert.equal(sheetPeriodText(sheetRow()), '2026-11（11-01 ~ 11-30）');
  assert.equal(sheetPeriodText(null), '—');
  assert.equal(daysOf('22.00'), '22');
  assert.equal(daysOf('0.50'), '0.5');
  assert.equal(daysOf(null), '—');
  assert.ok(slipAttendanceText(myRow()).includes('应出勤 22'));
  assert.ok(!slipAttendanceText(myRow()).includes('NaN'));
  assert.equal(slipAttendanceText(null), '—');
});

test('试算回执一句话：期间 · 人数 · 薪资项数', () => {
  const result: SalaryCalcResult = {
    period_code: '2026-11',
    period_start: '2026-11-01',
    period_end: '2026-11-30',
    attendance_period_id: null,
    dry_run: true,
    sheet: null,
    totals: {
      employee_count: 12,
      earning_total: '0',
      pre_deduct_total: '0',
      gross_total: '120000.00',
      withhold_total: '20000.00',
      net_total: '100000.00',
      si_company_total: '0',
      hf_company_total: '0',
      accrual_total: '0',
    },
    item_count: 15,
    missing_attendance: [],
    employees: [],
  };
  const text = calcResultText(result);
  assert.ok(text.includes('2026-11'));
  assert.ok(text.includes('12 人'));
  assert.ok(text.includes('15 个薪资项'));
  assert.equal(calcResultText(null), '');
});

test('缺考勤名单：前 5 个 + 还有 N 人', () => {
  assert.equal(missingAttendanceText([]), '');
  assert.equal(missingAttendanceText(null), '');
  const two = missingAttendanceText(['E001 张三', 'E002 李四']);
  assert.ok(two.includes('E001 张三') && two.includes('2 人'));
  assert.ok(!two.includes('还有'));
  const many = missingAttendanceText(Array.from({ length: 9 }, (_, index) => `E00${index} 员工${index}`));
  assert.ok(many.includes('9 人'));
  assert.ok(many.includes('还有 4 人'));
});

test('我的薪资：发放状态与明细项分组', () => {
  assert.equal(myPaidText(myRow()), '已发放');
  assert.equal(myPaidText(myRow({ paid: false, sheet_status: 'APPROVED', sheet_status_label: '已批准' })), '已批准待发放');
  assert.equal(myPaidText(myRow({ paid: false, sheet_status: 'REVERSED', sheet_status_label: '已红冲' })), '已红冲');
  assert.equal(myPaidText(null), '—');

  const sorted = myItemsSorted(myRow());
  // 后端按 line_no 给（这里故意把 WITHHOLD 放最前），页面必须按类型分组
  assert.deepEqual(
    sorted.map((item) => item.item_type),
    ['EARNING', 'EARNING', 'WITHHOLD'],
  );
  assert.deepEqual([...MY_ITEM_TYPE_ORDER], ['EARNING', 'PRE_DEDUCT', 'WITHHOLD']);
  assert.deepEqual(myItemsSorted(myRow({ items: [] })), []);
});

test('我的薪资查询串：include_pending 只在为真时才发', () => {
  assert.deepEqual(buildMyQuery(emptyMyFilter(), 1, 20), { page: 1, page_size: 20 });
  assert.deepEqual(buildMyQuery({ period_code: '2026-11', include_pending: true }, 1, 20), {
    page: 1,
    page_size: 20,
    period_code: '2026-11',
    include_pending: true,
  });
  // 后端默认只出已定稿三档（VISIBLE_STATUSES），在途要靠这个开关追加
  assert.ok(SELF_SERVICE_DTO.includes('include_pending'), '后端入参没有 include_pending 了');
  assert.ok(
    SELF_SERVICE_DTO.includes('PUB const VISIBLE_STATUSES') || SELF_SERVICE_DTO.includes('pub const VISIBLE_STATUSES'),
    '后端的 VISIBLE_STATUSES 定义不见了',
  );
});

test('详情深链与列字典', () => {
  assert.equal(salaryDetailPath('abc'), '/business/hr/salary/abc');
  const listKeys = SALARY_LIST_COLUMNS.map((column) => column.colKey);
  for (const key of ['sheet_no', 'period', 'status', 'employee_count', 'net_total', 'voucher', 'op']) {
    assert.ok(listKeys.includes(key), `薪资单列表少了 ${key} 列`);
  }
  const myKeys = SALARY_MY_COLUMNS.map((column) => column.colKey);
  for (const key of ['period_code', 'sheet_status', 'paid', 'net_amount']) {
    assert.ok(myKeys.includes(key), `我的薪资少了 ${key} 列`);
  }
  const slipKeys = SALARY_SLIP_COLUMNS.map((column) => column.colKey);
  for (const key of ['employee', 'attendance', 'gross_amount', 'net_amount']) {
    assert.ok(slipKeys.includes(key), `试算逐人表少了 ${key} 列`);
  }
});

// ==================== ⑥ 接线（api / 页面 / 路由） ====================

test('API 层：四条路径写全 /api，试算与建单各走一条', () => {
  assert.ok(API.includes("const BASE = '/api/business/hr';"), 'BASE 必须带 /api 前缀');
  for (const path of [
    '`${BASE}/salary/page`',
    '`${BASE}/salary/calc`',
    '`${BASE}/salary`',
    '`${BASE}/salary/my`',
  ]) {
    assert.ok(API.includes(path), `api 层漏了 ${path}`);
  }
  assert.ok(API.includes('export function fetchSalarySheetPage'), '缺列表接口');
  assert.ok(API.includes('export function previewSalarySheet'), '缺试算接口');
  assert.ok(API.includes('export function createSalarySheet'), '缺建单接口');
  assert.ok(API.includes('export function fetchMySalary'), '缺我的薪资接口');
  assert.ok(API.includes("export * from './salary-list-kinds'"), '页面要能从一个入口拿到纯逻辑');
});

test('页面：两个 tab 按权限显隐，四条接口都真接了', () => {
  // 数据来源
  assert.ok(PAGE.includes('fetchSalarySheetPage'), '薪资单 tab 必须走分页接口');
  assert.ok(PAGE.includes('fetchMySalary'), '我的薪资 tab 必须走自助接口');
  assert.ok(PAGE.includes('previewSalarySheet'), '试算按钮必须真调 #39');
  assert.ok(PAGE.includes('createSalarySheet'), '建单按钮必须真调 #40');
  assert.ok(PAGE.includes('buildCalcPayload'), 'dry_run 必须由 buildCalcPayload 生成');
  // 权限门：没有码就不画入口
  assert.ok(PAGE.includes('SALARY_LIST_PERMS.list'), '缺 salary:list 判据');
  assert.ok(PAGE.includes('SALARY_LIST_PERMS.calc'), '缺 salary:calc 判据');
  assert.ok(PAGE.includes('SALARY_LIST_PERMS.my'), '缺 salary:my 判据');
  assert.ok(PAGE.includes('v-if="canList"'), '「薪资单」tab 必须按 salary:list 显隐');
  assert.ok(PAGE.includes('v-if="canMy"'), '「我的薪资」tab 必须按 salary:my 显隐');
  assert.ok(PAGE.includes('v-if="canCalc"'), '试算 / 建单按钮必须按 salary:calc 显隐');
  // 列表 → 详情
  assert.ok(PAGE.includes('/business/hr/salary/'), '点单号必须能进详情页');
  assert.ok(PAGE.includes('@row-click'), '整行可点（列表页最常见的动作）');
  // 期间校验先手拦（不让用户等一次 400）
  assert.ok(PAGE.includes('calcFormError'), '试算前必须先过表单校验');
  // 凭证卡深链：`?sheet_no=` 必须被读成单号筛选（source-route 的 PAYROLL_* 已是 filter）
  assert.ok(PAGE.includes('route.query.sheet_no'), '凭证卡「查看薪资单」的 ?sheet_no= 没有本页读取就是假链接');
  const SOURCE_ROUTE = readFrontend('views/business/erp/shared/source-route.ts');
  for (const key of ['PAYROLL_ACCRUAL', 'PAYROLL_PAYMENT']) {
    const row = SOURCE_ROUTE.slice(SOURCE_ROUTE.indexOf(`  ${key}: {`));
    assert.ok(row.length > 0, `source-route 里没有 ${key}`);
    assert.ok(
      row.slice(0, 400).includes("deepLink: 'filter'"),
      `${key} 应改成 filter（薪资列表页已落地，不再 page-missing）`,
    );
  }
});

test('路由：注册了 /business/hr/salary，component 与 sys_menu.component 同指一个视图', () => {
  assert.ok(
    ROUTER.includes("path: 'business/hr/salary',"),
    'n08 判据：菜单 /business/hr/salary 必须在前端路由里（否则点进去 404）',
  );
  assert.ok(ROUTER.includes("name: 'BusinessHrSalary'"), '路由 name 与详情页（…SalaryDetail）要能区分');
  // sys_menu.component = business/hr/salary/index → views/business/hr/salary/index.vue
  const seeded = /'business\/hr\/salary\/index'/.exec(V0387);
  assert.ok(seeded, 'V0387 里没有 business/hr/salary/index 这个 component');
  assert.ok(
    ROUTER.includes("import('@/views/business/hr/salary/index.vue')"),
    'component 路径必须与 sys_menu.component 一致',
  );
  // 详情页路由仍在，且两条不是同一条（静态段优先于参数段）
  assert.ok(ROUTER.includes("path: 'business/hr/salary/:id'"), '详情页路由被覆盖了');
  assert.ok(
    ROUTER.indexOf("path: 'business/hr/salary',") < ROUTER.indexOf("path: 'business/hr/salary/:id'"),
    '列表路由应排在参数路由之前（读代码的人先看到入口）',
  );
  assert.ok(
    ROUTER.includes("permission: 'business:hr:salary:list'"),
    '列表页路由的权限码必须是 salary:list（V0387 的页面行）',
  );
});
