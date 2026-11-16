/**
 * 库龄页纯逻辑（M2-12）—— 三个 Tab 的筛选拼参、数值 / 比例格式化、口径文案、行键与钻取。
 *
 * # 为什么逻辑搬出 `.vue`
 *
 * 用例跑在没有 DOM 的 node 里（`tsx --test`，见 `tests/unit/m212-inventory-aging-page.test.ts`），
 * 而下面几处错一个就会让仓管 / 财务看错库龄账：
 *
 * * **拼参**：空筛选必须整键缺席（后端把 `?material_id=` 当非法 UUID → 400 + 66006，
 *   把 `?min_age_days=` 当解析失败）；`min_age_days > max_age_days` 后端会 400，
 *   页面要在发请求之前就拦住（否则用户只看到一句「库龄区间反了」而不知道该改哪个框）。
 * * **数量**：后端 `Decimal` 出 JSON 是字符串（`"120.0000"`），直接 `+` 是字符串拼接；
 *   `formatQty` 是唯一出口。
 * * **比例**：后端 `slow_ratio` 是 **4 位小数的小数**（`0.1538` = 15.38%），
 *   页面按百分数展示时不许再除一次 100。
 * * **合计**：报表的 `summary` / 呆滞的 `summary` 都是后端在**截断前**算的全量，
 *   页面把 `rows` 自己加一遍就是第二套账（截断时必然算小）。
 *
 * 纯函数、零副作用；字段名一律逐字抄后端（`inventory_aging::{dto,report::dto,slow_moving::dto}`）。
 */
import type {
  AgingBucketQty,
  AgingLayerQuery,
  AgingLayerRow,
  InventoryAgingReport,
  InventoryAgingReportQuery,
  InventoryAgingRow,
  SlowMovingQuery,
  SlowMovingReport,
  SlowMovingRow,
} from '@/api/business/erp/inventory-aging';
import { toNumber } from '../shared/money';

// ==================== 常量 ====================

/** 批次层每页条数（与其它 ERP 列表页同口径 20；后端上限 200） */
export const AGING_PAGE_SIZE = 20;

/**
 * 页面显形码（V0427 的菜单页面行 + 路由 `meta.permission`）。
 *
 * 它**只判前端**：侧栏显隐 + 路由守卫，后端源码零引用（登记在
 * `permission_audit_tests::ROUTE_LEVEL_ONLY`）。
 */
export const PERM_AGING_PAGE = 'business:erp:inventory-aging:page';

/** 批次层取数码（`GET …/inventory-aging/page`，V0424 按钮行） */
export const PERM_AGING_LIST = 'business:erp:inventory-aging:list';

/** 分段报表取数码（`GET …/inventory-aging/report`，V0425 按钮行） */
export const PERM_AGING_REPORT = 'business:erp:inventory-aging:report';

/** 呆滞料取数码（`GET …/inventory-aging/slow-moving`，V0426 按钮行） */
export const PERM_AGING_SLOW_MOVING = 'business:erp:inventory-aging:slow-moving';

/**
 * 呆滞阈值的系统参数键（V0426 种下，缺省 90 天）。
 *
 * 页面**不提供改阈值的入口**：改值走「系统参数」页（`/support/config`，
 * 权限码 `support:config:update`），本页只在提示里点名这个键。
 */
export const SLOW_MOVING_CONFIG_KEY = 'erp.inventory.slow-moving-days';

/** 阈值缺省天数（与后端 `DEFAULT_THRESHOLD_DAYS` 一致；只用于文案，判定在后端） */
export const SLOW_MOVING_DEFAULT_DAYS = 90;

/** 三个 Tab（顺序即侧栏内顺序：先分布、再处置清单、最后钻到层） */
export const AGING_TABS = [
  { key: 'report', label: '库龄分析' },
  { key: 'slow', label: '呆滞清单' },
  { key: 'layers', label: '批次层明细' },
] as const;

/** Tab 键 */
export type AgingTabKey = (typeof AGING_TABS)[number]['key'];

// ==================== 筛选状态 ====================

/** 报表 / 呆滞共用的三个筛选（留空 = 不限 / 今天） */
export interface AgingFilterState {
  /** 基准日（`yyyy-MM-dd`；留空 = 后端取今天 CST，页面不自己算） */
  as_of: string;
  /** 物料 id（留空 = 全部） */
  material_id: string;
  /** 仓库 id（留空 = 全部） */
  warehouse_id: string;
}

/** 报表 Tab 筛选 */
export function emptyAgingFilter(): AgingFilterState {
  return { as_of: '', material_id: '', warehouse_id: '' };
}

/** 呆滞 Tab 筛选（多一个阈值天数） */
export interface SlowFilterState extends AgingFilterState {
  /** 阈值天数（留空 = 系统参数 → 缺省 90；单位：天，范围 1..3650） */
  days: string | number;
}

/** 呆滞 Tab 空筛选 */
export function emptySlowFilter(): SlowFilterState {
  return { ...emptyAgingFilter(), days: '' };
}

/** 批次层 Tab 筛选（`ListPage` 的 filter 对象就地被组件读写） */
export interface LayerFilterState {
  /** 基准日（留空 = 今天 CST） */
  as_of: string;
  /** 物料 id（留空 = 全部） */
  material_id: string;
  /** 仓库 id（留空 = 全部） */
  warehouse_id: string;
  /** 批号（精确匹配；空 = 不筛） */
  lot_no: string;
  /** 最小库龄（天，含；空 = 不限下界） */
  min_age_days: string | number;
  /** 最大库龄（天，含；空 = 不限上界） */
  max_age_days: string | number;
  /** 是否连「已吃满（剩 0）」的层一起看（缺省 false） */
  include_consumed: boolean;
}

/** 批次层空筛选 */
export function emptyLayerFilter(): LayerFilterState {
  return {
    as_of: '',
    material_id: '',
    warehouse_id: '',
    lot_no: '',
    min_age_days: '',
    max_age_days: '',
    include_consumed: false,
  };
}

// ==================== 拼参 ====================

/** 空白 → `undefined`（整键缺席；不能发 `?x=`） */
function blankToUndefined(value: string | number | null | undefined): string | undefined {
  if (value === null || value === undefined) {
    return undefined;
  }
  const text = String(value).trim();
  return text === '' ? undefined : text;
}

/**
 * 库龄区间：把 `min_age_days` / `max_age_days` 归一成可发的值。
 *
 * 两条规矩（与后端 `ensure_age_range` 同口径，只是提前到请求前）：
 * 1. 非数字 / 空白 → `undefined`（不发）；
 * 2. `min > max` → 返回 `error` 文案，页面**不发请求**直接提示 ——
 *    后端也会 400 + 66006，但用户看不到是哪个框写反了。
 */
export function ageRangeOf(
  minRaw: string | number | null | undefined,
  maxRaw: string | number | null | undefined,
): { min?: number; max?: number; error?: string } {
  const minText = blankToUndefined(minRaw);
  const maxText = blankToUndefined(maxRaw);
  const min = minText === undefined ? undefined : Number(minText);
  const max = maxText === undefined ? undefined : Number(maxText);

  if (min !== undefined && (!Number.isFinite(min) || !Number.isInteger(min) || min < 0)) {
    return { error: `最小库龄必须是非负整数（当前 \`${minText}\`）` };
  }
  if (max !== undefined && (!Number.isFinite(max) || !Number.isInteger(max) || max < 0)) {
    return { error: `最大库龄必须是非负整数（当前 \`${maxText}\`）` };
  }
  if (min !== undefined && max !== undefined && min > max) {
    return { error: `库龄区间反了：最小 ${min} 天 > 最大 ${max} 天，请改正后再查` };
  }
  return { min, max };
}

/** 报表拼参（空筛选 → 空对象，后端取今天 + 全量） */
export function buildReportQuery(state: AgingFilterState): InventoryAgingReportQuery {
  const query: InventoryAgingReportQuery = {};
  const asOf = blankToUndefined(state.as_of);
  const materialId = blankToUndefined(state.material_id);
  const warehouseId = blankToUndefined(state.warehouse_id);
  if (asOf !== undefined) query.as_of = asOf;
  if (materialId !== undefined) query.material_id = materialId;
  if (warehouseId !== undefined) query.warehouse_id = warehouseId;
  return query;
}

/** 呆滞清单拼参（`days` 留空整键缺席 = 让后端按系统参数 / 缺省判定） */
export function buildSlowQuery(state: SlowFilterState): SlowMovingQuery {
  const query: SlowMovingQuery = { ...buildReportQuery(state) };
  const days = blankToUndefined(state.days);
  if (days !== undefined) {
    query.days = Number(days);
  }
  return query;
}

/**
 * 批次层拼参。
 *
 * `include_consumed` **总是显式发** `'true' / 'false'`：这是页面上的开关，
 * 用户看到的就是「含已耗竭」，不发等于让它悄悄按后端缺省（`false`）跑 ——
 * 开关拨了没反应是最难查的一类 bug。`page` / `page_size` 由 `ListPage` 传。
 */
export function buildLayerQuery(
  state: LayerFilterState,
  page: number,
  pageSize: number,
): AgingLayerQuery {
  const query: AgingLayerQuery = { page, page_size: pageSize };
  const asOf = blankToUndefined(state.as_of);
  const materialId = blankToUndefined(state.material_id);
  const warehouseId = blankToUndefined(state.warehouse_id);
  const lotNo = blankToUndefined(state.lot_no);
  if (asOf !== undefined) query.as_of = asOf;
  if (materialId !== undefined) query.material_id = materialId;
  if (warehouseId !== undefined) query.warehouse_id = warehouseId;
  if (lotNo !== undefined) query.lot_no = lotNo;
  const range = ageRangeOf(state.min_age_days, state.max_age_days);
  if (range.min !== undefined) query.min_age_days = range.min;
  if (range.max !== undefined) query.max_age_days = range.max;
  query.include_consumed = state.include_consumed ? 'true' : 'false';
  return query;
}

/**
 * 从报表 / 呆滞行「钻取到批次层」：把 物料 × 仓库 + 基准日 带过去，库龄区间清空。
 *
 * 为什么不带 `min_age_days = max_age_days + 1` 之类：用户是来看「这格由哪些层组成」，
 * 而不是来复现「呆滞」判定 —— 区间一带上去，看到的总量就不是行上的 `qty_on_hand` 了。
 */
export function drillLayerFilter(
  row: Pick<InventoryAgingRow | SlowMovingRow, 'material_id' | 'warehouse_id'>,
  asOf: string,
): LayerFilterState {
  return { ...emptyLayerFilter(), as_of: asOf, material_id: row.material_id, warehouse_id: row.warehouse_id };
}

/** 某格是否被筛出来了（决定空态是「没有匹配」还是「还没有数据」） */
export function isAgingFiltered(state: AgingFilterState): boolean {
  return Boolean(blankToUndefined(state.as_of) ?? blankToUndefined(state.material_id) ?? blankToUndefined(state.warehouse_id));
}

/** 批次层是否被筛过（含「含已耗竭」开关：打开也算动过筛选） */
export function isLayerFiltered(state: LayerFilterState): boolean {
  return (
    Boolean(blankToUndefined(state.as_of)) ||
    Boolean(blankToUndefined(state.material_id)) ||
    Boolean(blankToUndefined(state.warehouse_id)) ||
    Boolean(blankToUndefined(state.lot_no)) ||
    Boolean(blankToUndefined(state.min_age_days)) ||
    Boolean(blankToUndefined(state.max_age_days)) ||
    state.include_consumed
  );
}

// ==================== 数值 / 比例格式化 ====================

/**
 * 数量：最多 4 位小数（后端 `NUMERIC(18,4)`），去掉无意义的尾零。
 *
 * 入参可能是字符串（`Decimal` 出 JSON 就是字符串）；`''` / `null` 一律 `0`，
 * 与 `shared/money.toNumber` 同口径，避免页面上出现 `NaN`。
 */
export function formatQty(value: number | string | null | undefined): string {
  const num = toNumber(value);
  return String(Number(num.toFixed(4)));
}

/** 比例（后端 4 位小数，0.1538）→ 百分数文本（`15.38%`）；分母 0 时后端给 0 → `0.00%` */
export function formatRatioPercent(value: number | string | null | undefined): string {
  const num = toNumber(value);
  return `${(num * 100).toFixed(2)}%`;
}

/** 库龄天数 → 文字（负数 = 入库日期晚于基准日，后端**不夹 0**，这里照实说） */
export function ageDaysText(days: number): string {
  if (days < 0) {
    return `-${Math.abs(days)}（晚于基准日）`;
  }
  return String(days);
}

/** 库龄分级主题（`t-tag` / 文字上色）：≥91 天危险、61-90 提醒、其余默认 */
export function ageTheme(days: number): 'danger' | 'warning' | 'default' {
  if (days >= 91) return 'danger';
  if (days >= 61) return 'warning';
  return 'default';
}

/** 呆滞占比主题：≥50% 危险、>0 提醒 */
export function slowRatioTheme(ratio: number | string | null | undefined): 'danger' | 'warning' | 'default' {
  const value = toNumber(ratio);
  if (value >= 0.5) return 'danger';
  if (value > 0) return 'warning';
  return 'default';
}

/**
 * 按档位键取一格（行 / 汇总）。
 *
 * 为什么按 `key` 找而不是按下标：档位定义由后端 `report.buckets` 给，前端**不写死 4 列**
 * （以后加「180 天以上」档时，前端只按 key 渲染就自动跟上）。找不到时返回 `undefined`，
 * 页面显示 `—` 而不是 0 —— 0 会被读成「这一档确实没有货」。
 */
export function bucketCellOf(cells: AgingBucketQty[], key: string): AgingBucketQty | undefined {
  return cells.find((cell) => cell.key === key);
}

// ==================== 口径文案 ====================

/** 基准日来源 → 人话（后端 `as_of_source`） */
export function asOfSourceText(source: string | null | undefined): string {
  return source === 'QUERY' ? '手动指定' : '今天（CST）';
}

/** 阈值来源 → 人话（后端 `threshold_source`） */
export function thresholdSourceText(source: string | null | undefined): string {
  switch (source) {
    case 'QUERY':
      return '本次手填';
    case 'CONFIG':
      return '系统参数';
    default:
      return '缺省';
  }
}

/** 呆滞阈值的整句说明（页头 / KPI 用；生效值与来源都写清） */
export function thresholdHint(report: SlowMovingReport): string {
  return `阈值 ${report.threshold_days} 天（${thresholdSourceText(report.threshold_source)}），库龄严格大于该值且仍有剩余的批次算呆滞；改阈值去「系统参数」改 ${report.threshold_config_key}`;
}

/** 报表 / 呆滞行被截断时的提示（`''` = 没截断） */
export function truncatedNote(truncated: boolean, rowCount: number, rowLimit: number): string {
  if (!truncated) {
    return '';
  }
  return `明细只列前 ${rowLimit} 行（共 ${rowCount} 行）；合计与各档总量仍是全量，未受影响`;
}

/** 报表空态：筛过 = 没有匹配；没筛 = 基准日当天没有结存 */
export function reportEmptyText(report: InventoryAgingReport, filtered: boolean): string {
  if (filtered) {
    return '没有匹配的 物料 × 仓库 组合，试试清空物料 / 仓库筛选';
  }
  return `基准日 ${report.as_of} 没有任何有结存的入库层（这不是出错：期初 / 入库过账后才有库龄层）`;
}

/** 呆滞清单空态 */
export function slowEmptyText(report: SlowMovingReport, filtered: boolean): string {
  if (filtered) {
    return '当前筛选下没有呆滞物料，试试调小阈值天数或清空物料 / 仓库';
  }
  return `基准日 ${report.as_of} 没有库龄超过 ${report.threshold_days} 天的结存（呆滞为 0 是好消息，不是出错）`;
}

/** 批次层空态 */
export function layerEmptyText(filtered: boolean): string {
  return filtered
    ? '没有匹配的库龄层：放宽库龄区间、清空批号，或勾上「含已耗竭」看已被吃满的层'
    : '还没有任何入库层：期初建账或入库单过账后才会有库龄层';
}

/** 报表页脚的跨单位提示（数量总量跨物料不可比，金额是参考金额） */
export function reportTotalNote(): string {
  return '跨物料的数量单位不同，「合计数量」只作总量参考；金额 = Σ（层剩余 × 层入库单价），是参考金额，不等于账面结存金额（移动加权不按层拆）';
}

/** 把后端 / 网络错误翻译成人话（403 要点名缺哪个码，否则用户以为系统坏了） */
export function requestErrorMessage(error: unknown): string {
  const raw = error instanceof Error ? error.message : String(error);
  if (/403|无权限|forbidden/i.test(raw)) {
    return `${raw}；本页三个 Tab 分别要 ${PERM_AGING_REPORT} / ${PERM_AGING_SLOW_MOVING} / ${PERM_AGING_LIST}，请让管理员在角色授权里勾上对应的按钮行`;
  }
  return raw;
}

// ==================== 展示用文本 / 行键 ====================

/** 物料文本（主数据软删时点名，不留白也不显示 UUID） */
export function materialText(row: { material_code: string | null; material_name: string | null; spec?: string | null }): string {
  const code = row.material_code ?? '';
  const name = row.material_name ?? '';
  if (!code && !name) {
    return '（物料已删除）';
  }
  return `${code} ${name}`.trim();
}

/** 仓库文本 */
export function warehouseText(row: { warehouse_code: string | null; warehouse_name: string | null }): string {
  const code = row.warehouse_code ?? '';
  const name = row.warehouse_name ?? '';
  if (!code && !name) {
    return '（仓库已删除）';
  }
  return `${code} ${name}`.trim();
}

/**
 * 来源单据类型 → 人话（后端 `doc_type` 字面量）。
 *
 * 未知值原样返回：新增一种单据类型时页面显示原始码，而不是「未知」（后者等于把
 * 后端新增的事实藏起来）。
 */
export function docTypeText(docType: string): string {
  switch (docType) {
    case 'STOCK_IN':
      return '入库单';
    case 'STOCK_OUT':
      return '出库单';
    case 'STOCK_TRANSFER':
      return '调拨单';
    case 'STOCK_TAKE':
      return '盘点单';
    case 'OPENING':
      return '期初结转';
    case 'MATERIAL_SERIAL':
      return '序列号流水';
    case 'SERIAL_BATCH':
      return '批次流水';
    default:
      return docType;
  }
}

/** 业务类型 → 人话（未知值原样返回，理由同 `docTypeText`） */
export function bizTypeText(bizType: string): string {
  switch (bizType) {
    case 'PURCHASE_IN':
      return '采购入库';
    case 'SALES_RETURN_IN':
      return '销售退货入库';
    case 'PRODUCTION_IN':
      return '完工入库';
    case 'OPENING':
      return '期初结转';
    case 'PURCHASE_RETURN_OUT':
      return '采购退货出库';
    case 'SALES_OUT':
      return '销售出库';
    case 'TRANSFER_OUT':
    case 'TRANSFER_IN':
      return '调拨';
    case 'TAKE_GAIN':
      return '盘盈';
    case 'TAKE_LOSS':
      return '盘亏';
    default:
      return bizType;
  }
}

/** 报表 / 呆滞行的行键（同一物料可能有多个仓库，`material_id` 不唯一） */
export function agingRowKey(row: Pick<InventoryAgingRow | SlowMovingRow, 'material_id' | 'warehouse_id'>): string {
  return `${row.material_id}|${row.warehouse_id}`;
}

/** 批次层的行键（层主键；同一张入库单的多行是多个层） */
export function layerRowKey(row: Pick<AgingLayerRow, 'layer_id'>): string {
  return row.layer_id;
}
