/**
 * 财务基础页（会计科目 / 会计期间）纯逻辑单测（P4-C）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * 为什么测 `.ts` 不测 `.vue`：树拼得对不对、`PROFIT_LOSS` 译成什么中文、期间状态是灰还是主色，
 * 这些都是会直接误导财务的判定，却都埋在模板里 —— 搬进 `account-tree.ts` / `period-logic.ts`
 * 之后，`vue-tsc` 之外还多一层断言兜着。
 *
 * 断言口径照**库**抄（不是照卡面草稿抄）：
 * - `psql \d biz_account` → `ck_biz_account_category` = `ASSET|LIABILITY|EQUITY|COST|PROFIT_LOSS`
 *   （卡面写的 `PROFIT` 在库里不存在，这里验证它被当作别名宽容处理）；
 * - `psql \d biz_accounting_period` → `ck_biz_period_status` = `NOT_OPENED|OPEN|CLOSED`；
 * - 种子 4 条：`1405 库存商品 / 2202 应付账款 / 2221 应交税费 / 2221.01 应交税费-进项税`，
 *   `2221.01.parent_id → 2221.id`（`parent_id` 自引用，`level` 甲乙级）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { AccountRow } from '@/api/business/erp';
import {
  ACCOUNT_CATEGORY_OPTIONS,
  accountCategoryLabel,
  accountCategoryTheme,
  accountLevelLabel,
  accountNodeKey,
  accountStatusLabel,
  accountStatusTheme,
  accountTreeDepth,
  accountTreeKeys,
  accountTreeMeta,
  balanceDirectionLabel,
  buildAccountTree,
  compareAccountCode,
  countAccounts,
  emptyAccountFilter,
  expandableAccountIds,
  filterAccountTree,
  flattenAccountTree,
  initialExpandedAccountIds,
  isAccountFiltered,
  toggleExpandedAccountId,
  type AccountFilterState,
} from '@/views/business/erp/account/account-tree';
import {
  PERIOD_STATUS_OPTIONS,
  buildPeriodQuery,
  closedAtText,
  emptyPeriodFilter,
  isCurrentPeriod,
  isPeriodFiltered,
  periodDays,
  periodRangeText,
  periodStatusLabel,
  periodStatusTheme,
  periodTitle,
  periodYearOptions,
} from '@/views/business/erp/period/period-logic';

// ==================== 造数据 ====================

/** 造一条科目（只给断言需要的字段，其余照 `AccountRow` 补默认值） */
function account(partial: Partial<AccountRow> & Pick<AccountRow, 'id' | 'account_code' | 'account_name'>): AccountRow {
  return {
    parent_id: null,
    level: 1,
    account_category: 'ASSET',
    balance_direction: 'DEBIT',
    is_leaf: true,
    aux_dimensions: null,
    status: 1,
    remark: null,
    created_at: '2026-10-01T00:00:00Z',
    updated_at: '2026-10-01T00:00:00Z',
    ...partial,
  };
}

/** 库里的 4 条种子（`psql` 查出来的原样，id 用可读串代替 uuid） */
const SEED_ROWS: AccountRow[] = [
  account({ id: 'acc-1405', account_code: '1405', account_name: '库存商品', account_category: 'ASSET' }),
  account({
    id: 'acc-2202',
    account_code: '2202',
    account_name: '应付账款',
    account_category: 'LIABILITY',
    balance_direction: 'CREDIT',
  }),
  account({
    id: 'acc-2221',
    account_code: '2221',
    account_name: '应交税费',
    account_category: 'LIABILITY',
    balance_direction: 'CREDIT',
    is_leaf: false,
  }),
  account({
    id: 'acc-2221-01',
    account_code: '2221.01',
    account_name: '应交税费-进项税',
    account_category: 'LIABILITY',
    balance_direction: 'CREDIT',
    parent_id: 'acc-2221',
    level: 2,
  }),
];

/**
 * 3 层合成数据：`6602 管理费用` → `6602.01 办公费` → `6602.01.01 办公用品`。
 *
 * 种子只有 2 层，而验收①要的是「3 层结构正确展开」—— 真到 3 层得等企业自己加科目，
 * 所以这里造一条 3 层链，钉住 `level` 与树深的一致性。
 */
const THREE_LEVEL_ROWS: AccountRow[] = [
  account({
    id: 'acc-6602',
    account_code: '6602',
    account_name: '管理费用',
    account_category: 'PROFIT_LOSS',
    balance_direction: 'DEBIT',
    is_leaf: false,
  }),
  account({
    id: 'acc-6602-01',
    account_code: '6602.01',
    account_name: '办公费',
    account_category: 'PROFIT_LOSS',
    balance_direction: 'DEBIT',
    parent_id: 'acc-6602',
    level: 2,
    is_leaf: false,
  }),
  account({
    id: 'acc-6602-01-01',
    account_code: '6602.01.01',
    account_name: '办公用品',
    account_category: 'PROFIT_LOSS',
    balance_direction: 'DEBIT',
    parent_id: 'acc-6602-01',
    level: 3,
  }),
];

// ==================== ① 科目树：3 层结构正确展开 ====================

test('科目树：3 层结构逐层展开，父在前、子缩进深度 = level', () => {
  const tree = buildAccountTree([...SEED_ROWS, ...THREE_LEVEL_ROWS]);

  // 根只有 3 个：1405 / 2202 / 2221 / 6602 里，2221 与 6602 有子节点，都是根
  assert.deepEqual(
    tree.map((node) => node.account_code),
    ['1405', '2202', '2221', '6602'],
    '根节点按编码自然序，且 2221.01 / 6602.01 / 6602.01.01 不能被当成根',
  );
  assert.equal(tree.length, 4);

  // 深度：1405 第 1 层、2221.01 第 2 层、6602.01.01 第 3 层
  assert.equal(accountTreeDepth(tree), 3, '3 层结构必须真的到 3 层');
  assert.equal(countAccounts(tree), 7, '4 条种子 + 3 条合成，一条都不能丢');

  const flat = flattenAccountTree(tree);
  const byCode = new Map(flat.map((row) => [row.node.account_code, row.depth]));
  assert.equal(byCode.get('6602'), 1);
  assert.equal(byCode.get('6602.01'), 2);
  assert.equal(byCode.get('6602.01.01'), 3);
  // 深度与库列 `level` 一致（库里 level 是维护好的；不一致说明数据脏，树深才是渲染真相）
  for (const row of flat) {
    assert.equal(row.depth, row.node.level, `${row.node.account_code} 的渲染深度应与 level 一致`);
  }

  // 2221 下确实挂了 2221.01（验收①「2221 下有子节点」的直接断言）
  const tax = tree.find((node) => node.account_code === '2221');
  assert.ok(tax, '2221 应交税费必须存在');
  assert.deepEqual(tax?.children?.map((child) => child.account_code), ['2221.01']);

  // 叶子不能带 children（TDesign 树形表格靠「有没有 children」判箭头，空数组会画出空箭头）
  const leaf = flat.find((row) => row.node.account_code === '1405');
  assert.equal(leaf?.node.children, undefined, '叶子节点的 children 必须是 undefined，不能是 []');
});

test('科目树：孤儿 / 自环 / 互指环都不会让整枝消失', () => {
  const rows: AccountRow[] = [
    // 父 id 不在本批数据里（被过滤 / 被软删）→ 自己当根
    account({ id: 'orphan', account_code: '1122', account_name: '应收账款', parent_id: 'missing-parent', level: 2 }),
    // 自环
    account({ id: 'self', account_code: '9999', account_name: '自指科目', parent_id: 'self' }),
    // 互指环：A 父是 B，B 父是 A
    account({ id: 'ring-a', account_code: '8888', account_name: '环 A', parent_id: 'ring-b' }),
    account({ id: 'ring-b', account_code: '8887', account_name: '环 B', parent_id: 'ring-a' }),
  ];
  const tree = buildAccountTree(rows);
  assert.equal(
    countAccounts(tree),
    4,
    '孤儿 / 自环 / 环上的节点都必须出现在树上（否则用户会以为科目丢了）',
  );
  assert.equal(accountTreeDepth(tree), 1, '环上的节点不能再套娃（否则渲染无限缩进）');
});

test('科目树：编码自然序（1405 < 2202 < 2221 < 2221.01 < 2221.02）', () => {
  assert.ok(compareAccountCode('1405', '2202') < 0);
  assert.ok(compareAccountCode('2221', '2221.01') < 0);
  assert.ok(compareAccountCode('2221.01', '2221.02') < 0);
  // 段内按**数字**比：`2221.1` 是 1 号子科目，排在 `2221.02`（2 号）前面；
  // 字典序会给出相反结果（`'2221.02' < '2221.1'`），财务看科目树会错位
  assert.ok(compareAccountCode('2221.1', '2221.02') < 0, '点号分段后比数字，不是比字符串');
  assert.equal(compareAccountCode('2221.01', '2221.01'), 0);
});

test('科目树：展开态 = 有下级的节点，折叠只动自己一个', () => {
  const tree = buildAccountTree([...SEED_ROWS, ...THREE_LEVEL_ROWS]);
  const expandable = expandableAccountIds(tree);
  // 展开态按节点 id 记（页面 `expanded-tree-nodes` 的口径），有 children 的三个才可展开
  assert.deepEqual(expandable, ['acc-2221', 'acc-6602', 'acc-6602-01'], '能展开的只有带 children 的三个');

  // 首屏全展开（科目是「少量多层看全貌」的数据）
  assert.deepEqual(initialExpandedAccountIds(tree), expandable);

  // 逐节点折叠：只动自己，不联级
  assert.deepEqual(toggleExpandedAccountId(['acc-2221', 'acc-6602'], 'acc-2221'), ['acc-6602']);
  assert.deepEqual(toggleExpandedAccountId(['acc-6602'], 'acc-2221'), ['acc-6602', 'acc-2221']);
});

test('科目树：row-key 唯一（空 id 退到编码，重复补后缀）', () => {
  assert.equal(accountNodeKey({ id: 'abc', account_code: '1405' }), 'abc');
  assert.equal(accountNodeKey({ id: '', account_code: '1405' }), 'code:1405');
  assert.equal(accountNodeKey({ id: null, account_code: '' }, 3), 'account:#3');

  const keys = accountTreeKeys([...SEED_ROWS, ...THREE_LEVEL_ROWS]);
  assert.equal(new Set(keys).size, keys.length, 'row-key 不能撞车（撞车会让两行渲染成一行）');

  const dupKeys = accountTreeKeys([
    account({ id: '', account_code: '1405', account_name: '库存商品' }),
    account({ id: '', account_code: '1405', account_name: '库存商品（重复）' }),
  ]);
  assert.deepEqual(dupKeys, ['code:1405', 'code:1405~1']);
});

test('科目树：过滤保留祖先，但祖先不算命中', () => {
  const tree = buildAccountTree([...SEED_ROWS, ...THREE_LEVEL_ROWS]);

  // 关键词命中子科目 2221.01 → 父 2221 必须留在树上，否则整枝静默消失
  const keywordResult = filterAccountTree(tree, { ...emptyAccountFilter(), keyword: '进项' });
  assert.equal(keywordResult.matchedCount, 1, '只数真命中的行，父节点不算');
  assert.deepEqual(
    keywordResult.tree.map((node) => node.account_code),
    ['2221'],
    '父科目 2221 作为祖先保留',
  );
  assert.deepEqual(
    keywordResult.tree[0].children?.map((child) => child.account_code),
    ['2221.01'],
  );
  assert.deepEqual(keywordResult.expandedIds, ['acc-2221'], '过滤后要自动展开祖先，否则用户看不到命中行');

  // 类型过滤
  const liab = filterAccountTree(tree, { ...emptyAccountFilter(), account_category: 'LIABILITY' });
  assert.equal(liab.matchedCount, 3, '负债 3 条：2202 / 2221 / 2221.01');

  // 状态过滤（种子里 status 全是 1）
  const disabled = filterAccountTree(tree, { ...emptyAccountFilter(), status: 0 });
  assert.equal(disabled.matchedCount, 0);
  assert.deepEqual(disabled.tree, []);

  // 无筛选 = 原树，不复制
  const noFilter = filterAccountTree(tree, emptyAccountFilter());
  assert.equal(noFilter.tree, tree, '空筛选应原样返回（省一次深拷贝）');
  assert.equal(isAccountFiltered(emptyAccountFilter()), false);
  assert.equal(isAccountFiltered({ keyword: '  ', account_category: '', status: null }), false);
  assert.equal(isAccountFiltered({ ...emptyAccountFilter(), status: 0 }), true, 'status=0 是有效筛选');

  // 副标题文案
  assert.equal(accountTreeMeta([]), '暂无科目');
  assert.match(accountTreeMeta(tree), /共 7 条科目 · 4 个一级 · 最深 3 级/);
});

// ==================== ② 科目类型映射 ====================

test('科目类型映射：ASSET/LIABILITY/EQUITY/COST/PROFIT_LOSS → 中文（卡面的 PROFIT 是别名）', () => {
  assert.equal(accountCategoryLabel('ASSET'), '资产');
  assert.equal(accountCategoryLabel('LIABILITY'), '负债');
  assert.equal(accountCategoryLabel('EQUITY'), '权益');
  assert.equal(accountCategoryLabel('COST'), '成本');
  // 库里真值是 PROFIT_LOSS；卡面写的 PROFIT 当别名宽容处理（老数据不炸）
  assert.equal(accountCategoryLabel('PROFIT_LOSS'), '损益');
  assert.equal(accountCategoryLabel('PROFIT'), '损益');

  // 未知值原样露出（宁可显示后端新枚举，也不要空白），空值给破折号
  assert.equal(accountCategoryLabel('SOMETHING_NEW'), 'SOMETHING_NEW');
  assert.equal(accountCategoryLabel(''), '—');
  assert.equal(accountCategoryLabel(null), '—');

  // 下拉只给 5 个真值（顺序照 CHECK 约束），别把别名放进去让用户选
  assert.deepEqual(
    ACCOUNT_CATEGORY_OPTIONS.map((item) => item.value),
    ['ASSET', 'LIABILITY', 'EQUITY', 'COST', 'PROFIT_LOSS'],
  );
  assert.deepEqual(
    ACCOUNT_CATEGORY_OPTIONS.map((item) => item.label),
    ['资产', '负债', '权益', '成本', '损益'],
  );

  // 配色：真值都有主题色，未知值兜底 default（不写裸 hex，全走 TDesign 主题名）
  assert.equal(accountCategoryTheme('ASSET'), 'primary');
  assert.equal(accountCategoryTheme('LIABILITY'), 'warning');
  assert.equal(accountCategoryTheme('EQUITY'), 'success');
  assert.equal(accountCategoryTheme('COST'), 'default');
  assert.equal(accountCategoryTheme('PROFIT_LOSS'), accountCategoryTheme('PROFIT'));
  assert.equal(accountCategoryTheme('UNKNOWN'), 'default');
});

test('科目其余译名：余额方向 / 启用状态 / 层级', () => {
  assert.equal(balanceDirectionLabel('DEBIT'), '借');
  assert.equal(balanceDirectionLabel('CREDIT'), '贷');
  assert.equal(balanceDirectionLabel(null), '—');

  // status 是 smallint 1/0，不是布尔
  assert.equal(accountStatusLabel(1), '启用');
  assert.equal(accountStatusLabel(0), '停用');
  assert.equal(accountStatusLabel(null), '停用');
  assert.equal(accountStatusTheme(1), 'success');
  assert.equal(accountStatusTheme(0), 'default');

  assert.equal(accountLevelLabel(1), '一级');
  assert.equal(accountLevelLabel(2), '二级');
  assert.equal(accountLevelLabel(3), '三级');
  assert.equal(accountLevelLabel(4), '4 级', '超出常规层级如实报数字，不编不存在的口径');
  assert.equal(accountLevelLabel(0), '—');
});

// ==================== ③ 期间状态映射 ====================

test('期间状态映射：NOT_OPENED/OPEN/CLOSED → 中文 + 配色', () => {
  assert.equal(periodStatusLabel('NOT_OPENED'), '未开启');
  assert.equal(periodStatusLabel('OPEN'), '已开启');
  assert.equal(periodStatusLabel('CLOSED'), '已结账');
  assert.equal(periodStatusLabel(''), '—');
  assert.equal(periodStatusLabel('WEIRD'), 'WEIRD', '未知状态原样露出，便于发现后端新增枚举');

  assert.equal(periodStatusTheme('NOT_OPENED'), 'default');
  assert.equal(periodStatusTheme('OPEN'), 'primary');
  assert.equal(periodStatusTheme('CLOSED'), 'success');
  assert.equal(periodStatusTheme('WEIRD'), 'default');

  // 下拉只有 3 个真值，顺序照生命周期
  assert.deepEqual(
    PERIOD_STATUS_OPTIONS.map((item) => item.value),
    ['NOT_OPENED', 'OPEN', 'CLOSED'],
  );
  assert.deepEqual(
    PERIOD_STATUS_OPTIONS.map((item) => item.label),
    ['未开启', '已开启', '已结账'],
  );
});

test('期间其余逻辑：标题 / 区间 / 天数 / 当月 / 结账时间 / 查询入参', () => {
  assert.equal(periodTitle('2026-10'), '2026 年 10 月');
  assert.equal(periodTitle('bad'), 'bad');

  const oct = { start_date: '2026-10-01', end_date: '2026-10-31', period_code: '2026-10' };
  assert.equal(periodRangeText(oct), '2026-10-01 ~ 2026-10-31');
  assert.equal(periodDays(oct), 31);
  assert.equal(periodDays({ start_date: '2026-02-01', end_date: '2026-02-28' }), 28);
  assert.equal(periodDays({ start_date: '2026-02-28', end_date: '2026-02-01' }), 0, '倒置区间不抛错，返回 0');

  // 当月判定：显式传 now，钉死时区无关（用本地年月比 period_code）
  assert.equal(isCurrentPeriod(oct, new Date(2026, 9, 15)), true);
  assert.equal(isCurrentPeriod(oct, new Date(2026, 10, 1)), false);

  // 结账时间：CLOSED 但无时间 → 如实说，不拿 updated_at 顶替
  assert.equal(closedAtText({ status: 'CLOSED', closed_at: '2026-11-01T02:30:00Z' }), '2026-11-01 02:30:00');
  assert.equal(closedAtText({ status: 'CLOSED', closed_at: null }), '已结账（无结账时间）');
  assert.equal(closedAtText({ status: 'OPEN', closed_at: null }), '—');

  // 年份下拉：数据年份 + 当前年，倒序
  assert.deepEqual(periodYearOptions([{ period_year: 2025 }], new Date(2026, 9, 1)), [2026, 2025]);
  assert.deepEqual(periodYearOptions([{ period_year: 2026 }], new Date(2026, 9, 1)), [2026], '去重');

  // 查询入参：空筛选不带 period_year/status（带了后端会当「等于空串」查 0 行）
  assert.deepEqual(buildPeriodQuery(emptyPeriodFilter(), 1, 20), { page: 1, page_size: 20 });
  assert.deepEqual(buildPeriodQuery({ period_year: 2026, status: 'OPEN' }, 2, 20), {
    period_year: 2026,
    status: 'OPEN',
    page: 2,
    page_size: 20,
  });
  assert.equal(isPeriodFiltered(emptyPeriodFilter()), false);
  assert.equal(isPeriodFiltered({ period_year: 2026, status: '' }), true);
});
