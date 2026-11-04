/**
 * 即时库存页纯逻辑单测（P4-D）—— 三个 Tab 的类型映射 / 数量口径 / 查询参数。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * 为什么测 `.ts` 不测 `.vue`：用例跑在没有 DOM 的 node 里，而这几处错一个就会让仓管看错账：
 *
 * 1. **方向**：`txn_type` 只有 `IN`/`OUT`（`dto::TXN_TYPES`），颜色错了进出就反了；
 * 2. **业务类型**：`PURCHASE_IN` 显示成「采购入库」而不是英文码（或更糟：显示成调拨）；
 * 3. **可用数量**：接口**不给**这个字段，是前端 `qty − locked_qty`；
 * 4. **查询参数**：三个 Tab 各有自己的筛选字段，串了就是「Tab2 带上了 Tab3 的 doc_no」这类
 *    查不出东西还看不出原因的 bug（后端对未知参数静默忽略，空列表不报错）。
 *
 * 断言口径照后端抄：
 * - `txn_type` 白名单 = `dto.rs::TXN_TYPES`（`IN`/`OUT`）；
 * - `biz_type` 常量 = `stockin_model::BIZ_TYPE_PURCHASE_IN` / `transfer_model::BIZ_TYPE_TRANSFER_*` /
 *   `serial_model::BIZ_TYPE_SERIAL_*`；
 * - 数量列名 = `qty`（**不是** `quantity`，批次 1 踩过）；
 * - 日期区间 = `InventoryTxnFilter::validate`（`date_from > date_to` → 400）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  INVENTORY_PAGE_SIZE,
  INVENTORY_TABS,
  TXN_TYPE_OPTIONS,
  auditDiffText,
  auditDriftKind,
  auditDriftLabel,
  auditDriftTheme,
  auditEmptyText,
  auditFixableLabel,
  auditSummaryText,
  availableQty,
  bizTypeLabel,
  buildLocationQuery,
  buildStockQuery,
  buildTxnQuery,
  clampPage,
  docTypeLabel,
  emptyLocationFilter,
  emptyStockFilter,
  emptyTxnFilter,
  isAuditQuery,
  isLocationFiltered,
  isOverLocked,
  isStockFiltered,
  isTxnFiltered,
  isZeroStockRow,
  signedQtyText,
  snapshotConsistent,
  tabFromMeta,
  tabFromPath,
  txnRangeError,
  txnTypeLabel,
  txnTypeTheme,
} from '@/views/business/erp/inventory/inventory-logic';

// ==================== ① IN/OUT 类型映射 + 颜色 ====================

test('① 出入方向：IN→入库(绿) / OUT→出库(橙)，未知值不猜颜色', () => {
  // 白名单就两个值（dto::TXN_TYPES），文案与 TDesign 主题逐条钉死
  assert.equal(txnTypeLabel('IN'), '入库');
  assert.equal(txnTypeLabel('OUT'), '出库');
  assert.equal(txnTypeTheme('IN'), 'success');
  assert.equal(txnTypeTheme('OUT'), 'warning');

  // 未知 / 空值：原样（或占位）显示 + 默认灰，绝不硬塞「入库」
  assert.equal(txnTypeLabel('ADJUST'), 'ADJUST');
  assert.equal(txnTypeTheme('ADJUST'), 'default');
  assert.equal(txnTypeLabel(null), '—');
  assert.equal(txnTypeLabel(undefined), '—');

  // 下拉只给这两个（不是 4 个状态，也不是带空值的「全部」——全部由 clearable 表达）
  assert.deepEqual(
    TXN_TYPE_OPTIONS.map((item) => item.value),
    ['IN', 'OUT'],
  );
});

test('①附 流水数量签名：IN 加 +、OUT 用 U+2212 减号，库里 qty 恒正', () => {
  // 库里 qty 恒正（ck_biz_inv_txn_qty: qty > 0），符号是前端按方向加的
  assert.equal(signedQtyText({ txn_type: 'IN', qty: '200.0000' }), '+200');
  assert.equal(signedQtyText({ txn_type: 'OUT', qty: '50.0000' }), '−50');
  // 未知方向不加符号（宁可中性，也不要给错方向）
  assert.equal(signedQtyText({ txn_type: 'ADJUST', qty: 7 }), '7');
});

// ==================== ② biz_type 中文映射 ====================

test('② 业务类型中文：PURCHASE_IN→采购入库，调拨区分进出，未知码原样返回', () => {
  // 取值与文案的唯一来源 = doc-kinds.ts 的 TXN_BIZ_TYPE_LABELS（= 后端 TxnBizType）
  assert.equal(bizTypeLabel('PURCHASE_IN'), '采购入库');
  assert.equal(bizTypeLabel('TRANSFER_IN'), '调拨入库');
  assert.equal(bizTypeLabel('TRANSFER_OUT'), '调拨出库');
  assert.equal(bizTypeLabel('SERIAL_IN'), '序列号入库');
  assert.equal(bizTypeLabel('SERIAL_OUT'), '序列号出库');
  assert.equal(bizTypeLabel('SERIAL_SCRAP'), '序列号报废');
  // 卡面点名的销售出库
  assert.equal(bizTypeLabel('SALE_OUT'), '销售出库');
  // P2-5：真实存在的后端值（旧前端词表里没有，靠英文码兜底）
  assert.equal(bizTypeLabel('PURCHASE_RETURN'), '采购退货');
  assert.equal(bizTypeLabel('PRODUCTION_IN'), '生产入库');
  assert.equal(bizTypeLabel('PURCHASE_IN_CANCEL'), '采购入库红冲');

  // 进/出不能同文案（串了就等于把出库看成入库）
  assert.notEqual(bizTypeLabel('TRANSFER_IN'), bizTypeLabel('TRANSFER_OUT'));

  // P2-5 消灭「第 8 套词表」：旧前端自造的 key 后端根本没有 →
  // 现在一律原样返回英文码（以前会给一个永远不会被命中的「中文」，
  // 让人误以为系统支持这些业务类型）
  for (const invented of [
    'PURCHASE_RETURN_OUT',
    'SALE_RETURN_IN',
    'PRODUCE_IN',
    'MATERIAL_ISSUE_OUT',
    'INIT_IN',
  ]) {
    assert.equal(bizTypeLabel(invented), invented, `自造 key ${invented} 不该有中文`);
  }

  // 未知码原样返回：后端加新类型时，显示英文码也比显示错的中文强
  assert.equal(bizTypeLabel('FUTURE_BIZ'), 'FUTURE_BIZ');
  assert.equal(bizTypeLabel(null), '—');
  assert.equal(bizTypeLabel(''), '—');

  // 单据类型映射（列表副行用）—— 文案与业务对象词表统一（旧值是「调拨单」）
  assert.equal(docTypeLabel('STOCK_IN'), '收货入库单');
  assert.equal(docTypeLabel('STOCK_TRANSFER'), '库存调拨单');
  assert.equal(docTypeLabel('MATERIAL_SERIAL'), '序列号');
  assert.equal(docTypeLabel('UNKNOWN_DOC'), 'UNKNOWN_DOC');
});

// ==================== ③ 可用数量 = qty − locked_qty ====================

test('③ 可用数量 = qty − locked_qty（后端 Decimal 出的是字符串，不能直接减）', () => {
  // 主用例：分仓 200 件、锁定 50 → 可用 150
  assert.equal(availableQty({ qty: '200.0000', locked_qty: '50.0000' }), 150);
  assert.equal(availableQty({ qty: 200, locked_qty: 0 }), 200);
  // 全锁：可用 0（不是 200）
  assert.equal(availableQty({ qty: '80.0000', locked_qty: '80.0000' }), 0);
  // 超锁：负数要能被识别出来（页面标红，不装作 0）
  assert.equal(availableQty({ qty: '10.0000', locked_qty: '12.0000' }), -2);
  assert.equal(isOverLocked({ qty: '10.0000', locked_qty: '12.0000' }), true);
  assert.equal(isOverLocked({ qty: '10.0000', locked_qty: '10.0000' }), false);
  // 字符串减法若不过 toNumber 会得 NaN —— 这条钉住它没退化成 NaN
  assert.ok(Number.isFinite(availableQty({ qty: '0.0001', locked_qty: '0.0002' })));

  // 「数量 0 且金额 0」才算零行（只有金额非 0 的历史痕迹要留着）
  assert.equal(isZeroStockRow({ qty: '0.0000', amount: '0.0000' }), true);
  assert.equal(isZeroStockRow({ qty: '0.0000', amount: '12.3400' }), false);
  assert.equal(isZeroStockRow({ qty: '1.0000', amount: '0.0000' }), false);

  // 流水快照自洽：before ± qty = after
  assert.equal(
    snapshotConsistent({ txn_type: 'IN', qty: '200', before_qty: '0', after_qty: '200' }),
    true,
  );
  assert.equal(
    snapshotConsistent({ txn_type: 'OUT', qty: '50', before_qty: '200', after_qty: '150' }),
    true,
  );
  // 方向写错就一定不自洽（OUT 被当成 IN 会得 250 ≠ 150）
  assert.equal(
    snapshotConsistent({ txn_type: 'IN', qty: '50', before_qty: '200', after_qty: '150' }),
    false,
  );
});

// ==================== ④ 三个 Tab 的请求参数 ====================

test('④ 三个 Tab 各带各的筛选：字段不串、空串不发、only_nonzero 只在开时发', () => {
  // ---- Tab1 即时库存：material_id / warehouse_id / only_nonzero ----
  assert.deepEqual(buildStockQuery(emptyStockFilter(), 1), { page: 1, page_size: INVENTORY_PAGE_SIZE });
  assert.deepEqual(buildStockQuery({ material_id: 'm1', warehouse_id: 'w1', only_nonzero: true }, 2), {
    page: 2,
    page_size: INVENTORY_PAGE_SIZE,
    material_id: 'm1',
    warehouse_id: 'w1',
    only_nonzero: true,
  });
  // 「隐藏零库存」关着就别发 false：后端 serde 会把 `only_nonzero=` 当空串解析失败
  assert.equal('only_nonzero' in buildStockQuery(emptyStockFilter(), 1), false);

  // ---- Tab2 库位库存：多一个 location_id，且**不能**夹带 only_nonzero / doc_no ----
  const locationQuery = buildLocationQuery(
    { material_id: 'm1', warehouse_id: 'w1', location_id: 'loc1' },
    3,
  );
  assert.deepEqual(locationQuery, {
    page: 3,
    page_size: INVENTORY_PAGE_SIZE,
    material_id: 'm1',
    warehouse_id: 'w1',
    location_id: 'loc1',
  });
  assert.equal('only_nonzero' in locationQuery, false);
  assert.equal('doc_no' in locationQuery, false);

  // ---- Tab3 库存流水：txn_type / doc_no（去空白）/ date_from / date_to ----
  assert.deepEqual(buildTxnQuery(emptyTxnFilter(), 1), { page: 1, page_size: INVENTORY_PAGE_SIZE });
  const txnQuery = buildTxnQuery(
    {
      material_id: 'm1',
      warehouse_id: 'w1',
      txn_type: 'IN',
      doc_no: '  IN-20261031-000001  ',
      date_from: '2026-10-01',
      date_to: '2026-10-31',
    },
    4,
  );
  assert.deepEqual(txnQuery, {
    page: 4,
    page_size: INVENTORY_PAGE_SIZE,
    material_id: 'm1',
    warehouse_id: 'w1',
    txn_type: 'IN',
    doc_no: 'IN-20261031-000001',
    date_from: '2026-10-01',
    date_to: '2026-10-31',
  });
  // 流水 Tab 没有 only_nonzero / location_id（那是另外两张表的筛选）
  assert.equal('only_nonzero' in txnQuery, false);
  assert.equal('location_id' in txnQuery, false);

  // 空白单据号 = 不筛选（否则后端 doc_no = '' 精确匹配，一条都查不出）
  assert.equal('doc_no' in buildTxnQuery({ ...emptyTxnFilter(), doc_no: '   ' }, 1), false);
});

test('④附 Tab 与菜单路径互认：三条 sys_menu 路径都落到本页，不缺 Tab、不 404', () => {
  // 三条路径 = V0141 种的三条菜单（…0946 / …0947 / …0948）
  assert.equal(tabFromPath('/business/erp/inventory'), 'stock');
  assert.equal(tabFromPath('/business/erp/inventory-location'), 'location');
  assert.equal(tabFromPath('/business/erp/inventory-txn'), 'txn');

  // route.meta.tab 优先（三条独立 route 各自写死目标 Tab）
  assert.equal(tabFromMeta('txn', '/business/erp/inventory'), 'txn');
  assert.equal(tabFromMeta('location', null), 'location');
  // meta 缺失 / 脏值时按路径兜底
  assert.equal(tabFromMeta(undefined, '/business/erp/inventory-txn'), 'txn');
  assert.equal(tabFromMeta('bogus', '/business/erp/inventory-location'), 'location');

  // 末尾斜杠 / query / hash 不能把 Tab 判错（刷新带参数时就是这个形态）
  assert.equal(tabFromPath('/business/erp/inventory-txn/'), 'txn');
  assert.equal(tabFromPath('/business/erp/inventory-location/?page=2'), 'location');

  // 未知路径回落 Tab1（宁可多显示一张报表，也不要白屏）
  assert.equal(tabFromPath('/business/erp/inventory-xxx'), 'stock');
  assert.equal(tabFromPath(null), 'stock');

  // Tab 定义：3 个、key 唯一、路径与 tabFromPath 互为逆运算（切 Tab 改 URL 才不错位）
  assert.equal(INVENTORY_TABS.length, 3);
  assert.deepEqual(
    INVENTORY_TABS.map((item) => item.key),
    ['stock', 'location', 'txn'],
  );
  for (const tab of INVENTORY_TABS) {
    assert.equal(tabFromPath(tab.path), tab.key);
    assert.match(tab.permission, /^business:erp:inventory(-location|-txn)?:list$/);
  }
});

// ==================== ⑤ 筛选态 / 区间校验 / 翻页夹取 ====================

test('⑤ 筛选态判定与日期区间校验（空态文案、400 前置拦截都靠它）', () => {
  // 没任何筛选 → 空态说「还没有数据」而不是「没匹配」
  assert.equal(isStockFiltered(emptyStockFilter()), false);
  assert.equal(isStockFiltered({ material_id: '', warehouse_id: '', only_nonzero: true }), true);
  assert.equal(isLocationFiltered(emptyLocationFilter()), false);
  assert.equal(isLocationFiltered({ material_id: '', warehouse_id: '', location_id: 'loc1' }), true);
  assert.equal(isTxnFiltered(emptyTxnFilter()), false);
  assert.equal(isTxnFiltered({ ...emptyTxnFilter(), doc_no: '  ' }), false); // 纯空白不算筛选
  assert.equal(isTxnFiltered({ ...emptyTxnFilter(), date_to: '2026-10-31' }), true);

  // 起 > 止：本地拦下来（后端 InventoryTxnFilter::validate 会 400），不发请求
  assert.equal(txnRangeError({ date_from: '2026-10-31', date_to: '2026-10-01' }), '起始日不能晚于结束日');
  // 同一天合法（后端右开区间按「< 次日零点」算，当天流水不会漏）
  assert.equal(txnRangeError({ date_from: '2026-10-31', date_to: '2026-10-31' }), '');
  assert.equal(txnRangeError({ date_from: '', date_to: '2026-10-31' }), '');

  // 翻页夹取：后端回越界页时夹回最后一页，至少 1
  assert.equal(clampPage(5, 41), 3); // 41 条 / 20 = 3 页
  assert.equal(clampPage(0, 0), 1);
  assert.equal(clampPage(2, 41), 2);
  assert.equal(clampPage(Number.NaN, 41), 1);
});

// ==================== ④ 巡检模式（`?audit=1`，P2-2B 缺陷①） ====================

test('④ 巡检模式：只认 1 / true（大小写、空白、数组首值），拼错的参数退回普通库存', () => {
  // 后端点「去看看」给的就是 ?audit=1（period.rs 的 Blocker::with_url）
  assert.equal(isAuditQuery('1'), true);
  assert.equal(isAuditQuery('true'), true);
  assert.equal(isAuditQuery(' TRUE '), true);
  // 数组（?audit=1&audit=0）取第一个
  assert.equal(isAuditQuery(['1', '0']), true);
  assert.equal(isAuditQuery(['0', '1']), false);
  // 数字 / 布尔（测试或程序内调用）
  assert.equal(isAuditQuery(1), true);
  assert.equal(isAuditQuery(true), true);

  // 这些**都不算**巡检：宁可退回普通库存页，也别把仓管扔进「只列漂移」视图
  assert.equal(isAuditQuery('0'), false);
  assert.equal(isAuditQuery('false'), false);
  assert.equal(isAuditQuery(''), false);
  assert.equal(isAuditQuery(undefined), false);
  assert.equal(isAuditQuery(null), false);
  assert.equal(isAuditQuery(0), false);
  assert.equal(isAuditQuery('yes'), false);
  assert.equal(isAuditQuery([]), false);
  assert.equal(isAuditQuery({}), false);
});

test('④ 漂移方向：diff 正=账多 / 负=账少 / 零=一致，文案·配色·符号三处一致', () => {
  // diff = 账面 − 实际（后端口径）
  assert.equal(auditDriftKind('2.0000'), 'over');
  assert.equal(auditDriftKind(-2), 'under');
  assert.equal(auditDriftKind('0.0000'), 'even');
  assert.equal(auditDriftKind(null), 'even');

  assert.equal(auditDriftLabel(2), '账多');
  assert.equal(auditDriftLabel('-1.5'), '账少');
  assert.equal(auditDriftLabel(0), '一致');

  assert.equal(auditDriftTheme(2), 'danger'); // 账多 = 红（真正的账实不符）
  assert.equal(auditDriftTheme(-2), 'warning'); // 账少 = 橙
  assert.equal(auditDriftTheme(0), 'default');

  // 符号用 U+2212（与流水页 signedQtyText 同风格），去尾零
  assert.equal(auditDiffText('2.0000'), '+2');
  assert.equal(auditDiffText('-2.5000'), '−2.5');
  assert.equal(auditDiffText('0.0000'), '0');
  assert.equal(auditDiffText(null), '0');
});

test('④ 可修正标记与结论文案：把 only-read 口径写进页面，别让用户以为点了就会改账', () => {
  assert.equal(auditFixableLabel(true), '可修正');
  // 在库序列号没有仓库归属 → 写不进 biz_inventory，得说清原因
  assert.equal(auditFixableLabel(false), '缺仓库归属');

  const summary = auditSummaryText({ dry_run: true, checked: 12, mismatched: 3, unfixable: 1 });
  assert.match(summary, /12 个（物料 × 仓库）组合/);
  assert.match(summary, /发现 3 个漂移/);
  assert.match(summary, /其中 1 个缺仓库归属、无法自动修正/);
  assert.match(summary, /dry_run=true，不改任何库存/);

  // 没有 unfixable 时不该多出一句「其中 0 个…」
  const clean = auditSummaryText({ dry_run: true, checked: 5, mismatched: 0, unfixable: 0 });
  assert.doesNotMatch(clean, /其中/);
  assert.match(clean, /发现 0 个漂移/);
});

test('④ 空态文案：check=0（没有序列号物料）与「巡检干净」是两件事，别都说成没数据', () => {
  assert.match(auditEmptyText({ checked: 0, mismatched: 0 }), /排查对象|没有可巡检的对象/);
  assert.match(auditEmptyText({ checked: 8, mismatched: 0 }), /8 个组合/);
  assert.match(auditEmptyText({ checked: 8, mismatched: 0 }), /全部一致/);
  // 后端口径异常（有漂移但明细空）也要有话说，不能显示「没有漂移」
  assert.match(auditEmptyText({ checked: 8, mismatched: 2 }), /明细为空/);
});
