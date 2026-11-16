/**
 * M1-23：应收账龄分析表**页面**契约单测（页面主体为新落地）。
 *
 * 运行：`cd frontend && pnpm test:unit`（`tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 两层断言
 *
 * 1. **纯逻辑**（`receivable-aging-logic.ts`）：金额格式化不过浮点、客户排序用十进制字符串
 *    精确比较、筛选不改合计、明细按客户收窄、期间下拉的口径。这些是「页面算错帐」的入口。
 * 2. **源码即契约**（`index.vue` / `router/index.ts` / 迁移 `V0418`）：真正会静默出错的是
 *    「模板里调了谁」—— 例如把合计行改成客户端加总、把表格档位写死成 30/60/90、
 *    路由 path 与迁移 `sys_menu.path` 漂了（点菜单 404）。逻辑函数没变、逻辑单测全绿，页面却错了。
 *
 * 只读源码做子串 / 正则断言，不引 DOM、不引浏览器（与 `m112-fx-balance-page.test.ts` 同一套做法）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

import {
  CUSTOMER_SORT_OPTIONS,
  DETAIL_LIMIT,
  PERM_RECEIVABLE_AGING_DATA,
  PERM_RECEIVABLE_AGING_PAGE,
  RECEIVABLE_AGING_REPORT_CODE,
  agingExportFileName,
  arrangeCustomers,
  asOfSourceText,
  bucketShare,
  bucketThemeOf,
  compareDecimalStrings,
  detailsForCustomer,
  detailsScopeText,
  filterCustomers,
  formatAmount,
  isZeroAmount,
  maxOverdueDays,
  overdueText,
  periodStatusText,
  periodsToOptions,
  requestErrorMessage,
  sortCustomers,
  sourceTypeText,
  statusText,
} from '@/views/business/erp/report/receivable-aging/receivable-aging-logic';
import type {
  ReceivableAging,
  ReceivableAgingCustomer,
  ReceivableAgingDetailRow,
} from '@/api/business/erp';

const ROOT = resolve(process.cwd());

function sourceOf(relative: string): string {
  return readFileSync(join(ROOT, relative), 'utf8');
}

const PAGE = sourceOf('src/views/business/erp/report/receivable-aging/index.vue');
const LOGIC = sourceOf('src/views/business/erp/report/receivable-aging/receivable-aging-logic.ts');
const ROUTER = sourceOf('src/router/index.ts');
const MIGRATION_UP = sourceOf('../backend/app/migrations/business/V0418__receivable_aging_page.up.sql');
const MIGRATION_DOWN = sourceOf('../backend/app/migrations/business/V0418__receivable_aging_page.down.sql');

/** 去掉注释再断言实现约束：文档里为了解释「为什么不用 toFixed」会提到 toFixed */
function withoutComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, '').replace(/(^|[^:])\/\/.*$/gm, '$1');
}

const LOGIC_CODE = withoutComments(LOGIC);

/** 取 `<template>` 段：样式与脚本里的同名函数不算页面渲染契约 */
function templateOf(source: string): string {
  const start = source.indexOf('<template>');
  const end = source.lastIndexOf('</template>');
  assert.ok(start >= 0 && end > start, '页面必须有 <template> 段');
  return source.slice(start, end);
}

const TPL = templateOf(PAGE);

// ==================== 夹具 ====================

function customer(over: Partial<ReceivableAgingCustomer> = {}): ReceivableAgingCustomer {
  return {
    customer_id: over.customer_id ?? 'c1',
    customer_name: over.customer_name ?? '客户甲',
    doc_count: over.doc_count ?? 1,
    total_unsettled: over.total_unsettled ?? '1000.0000',
    buckets: over.buckets ?? [{ key: 'D0_30', label: '1-30 天', unpaid: '1000.0000', doc_count: 1 }],
    max_overdue_days: over.max_overdue_days ?? 10,
  };
}

function detail(over: Partial<ReceivableAgingDetailRow> = {}): ReceivableAgingDetailRow {
  return {
    receivable_id: over.receivable_id ?? 'r1',
    ar_no: over.ar_no ?? 'AR-20261116-000001',
    customer_id: over.customer_id ?? 'c1',
    customer_name: over.customer_name ?? '客户甲',
    source_no: over.source_no ?? 'SO-20261116-000001',
    source_type: over.source_type ?? 'SALES_OUT',
    invoice_no: over.invoice_no ?? null,
    period_code: over.period_code ?? '2026-11',
    credit_days: over.credit_days ?? 30,
    due_date: over.due_date ?? '2026-11-01',
    overdue_days: over.overdue_days ?? 15,
    bucket: over.bucket ?? 'D0_30',
    bucket_label: over.bucket_label ?? '1-30 天',
    amount: over.amount ?? '1000.0000',
    received_amount: over.received_amount ?? '0.0000',
    unsettled_amount: over.unsettled_amount ?? '1000.0000',
    status: over.status ?? 'OPEN',
  };
}

// ==================== ① 路由 / 权限码 / 迁移 三处对齐 ====================

test('路由：path / component / meta.permission 与迁移 V0418 逐字一致', () => {
  assert.ok(
    ROUTER.includes("path: 'business/erp/report/receivable-aging'"),
    '路由未注册 = 点菜单 404（n08 会判红）',
  );
  assert.ok(
    ROUTER.includes('@/views/business/erp/report/receivable-aging/index.vue'),
    '路由组件路径与 views 目录不一致',
  );
  assert.ok(
    ROUTER.includes("permission: 'business:erp:report:receivable-aging'"),
    '路由 meta.permission 缺失 = 侧栏显形/前端守卫失效',
  );
  assert.equal(PERM_RECEIVABLE_AGING_PAGE, 'business:erp:report:receivable-aging');
  assert.equal(PERM_RECEIVABLE_AGING_DATA, 'business:erp:receivable:aging');
  assert.equal(RECEIVABLE_AGING_REPORT_CODE, 'RECEIVABLE_AGING');
});

test('迁移：页面行 path / component / 父目录 / sort / title 与路由同形', () => {
  assert.ok(MIGRATION_UP.includes("'/business/erp/report/receivable-aging'"), '迁移 path 与路由不一致');
  assert.ok(
    MIGRATION_UP.includes("'business/erp/report/receivable-aging/index'"),
    '迁移 component 与 views 目录不一致',
  );
  assert.ok(
    MIGRATION_UP.includes("'20000000-0000-0000-0000-000000000e02'::uuid"),
    '页面行必须挂「财务管理」目录下（否则侧栏位置不对）',
  );
  assert.match(MIGRATION_UP, /'应收账龄分析'/, 'title 必须是 6 字菜单名（不带说明/标点）');
  assert.match(MIGRATION_UP, /\n\s+52, 1, NULL, '应收账龄分析'/, 'sort_order 必须是 52 且 icon 留 NULL');
  // 取数码承载行（V0288 …2882）：页面码之外必须补授，否则「点得进、永远 403」
  assert.ok(
    MIGRATION_UP.includes("'20000000-0000-0000-0000-000000002882'::uuid"),
    '迁移必须补授取数码 business:erp:receivable:aging（…2882）',
  );
  // 回滚：先删授权再删菜单行
  const grantIdx = MIGRATION_DOWN.indexOf('DELETE FROM sys_role_menu');
  const menuIdx = MIGRATION_DOWN.indexOf('DELETE FROM sys_menu');
  assert.ok(grantIdx >= 0 && menuIdx > grantIdx, 'down 必须先删 sys_role_menu 再删 sys_menu（外键）');
});

// ==================== ② 金额：十进制字符串，绝不过浮点 ====================

test('formatAmount：千分位 + 2 位小数，四舍五入按十进制第三位（0.595 → 0.60）', () => {
  assert.equal(formatAmount('0.595'), '0.60', '浮点 toFixed 会给 0.59（少一分钱）');
  assert.equal(formatAmount('1234567.8912'), '1,234,567.89');
  assert.equal(formatAmount('1234567.895'), '1,234,567.90');
  assert.equal(formatAmount('0.0049'), '0.00');
  assert.equal(formatAmount('-0.0000'), '0.00', '负零不许显示 -0.00');
  assert.equal(formatAmount('-1234.5'), '-1,234.50');
  assert.equal(formatAmount('99999999999999999999.9999'), '100,000,000,000,000,000,000.00', '20 位整数进位不能丢');
  assert.equal(formatAmount(''), '');
  assert.equal(formatAmount(null), '');
});

test('isZeroAmount：各种零写法都算零，0.01 不算', () => {
  for (const zero of ['0', '0.0', '0.0000', '-0.0000', '', null, undefined]) {
    assert.equal(isZeroAmount(zero), true, `${String(zero)} 应判为零`);
  }
  for (const nonZero of ['0.01', '-0.01', '1.0000']) {
    assert.equal(isZeroAmount(nonZero), false, `${String(nonZero)} 不该判为零`);
  }
});

test('compareDecimalStrings：精确比较，20 位精度不塌缩（Number 会判相等）', () => {
  assert.equal(compareDecimalStrings('1000.00', '999.99'), 1);
  assert.equal(compareDecimalStrings('100', '100.0000'), 0, '不同小数位表示同一个数');
  assert.equal(compareDecimalStrings('0.595', '0.5949'), 1);
  // 这一对是 Number() 的死角：两者转 double 后完全相等
  assert.equal(Number('1000000000000000.0001'), Number('1000000000000000.0002'));
  assert.equal(compareDecimalStrings('1000000000000000.0001', '1000000000000000.0002'), -1);
  assert.equal(compareDecimalStrings('-5', '3'), -1);
  assert.equal(compareDecimalStrings('-5.0001', '-5'), -1);
  assert.equal(compareDecimalStrings('-0.0000', '0'), 0, '负零等于零');
  assert.equal(compareDecimalStrings('', '0'), 0, '空值按 0 处理不抛错');
  // 金额路径上不许出现浮点转换 / 浮点四舍五入（格式化走十进制字符串逐位进位）
  assert.doesNotMatch(LOGIC_CODE, /toFixed\(/, '金额格式化里出现 toFixed = 浮点四舍五入');
  assert.doesNotMatch(LOGIC_CODE, /parseFloat\(/, '金额解析出现 parseFloat = 精度塌缩');
  assert.doesNotMatch(LOGIC_CODE, /Number\([^)]*total_unsettled/, '客户金额被转成浮点数再比较');
});

// ==================== ③ 客户行：筛 / 排（只改显示，不改合计） ====================

test('filterCustomers：按客户名筛（去空白、大小写不敏感），不改任何金额字段', () => {
  const rows = [
    customer({ customer_id: 'a', customer_name: '华为技术有限公司', total_unsettled: '500.0000' }),
    customer({ customer_id: 'b', customer_name: 'Alibaba Cloud', total_unsettled: '700.0000' }),
  ];
  assert.equal(filterCustomers(rows, '').length, 2, '空关键字 = 全命中');
  assert.equal(filterCustomers(rows, '  ').length, 2, '纯空白 = 全命中');
  assert.deepEqual(filterCustomers(rows, '华为').map((r) => r.customer_id), ['a']);
  assert.deepEqual(filterCustomers(rows, 'alibaba').map((r) => r.customer_id), ['b'], '大小写不敏感');
  assert.deepEqual(filterCustomers(rows, ' 华为 ').map((r) => r.customer_id), ['a'], '两端空白要去掉');
  assert.deepEqual(filterCustomers(rows, '不存在').map((r) => r.customer_id), []);
  // 筛出来的行与原始行是同一对象、金额原样（页面拿它只负责渲染）
  assert.equal(filterCustomers(rows, '华为')[0].total_unsettled, '500.0000');
});

test('sortCustomers：未收降序用精确比较、逾期降序、客户名升序（空名排最后）', () => {
  const rows = [
    customer({ customer_id: 'a', customer_name: '乙', total_unsettled: '999.99', max_overdue_days: 3 }),
    customer({ customer_id: 'b', customer_name: '甲', total_unsettled: '1000.00', max_overdue_days: 90 }),
    customer({ customer_id: 'c', customer_name: '', total_unsettled: '1000.0000', max_overdue_days: 0 }),
  ];
  // 1000.00 与 1000.0000 相等 → 保持原相对顺序（稳定），且都排在 999.99 前面
  assert.deepEqual(
    sortCustomers(rows, 'AMOUNT_DESC').map((r) => r.customer_id),
    ['b', 'c', 'a'],
  );
  assert.deepEqual(
    sortCustomers(rows, 'OVERDUE_DESC').map((r) => r.customer_id),
    ['b', 'a', 'c'],
  );
  assert.deepEqual(
    sortCustomers(rows, 'NAME_ASC').map((r) => r.customer_id),
    ['b', 'a', 'c'],
    '按拼音「甲(jia)」在「乙(yi)」前，空名排最后',
  );
  assert.deepEqual(
    sortCustomers(rows, 'SERVER').map((r) => r.customer_id),
    ['a', 'b', 'c'],
    'SERVER = 后端原序，原样复制',
  );
  // 不原地改
  assert.deepEqual(rows.map((r) => r.customer_id), ['a', 'b', 'c']);
});

test('arrangeCustomers：先筛后排，两步都不碰金额', () => {
  const rows = [
    customer({ customer_id: 'a', customer_name: '甲公司', total_unsettled: '10.0000' }),
    customer({ customer_id: 'b', customer_name: '甲集团', total_unsettled: '2000.0000' }),
    customer({ customer_id: 'c', customer_name: '乙公司', total_unsettled: '3000.0000' }),
  ];
  assert.deepEqual(
    arrangeCustomers(rows, '甲', 'AMOUNT_DESC').map((r) => r.customer_id),
    ['b', 'a'],
  );
  assert.deepEqual(
    arrangeCustomers(rows, '甲', 'NAME_ASC').map((r) => r.customer_id),
    ['a', 'b'],
    '共同前缀「甲」之后按拼音比：公(gong) < 集(ji)',
  );
});

test('CUSTOMER_SORT_OPTIONS：三种排序 + 后端原序，值域与类型一致', () => {
  assert.deepEqual(
    CUSTOMER_SORT_OPTIONS.map((o) => o.value),
    ['SERVER', 'OVERDUE_DESC', 'NAME_ASC'],
  );
  assert.ok(CUSTOMER_SORT_OPTIONS.every((o) => o.label.trim().length > 0));
});

// ==================== ④ 明细：按客户收窄 + 标题文案 ====================

test('detailsForCustomer：不传 = 全部；传 id = 只留该客户；未知 id = 空数组', () => {
  const rows = [
    detail({ receivable_id: 'r1', customer_id: 'c1' }),
    detail({ receivable_id: 'r2', customer_id: 'c2' }),
    detail({ receivable_id: 'r3', customer_id: 'c1' }),
  ];
  assert.deepEqual(detailsForCustomer(rows, '').map((r) => r.receivable_id), ['r1', 'r2', 'r3']);
  assert.deepEqual(detailsForCustomer(rows, null).map((r) => r.receivable_id), ['r1', 'r2', 'r3']);
  assert.deepEqual(detailsForCustomer(rows, 'c1').map((r) => r.receivable_id), ['r1', 'r3']);
  assert.deepEqual(detailsForCustomer(rows, 'zzz'), []);
});

test('detailsScopeText：点名客户 / 张数 / 截断说明都在一句话里', () => {
  assert.equal(detailsScopeText(5, 5, '', false), '全部客户 5 张（共 5 张）');
  assert.equal(detailsScopeText(2, 5, '客户甲', false), '客户「客户甲」 2 张（已筛出 2 / 共 5 张）');
  assert.match(detailsScopeText(1000, 1200, '', true), /最多回 1000 张/, '截断必须明说，别让人以为导不全');
  assert.equal(DETAIL_LIMIT, 1000, '与后端 payable_aging::DETAIL_LIMIT 一致');
});

// ==================== ⑤ 档位 / 合计 / 期间 ====================

test('maxOverdueDays：取各客户最大值，不是相加；空表 0', () => {
  const report = {
    customers: [customer({ max_overdue_days: 12 }), customer({ max_overdue_days: 365 })],
  } as Pick<ReceivableAging, 'customers'>;
  assert.equal(maxOverdueDays(report), 365);
  assert.equal(maxOverdueDays({ customers: [] }), 0);
  assert.equal(maxOverdueDays(null), 0);
});

test('bucketShare：只画进度条（0 不留残影、超总额截 100）', () => {
  assert.equal(bucketShare('0.0000', '1000.0000'), 0);
  assert.equal(bucketShare('250.0000', '1000.0000'), 25);
  assert.equal(bucketShare('2000.0000', '1000.0000'), 100, '单档不会超过合计（浮点残差也不能画成 100.1）');
  assert.equal(bucketShare('1000.0000', '0'), 0, '合计为 0 时不能除零');
});

test('档位配色：七档全覆盖，认不出的键回落 default（不白屏）', () => {
  assert.equal(bucketThemeOf('NOT_DUE'), 'success');
  assert.equal(bucketThemeOf('D0_30'), 'primary');
  assert.equal(bucketThemeOf('D31_60'), 'warning');
  assert.equal(bucketThemeOf('D61_90'), 'warning');
  assert.equal(bucketThemeOf('D91_180'), 'danger');
  assert.equal(bucketThemeOf('D181_365'), 'danger');
  assert.equal(bucketThemeOf('OVER_365'), 'danger');
  assert.equal(bucketThemeOf('OVER_90'), 'danger', '旧 5 档口径的兜底键保留');
  assert.equal(bucketThemeOf('WHATEVER_NEW'), 'default', '后端加档位时只是灰色，不白屏');
});

test('periodsToOptions：不列未开账期间，按期间倒序，标签带结账状态', () => {
  const rows = [
    { period_code: '2026-09', status: 'CLOSED' },
    { period_code: '2026-11', status: 'NOT_OPENED' },
    { period_code: '2026-10', status: 'OPEN' },
    { period_code: '2026-08', status: 'DERIVED' },
  ] as unknown as Parameters<typeof periodsToOptions>[0];
  const options = periodsToOptions(rows);
  assert.deepEqual(options.map((o) => o.value), ['2026-10', '2026-09', '2026-08']);
  assert.equal(options[0].label, '2026-10（未结账）');
  assert.equal(options[1].label, '2026-09（已结账）');
  assert.equal(options[2].label, '2026-08（未开账·按自然月推算）');
});

// ==================== ⑥ 文案 / 报错 ====================

test('枚举文案：状态 / 来源 / 基准日来源 / 期间状态逐字对后端', () => {
  assert.equal(statusText('OPEN'), '未收');
  assert.equal(statusText('PARTIAL'), '部分已收');
  assert.equal(statusText('SETTLED'), '已收清');
  assert.equal(statusText('WRITTEN_OFF'), '坏账核销');
  assert.equal(statusText('CANCELLED'), '已作废');
  assert.equal(statusText('WEIRD'), 'WEIRD', '未知状态原样显示，不吞成空');
  assert.equal(sourceTypeText('SALES_OUT'), '销售出库');
  assert.equal(sourceTypeText('MANUAL'), '手工挂账');
  assert.equal(asOfSourceText('QUERY'), '手动指定');
  assert.equal(asOfSourceText('PERIOD'), '期间末日');
  assert.equal(asOfSourceText('TODAY'), '今天');
  assert.equal(periodStatusText('CLOSED'), '已结账');
  assert.equal(periodStatusText('DERIVED'), '未开账·按自然月推算');
  assert.equal(periodStatusText(null), '');
});

test('overdueText：正数逾期 / 0 到期当天 / 负数未到期（与应付账龄同口径）', () => {
  assert.equal(overdueText(15), '逾期 15');
  assert.equal(overdueText(0), '到期当天');
  assert.equal(overdueText(-7), '未到期 7');
});

test('requestErrorMessage：403 点名缺哪个码，别只写「无权限」', () => {
  const forbidden = requestErrorMessage(new Error('Request failed with status code 403'));
  assert.ok(forbidden.includes('business:erp:receivable:aging'), '403 必须点名取数码');
  assert.ok(forbidden.includes('菜单授了、取数码没授'), '403 必须给出最常见的成因');
  assert.equal(requestErrorMessage(new Error('数据库连接失败')), '数据库连接失败', '非 403 原样透出');
  assert.equal(requestErrorMessage(''), '查询失败（后端没有返回原因）');
});

test('agingExportFileName：与 receivable-logic 同一套命名', () => {
  assert.equal(
    agingExportFileName({ as_of: '2026-11-30', period: '2026-11' }),
    '应收账款账龄分析表_2026-11_截止2026-11-30.xlsx',
  );
  assert.equal(
    agingExportFileName({ as_of: '2026-11-30', period: null }),
    '应收账款账龄分析表_无期间_截止2026-11-30.xlsx',
  );
});

// ==================== ⑦ 页面源码契约 ====================

test('前端不算账：页面里没有 toFixed，也没有任何客户端金额加总', () => {
  assert.doesNotMatch(PAGE, /toFixed\(/, '页面出现 toFixed = 金额被浮点化');
  assert.doesNotMatch(TPL, /\.reduce\(/, '模板里出现 reduce = 前端在自己加总金额（第二套账）');
  assert.doesNotMatch(PAGE, /Number\([^)]*total_unsettled/, '合计被转成浮点数');
  assert.doesNotMatch(PAGE, /total_unsettled\s*=/, '页面不许自己写 total_unsettled');
});

test('分档不写死：表头按 report.buckets 渲染，客户行按 customer.buckets 渲染', () => {
  assert.ok(TPL.includes('v-for="bucket in report.buckets"'), '表头必须按后端 buckets 渲染');
  assert.ok(TPL.includes('v-for="cell in customer.buckets"'), '客户行必须按行内 buckets 渲染');
  assert.ok(TPL.includes('v-for="cell in report.summary"'), '合计行必须按 summary 渲染');
  for (const hardcoded of ['1-30 天', '31-60 天', '365 天以上']) {
    assert.ok(!TPL.includes(hardcoded), `模板里写死了档位名「${hardcoded}」——加档位前端不会跟上`);
  }
});

test('合计行：取后端 total_unsettled，且标注「全表」不受客户筛选影响', () => {
  assert.ok(TPL.includes('{{ formatAmount(report.total_unsettled) }}'), '合计行必须用后端合计');
  assert.ok(TPL.includes('合计（全表 {{ report.customer_count }} 家）'), '合计行要写明是全表口径');
  assert.ok(TPL.includes('data-testid="ra-filter-note"'), '筛选生效时要有「合计不重算」的说明');
  assert.ok(TPL.includes('合计行仍是全表数（不按筛选重算）'));
  assert.ok(PAGE.includes('arrangeCustomers(report.value.customers, keyword.value, sortMode.value)'), '客户行必须走 arrangeCustomers（筛+排收在纯逻辑里）');
  assert.ok(PAGE.includes('filterActive'), '筛选提示要有开关，否则空表时也显示');
});

test('明细：按选中客户收窄，标题文案由 detailsScopeText 生成', () => {
  assert.ok(PAGE.includes('detailsForCustomer(report.value.details, selectedCustomerId.value)'));
  assert.ok(TPL.includes('detailsScopeText(visibleDetails.length, report.detail_count, selectedCustomerName, report.detail_truncated)'));
  assert.ok(TPL.includes('@click="toggleCustomer(customer)"'), '点客户行要能收窄明细');
  assert.ok(TPL.includes('@click="clearSelectedCustomer()"'), '要有「看全部客户」的复位入口');
  assert.ok(PAGE.includes('selectedCustomerId.value = \'\''), '重新出表后必须复位收窄（客户可能已不在表里）');
});

test('明细列：14 个数据列逐字固定（少一列要人判断，不许悄悄丢）', () => {
  const tables = TPL.split('<thead>').map((chunk) => chunk.slice(0, chunk.indexOf('</thead>')));
  const headers = tables
    .filter((chunk) => chunk.includes('应收单号'))
    .flatMap((chunk) =>
      Array.from(chunk.matchAll(/<th[^>]*>([\s\S]*?)<\/th>/g)).map((m) =>
        m[1]
          .replace(/<[^>]+>/g, '')
          .replace(/\s+/g, ' ')
          .trim(),
      ),
    );
  assert.deepEqual(headers, [
    '应收单号',
    '客户',
    '来源单号',
    '来源类型',
    '发票号',
    '所属期间',
    '到期日',
    '账期',
    '逾期天数',
    '账龄档',
    '应收',
    '已收',
    '未收',
    '状态',
  ]);
});

test('导出：走 exportReceivableAging + saveBlob，入参与查询完全一致', () => {
  assert.ok(PAGE.includes('exportReceivableAging({'), '导出必须调后端接口（前端没有 xlsx 能力）');
  assert.ok(PAGE.includes('saveBlob(blob, agingExportFileName(report.value))'), '要保存成文件并带兜底文件名');
  assert.ok(PAGE.includes('period: period.value || undefined'), '导出必须与列表同参数');
  assert.ok(PAGE.includes('as_of: asOf.value || undefined'), '导出必须与列表同参数');
  assert.ok(TPL.includes(':disabled="!report"'), '没出表时导出按钮必须置灰（点下去 400）');
});

test('基准日来源照后端回显：不猜、不本地补算', () => {
  assert.ok(TPL.includes('asOfSourceText(report.as_of_source)'), '页头必须写基准日来源');
  assert.ok(
    PAGE.includes('asOf.value = \'\''),
    '切期间时基准日必须清空 = 交给后端按期间末日算（自己填末日会让来源永远显示「手动指定」）',
  );
  // 期间下拉：只列已建立期间，口径在纯逻辑里
  assert.ok(PAGE.includes('periodsToOptions(') || PAGE.includes('periodsToOptions'), '期间下拉必须走 periodsToOptions');
});

test('空态 / 错误态：三种「没有数据」要分开说，403 走人话', () => {
  assert.ok(TPL.includes('data-testid="ra-empty"'), '缺空态锚点（真机验收要用）');
  assert.ok(TPL.includes('没有客户名包含'), '筛选把客户筛空时要说清是筛选的锅');
  assert.ok(TPL.includes('没有未收清的应收单（这不是出错）'), '真空表要说清不是出错');
  assert.ok(TPL.includes('data-testid="ra-tip"'), '未查询时要有引导文案');
  assert.ok(PAGE.includes('requestErrorMessage(err)'), '错误文案必须走 requestErrorMessage（403 点名缺码）');
  assert.ok(TPL.includes('data-testid="ra-error"'), '错误提示要能重试');
});
