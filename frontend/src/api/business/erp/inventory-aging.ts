/**
 * 库龄 API 层（后端 `backend/app/src/business/erp/inventory_aging/`，
 * M2-08 批次层 / M2-09 分段报表 / M2-10 呆滞料；M2-12 前端页消费这三个只读面）。
 *
 * # 为什么单独一个文件（不并进 `erp/index.ts`）
 *
 * 与 `inventory-transit.ts` 同一口径：库龄是独立后端模块（自己的 handler / model /
 * 迁移 V0424~V0426），按模块边界单独成文件，页面只
 * `import '@/api/business/erp/inventory-aging'`。
 *
 * # 三个只读接口（字段名逐字抄后端，不是猜的）
 *
 * | 语义 | 路径 | 权限码 | 出参 |
 * |------|------|--------|------|
 * | 批次层明细（一层一行） | `GET …/inventory-aging/page` | `…:inventory-aging:list`（V0424） | `PageData<AgingLayerRow>` |
 * | 库龄分段报表（物料×仓库 一行） | `GET …/inventory-aging/report` | `…:inventory-aging:report`（V0425） | `InventoryAgingReport` |
 * | 呆滞料清单（物料×仓库 一行） | `GET …/inventory-aging/slow-moving` | `…:inventory-aging:slow-moving`（V0426） | `SlowMovingReport` |
 *
 * # 三个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端路由宏不含前缀，漏了会命中 SPA fallback 返回 HTML
 *    → 前端报「响应体格式不符合」）；
 * 2. **`as_of` 留空 = 后端取「今天（CST）」**，而前端**不自己算今天再传**
 *    （浏览器时区 / 夏令时会把「今天入库」算成库龄 1 天）。页头显示后端回显的 `as_of`
 *    与 `as_of_source`，不猜；
 * 3. **数量 / 金额是 `Decimal`，出参可能是字符串**（`"120.0000"`）。一律过
 *    `aging-logic.ts::formatQty` / `shared/money.formatMoney`，别直接 `+`
 *    （`"120.0000" + 1` 是字符串拼接）。
 *
 * # 只读
 *
 * 本模块**没有任何写接口**：层是从 `biz_inventory_txn` 的 IN 流水按 FIFO 推导出来的
 * （V0424 的库函数 `biz_inventory_aging_layers`），呆滞是同一批层按天数阈值再切一刀。
 * 页面因此不挂新建 / 编辑 / 删除，也不挂导出（后端没有导出接口 —— 画一个点了 404 的
 * 按钮比没有按钮更糟）。
 */
import { get, type PageData } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

// ==================== 批次层明细（M2-08 · GET …/page） ====================

/**
 * 一层入库（= 一个批次层）在基准日的库龄快照（逐字抄 `AgingLayerRes`）。
 *
 * 数量口径全部是 SQL 算出来的列（`qty_in` / `qty_consumed` / `qty_remaining`），
 * 前端**不重算 FIFO** —— 两处各算一遍迟早会漂。
 *
 * 主数据名字（`material_*` / `warehouse_*` / `spec` / `unit_name`）由后端**批量补齐**，
 * 主数据软删后为 `null`（AGENTS.md 约定 13：业务展示类实时 join，不存快照）。
 */
export interface AgingLayerRow {
  /** 层主键（= 那笔 `IN` 流水的 id） */
  layer_id: string;
  /** 物料 id */
  material_id: string;
  /** 物料编码（物料软删后 `null`） */
  material_code: string | null;
  /** 物料名称（物料软删后 `null`） */
  material_name: string | null;
  /** 规格型号 */
  spec: string | null;
  /** 计量单位名 */
  unit_name: string | null;
  /** 仓库 id */
  warehouse_id: string;
  /** 仓库编码（仓库软删后 `null`） */
  warehouse_code: string | null;
  /** 仓库名称（仓库软删后 `null`） */
  warehouse_name: string | null;
  /** 批号（入库单明细 `lot_no`；该层没有批号时为 `null`） */
  lot_no: string | null;
  /** 来源单据类型（`STOCK_IN` / `STOCK_OUT` / `STOCK_TRANSFER` / `STOCK_TAKE` / `OPENING` …） */
  doc_type: string;
  /** 来源单号 */
  doc_no: string;
  /** 来源单据行号 */
  line_no: number;
  /** 业务类型（`PURCHASE_IN` / `SALES_RETURN_IN` / `PRODUCTION_IN` / `OPENING` …） */
  biz_type: string;
  /** 层的入库日期（业务日期，`yyyy-MM-dd`） */
  inbound_date: string;
  /** 库龄（天）= 基准日 − 入库日期；入库日期晚于基准日时为**负数**（后端不夹 0） */
  age_days: number;
  /** 本层入库数量（原始数量，恒正） */
  qty_in: number | string;
  /** 已被出库按 FIFO 吃掉的数量 */
  qty_consumed: number | string;
  /** 本层在基准日还剩的数量 */
  qty_remaining: number | string;
  /** 层单价（历史成本快照；**不是**账面移动加权单价） */
  unit_cost: number | string;
  /** 参考金额 = `qty_remaining × unit_cost`（**不是**账面结存金额） */
  amount_remaining: number | string;
  /** 过账时间（那笔 `IN` 流水的 `created_at`，RFC3339） */
  posted_at: string;
  /** 基准日（后端回显） */
  as_of: string;
}

/**
 * 批次层分页入参（逐字抄 `AgingFilter`）。
 *
 * * 所有字段留空 = 不加该条件；
 * * **`as_of` 留空 ≠ 不加条件**：留空取「今天（CST）」；
 * * `include_consumed` 用 `'true' | 'false'` 字符串：后端收的是 `String`
 *   （`true/false/1/0/yes/no/on/off`，其它值 400 + 66006）。
 */
export interface AgingLayerQuery {
  /** 物料 id（精确 UUID） */
  material_id?: string;
  /** 仓库 id（精确 UUID） */
  warehouse_id?: string;
  /** 批号（精确匹配；空串表示「不筛」，**不是**「筛没有批号的层」） */
  lot_no?: string;
  /** 基准日（`yyyy-MM-dd`，含当天；留空 = 今天 CST） */
  as_of?: string;
  /** 最小库龄（天，含） */
  min_age_days?: number | string;
  /** 最大库龄（天，含） */
  max_age_days?: number | string;
  /** 是否连「已吃满（剩 0）」的层一起返回（缺省不发 = 后端默认 `false`） */
  include_consumed?: 'true' | 'false';
  /** 页码（从 1 起） */
  page?: number;
  /** 每页条数（0 → 默认 20，上限 200） */
  page_size?: number;
}

// ==================== 分段报表（M2-09 · GET …/report） ====================

/** 档位定义（渲染表头用；逐字抄 `AgingBucketDef`） */
export interface AgingBucketDef {
  /** 档位键（`D0_30` / `D31_60` / `D61_90` / `OVER_90`） */
  key: string;
  /** 档位中文名（`0-30 天` … `90 天以上`） */
  label: string;
  /** 库龄下界（含） */
  min_days: number;
  /** 库龄上界（含）；`null` = 无上界 */
  max_days: number | null;
}

/** 一个档位在一行 / 汇总里的数量、金额、层数（逐字抄 `AgingBucketQtyRes`） */
export interface AgingBucketQty {
  /** 档位键 */
  key: string;
  /** 档位中文名 */
  label: string;
  /** 本档层数 */
  layer_count: number;
  /** 本档剩余数量合计 */
  qty: number | string;
  /** 本档参考金额合计（Σ 层剩余 × 层单价） */
  amount: number | string;
}

/** 报表里的一行 = 一个 物料 × 仓库 组合（逐字抄 `MaterialWarehouseAgingRes`） */
export interface InventoryAgingRow {
  /** 物料 id */
  material_id: string;
  /** 物料编码（软删后 `null`） */
  material_code: string | null;
  /** 物料名称（软删后 `null`） */
  material_name: string | null;
  /** 规格型号 */
  spec: string | null;
  /** 计量单位名 */
  unit_name: string | null;
  /** 仓库 id */
  warehouse_id: string;
  /** 仓库编码（软删后 `null`） */
  warehouse_code: string | null;
  /** 仓库名称（软删后 `null`） */
  warehouse_name: string | null;
  /** 本行层数 */
  layer_count: number;
  /** 本行剩余数量合计（= Σ 各档 `qty`） */
  qty_total: number | string;
  /** 本行参考金额合计（= Σ 各档 `amount`） */
  amount_total: number | string;
  /** 各档（按键位表顺序，缺档给 0） */
  buckets: AgingBucketQty[];
  /** 本行最老那层还剩货的入库日期 */
  oldest_inbound_date: string;
  /** 本行最大库龄（天） */
  max_age_days: number;
  /** 数量加权平均库龄（天，2 位小数） */
  avg_age_days: number | string;
}

/** 库龄分段报表出参（逐字抄 `InventoryAgingReportRes`） */
export interface InventoryAgingReport {
  /** 报表编码（`INVENTORY_AGING`） */
  report_code: string;
  /** 报表名称（`库存库龄分析表`） */
  report_name: string;
  /** 基准日（后端回显） */
  as_of: string;
  /** 基准日来源：`QUERY`（请求里传了 `as_of`）/ `TODAY`（留空 → 今天 CST） */
  as_of_source: 'QUERY' | 'TODAY';
  /** 档位定义（按钮表头用，**不写死 4 列**） */
  buckets: AgingBucketDef[];
  /** 全表各档合计（按键位表顺序） */
  summary: AgingBucketQty[];
  /** 全表剩余数量合计（跨物料时单位不同，只作总量参考） */
  qty_total: number | string;
  /** 全表参考金额合计 */
  amount_total: number | string;
  /** 全表层数 */
  layer_count: number;
  /** 有结存的 物料 × 仓库 行数（**截断前**） */
  row_count: number;
  /** 涉及物料数 */
  material_count: number;
  /** 涉及仓库数 */
  warehouse_count: number;
  /** 行上限 */
  row_limit: number;
  /** `rows` 是否被截断（`summary` / 总量不受影响） */
  rows_truncated: boolean;
  /** 明细行（最老货在前） */
  rows: InventoryAgingRow[];
  /** 口径提示（页面直接展示在表下方） */
  hints: string[];
}

/** 分段报表入参（逐字抄 `InventoryAgingReportQuery`；三个字段都可留空） */
export interface InventoryAgingReportQuery {
  /** 基准日（`yyyy-MM-dd`；留空 = 今天 CST） */
  as_of?: string;
  /** 物料 id（留空 = 全部物料） */
  material_id?: string;
  /** 仓库 id（留空 = 全部仓库） */
  warehouse_id?: string;
}

// ==================== 呆滞料清单（M2-10 · GET …/slow-moving） ====================

/** 呆滞料清单里的一行 = 一个 物料 × 仓库 组合（逐字抄 `SlowMovingRowRes`） */
export interface SlowMovingRow {
  /** 物料 id */
  material_id: string;
  /** 物料编码（软删后 `null`） */
  material_code: string | null;
  /** 物料名称（软删后 `null`） */
  material_name: string | null;
  /** 规格型号 */
  spec: string | null;
  /** 计量单位名 */
  unit_name: string | null;
  /** 仓库 id */
  warehouse_id: string;
  /** 仓库编码（软删后 `null`） */
  warehouse_code: string | null;
  /** 仓库名称（软删后 `null`） */
  warehouse_name: string | null;
  /** 本格有结存的层数（呆滞 + 不呆滞） */
  layer_count: number;
  /** 本格总剩余数量 */
  qty_on_hand: number | string;
  /** 本格总参考金额 */
  amount_on_hand: number | string;
  /** 本格呆滞层数 */
  slow_layer_count: number;
  /** 本格呆滞剩余数量合计（> 0，本表只返回有呆滞的行） */
  qty_slow: number | string;
  /** 本格呆滞参考金额合计 */
  amount_slow: number | string;
  /** 呆滞占本格结存数量的比例（4 位小数；分母 0 时为 0） */
  slow_ratio: number | string;
  /** 本格最老（还有剩余的）层的入库日期 */
  oldest_inbound_date: string;
  /** 本格最大库龄（天） */
  max_age_days: number;
}

/** 全表汇总（**截断前**统计，逐字抄 `SlowMovingSummaryRes`） */
export interface SlowMovingSummary {
  /** 有结存的格数（分母：全库扫过多少格） */
  scanned_row_count: number;
  /** 其中被判为呆滞的格数 */
  slow_row_count: number;
  /** 扫描到的层数 */
  layer_count: number;
  /** 其中呆滞层数 */
  slow_layer_count: number;
  /** 全库结存数量合计 */
  qty_on_hand: number | string;
  /** 全库呆滞数量合计 */
  qty_slow: number | string;
  /** 全库结存参考金额合计 */
  amount_on_hand: number | string;
  /** 全库呆滞参考金额合计 */
  amount_slow: number | string;
  /** 呆滞数量占全库结存的比例（4 位小数） */
  slow_ratio: number | string;
}

/** 呆滞料清单出参（逐字抄 `SlowMovingRes`） */
export interface SlowMovingReport {
  /** 报表编码（`INVENTORY_SLOW_MOVING`） */
  report_code: string;
  /** 报表名称（`呆滞料清单`） */
  report_name: string;
  /** 基准日（后端回显） */
  as_of: string;
  /** 基准日来源：`QUERY` / `TODAY` */
  as_of_source: 'QUERY' | 'TODAY';
  /** 生效阈值（天） */
  threshold_days: number;
  /** 阈值来源：`QUERY`（请求参数 days）/ `CONFIG`（系统参数）/ `DEFAULT`（缺省 90） */
  threshold_source: 'QUERY' | 'CONFIG' | 'DEFAULT';
  /** 阈值的系统参数键（前端「去改配置」的跳转锚点） */
  threshold_config_key: string;
  /** 全表汇总（截断前的全量） */
  summary: SlowMovingSummary;
  /** 呆滞行总数（**截断前**） */
  row_count: number;
  /** 行上限 */
  row_limit: number;
  /** `rows` 是否被截断（`summary` 不受影响） */
  rows_truncated: boolean;
  /** 呆滞明细行（呆滞金额降序） */
  rows: SlowMovingRow[];
  /** 口径提示（含「系统参数配了坏值」的告警） */
  hints: string[];
}

/** 呆滞料清单入参（逐字抄 `SlowMovingQuery`；四个字段都可留空） */
export interface SlowMovingQuery {
  /** 基准日（`yyyy-MM-dd`；留空 = 今天 CST） */
  as_of?: string;
  /** 物料 id（留空 = 全部物料） */
  material_id?: string;
  /** 仓库 id（留空 = 全部仓库） */
  warehouse_id?: string;
  /** 阈值天数（留空 = 系统参数 → 缺省 90） */
  days?: number | string;
}

// ==================== 三个只读接口 ====================

/**
 * 批次层明细分页（`GET /business/erp/inventory-aging/page`，权限
 * `business:erp:inventory-aging:list`）。
 *
 * 空串一律**不发**（后端把 `?lot_no=` 归一化成「不筛」，但把 `?material_id=` 当非法 UUID
 * 400）—— 拼参由 `aging-logic.buildLayerQuery` 负责，本函数只做一次兜底 `asParams`。
 */
export function fetchAgingLayers(query: AgingLayerQuery): Promise<PageData<AgingLayerRow>> {
  return get<PageData<AgingLayerRow>>(`${BASE}/inventory-aging/page`, asParams(query));
}

/**
 * 库龄分段报表（`GET /business/erp/inventory-aging/report`，权限
 * `business:erp:inventory-aging:report`）。
 *
 * 报表是**汇总**，不分页（要钻取用上面的批次层分页）。
 */
export function fetchAgingReport(query: InventoryAgingReportQuery = {}): Promise<InventoryAgingReport> {
  return get<InventoryAgingReport>(`${BASE}/inventory-aging/report`, asParams(query));
}

/**
 * 呆滞料清单（`GET /business/erp/inventory-aging/slow-moving`，权限
 * `business:erp:inventory-aging:slow-moving`）。
 *
 * 阈值三级：请求参数 `days` > 系统参数 `erp.inventory.slow-moving-days` > 缺省 90 天；
 * 生效值与来源由后端在 `threshold_days` / `threshold_source` 里回显，前端照实写。
 */
export function fetchSlowMoving(query: SlowMovingQuery = {}): Promise<SlowMovingReport> {
  return get<SlowMovingReport>(`${BASE}/inventory-aging/slow-moving`, asParams(query));
}

/** 丢掉空串 / null / undefined（后端把 `?x=` 当「等于空串」过滤，必须根本不发） */
function asParams<T extends object>(query: T): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === '') {
      continue;
    }
    out[key] = value;
  }
  return out;
}
