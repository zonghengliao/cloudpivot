/**
 * 在途库存页纯逻辑（M2-07）—— 两个 Tab 的筛选拼参、状态 / 账龄判定、数量格式化、空态与 403 人话。
 *
 * # 为什么逻辑全搬出 `.vue`
 *
 * 用例跑在没有 DOM 的 node 里（`tsx --test`），而下面这几处错一个就会让仓管看错账：
 *
 * * **拼参**：`status` 留空必须**整键缺席**（后端把「留空」当默认 `IN_TRANSIT`；
 *   发 `?status=` 反倒变成「等于空串」→ 0 行）；日期区间必须拆成
 *   `ship_date_from` / `ship_date_to`（后端不收数组）。
 * * **账龄**：`age_days` 由 PG 算，前端**不许**拿 `today - ship_date` 再算一遍
 *   （浏览器时区 / 夏令时会差一天，而「挂了几天」是业务口径）。
 * * **合计**：汇总的 `total_*` 是后端另一条聚合查询算的（不受行截断影响），
 *   页面把它们自己加一遍就是第二套账 —— `summaryTotalsSource` 之外没有别的来源。
 *
 * 纯函数、零副作用，被 `frontend/tests/unit/m207-transit-page.test.ts` 钉住。
 *
 * # 字段名一律逐字抄后端
 *
 * `qty_in_transit` / `shipped_qty` / `received_qty` / `age_days` / `first_ship_date` /
 * `max_age_days` / `total_qty_in_transit`（`inventory_transit::dto`），不在这里起别名。
 */
import type {
  TransitQuery,
  TransitReconcile,
  TransitReconcileQuery,
  TransitReconcileRow,
  TransitRow,
  TransitStatus,
  TransitSummary,
  TransitSummaryQuery,
} from '@/api/business/erp/inventory-transit';

// ==================== 常量 ====================

/** 每页条数（与其它 ERP 列表页同口径 20；后端上限 200） */
export const TRANSIT_PAGE_SIZE = 20;

/**
 * 页面显形码（V0423 的菜单页面行 + 路由 `meta.permission`）。
 *
 * 它**只判前端**：侧栏显隐 + 路由守卫，后端源码零引用（登记的
 * `permission_audit_tests::ROUTE_LEVEL_ONLY` C 类例外）。
 */
export const PERM_TRANSIT_PAGE = 'business:erp:inventory-transit:page';

/**
 * 取数码（`GET /inventory-transit/page` 与 `/summary` 的框架闸，V0421 的按钮行）。
 *
 * 与页面码**成对授予**是迁移 V0423 自检的一条：只授页面码 = 菜单点得进、表格永远 403。
 */
export const PERM_TRANSIT_LIST = 'business:erp:inventory-transit:list';

/**
 * 对账码（V0422 按钮行 …4220）。
 *
 * 本页**用它判第三个 Tab「在途对账」的显隐**（M2-07b）：没有这个码的角色看不到 Tab，
 * 但页面照常能开 —— 对账是「结论类」授权面，不该因为它把整个在途页 403 掉。
 * 有码没 `:list` 的角色另说（明细 Tab 会 403，`requestErrorMessage` 点名缺哪个码）。
 */
export const PERM_TRANSIT_AUDIT = 'business:erp:inventory-transit:audit';

/** 两个常驻 Tab 的 key（明细是首屏） */
export type TransitTabKey = 'detail' | 'summary' | 'reconcile';

/** Tab 声明（`value` 同时用于 `t-tab-panel` 与单测契约） */
export const TRANSIT_TABS: ReadonlyArray<{ key: 'detail' | 'summary'; label: string }> = [
  { key: 'detail', label: '在途明细' },
  { key: 'summary', label: '在途汇总' },
];

/**
 * 第三个 Tab：**在途对账**（M2-07b）。
 *
 * 与前两个分开声明，因为它**不是常驻 Tab**：按 `business:erp:inventory-transit:audit`
 * 判显隐 —— 没有对账码的角色看不到这个 Tab（但页面照常能开，**不做整页 403**）。
 * 把「显隐」写进 `TRANSIT_TABS` 会让「常驻 Tab 有几个」这个事实变得不可测。
 */
export const RECONCILE_TAB = { key: 'reconcile', label: '在途对账' } as const;

/**
 * 状态下拉（后端 3 值域；`''` = 默认「只看在途」）。
 *
 * ⚠️ 第一项**不是**「全部状态」：在途页缺省就该只看在途，想看历史必须显式选「已签收」。
 * 选项值里不放空串当「全部」的意义见 `docs/编码规范.md`（TDesign 把空串当未选，
 * 显示成 placeholder，用户会以为是「已选全部」）。
 */
export const TRANSIT_STATUS_OPTIONS: ReadonlyArray<{ label: string; value: TransitStatus }> = [
  { label: '在途（未签收）', value: 'IN_TRANSIT' },
  { label: '已签收', value: 'RECEIVED' },
  { label: '已作废', value: 'CANCELLED' },
];

/** 状态 → 中文（后端 `status_label` 缺失时的兜底；正常路径用后端给的值） */
const STATUS_TEXT: Record<string, string> = {
  IN_TRANSIT: '在途（未签收）',
  RECEIVED: '已签收',
  CANCELLED: '已作废',
};

/** 账龄分级：< 8 天正常，8~30 天关注，≥ 31 天逾期警示（与仓管「月结前清在途」的节奏对表） */
export const AGE_WARN_DAYS = 8;
export const AGE_DANGER_DAYS = 31;

// ==================== 筛选状态 ====================

/** 明细 Tab 的筛选状态（键名与后端入参同名；`date_range` 是 UI 内部形态） */
export interface TransitFilterState {
  /** 物料 id */
  material_id: string;
  /** 发货仓库 id */
  warehouse_id: string;
  /** 客户 id */
  customer_id: string;
  /** 发货单 id */
  shipment_id: string;
  /** 状态（`''` = 后端默认 `IN_TRANSIT`） */
  status: TransitStatus | '';
  /** 发货日期区间 `[from, to]`（`yyyy-MM-dd`；空数组 = 不限） */
  date_range: string[];
}

/** 汇总 Tab 的筛选状态（没有 `status`：汇总恒为未签收） */
export interface TransitSummaryFilterState {
  /** 物料 id */
  material_id: string;
  /** 发货仓库 id */
  warehouse_id: string;
  /** 客户 id */
  customer_id: string;
}

/** 明细筛选的初值 */
export function emptyTransitFilter(): TransitFilterState {
  return {
    material_id: '',
    warehouse_id: '',
    customer_id: '',
    shipment_id: '',
    status: '',
    date_range: [],
  };
}

/** 汇总筛选的初值 */
export function emptySummaryFilter(): TransitSummaryFilterState {
  return { material_id: '', warehouse_id: '', customer_id: '' };
}

/** 有没有真的在筛（决定空态文案与「筛选生效」提示） */
export function isTransitFiltered(state: TransitFilterState): boolean {
  return Boolean(
    state.material_id ||
      state.warehouse_id ||
      state.customer_id ||
      state.shipment_id ||
      state.status ||
      (state.date_range ?? []).filter(Boolean).length > 0,
  );
}

/** 汇总筛选有没有真的在筛 */
export function isSummaryFiltered(state: TransitSummaryFilterState): boolean {
  return Boolean(state.material_id || state.warehouse_id || state.customer_id);
}

/**
 * 明细入参拼装（页面唯一一处拼参，被单测钉住）。
 *
 * 规则（逐条都有代价，别顺手改）：
 * 1. 空串 / 空数组 **整键缺席**（后端把 `?x=` 当「等于空串」过滤 → 0 行）；
 * 2. `status` 只有显式选了才发 —— 缺省让后端用它的默认值 `IN_TRANSIT`；
 * 3. 日期区间拆成 `ship_date_from` / `ship_date_to`；只给一端就只发那一端
 *    （「截至某天」是常用查法）；反区间由后端 400 点名，前端不悄悄对调；
 * 4. `page` / `page_size` 永远发（后端 0 → 默认 20）。
 */
export function buildTransitQuery(
  state: TransitFilterState,
  page = 1,
  pageSize = TRANSIT_PAGE_SIZE,
): TransitQuery {
  const query: TransitQuery = {
    page: page > 0 ? page : 1,
    page_size: pageSize > 0 ? pageSize : TRANSIT_PAGE_SIZE,
  };
  if (state.material_id) query.material_id = state.material_id;
  if (state.warehouse_id) query.warehouse_id = state.warehouse_id;
  if (state.customer_id) query.customer_id = state.customer_id;
  if (state.shipment_id) query.shipment_id = state.shipment_id;
  if (state.status) query.status = state.status;
  const [from, to] = (state.date_range ?? []).map((item) => (item ?? '').trim());
  if (from) query.ship_date_from = from;
  if (to) query.ship_date_to = to;
  return query;
}

/** 汇总入参拼装（空串整键缺席；没有分页、没有 status） */
export function buildSummaryQuery(state: TransitSummaryFilterState): TransitSummaryQuery {
  const query: TransitSummaryQuery = {};
  if (state.material_id) query.material_id = state.material_id;
  if (state.warehouse_id) query.warehouse_id = state.warehouse_id;
  if (state.customer_id) query.customer_id = state.customer_id;
  return query;
}

// ==================== 判定与格式化 ====================

/** 状态 → 中文名（优先用后端 `status_label`） */
export function statusLabel(status: string | null | undefined, backendLabel?: string | null): string {
  if (backendLabel) return backendLabel;
  return STATUS_TEXT[status ?? ''] ?? (status ?? '—');
}

/** 状态 → `t-tag` 主题 */
export function statusTheme(status: string | null | undefined): 'success' | 'warning' | 'danger' | 'default' {
  switch (status) {
    case 'IN_TRANSIT':
      return 'warning';
    case 'RECEIVED':
      return 'success';
    case 'CANCELLED':
      return 'danger';
    default:
      return 'default';
  }
}

/** 账龄 → `t-tag` 主题（分级见 {@link AGE_WARN_DAYS} / {@link AGE_DANGER_DAYS}） */
export function ageTheme(days: number | null | undefined): 'success' | 'warning' | 'danger' {
  const value = Number(days ?? 0);
  if (!Number.isFinite(value) || value < AGE_WARN_DAYS) return 'success';
  if (value < AGE_DANGER_DAYS) return 'warning';
  return 'danger';
}

/** 账龄文案：`3 天` / `—`（后端没给 age_days 时） */
export function ageText(days: number | null | undefined): string {
  if (days === null || days === undefined) return '—';
  const value = Number(days);
  if (!Number.isFinite(value)) return '—';
  return `${value} 天`;
}

/**
 * 数量格式化：`6` / `6.5` / `1,234.125`（去尾零 + 三位千分位）。
 *
 * 后端 `NUMERIC(18,4)` 出 JSON 是字符串（`"6.0000"`）。这里**按字符串处理**：
 * 先校验形态，再去尾零，避免 `Number()` 把 `12345678901234.5678` 抖成科学计数法；
 * 形态不认识就原样返回（宁可显示 `6.0000` 也不显示 `0`）。
 */
export function formatQty(value: number | string | null | undefined): string {
  if (value === null || value === undefined || value === '') return '0';
  const text = String(value).trim();
  if (!/^-?\d+(\.\d+)?$/.test(text)) return text;
  const negative = text.startsWith('-');
  const body = negative ? text.slice(1) : text;
  const [intPart, fracPart = ''] = body.split('.');
  const trimmedFrac = fracPart.replace(/0+$/, '');
  const grouped = intPart.replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  const result = trimmedFrac ? `${grouped}.${trimmedFrac}` : grouped;
  if (result === '0' || /^0(\.0*)?$/.test(result)) return '0';
  return negative ? `-${result}` : result;
}

/** 数量是否 > 0（字符串安全：`"0.0000"` 是 0） */
export function isPositiveQty(value: number | string | null | undefined): boolean {
  const num = Number(value ?? 0);
  return Number.isFinite(num) && num > 0;
}

/** 明细空态文案（区分「筛出来是空」与「本来就没有在途」） */
export function detailEmptyText(state: TransitFilterState): { title: string; desc: string } {
  if (isTransitFiltered(state)) {
    return {
      title: '当前筛选下没有在途行',
      desc: '换个物料 / 仓库 / 日期区间再试；想看已签收的历史行，请把状态切到「已签收」。',
    };
  }
  return {
    title: '当前没有在途库存',
    desc: '发货过账后才会写 biz_inventory_in_transit（在途 = 已发货 − 已签收）；签收确认后这一行自动清零。',
  };
}

/** 汇总空态文案 */
export function summaryEmptyText(state: TransitSummaryFilterState): { title: string; desc: string } {
  if (isSummaryFiltered(state)) {
    return { title: '当前筛选下没有在途库存', desc: '换个物料 / 仓库 / 客户再试。' };
  }
  return {
    title: '当前没有在途库存',
    desc: '汇总恒为「未签收」的在途量；全部签收后这一页就是空的 —— 这是正常形态，不是数据缺失。',
  };
}

/**
 * 汇总截断提示（`truncated = true` 时）。
 *
 * 必须同时说清「行不全」和「合计是全量的」：只提示前半句会让人以为合计也少了。
 */
export function truncatedNote(summary: TransitSummary | null): string {
  if (!summary || !summary.truncated) return '';
  return (
    `汇总行已达上限，只显示前 ${summary.row_count} 组（按在途量倒序）；` +
    '下面的合计仍是**全量**（后端另一条聚合查询，不受行截断影响）。'
  );
}

/**
 * 403 人话：点名缺哪个码。
 *
 * 页面码与取数码分开授予（`PERM_TRANSIT_PAGE` / `PERM_TRANSIT_LIST`），最常见的错配是
 * 「菜单授了、取数码没授」—— 表格永远 403，而 403 原文只有「无权限」四个字。
 */
export function requestErrorMessage(err: unknown): string {
  const raw = err instanceof Error ? err.message : String(err ?? '');
  const text = (raw || '').trim();
  if (!text) return '查询在途库存失败（后端没有返回原因）';
  if (/\b403\b|无权限|权限不足|forbidden/i.test(text)) {
    return (
      `没有在途取数权限：本页需要 ${PERM_TRANSIT_LIST}（页面显形另需 ${PERM_TRANSIT_PAGE}）。` +
      '页面能打开但表格是空的，通常是「菜单授了、取数码没授」—— 请管理员在角色里补上这个权限码。'
    );
  }
  return text;
}

/** 一行明细的「物料」展示文本（主数据软删时点名，不显示裸 UUID） */
export function materialText(row: Pick<TransitRow, 'material_code' | 'material_name'>): string {
  const code = row.material_code ?? '';
  const name = row.material_name ?? '';
  if (code && name) return `${code} ${name}`;
  if (code || name) return code || name;
  return '（物料已删除）';
}

/** 一行明细的「客户」展示文本 */
export function customerText(row: Pick<TransitRow, 'customer_code' | 'customer_name'>): string {
  const code = row.customer_code ?? '';
  const name = row.customer_name ?? '';
  if (code && name) return `${code} ${name}`;
  if (code || name) return code || name;
  return '（客户已删除）';
}

/** 一行明细的「仓库」展示文本 */
export function warehouseText(row: Pick<TransitRow, 'warehouse_code' | 'warehouse_name'>): string {
  const code = row.warehouse_code ?? '';
  const name = row.warehouse_name ?? '';
  if (code && name) return `${code} ${name}`;
  if (code || name) return code || name;
  return '（仓库已删除）';
}

// ==================== 对账（M2-07b：逐「发货单 × 物料」的三侧比对） ====================

/**
 * 对账 Tab 的筛选状态（键名与后端 `TransitReconcileFilter` 同名）。
 *
 * 没有 `status`：对账要的是「全量是否恒等」，不是「某个状态的行长什么样」——
 * 作废行由报告里的 `CANCELLED_ONLY` / `cancelled_qty` 表达（后端 `TransitReconcileFilter` 同样没有 status）。
 */
export interface TransitReconcileFilterState {
  /** 发货单 id（从发货单详情「对不对得上」跳过来时用） */
  shipment_id: string;
  /** 物料 id */
  material_id: string;
  /** 发货仓库 id */
  warehouse_id: string;
  /** 发货日期区间 `[from, to]`（`yyyy-MM-dd`；空数组 = 不限） */
  date_range: string[];
  /** 是否连**对平**的组一起返回（缺省 false = 只看有差异的组） */
  include_matched: boolean;
}

/** 对账筛选初值 */
export function emptyReconcileFilter(): TransitReconcileFilterState {
  return { shipment_id: '', material_id: '', warehouse_id: '', date_range: [], include_matched: false };
}

/**
 * 对账筛选有没有真的在筛。
 *
 * `include_matched` **不算**：它只改「返回集合的大小」，不改对账口径 ——
 * 把它算进「筛选生效」会让「全部对平」的空态退化成「当前筛选下没有差异组」。
 */
export function isReconcileFiltered(state: TransitReconcileFilterState): boolean {
  return Boolean(
    state.shipment_id ||
      state.material_id ||
      state.warehouse_id ||
      (state.date_range ?? []).filter(Boolean).length > 0,
  );
}

/**
 * 对账入参拼装（页面唯一一处拼参，被单测钉住）。
 *
 * 1. 空串整键缺席（后端把 `?x=` 当「等于空串」过滤 → 0 组）；
 * 2. 日期区间拆成 `ship_date_from` / `ship_date_to`，只给一端就只发那一端；
 * 3. `include_matched` 只在**勾上**时才发 `'true'`（后端缺省 `false`；发 `'false'` 是等价但多余）。
 */
export function buildReconcileQuery(state: TransitReconcileFilterState): TransitReconcileQuery {
  const query: TransitReconcileQuery = {};
  if (state.shipment_id) query.shipment_id = state.shipment_id;
  if (state.material_id) query.material_id = state.material_id;
  if (state.warehouse_id) query.warehouse_id = state.warehouse_id;
  const [from, to] = (state.date_range ?? []).map((item) => (item ?? '').trim());
  if (from) query.ship_date_from = from;
  if (to) query.ship_date_to = to;
  if (state.include_matched) query.include_matched = 'true';
  return query;
}

/** 行键：对账行是 `发货单 × 物料` 唯一（后端 `ComboRow` 按这两列 GROUP BY） */
export function reconcileRowKey(row: Pick<TransitReconcileRow, 'shipment_id' | 'material_id'>): string {
  return `${row.shipment_id}|${row.material_id}`;
}

/**
 * 差异码 → `t-tag` 主题（**只上色，不译字**）。
 *
 * * 5 个硬问题（`QTY_MISMATCH` / `MISSING_TRANSIT` / `ORPHAN_TRANSIT` /
 *   `IDENTITY_BROKEN` / `MULTI_WAREHOUSE`）→ `danger`；
 * * `CANCELLED_ONLY`（整批作废，后端不算「不平」）→ `warning`；
 * * 未知码 → `default`（后端也给了「未知差异」的文案，这里不重复造）。
 */
export function issueTheme(issue: string | null | undefined): 'danger' | 'warning' | 'default' {
  switch (issue) {
    case 'QTY_MISMATCH':
    case 'MISSING_TRANSIT':
    case 'ORPHAN_TRANSIT':
    case 'IDENTITY_BROKEN':
    case 'MULTI_WAREHOUSE':
      return 'danger';
    case 'CANCELLED_ONLY':
      return 'warning';
    default:
      return 'default';
  }
}

/**
 * 一行的结论文案：**逐字取后端 `issue_labels`**（空 = 对平）。
 *
 * 中文差异说明是业务口径（后端 `reconcile::issue_label`），前端抄一份必然漂：
 * 这里只做拼接（`；`），一个字都不新造。
 */
export function issueText(row: Pick<TransitReconcileRow, 'issue_labels' | 'matched'>): string {
  const labels = (row.issue_labels ?? []).filter((label) => label && label.trim().length > 0);
  if (labels.length) return labels.join('；');
  return row.matched ? '对平' : '未知差异';
}

/**
 * 一行的判定：`对平` / `不平` / `整批作废`。
 *
 * 「整批作废」= 只有 `CANCELLED_ONLY` 一个码（后端也不把它算进 `mismatched`）——
 * 与「不平」分开显示，否则仓管会把正常作废当成账错。
 */
export function reconcileResultText(row: Pick<TransitReconcileRow, 'matched' | 'issues'>): string {
  if (row.matched) return '对平';
  const issues = row.issues ?? [];
  if (issues.length > 0 && issues.every((issue) => issue === 'CANCELLED_ONLY')) return '整批作废';
  return '不平';
}

/** 行判定 → `t-tag` 主题（与 {@link reconcileResultText} 一一对应） */
export function reconcileResultTheme(
  row: Pick<TransitReconcileRow, 'matched' | 'issues'>,
): 'success' | 'warning' | 'danger' {
  if (row.matched) return 'success';
  const issues = row.issues ?? [];
  if (issues.length > 0 && issues.every((issue) => issue === 'CANCELLED_ONLY')) return 'warning';
  return 'danger';
}

/**
 * 数量是否**恰好为 0**（字符串安全：`"0.0000"` 是 0）。
 *
 * 与 {@link isPositiveQty} 不同：差额可能是负数，判「对平」不能只判 > 0。
 */
export function isZeroQty(value: number | string | null | undefined): boolean {
  if (value === null || value === undefined || value === '') return true;
  const text = String(value).trim();
  if (!/^-?\d+(\.\d+)?$/.test(text)) return false;
  return Number(text) === 0;
}

/**
 * 差额的展示：取后端 `diff` 原值（正 = 少记了，负 = 多记了），**页面不做减法**。
 *
 * 非 0 时带符号提示（`+` / `−` 由数值本身给出），避免「差 5」被误读成「差 0」。
 */
export function diffText(row: Pick<TransitReconcileRow, 'diff'>): string {
  return formatQty(row.diff);
}

/**
 * 对账行的「仓库」展示文本。
 *
 * `warehouse_id === null` 是后端的**语义值**（这组没有在途行，即 `MISSING_TRANSIT` 侧），
 * 不能沿用明细页的「（仓库已删除）」—— 那会被读成「仓库主数据被删了」。
 */
export function reconcileWarehouseText(
  row: Pick<TransitReconcileRow, 'warehouse_id' | 'warehouse_code' | 'warehouse_name'>,
): string {
  if (!row.warehouse_id) return '—（无在途行）';
  const code = row.warehouse_code ?? '';
  const name = row.warehouse_name ?? '';
  if (code && name) return `${code} ${name}`;
  if (code || name) return code || name;
  return '（仓库已删除）';
}

/** 对账空态（**三种形态分开写**：未查 / 筛选下无差异 / 全部对平） */
export function reconcileEmptyText(
  report: TransitReconcile | null,
  state: TransitReconcileFilterState,
): { title: string; desc: string } {
  if (!report) {
    return {
      title: '还没有对账结果',
      desc: '点「开始对账」跑一次：按「发货单 × 物料」逐组比对发货总量（SSOT）、已出与在途。',
    };
  }
  if (isReconcileFiltered(state)) {
    if (report.mismatched === 0) {
      return {
        title: `当前筛选下没有差异组（${report.checked} 组全对平）`,
        desc: '勾上「连对平的组一起看」可以逐组核对三侧数量；换筛选条件可以看别的区间。',
      };
    }
    return {
      title: '当前筛选下没有可对账的组',
      desc: '换个发货单 / 物料 / 仓库，或把发货日期区间放宽再试。',
    };
  }
  if (report.checked === 0) {
    return {
      title: '还没有可对账的发货单',
      desc:
        '对账只覆盖已过账（POSTED）的发货单，且比对的是「发货单明细 Σqty = 已出 + 在途」；' +
        '一张已过账的单都没有时这里就是空的。',
    };
  }
  return {
    title: `全部对平（${report.checked} 组）`,
    desc: '零硬问题、对账差额 0 —— 这一页空着是好消息，不是没数据。想看逐组的三侧数量请勾「连对平的组一起看」。',
  };
}

/** 对账横幅（**结论优先**：页面最上面一句话说清「平不平」） */
export interface ReconcileBanner {
  /** `t-alert` 主题 */
  theme: 'success' | 'warning' | 'error';
  /** 标题（结论） */
  title: string;
  /** 说明（三侧合计与差额都取后端 `totals`） */
  desc: string;
}

/**
 * 顶部结论横幅。
 *
 * 三种形态（与后端 `TransitReconcileRes` 的字段严格对应，不自己推测）：
 * 1. `balanced = true` → 对平（零硬问题 + 差额 0 + 未截断）；
 * 2. `totals.mismatched > 0` → 有硬问题（差额可能为 0，别拿差额当唯一判据）；
 * 3. 其它（零硬问题但差额 ≠ 0 / 被截断）→ 「未对平」警示。
 */
export function reconcileBanner(report: TransitReconcile | null): ReconcileBanner | null {
  if (!report) return null;
  const totals = report.totals;
  const sides = `发货总量 ${formatQty(totals.item_qty)} = 已出 ${formatQty(totals.received_qty)} + 在途 ${formatQty(
    totals.in_transit_qty,
  )}`;
  if (report.balanced) {
    return {
      theme: 'success',
      title: `全部对平：已比对 ${report.checked} 组（${report.matched} 组对平）`,
      desc: `${sides}，差额 ${formatQty(totals.diff)}。`,
    };
  }
  if (totals.mismatched > 0) {
    return {
      theme: 'error',
      title: `有 ${totals.mismatched} 组不平（共比对 ${report.checked} 组）`,
      desc: `${sides}，对账差额 ${formatQty(totals.diff)}。按下面的「结论」列逐行修：发货单明细是 SSOT，差在哪一侧就看那一侧的差异码。`,
    };
  }
  return {
    theme: 'warning',
    title: `整体未对平（零硬问题，对账差额 ${formatQty(totals.diff)}）`,
    desc: `${sides}。差额不为 0 而逐组都没有差异码，通常是扫描被上限截断或存在整批作废组 —— 看下面的提示逐条核。`,
  };
}

/**
 * 扫描截断提示（`truncated = true`）。
 *
 * ⚠️ 与「在途汇总」Tab 不同：对账的 `totals` 是**对已扫描组**求的
 * （`reconcile::summarize(&combos)`，combos 已被 `RECONCILE_SCAN_LIMIT` 截断），
 * 所以这里必须说清「合计也只覆盖前 N 组」，否则会让人以为合计是全量。
 */
export function reconcileTruncatedNote(report: TransitReconcile | null): string {
  if (!report || !report.truncated) return '';
  return (
    `本轮扫描到上限 ${report.scan_limit} 组，结论「只覆盖」这些组：` +
    '请用发货单 / 物料 / 仓库 / 发货日期区间把范围收窄后再对一次（合计与差额同样只统计已扫描的组）。'
  );
}

/**
 * 对账 403 人话：点名缺 `:audit` 码。
 *
 * 对账是**独立授权面**（V0422 的按钮行）：有在途取数码（`:list`）不等于能出对账结论。
 * 本 Tab 按该码判显隐，所以正常情况下看不到就不会 403；真拿到 403 说明角色矩阵刚被改过。
 */
export function reconcileErrorMessage(err: unknown): string {
  const raw = err instanceof Error ? err.message : String(err ?? '');
  const text = (raw || '').trim();
  if (!text) return '在途对账失败（后端没有返回原因）';
  if (/\b403\b|无权限|权限不足|forbidden/i.test(text)) {
    return (
      `没有在途对账权限：本 Tab 需要 ${PERM_TRANSIT_AUDIT}（V0422 的按钮行，与取数码 ${PERM_TRANSIT_LIST} 是两个码）。` +
      '请管理员在角色里补上这个权限码。'
    );
  }
  return text;
}
