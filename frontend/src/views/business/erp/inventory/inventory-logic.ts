/**
 * 即时库存页纯逻辑（P4-D）—— 三个 Tab 的筛选/查询参数、IN/OUT 与业务类型映射、
 * 可用数量与快照自洽判定。
 *
 * # 三个 Tab 的主表与接口（一张表一个 Tab，别混）
 *
 * ```text
 * Tab1 即时库存   biz_inventory           GET /erp/inventory/page           qty 含锁定，可用 = qty − locked_qty
 * Tab2 库位库存   biz_inventory_location  GET /erp/inventory-location/page  库位维度，location_id 指 biz_location.id
 * Tab3 库存流水   biz_inventory_txn       GET /erp/inventory-txn/page       qty 恒正，方向看 txn_type
 * ```
 *
 * # 为什么逻辑全搬出 `.vue`
 *
 * 用例跑在没有 DOM 的 node 里（`tsx --test`），而这几处判定错一个就会让仓管看错账：
 * 「IN 是绿还是橙」「可用数量是不是减了锁定」「流水数量签名」「翻页时筛选参数有没有
 * 带错 Tab 的字段」。所以 `.vue` 只渲染，判定与拼参数都在这里，被
 * `tests/unit/erp-inventory.test.ts` 钉住。
 *
 * # 字段名
 *
 * 数量列名是 **`qty`**（不是 `quantity`）—— 抄自 `dto.rs::InventoryFilter/InventoryRes`。
 */
import type { InventoryRow, InventoryTxnType, InventoryTxnRow } from '@/api/business/erp';
import { txnBizTypeLabel, txnDocTypeLabel } from '@/api/business/erp/doc-kinds';
import { toCents, toNumber } from '../shared/money';

/** 每页条数（与其它 ERP 列表页同口径 20） */
export const INVENTORY_PAGE_SIZE = 20;

// ==================== Tab 定义 ====================

/** 三个 Tab 的 key */
export type InventoryTabKey = 'stock' | 'location' | 'txn';

/**
 * Tab 与 `sys_menu` 路径的对应（V0141 种了三条菜单：…0946/0947/0948）。
 *
 * 一个页面三个 Tab，但菜单是三行 —— 所以三条路径都指向本页，按路径决定初始 Tab，
 * 否则从侧边栏点「库位库存」「库存流水」会各自 404（就是本卡要修的那个毛病）。
 */
export const INVENTORY_TABS: ReadonlyArray<{
  key: InventoryTabKey;
  label: string;
  path: string;
  permission: string;
}> = [
  { key: 'stock', label: '即时库存', path: '/business/erp/inventory', permission: 'business:erp:inventory:list' },
  {
    key: 'location',
    label: '库位库存',
    path: '/business/erp/inventory-location',
    permission: 'business:erp:inventory-location:list',
  },
  {
    key: 'txn',
    label: '库存流水',
    path: '/business/erp/inventory-txn',
    permission: 'business:erp:inventory-txn:list',
  },
];

/**
 * 路由路径 → 初始 Tab。
 *
 * 认 `inventory-location` / `inventory-txn` 两种后缀（菜单种子与卡面验收路径同口径），
 * 其它一律落到 Tab1（`/business/erp/inventory` 本体）。末尾斜杠、query、hash 由调用方
 * 先剥掉；这里也兜一层，避免 `/business/erp/inventory-txn/` 被误判成 Tab1。
 */
export function tabFromPath(path: string | null | undefined): InventoryTabKey {
  const clean = (path ?? '').split(/[?#]/)[0]?.replace(/\/+$/, '') ?? '';
  if (clean.endsWith('/inventory-location')) {
    return 'location';
  }
  if (clean.endsWith('/inventory-txn')) {
    return 'txn';
  }
  return 'stock';
}

/**
 * 初始 Tab 判定：**优先 `route.meta.tab`**（路由表里写死，最准），
 * 其次按路径后缀（`tabFromPath`），都没有就 Tab1。
 *
 * 为什么两条都用：三条菜单 → 三条独立 route（`meta.tab` = 目标 Tab），
 * 但用户也可能手输带后缀的 URL 或从旧书签进来 —— 路径兜底保证不漏。
 */
export function tabFromMeta(metaTab: unknown, path: string | null | undefined): InventoryTabKey {
  if (metaTab === 'stock' || metaTab === 'location' || metaTab === 'txn') {
    return metaTab;
  }
  return tabFromPath(path);
}

// ==================== ① 数量口径 ====================

/**
 * 可用数量 = `qty − locked_qty`。
 *
 * 后端 `InventoryRes` **不吐**「可用」，这是前端算的派生值（卡面 §三 Tab1 明写）。
 * `qty/locked_qty` 出参可能是字符串（`Decimal` → `"200.0000"`），必须过 `toNumber`，
 * 否则 `"200.0000" - 0` 变 200（侥幸对）而 `"200.0000" - "50.0000"` 变 NaN。
 *
 * 浮点减法后**不**四舍五入（`NUMERIC(18,4)` 最多 4 位小数，`toFixed` 会引入二次舍入），
 * 保留调用方格式化。
 *
 * ⚠️ **P2-2 ③ 之后**：`locked_qty` 是预留列（无占用流程，恒为 0），所以本函数当前恒等于
 * `qty`，页面上「锁定」列已撤掉。公式保留是因为「销售订单占用 / 生产预占」落地后它就是
 * 真实口径 —— 那时只改数据，不改 UI 公式（详见 `docs/ERP-摆设字段口径.md`）。
 */
export function availableQty(row: Pick<InventoryRow, 'qty' | 'locked_qty'>): number {
  return toNumber(row.qty) - toNumber(row.locked_qty);
}

/**
 * 「数量为 0 且金额为 0」的行（卡面 §三：可默认隐藏，加开关）。
 *
 * 判定用**整数分 + 数量**双零，`-0.0001` 这种抹平的量不进这个分支
 * ——「有痕迹」的行要留着（另一条要求是「库存为 0 的行也显示」）。
 */
export function isZeroStockRow(row: Pick<InventoryRow, 'qty' | 'amount'>): boolean {
  return toNumber(row.qty) === 0 && toCents(row.amount) === 0;
}

/** 可用数量 < 0（超卖/锁定写多了）—— 页面上标红，别让负数混在正常行里 */
export function isOverLocked(row: Pick<InventoryRow, 'qty' | 'locked_qty'>): boolean {
  return availableQty(row) < 0;
}

// ==================== ② 流水类型映射 ====================

/** 出入方向中文（`dto::TXN_TYPES` 只有 IN/OUT 两个值） */
export function txnTypeLabel(txnType: string | null | undefined): string {
  if (txnType === 'IN') {
    return '入库';
  }
  if (txnType === 'OUT') {
    return '出库';
  }
  return txnType ? String(txnType) : '—';
}

/**
 * 出入方向的 tag 配色（只用 TDesign 主题名，具体色值由 `--td-*` 变量决定）。
 *
 * `IN` = success（绿，进货）/ `OUT` = warning（橙，出货）；未知值 = default，不猜颜色。
 */
export function txnTypeTheme(txnType: string | null | undefined): 'success' | 'warning' | 'default' {
  if (txnType === 'IN') {
    return 'success';
  }
  if (txnType === 'OUT') {
    return 'warning';
  }
  return 'default';
}

/**
 * 业务类型 → 中文。
 *
 * P2-5 词表收敛：不再自己维护一份 key 表 —— 旧版这里抄了 13 个 key，
 * 其中 `PURCHASE_RETURN_OUT` / `SALE_RETURN_IN` / `PRODUCE_IN` /
 * `MATERIAL_ISSUE_OUT` / `INIT_IN` **后端根本没有这些值**（第 8 套词表的真身，
 * 那几行标签永远不会被命中，反而让人以为系统支持这些业务类型）。
 * 现在标签唯一来源是 `doc-kinds.ts` 的 `TXN_BIZ_TYPE_LABELS`（= 后端 `TxnBizType`）。
 * 未知码原样返回，不猜。
 */
export function bizTypeLabel(bizType: string | null | undefined): string {
  return txnBizTypeLabel(bizType);
}

/** 单据类型 → 中文（`STOCK_IN` / `STOCK_TRANSFER` / `MATERIAL_SERIAL` …，取自 `TxnDocType`）。 */
export function docTypeLabel(docType: string | null | undefined): string {
  return txnDocTypeLabel(docType);
}

/**
 * 流水数量带符号展示：`IN` → `+200`，`OUT` → `−200`（U+2212 减号，与网格对齐更好看）。
 *
 * ⚠️ 库里 `qty` 恒正（`ck_biz_inv_txn_qty: qty > 0`），符号是**前端**加的；
 * 别把它再写回接口，也别用 `qty < 0` 判断出库。
 */
export function signedQtyText(row: Pick<InventoryTxnRow, 'txn_type' | 'qty'>): string {
  const abs = Math.abs(toNumber(row.qty));
  const body = Number.isInteger(abs) ? String(abs) : String(abs);
  if (row.txn_type === 'OUT') {
    return `−${body}`;
  }
  if (row.txn_type === 'IN') {
    return `+${body}`;
  }
  return body;
}

/**
 * 快照自洽：`before_qty ± qty == after_qty`（IN 加、OUT 减）。
 *
 * 详情抽屉里对账用。后端在一个事务内算好六列，正常必然自洽；不自洽说明流水与
 * 库存被绕过接口改过，界面上要能看见（而不是替它圆场）。
 */
export function snapshotConsistent(
  row: Pick<InventoryTxnRow, 'txn_type' | 'qty' | 'before_qty' | 'after_qty'>,
): boolean {
  const qty = Math.abs(toNumber(row.qty));
  const expected = toNumber(row.before_qty) + (row.txn_type === 'OUT' ? -qty : qty);
  // 数量列最多 4 位小数，用分的口径比会丢精度 —— 这里按 1e-6 容差比
  return Math.abs(expected - toNumber(row.after_qty)) < 1e-6;
}

// ==================== ③ 筛选状态与查询参数 ====================

/** Tab1 筛选（后端 `InventoryFilter`：物料 / 仓库 / 只看有货） */
export interface StockFilterState {
  /** 物料 id（空 = 全部） */
  material_id: string;
  /** 仓库 id（空 = 全部） */
  warehouse_id: string;
  /**
   * 只看有货（服务端 `only_nonzero` → `AND qty <> 0`）。
   *
   * 默认 `false`：卡面要求「库存为 0 的行也显示（便于看到曾经有过）」，但给个开关
   * 一键收掉零行。开关走服务端参数而不是前端滤当前页，翻页总数才不会错。
   */
  only_nonzero: boolean;
}

/** Tab1 空筛选 */
export function emptyStockFilter(): StockFilterState {
  return { material_id: '', warehouse_id: '', only_nonzero: false };
}

/** Tab1 是否处于筛选态（空态文案分「没数据」/「没匹配」） */
export function isStockFiltered(filter: StockFilterState): boolean {
  return Boolean(filter.material_id || filter.warehouse_id || filter.only_nonzero);
}

/** Tab1 查询参数（`material_id` / `warehouse_id` 空串不发，`only_nonzero` 只在开时发） */
export function buildStockQuery(
  filter: StockFilterState,
  page: number,
  pageSize: number = INVENTORY_PAGE_SIZE,
): Record<string, unknown> {
  const query: Record<string, unknown> = { page, page_size: pageSize };
  if (filter.material_id) {
    query.material_id = filter.material_id;
  }
  if (filter.warehouse_id) {
    query.warehouse_id = filter.warehouse_id;
  }
  if (filter.only_nonzero) {
    query.only_nonzero = true;
  }
  return query;
}

/** Tab2 筛选（后端 `InventoryLocationFilter`：物料 / 仓库 / 库位） */
export interface LocationFilterState {
  /** 物料 id */
  material_id: string;
  /** 仓库 id */
  warehouse_id: string;
  /** 库位 id（`biz_location.id`） */
  location_id: string;
}

/** Tab2 空筛选 */
export function emptyLocationFilter(): LocationFilterState {
  return { material_id: '', warehouse_id: '', location_id: '' };
}

/** Tab2 是否处于筛选态 */
export function isLocationFiltered(filter: LocationFilterState): boolean {
  return Boolean(filter.material_id || filter.warehouse_id || filter.location_id);
}

/** Tab2 查询参数 */
export function buildLocationQuery(
  filter: LocationFilterState,
  page: number,
  pageSize: number = INVENTORY_PAGE_SIZE,
): Record<string, unknown> {
  const query: Record<string, unknown> = { page, page_size: pageSize };
  if (filter.material_id) {
    query.material_id = filter.material_id;
  }
  if (filter.warehouse_id) {
    query.warehouse_id = filter.warehouse_id;
  }
  if (filter.location_id) {
    query.location_id = filter.location_id;
  }
  return query;
}

/** Tab3 筛选（后端 `InventoryTxnFilter`：物料 / 仓库 / 方向 / 单据号 / 日期区间） */
export interface TxnFilterState {
  /** 物料 id */
  material_id: string;
  /** 仓库 id */
  warehouse_id: string;
  /** 方向（空 = 全部） */
  txn_type: InventoryTxnType | '';
  /** 单据号（后端**精确**匹配 `doc_no = ?`） */
  doc_no: string;
  /** 起始日 `YYYY-MM-DD`（含） */
  date_from: string;
  /** 结束日 `YYYY-MM-DD`（含，后端转 `< 次日零点`） */
  date_to: string;
}

/** Tab3 空筛选 */
export function emptyTxnFilter(): TxnFilterState {
  return { material_id: '', warehouse_id: '', txn_type: '', doc_no: '', date_from: '', date_to: '' };
}

/** Tab3 是否处于筛选态 */
export function isTxnFiltered(filter: TxnFilterState): boolean {
  return Boolean(
    filter.material_id ||
      filter.warehouse_id ||
      filter.txn_type ||
      filter.doc_no.trim() ||
      filter.date_from ||
      filter.date_to,
  );
}

/**
 * 日期区间校验（后端 `InventoryTxnFilter::validate` 同口径：`date_from > date_to` → 400）。
 * 返回提示文案，空串表示合法。
 */
export function txnRangeError(filter: Pick<TxnFilterState, 'date_from' | 'date_to'>): string {
  const from = filter.date_from.trim();
  const to = filter.date_to.trim();
  if (from && to && from > to) {
    return '起始日不能晚于结束日';
  }
  return '';
}

/** 方向筛选下拉（`IN` / `OUT`，值即后端 `txn_type`） */
export const TXN_TYPE_OPTIONS: ReadonlyArray<{ label: string; value: InventoryTxnType }> = [
  { label: '入库', value: 'IN' },
  { label: '出库', value: 'OUT' },
];

/** Tab3 查询参数（`doc_no` 去首尾空白后为空则不发明细） */
export function buildTxnQuery(
  filter: TxnFilterState,
  page: number,
  pageSize: number = INVENTORY_PAGE_SIZE,
): Record<string, unknown> {
  const query: Record<string, unknown> = { page, page_size: pageSize };
  if (filter.material_id) {
    query.material_id = filter.material_id;
  }
  if (filter.warehouse_id) {
    query.warehouse_id = filter.warehouse_id;
  }
  if (filter.txn_type) {
    query.txn_type = filter.txn_type;
  }
  const docNo = filter.doc_no.trim();
  if (docNo) {
    query.doc_no = docNo;
  }
  const from = filter.date_from.trim();
  if (from) {
    query.date_from = from;
  }
  const to = filter.date_to.trim();
  if (to) {
    query.date_to = to;
  }
  return query;
}

/**
 * 页面当前页的「规范化」：后端可能回一个越界页（删数据后停在最后一页之前）。
 * 与总账页同口径：夹到 `[1, ceil(total/pageSize)]`，至少 1。
 */
export function clampPage(page: number, total: number, pageSize: number = INVENTORY_PAGE_SIZE): number {
  const maxPage = Math.max(1, Math.ceil(total / Math.max(1, pageSize)));
  return Math.min(Math.max(1, Math.floor(page) || 1), maxPage);
}

// ==================== ④ 巡检模式（`?audit=1`） ====================

/**
 * `?audit=1` 的判定（P2-2B 缺陷①）。
 *
 * 后端 `period.rs::global_health_warnings` / `period_items.rs` / `year_end.rs` 给巡检提醒挂的
 * 落点都是 `/business/erp/inventory?audit=1`（`Blocker::with_url`），前端此前**没读这个参数**，
 * 点「去看看」与普通即时库存完全一样。这里只认 `1` / `true`（大小写不敏感、容忍首尾空白），
 * 其余值（`0` / `false` / 空串 / `undefined`，或 `?audit` 这种空值）一律**不算**巡检模式 ——
 * 宁可退回普通库存页，也不要因为一个拼错的参数把仓管扔进「只列漂移」的视图里。
 *
 * `vue-router` 的 query 值可能是数组（`?audit=1&audit=0`），取第一个。
 */
export function isAuditQuery(raw: unknown): boolean {
  const value = Array.isArray(raw) ? raw[0] : raw;
  if (typeof value === 'boolean') {
    return value;
  }
  if (typeof value === 'number') {
    return value === 1;
  }
  if (typeof value !== 'string') {
    return false;
  }
  const lowered = value.trim().toLowerCase();
  return lowered === '1' || lowered === 'true';
}

/** 漂移方向：`over` = 账多（账面 > 在库），`under` = 账少，`even` = 一致 */
export type AuditDriftKind = 'over' | 'under' | 'even';

/** 漂移方向判定（`diff = 账面 − 实际`，与后端 `AuditItem.diff` 同口径） */
export function auditDriftKind(diff: number | string | null | undefined): AuditDriftKind {
  const value = toNumber(diff);
  if (value > 0) {
    return 'over';
  }
  if (value < 0) {
    return 'under';
  }
  return 'even';
}

/** 漂移方向中文（表格「状态」列） */
export function auditDriftLabel(diff: number | string | null | undefined): string {
  const kind = auditDriftKind(diff);
  if (kind === 'over') {
    return '账多';
  }
  if (kind === 'under') {
    return '账少';
  }
  return '一致';
}

/** 漂移方向的 tag 配色（账多 = 红，账少 = 橙，一致 = 默认） */
export function auditDriftTheme(diff: number | string | null | undefined): 'danger' | 'warning' | 'default' {
  const kind = auditDriftKind(diff);
  if (kind === 'over') {
    return 'danger';
  }
  if (kind === 'under') {
    return 'warning';
  }
  return 'default';
}

/**
 * 差异带符号展示：账多 `+2`、账少 `−2`（U+2212，与流水页 `signedQtyText` 同一风格）。
 * 数量最多 4 位小数，去尾零；`even` 显示 `0`。
 */
export function auditDiffText(diff: number | string | null | undefined): string {
  const value = toNumber(diff);
  const body = String(Number(Math.abs(value).toFixed(4)));
  if (value > 0) {
    return `+${body}`;
  }
  if (value < 0) {
    return `−${body}`;
  }
  return '0';
}

/** 能否被「修正」（`dry_run=false`）修好 —— 缺仓库归属的行修不了，页面上要说清原因 */
export function auditFixableLabel(fixable: boolean): string {
  return fixable ? '可修正' : '缺仓库归属';
}

/**
 * 巡检结论一句话（页面顶部提示条正文）。
 *
 * 只读口径写进文案：`dry_run` 由后端回包给出，前端不假设（万一将来页面加了修正入口，
 * 这句话会跟着后端口径变，不会骗人）。
 */
export function auditSummaryText(report: {
  dry_run: boolean;
  checked: number;
  mismatched: number;
  unfixable: number;
}): string {
  const mode = report.dry_run ? '只读巡检（dry_run=true，不改任何库存）' : '巡检并修正（dry_run=false）';
  const unfixable = report.unfixable > 0 ? `，其中 ${report.unfixable} 个缺仓库归属、无法自动修正` : '';
  return `本次检查 ${report.checked} 个（物料 × 仓库）组合，发现 ${report.mismatched} 个漂移${unfixable}。${mode}。`;
}

/** 巡检空态文案（分「库里没有序列号物料」与「巡检干净」两种，别都说成「没数据」） */
export function auditEmptyText(report: { checked: number; mismatched: number }): string {
  if (report.checked === 0) {
    return '库里没有启用「序列号管理」的物料，本次巡检检查了 0 个组合 —— 不是失败，是没有可巡检的对象。';
  }
  if (report.mismatched === 0) {
    return `本次巡检的 ${report.checked} 个组合，账面数量与在库序列号数全部一致（没有漂移）。`;
  }
  return '有漂移但明细为空（后端口径异常），请刷新重试或走巡检接口核对。';
}
