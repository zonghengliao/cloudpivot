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

/** 对账码（V0422；本页**不消费**，只在 403 人话里提示它属于哪张单） */
export const PERM_TRANSIT_AUDIT = 'business:erp:inventory-transit:audit';

/** 两个 Tab 的 key（明细是首屏） */
export type TransitTabKey = 'detail' | 'summary';

/** Tab 声明（`value` 同时用于 `t-tab-panel` 与单测契约） */
export const TRANSIT_TABS: ReadonlyArray<{ key: TransitTabKey; label: string }> = [
  { key: 'detail', label: '在途明细' },
  { key: 'summary', label: '在途汇总' },
];

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
