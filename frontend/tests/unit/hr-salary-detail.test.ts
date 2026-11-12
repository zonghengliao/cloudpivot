/**
 * 薪资单详情页（HR-43）单测：全部跑在无 DOM 的 `tsx --test` 里。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 测什么
 *
 * 1. **词表镜像**：`salary-kinds.ts` 的六档状态（码值 + 中文名）与后端
 *    `salary_sheet_dto.rs::STATUSES` 逐条对拍（直接读 Rust 源码）—— 后端加状态而前端没跟，
 *    这里先红，而不是线上看到 `CANCELLED` 这种码值。
 * 2. **权限码镜像**：7 个码与 `salary_sheet_handler.rs` 的 `PERM_SALARY_*` 常量逐字相等；
 *    并且 `submit / post / reverse / export` 四个动作的码必须能在后端的 `RouteSpec` 里
 *    找到对应的 **METHOD + 路径**（「按钮点下去 403 / 404」是这类页面最常见的坑）。
 * 3. **动作矩阵**：设计 §6.2.5 的按钮态表里「试算 / 删除 / 作废」三条在 §5.2 的接口表中
 *    **没有路由**（`POST /salary/calc` 是按期间试算，不是按单重算），所以矩阵里不能出现它们；
 *    同时 `isSalaryDetailEditable` 必须与 `state_machine::SALARY_SHEET_LOCKED` 同域。
 * 4. **上限对齐**：红冲原因 5/255、改金额原因 5/500、明细页码 20/200，逐条与后端常量对拍。
 * 5. **口径**：部门汇总按「整数分」累加（浮点会假报警）、合计行与单头对拍要过 8 个金额列、
 *    按字符数（不是 `String.length`）校验中文/emoji 原因长度。
 * 6. **凭证卡**：未过账 → 两张卡都没有号、没有深链；已过账 → 计提 / 发放各有深链；
 *    已红冲 → 红冲卡点亮但仍然不给猜出来的链接。
 * 7. **接线**：`detail.vue` 真的用了 `fetchSalaryDetailPage` / `:expand-icon` / `#expandedRow`
 *    （明细可展开是本卡验收项）、真的渲染 `DocActionBar` / `FlowProgress` / 凭证卡；
 *    `router/index.ts` 注册了 `/business/hr/salary/:id`（漏了就是 404）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import {
  DEPT_SUMMARY_COLUMNS,
  DETAIL_COLUMNS,
  ITEM_COLUMNS,
  MAX_DETAIL_REMARK,
  MAX_OVERRIDE_REASON,
  MAX_REVERSE_REASON,
  MIN_OVERRIDE_REASON,
  MIN_REVERSE_REASON,
  SALARY_ACTIONS,
  SALARY_ACTIONS_BY_STATUS,
  SALARY_DETAIL_MAX_PAGE_SIZE,
  SALARY_DETAIL_PAGE_SIZE,
  SALARY_PERMS,
  SALARY_STATUS_HINTS,
  SALARY_STATUS_LABELS,
  SALARY_STATUS_VALUES,
  buildDetailQuery,
  daysText,
  deptTotalsMatchHeader,
  datetimeText,
  emptyDetailFilter,
  emptyDeptTotals,
  generatedVoucherCount,
  isDetailFiltered,
  isSalaryDetailEditable,
  itemCountText,
  itemExplainText,
  itemTypeTheme,
  moneyText,
  periodText,
  reasonError,
  salaryActionsOf,
  salaryStatusHint,
  salaryStatusLabel,
  salaryStatusTheme,
  sumDeptTotals,
  voucherCardsOf,
  type SalaryDeptSummary,
  type SalaryDetailRow,
  type SalarySheetHeader,
  type SalaryTotals,
} from '../../src/api/business/hr/salary-kinds';

// ==================== 源码（后端契约的唯一真相） ====================

const RUST_SHEET_DTO = readFileSync(
  new URL('../../../backend/src/business/hr/salary_sheet_dto.rs', import.meta.url),
  'utf8',
);
const RUST_SHEET_HANDLER = readFileSync(
  new URL('../../../backend/src/business/hr/salary_sheet_handler.rs', import.meta.url),
  'utf8',
);
const RUST_EXPORT = readFileSync(
  new URL('../../../backend/src/business/hr/salary_export.rs', import.meta.url),
  'utf8',
);
const RUST_OVERRIDE_DTO = readFileSync(
  new URL('../../../backend/src/business/hr/salary_override_dto.rs', import.meta.url),
  'utf8',
);
const RUST_STATE_MACHINE = readFileSync(
  new URL('../../../backend/src/business/shared/state_machine.rs', import.meta.url),
  'utf8',
);
const RUST_HR_HANDLER = readFileSync(
  new URL('../../../backend/src/business/hr/handler.rs', import.meta.url),
  'utf8',
);
const PAGE = readFileSync(
  new URL('../../src/views/business/hr/salary/detail.vue', import.meta.url),
  'utf8',
);
const API = readFileSync(new URL('../../src/api/business/hr/salary.ts', import.meta.url), 'utf8');
const ROUTER = readFileSync(new URL('../../src/router/index.ts', import.meta.url), 'utf8');

/** 从后端源码里抠一个 `pub const NAME: usize|u64 = N;` 的数值 */
function rustUsize(source: string, name: string): number {
  const match = new RegExp(`pub const ${name}: (?:usize|u64) = (\\d+);`).exec(source);
  assert.ok(match, `后端源码里找不到 pub const ${name}: usize|u64`);
  return Number(match[1]);
}

// ==================== 1. 状态词表对拍 ====================

test('六档状态：码值与中文名逐条等于后端 STATUSES', () => {
  const block = /pub const STATUSES: \[\(&str, &str\); \d+\] = \[([\s\S]*?)\];/.exec(RUST_SHEET_DTO);
  assert.ok(block, '解析 salary_sheet_dto.rs 的 STATUSES 失败');
  const pairs = [...block[1].matchAll(/\((STATUS_[A-Z_]+), "([^"]+)"\)/g)].map((m) => ({
    code: m[1].replace('STATUS_', ''),
    label: m[2],
  }));
  assert.equal(pairs.length, SALARY_STATUS_VALUES.length, '状态数量与后端不一致');
  for (const [index, pair] of pairs.entries()) {
    assert.equal(SALARY_STATUS_VALUES[index], pair.code);
    assert.equal(SALARY_STATUS_LABELS[pair.code as keyof typeof SALARY_STATUS_LABELS], pair.label);
  }
});

test('状态渲染：未知码原样返回、已知码给中文与配色', () => {
  assert.equal(salaryStatusLabel('POSTED'), '已过账');
  assert.equal(salaryStatusLabel('WHAT'), 'WHAT');
  assert.equal(salaryStatusLabel(null), '—');
  assert.equal(salaryStatusTheme('REVERSED'), 'danger');
  assert.equal(salaryStatusTheme('WHAT'), 'default');
  for (const code of SALARY_STATUS_VALUES) {
    assert.ok(SALARY_STATUS_HINTS[code].length > 10, `${code} 的提示太短，用户看不懂下一步`);
  }
  assert.equal(salaryStatusHint('WHAT'), '');
});

// ==================== 2. 权限码 + 路由对拍 ====================

test('7 个权限码逐字等于后端常量', () => {
  const expected: Array<[string, string]> = [
    ['list', 'PERM_SALARY_LIST'],
    ['detail', 'PERM_SALARY_DETAIL'],
    ['calc', 'PERM_SALARY_CALC'],
    ['submit', 'PERM_SALARY_SUBMIT'],
    ['post', 'PERM_SALARY_POST'],
    ['reverse', 'PERM_SALARY_REVERSE'],
  ];
  for (const [key, constant] of expected) {
    const match = new RegExp(`pub const ${constant}: &str = "([^"]+)";`).exec(RUST_SHEET_HANDLER);
    assert.ok(match, `后端找不到 ${constant}`);
    assert.equal(SALARY_PERMS[key as keyof typeof SALARY_PERMS], match[1], `${constant} 漂移`);
  }
  // 导出码在 salary_export.rs 的 #[nik::permission] 注解里（没有 pub const）
  assert.ok(
    RUST_EXPORT.includes(`permission = "${SALARY_PERMS.export}"`),
    '导出权限码与 salary_export.rs 的注解不一致',
  );
});

test('四个动作的权限码都能在后端 RouteSpec 里找到 METHOD + 路径', () => {
  // RouteSpec 是「方法 / 路径 / 权限码」三元组，逐条拆出来对拍
  const specs = [...RUST_HR_HANDLER.matchAll(
    /method: "(GET|POST|PUT|DELETE)",\s*\n\s*path: "([^"]+)",\s*\n\s*permission: (?:Some\("([^"]+)"\)|None),/g,
  )].map((m) => ({ method: m[1], path: m[2], permission: m[3] ?? null }));
  assert.ok(specs.length > 10, `ROUTE_CONTRACT 解析结果太少（${specs.length} 条），解析逻辑要更新`);

  const wanted: Array<[keyof typeof SALARY_ACTIONS, string, string]> = [
    ['submit', 'POST', '/business/hr/salary/{id}/submit'],
    ['post', 'POST', '/business/hr/salary/{id}/post'],
    ['reverse', 'POST', '/business/hr/salary/{id}/reverse'],
    ['export', 'GET', '/business/hr/salary/{id}/export'],
  ];
  for (const [key, method, path] of wanted) {
    const spec = specs.find((row) => row.method === method && row.path === path);
    assert.ok(spec, `${key} 对应的路由 ${method} ${path} 不在 ROUTE_CONTRACT 里`);
    assert.equal(spec.permission, SALARY_ACTIONS[key].permission, `${key} 的权限码与路由不一致`);
  }
});

// ==================== 3. 动作矩阵 ====================

test('动作矩阵只列真接口：没有试算 / 删除 / 作废，也没有假 CANCELLED', () => {
  const allKeys = Object.values(SALARY_ACTIONS_BY_STATUS).flat();
  const unique = [...new Set(allKeys)];
  assert.deepEqual(unique.sort(), ['export', 'post', 'reverse', 'submit']);

  // 设计 §6.2.5 的三条在接口表里没有路由，矩阵里不能出现
  for (const forbidden of ['calc', 'delete', 'cancel', 'void']) {
    assert.ok(
      !unique.includes(forbidden as never),
      `${forbidden} 没有后端路由，画出来就是点了必 404 的假按钮`,
    );
  }
  // 状态机没有 CANCELLED
  assert.ok(RUST_SHEET_DTO.includes('ck_biz_sal_sheet_status') || RUST_STATE_MACHINE.includes('SALARY_SHEET_EDGES'));
  assert.ok(!SALARY_STATUS_VALUES.includes('CANCELLED' as never));
});

test('每个状态的可用动作与状态机主链一致', () => {
  assert.deepEqual(SALARY_ACTIONS_BY_STATUS.DRAFT, []);
  assert.deepEqual(SALARY_ACTIONS_BY_STATUS.CALCULATED, ['submit']);
  assert.deepEqual(SALARY_ACTIONS_BY_STATUS.SUBMITTED, []);
  assert.deepEqual(SALARY_ACTIONS_BY_STATUS.APPROVED, ['post']);
  assert.deepEqual(SALARY_ACTIONS_BY_STATUS.POSTED, ['reverse']);
  assert.deepEqual(SALARY_ACTIONS_BY_STATUS.REVERSED, ['export']);

  // 导出对所有状态都开放（后端 export_bundle 不看状态）
  for (const code of SALARY_STATUS_VALUES) {
    const keys = salaryActionsOf(code).map((action) => action.key);
    assert.ok(keys.includes('export'), `${code} 少了导出`);
  }
  // 未知状态：只给导出，不给任何状态流转
  assert.deepEqual(
    salaryActionsOf('WHAT').map((action) => action.key),
    ['export'],
  );
  assert.deepEqual(salaryActionsOf(null).map((action) => action.key), ['export']);
});

test('明细可编辑状态与后端 SALARY_SHEET_LOCKED 同域', () => {
  const match = /pub const SALARY_SHEET_LOCKED: \[&str; \d+\] = \[([^\]]+)\];/.exec(RUST_STATE_MACHINE);
  assert.ok(match, '解析 SALARY_SHEET_LOCKED 失败');
  const locked = [...match[1].matchAll(/"([A-Z]+)"/g)].map((m) => m[1]);
  assert.deepEqual(locked.sort(), ['APPROVED', 'POSTED', 'REVERSED', 'SUBMITTED']);
  for (const code of SALARY_STATUS_VALUES) {
    assert.equal(
      isSalaryDetailEditable(code),
      !locked.includes(code),
      `${code} 的可编辑判定与后端锁表不一致`,
    );
  }
  assert.equal(isSalaryDetailEditable('WHAT'), false);
});

// ==================== 4. 上限对齐 ====================

test('原因长度上限逐条等于后端常量', () => {
  assert.equal(MIN_REVERSE_REASON, rustUsize(RUST_SHEET_DTO, 'MIN_REVERSE_REASON_CHARS'));
  assert.equal(MAX_REVERSE_REASON, rustUsize(RUST_SHEET_DTO, 'MAX_REVERSE_REASON_CHARS'));
  assert.equal(MIN_OVERRIDE_REASON, rustUsize(RUST_OVERRIDE_DTO, 'MIN_REASON_CHARS'));
  assert.equal(MAX_OVERRIDE_REASON, rustUsize(RUST_OVERRIDE_DTO, 'MAX_REASON_CHARS'));
  assert.equal(MAX_DETAIL_REMARK, rustUsize(RUST_SHEET_DTO, 'MAX_REMARK_LEN'));
});

test('分页上限逐条等于后端常量', () => {
  assert.equal(SALARY_DETAIL_PAGE_SIZE, rustUsize(RUST_SHEET_DTO, 'DEFAULT_PAGE_SIZE'));
  assert.equal(SALARY_DETAIL_MAX_PAGE_SIZE, rustUsize(RUST_SHEET_DTO, 'MAX_PAGE_SIZE'));
});

// ==================== 5. 口径 ====================

test('原因校验：按字符数（不是 String.length）判长度', () => {
  assert.equal(reasonError(''), '原因必填（至少 5 个字，会落到单据与状态日志上）');
  assert.ok(reasonError('太短了')?.includes('至少 5 个字'));
  assert.equal(reasonError('重算后重发一次'), null);
  // 4 个 emoji：String.length = 8（>5 会被放过），字符数 = 4（后端会拒）—— 必须按字符算
  const emoji = '😀😀😀😀';
  assert.equal(emoji.length, 8);
  assert.ok(reasonError(emoji)?.includes('至少 5 个字'), 'emoji 必须按字符数判（后端 chars().count()）');
  assert.equal(reasonError('😀😀😀😀😀'), null);
  // 超上限
  assert.ok(reasonError('字'.repeat(MAX_REVERSE_REASON + 1))?.includes('最多'));
  // 自定义 label 出现在报文里
  assert.ok(reasonError('', { label: '红冲原因' })?.startsWith('红冲原因必填'));
});

test('部门汇总按整数分累加，合计与单头对拍过 8 个金额列 + 人数', () => {
  const rows: SalaryDeptSummary[] = [
    {
      dept_id: 'd1',
      dept_name: '生产部',
      employee_count: 2,
      earning_total: '9889.66',
      pre_deduct_total: '0.00',
      gross_total: '9154.03',
      withhold_total: '1800.00',
      net_total: '7354.03',
      si_company_total: '1680.00',
      hf_company_total: '960.00',
      accrual_total: '11794.03',
    },
    {
      dept_id: null,
      dept_name: '(未分配部门)',
      employee_count: 1,
      earning_total: '0.10',
      pre_deduct_total: '0.20',
      gross_total: '0.30',
      withhold_total: '0.00',
      net_total: '0.30',
      si_company_total: '0.00',
      hf_company_total: '0.00',
      accrual_total: '0.30',
    },
  ];
  const sum = sumDeptTotals(rows);
  assert.equal(sum.employee_count, 3);
  // 0.1 + 0.2 在浮点里是 0.30000000000000004，按分累加必须精确
  assert.equal(sum.pre_deduct_total, 0.2);
  assert.equal(sum.gross_total, 9154.33);
  assert.equal(sum.earning_total, 9889.76);

  const header: SalaryTotals = {
    employee_count: 3,
    earning_total: '9889.76',
    pre_deduct_total: '0.20',
    gross_total: '9154.33',
    withhold_total: '1800.00',
    net_total: '7354.33',
    si_company_total: '1680.00',
    hf_company_total: '960.00',
    accrual_total: '11794.33',
  };
  assert.equal(deptTotalsMatchHeader(sum, header), true);
  // 差 1 分就必须报不平
  assert.equal(deptTotalsMatchHeader(sum, { ...header, gross_total: '9154.32' }), false);
  // 人数差 1 也要报
  assert.equal(deptTotalsMatchHeader(sum, { ...header, employee_count: 2 }), false);
  assert.equal(deptTotalsMatchHeader(sum, null), false);
  // 空表：等于空的合计
  assert.deepEqual(sumDeptTotals([]), emptyDeptTotals());
  assert.deepEqual(sumDeptTotals(null), emptyDeptTotals());
});

test('金额 / 天数 / 期间 / 时间戳的显示口径', () => {
  assert.equal(moneyText('9154.03'), '9,154.03');
  assert.equal(moneyText('-1200'), '-1,200.00');
  assert.equal(moneyText(null), '0.00');
  assert.equal(daysText('22.00'), '22');
  assert.equal(daysText('0.50'), '0.5');
  assert.equal(periodText('2026-11', '2026-11-01', '2026-11-30'), '2026-11（11-01 ~ 11-30）');
  assert.equal(periodText('2026-11'), '2026-11');
  assert.equal(periodText(''), '—');
  // 后端出的是 UTC，界面按北京时间显示
  assert.equal(datetimeText('2026-11-13T01:22:33Z'), '2026-11-13 09:22');
  assert.equal(datetimeText(null), '—');
  assert.equal(datetimeText('不是时间'), '不是时间');
});

test('明细行展示：说明优先 calc_note、条数看 item_count、类型配色分四档', () => {
  const row = {
    id: 'det-1',
    sheet_id: 'sheet-1',
    employee_id: 'emp-1',
    employee_no: 'E001',
    employee_name: '张三',
    dept_id: 'd1',
    dept_name: '生产部',
    user_id: null,
    position_code: null,
    job_level: null,
    should_days: '22.00',
    actual_days: '21.00',
    leave_unpaid_days: '0.00',
    ot_hours: '2.00',
    earning_amount: '9889.66',
    pre_deduct_amount: '0.00',
    gross_amount: '9154.03',
    withhold_amount: '1800.00',
    net_amount: '7354.03',
    social_personal: '1200.00',
    fund_personal: '600.00',
    iit_amount: '0.00',
    other_withhold: '0.00',
    si_company: '1680.00',
    hf_company: '960.00',
    accrual_amount: '11794.03',
    remark: null,
    items: [],
    item_count: 15,
  } satisfies SalaryDetailRow;
  // with_items=false 时 items 是空数组，但条数仍是 15 —— 不要数数组长度
  assert.equal(itemCountText(row), '15 项');
  assert.equal(itemCountText(null), '0 项');

  assert.equal(itemExplainText({ item_code: 'OT_WD', calc_note: '8000/21.75*2', expr_snapshot: '{BASE}/21.75*2' } as never), '8000/21.75*2');
  assert.equal(itemExplainText({ item_code: 'OT_WD', calc_note: '', expr_snapshot: '{BASE}/21.75*2' } as never), '{BASE}/21.75*2');
  assert.equal(itemExplainText({ item_code: 'OT_WD', calc_note: '', expr_snapshot: null } as never), '—');

  assert.equal(itemTypeTheme('EARNING'), 'success');
  assert.equal(itemTypeTheme('PRE_DEDUCT'), 'warning');
  assert.equal(itemTypeTheme('WITHHOLD'), 'danger');
  assert.equal(itemTypeTheme('EMPLOYER'), 'primary');
  assert.equal(itemTypeTheme('WHAT'), 'default');
});

test('明细查询串：with_items 恒真、空筛选不发空参数、页码按后端口径兜底', () => {
  assert.deepEqual(buildDetailQuery(1, 20), { page: 1, page_size: 20, with_items: true });
  assert.deepEqual(buildDetailQuery(0, 9999, { dept_id: '', keyword: '  ' }), {
    page: 1,
    page_size: SALARY_DETAIL_MAX_PAGE_SIZE,
    with_items: true,
  });
  assert.deepEqual(buildDetailQuery(2, 50, { dept_id: ' d1 ', keyword: ' 张三 ' }), {
    page: 2,
    page_size: 50,
    with_items: true,
    dept_id: 'd1',
    keyword: '张三',
  });
  assert.equal(isDetailFiltered(emptyDetailFilter()), false);
  assert.equal(isDetailFiltered({ dept_id: 'd1' }), true);
  assert.equal(isDetailFiltered({ keyword: '  ' }), false);
});

// ==================== 6. 凭证卡 ====================

const HEADER: SalarySheetHeader = {
  id: 'sheet-1',
  sheet_no: 'SAL-202611-000001',
  period_code: '2026-11',
  period_start: '2026-11-01',
  period_end: '2026-11-30',
  status: 'CALCULATED',
  status_label: '已计算',
  attendance_period_id: 'ap-1',
  totals: {
    employee_count: 42,
    earning_total: '9889.66',
    pre_deduct_total: '0.00',
    gross_total: '9154.03',
    withhold_total: '1800.00',
    net_total: '7354.03',
    si_company_total: '1680.00',
    hf_company_total: '960.00',
    accrual_total: '11794.03',
  },
  accrual_voucher_id: null,
  accrual_voucher_no: null,
  payment_voucher_id: null,
  payment_voucher_no: null,
  reversal_voucher_id: null,
  approval_instance_id: null,
  calculated_at: '2026-11-13T01:22:33Z',
  calculated_by: 'u1',
  submitted_at: null,
  posted_at: null,
  reversed_at: null,
  reverse_reason: null,
  remark: null,
  created_at: '2026-11-13T01:22:33Z',
  updated_at: '2026-11-13T01:22:33Z',
};

test('凭证卡：未过账两张都空、过账后各有深链、红冲后点亮但不猜单号', () => {
  const empty = voucherCardsOf(HEADER);
  assert.equal(empty.length, 3);
  assert.deepEqual(empty.map((card) => card.role), ['accrual', 'payment', 'reversal']);
  assert.equal(empty[0].no, null);
  assert.equal(empty[0].path, null);
  // 金额口径：计提 = accrual_total，发放 = net_total（实发）
  assert.equal(moneyText(empty[0].amount), '11,794.03');
  assert.equal(moneyText(empty[1].amount), '7,354.03');
  assert.equal(generatedVoucherCount(HEADER), 0);
  assert.equal(voucherCardsOf(null).length, 3);

  const posted: SalarySheetHeader = {
    ...HEADER,
    status: 'POSTED',
    status_label: '已过账',
    accrual_voucher_id: 'v-1',
    accrual_voucher_no: 'PZ-20261130-000012',
    payment_voucher_id: 'v-2',
    payment_voucher_no: 'PZ-20261205-000003',
  };
  const cards = voucherCardsOf(posted);
  assert.equal(cards[0].no, 'PZ-20261130-000012');
  assert.equal(cards[0].path, '/business/erp/voucher?voucher_no=PZ-20261130-000012');
  assert.equal(cards[1].path, '/business/erp/voucher?voucher_no=PZ-20261205-000003');
  assert.equal(generatedVoucherCount(posted), 2);
  // 红冲还没发生时，红冲卡是空的
  assert.equal(cards[2].no, null);

  const reversed: SalarySheetHeader = {
    ...posted,
    status: 'REVERSED',
    reversal_voucher_id: 'v-3',
  };
  const reversedCards = voucherCardsOf(reversed);
  assert.ok(reversedCards[2].no?.includes('红字'), '红冲后红冲卡必须点亮');
  // 单头没有红冲凭证号 → 不给深链（不猜单号）
  assert.equal(reversedCards[2].path, null);
  assert.equal(reversedCards[2].id, 'v-3');
  // 红冲卡不算进「查看凭证 (N)」
  assert.equal(generatedVoucherCount(reversed), 2);
});

test('表格列字典：明细表必须给薪资项列留位置（展开是验收项）', () => {
  const deptKeys = DEPT_SUMMARY_COLUMNS.map((col) => col.colKey);
  assert.deepEqual(deptKeys, [
    'dept_name',
    'employee_count',
    'gross_total',
    'si_company_total',
    'hf_company_total',
    'net_total',
  ]);
  const detailKeys = DETAIL_COLUMNS.map((col) => col.colKey);
  assert.ok(detailKeys.includes('item_count'), '明细表要能显示「15 项」');
  assert.ok(detailKeys.includes('net_amount'), '明细表要能显示实发');
  const itemKeys = ITEM_COLUMNS.map((col) => col.colKey);
  for (const key of ['item_code', 'item_name', 'item_type', 'calc_type', 'amount', 'calc_note']) {
    assert.ok(itemKeys.includes(key), `明细项表少了 ${key}`);
  }
});

// ==================== 7. 接线（页面 / api / 路由） ====================

test('API 层：路径写全 /api、导出走 blob、红冲带 reason', () => {
  for (const path of [
    '`${BASE}/salary/${id}`',
    '`${BASE}/salary/${id}/detail/page`',
    '`${BASE}/salary/${sheetId}/detail/${detailId}`',
    '`${BASE}/salary/${id}/submit`',
    '`${BASE}/salary/${id}/post`',
    '`${BASE}/salary/${id}/reverse`',
    '`${BASE}/salary/${id}/export`',
  ]) {
    assert.ok(API.includes(path), `api 层漏了 ${path}`);
  }
  assert.ok(API.includes("const BASE = '/api/business/hr';"), 'BASE 必须带 /api 前缀');
  assert.ok(API.includes("responseType: 'blob'"), '导出必须走 blob，否则拿到的是乱码');
  assert.ok(API.includes('{ reason }'), '红冲必须把 reason 传给后端');
  assert.ok(
    API.includes('text.includes(\'<html\')') || API.includes("includes('<html')"),
    '导出失败要能指出「命中了 SPA fallback」',
  );
});

test('页面：四段齐全、明细可展开、动作由状态算', () => {
  // ① 单头卡 + 操作栏
  assert.ok(PAGE.includes('hr-salary-action-bar'), '缺操作栏');
  assert.ok(PAGE.includes('DocActionBar'), '操作栏必须复用 DocActionBar（吸顶）');
  assert.ok(PAGE.includes('salaryActionsOf'), '动作必须由状态算，不能手写 if');
  assert.ok(PAGE.includes('hr-salary-gross'), '缺应发指标');
  // ② 汇总卡
  assert.ok(PAGE.includes('hr-salary-dept-table'), '缺部门汇总表');
  assert.ok(PAGE.includes('hr-salary-item-totals'), '缺明细项按类型合计');
  // ③ 明细表可展开（本卡验收项）
  assert.ok(PAGE.includes(':expand-icon="true"'), '明细表必须可展开');
  assert.ok(PAGE.includes('#expandedRow'), '缺展开行模板');
  assert.ok(PAGE.includes('fetchSalaryDetailPage'), '明细要走分页接口');
  assert.ok(PAGE.includes('hr-salary-items-table'), '展开行里要画薪资项表');
  assert.ok(!PAGE.includes('fetchSalaryDetailPage(sheetId.value, 1, 200') , '不要为了展开一次性拉 200 人');
  // ④ 审批轨迹
  assert.ok(PAGE.includes('FlowProgress'), '审批轨迹要复用 FlowProgress');
  assert.ok(PAGE.includes('loadApprovalProgress'), '审批进度只来自 flow 实例接口');
  // ⑤ 凭证卡
  assert.ok(PAGE.includes('voucherCards'), '缺凭证卡');
  assert.ok(PAGE.includes('hr-salary-voucher-cards'), '凭证卡缺 testid');
  // ⑥ 附件
  assert.ok(PAGE.includes('DocAttachmentPanel'), '缺附件面板');
  assert.ok(PAGE.includes('DOC_KIND.SALARY_SHEET'), '附件 doc_type 必须是 biz_salary_sheet');
  // 兜底：不一致要显示、但不整页报错
  assert.ok(PAGE.includes('hr-salary-inconsistent'), '三口径不一致必须显式提示');
});

test('路由：注册了 /business/hr/salary/:id 且指向详情页组件', () => {
  assert.ok(ROUTER.includes("path: 'business/hr/salary/:id'"), '路由没注册，点单号会 404');
  assert.ok(
    ROUTER.includes("import('@/views/business/hr/salary/detail.vue')"),
    '路由指向的组件不对',
  );
  assert.ok(
    ROUTER.includes("permission: 'business:hr:salary:detail'"),
    '详情页路由的权限码必须是 salary:detail',
  );
});
