/**
 * 「未归集清单」的类型与纯逻辑（COST-B2-12 / ARCH-9 §5.2 #33 · §6.2.2 ②）。
 *
 * 后端：`backend/src/business/erp/cost_uncollected_{dto,model,handler}.rs`。
 * 两侧契约是**机器码 + 字符串金额**，本文件是前端唯一的落点：
 *
 * | 后端常量 | 值 | 前端用途 |
 * |---|---|---|
 * | `KIND_STOCK_OUT` | `STOCK_OUT_WITHOUT_OBJECT` | 块标题 / 图标 / 表格列 |
 * | `KIND_LABOR_MISSING` | `LABOR_MISSING` | 同上 |
 * | `KIND_LABOR_RATE` | `LABOR_RATE_MISSING` | 同上 |
 * | `KIND_POOL` | `POOL_UNALLOCATED` | 同上 |
 * | `REASON_NO_RULE` / `NO_OBJECT` / `NOT_ALLOCATED` | 池缺口三态 | 标签颜色 + 提示 |
 *
 * # 三条不能想当然的口径
 *
 * 1. **金额一律 `string`**（后端 `NUMERIC(18,4)` → JSON 字符串），要显示就 `formatMoney`，
 *    要累加就用 `sumDecimal` 这类定点函数，别 `Number()`；
 * 2. **`count` 是真实条数，`rows` 可能被截断**（后端每块上限 200 行）——
 *    `truncated` 为真时必须提示「只显示前 N 条」，否则用户会以为缺口只有这么多；
 * 3. **`jump` 是前端路由串**（后端按 `PATH_*` 常量拼好），`jump === null` 表示
 *    「动作就在本页」（例如池缺口「未摊完」⇒ 回本页点计算），不要渲染成空链接。
 */

/** 块的机器码（与后端 `cost_uncollected_dto::KIND_*` 逐字一致） */
export const GAP_KIND = {
  stockOut: 'STOCK_OUT_WITHOUT_OBJECT',
  laborMissing: 'LABOR_MISSING',
  laborRate: 'LABOR_RATE_MISSING',
  pool: 'POOL_UNALLOCATED',
} as const;

/** 块机器码的取值 */
export type GapKind = (typeof GAP_KIND)[keyof typeof GAP_KIND];

/** 池缺口原因（与后端 `REASON_*` 逐字一致） */
export type PoolReason = 'NO_RULE' | 'NO_OBJECT' | 'NOT_ALLOCATED';

/** 一块缺口（四块同构，`rows` 的元素类型随块变） */
export interface GapBlock<T> {
  /** 机器码 */
  kind: string;
  /** 中文块名 */
  title: string;
  /** 真实条数（不受 `rows` 截断影响） */
  count: number;
  /** 金额合计（字符串，4 位小数） */
  amount_total: string;
  /** 缺费率块的受影响工时合计（其它块恒 `"0"`） */
  hours_total: string;
  /** 这块怎么修 */
  hint: string;
  /** 整块跳转（`null` = 动作在本页） */
  jump: string | null;
  /** `rows` 是否被截断 */
  truncated: boolean;
  /** 行（最多 200 条） */
  rows: T[];
}

/** ① 一张未挂成本对象的已过账领料单 */
export interface StockOutGapRow {
  /** 来源单据 id（点击跳转用） */
  stock_out_id: string;
  /** 出库单号 */
  doc_no: string;
  /** 出库日期 `yyyy-MM-dd` */
  doc_date: string;
  /** 出库类型（恒 `ISSUE`） */
  out_type: string;
  /** 出库仓库名 */
  warehouse_name: string | null;
  /** 明细行数 */
  item_count: number;
  /** 数量合计 */
  qty_total: string;
  /** 金额合计 */
  amount_total: string;
  /** 怎么修（单行） */
  hint: string;
  /** 单行跳转（领料单列表按单号定位） */
  jump: string;
}

/** ② 一个缺工时的在产对象 */
export interface LaborMissingGapRow {
  /** 成本对象 id */
  cost_object_id: string;
  /** 对象号 */
  object_no: string;
  /** 本期草稿工时行数（> 0 ⇒ 已录未确认） */
  draft_labor_rows: number;
  /** 本期材料投入 */
  material_amount: string;
  /** 怎么修 */
  hint: string;
  /** 跳转（工时台账，按期间 + 对象筛） */
  jump: string;
}

/** ③ 一条缺费率的已确认工时行 */
export interface LaborRateGapRow {
  /** 工时台账行 id */
  labor_id: string;
  /** 成本对象 id */
  cost_object_id: string;
  /** 对象号 */
  cost_object_no: string | null;
  /** 员工姓名快照 */
  employee_name: string;
  /** 工作中心 id */
  dept_id: string;
  /** 工作中心名 */
  dept_name: string | null;
  /** 工作日期 `yyyy-MM-dd` */
  work_date: string;
  /** 未归集工时 */
  hours: string;
  /** 台账上的工资率（`0` = 未维护） */
  rate: string;
  /** 为什么没归集（后端口径文案） */
  reason: string;
  /** 怎么修（费率三级取数建议） */
  hint: string;
  /** 跳转 */
  jump: string;
}

/** ④ 一个未摊完的 (车间, 科目) 费用池 */
export interface PoolGapRow {
  /** 车间 id */
  dept_id: string;
  /** 车间名 */
  dept_name: string;
  /** 费用科目编码 */
  account_code: string;
  /** 科目名 */
  account_name: string | null;
  /** 「4101.01 制造费用-折旧费」 */
  account_label: string;
  /** 池总额 */
  pool_amount: string;
  /** 已分摊合计 */
  allocated_amount: string;
  /** 未摊余额 */
  remaining_amount: string;
  /** 池里的来源行数 */
  source_rows: number;
  /** 缺口原因 */
  reason: PoolReason;
  /** 原因中文标签 */
  reason_label: string;
  /** 怎么修 */
  hint: string;
  /** 跳转（`null` = 回本页重算） */
  jump: string | null;
}

/** 当期成本批次摘要（没有批次时整体为 `null`） */
export interface CostBatchBrief {
  /** 批次 id */
  id: string;
  /** 批次号 */
  batch_no: string;
  /** 批次状态 */
  status: string;
}

/** 「未归集清单」出参（`GET /cost-period/uncollected` 与 `/{id}/uncollected` 同形） */
export interface CostUncollectedRes {
  /** 期间 `yyyy-MM` */
  period_code: string;
  /** 期间首日 */
  period_start: string;
  /** 期间末日 */
  period_end: string;
  /** 当期活跃批次 */
  batch: CostBatchBrief | null;
  /** 期间已结算 / 已结账（缺口降级为历史解释） */
  settled: boolean;
  /** 四块条数合计 */
  gap_count: number;
  /** ① 未挂成本对象的领料 */
  stock_out_without_object: GapBlock<StockOutGapRow>;
  /** ② 缺工时的在产对象 */
  labor_missing: GapBlock<LaborMissingGapRow>;
  /** ③ 缺费率的工时行 */
  labor_rate_missing: GapBlock<LaborRateGapRow>;
  /** ④ 未摊完的费用池 */
  pool_unallocated: GapBlock<PoolGapRow>;
}

/** 四块的固定顺序（页面按它渲染，后端加块也不会打乱既有顺序） */
export const GAP_BLOCK_ORDER: GapKind[] = [
  GAP_KIND.stockOut,
  GAP_KIND.laborMissing,
  GAP_KIND.laborRate,
  GAP_KIND.pool,
];

/** 块标题（后端也会给 `title`，这里只给「块跳转按钮」与徽标配色用不到标题时的兜底） */
export function gapKindTitle(kind: string): string {
  switch (kind) {
    case GAP_KIND.stockOut:
      return '未挂成本对象的领料';
    case GAP_KIND.laborMissing:
      return '缺工时的在产对象';
    case GAP_KIND.laborRate:
      return '缺费率的工时';
    case GAP_KIND.pool:
      return '未摊完的费用池';
    default:
      return kind;
  }
}

/** 块徽标配色（TDesign `t-tag` 的 theme） */
export function gapKindTheme(kind: string): 'danger' | 'warning' | 'primary' | 'default' {
  switch (kind) {
    case GAP_KIND.stockOut:
      return 'danger';
    case GAP_KIND.laborMissing:
    case GAP_KIND.laborRate:
      return 'warning';
    case GAP_KIND.pool:
      return 'primary';
    default:
      return 'default';
  }
}

/** 池原因配色 */
export function poolReasonTheme(reason: string): 'danger' | 'warning' | 'primary' {
  if (reason === 'NO_RULE') {
    return 'danger';
  }
  return reason === 'NO_OBJECT' ? 'warning' : 'primary';
}

/** 顶部汇总文案（`0 N 处缺口` / 已结算时说明降级为历史解释） */
export function summaryText(res: CostUncollectedRes): string {
  if (res.gap_count === 0) {
    return res.settled ? '本期已结算，且没有未归集项' : '本期没有未归集项，可以放心计算';
  }
  const base = `本期有 ${res.gap_count} 处未归集项`;
  return res.settled
    ? `${base}（本期批次已结算，这些只是历史解释，不再影响成本）`
    : `${base}：可以带着缺口计算，但结算前必须有解释`;
}

/** 块表尾文案（截断时点名「只显示前 N 条」） */
export function blockFooterText(block: GapBlock<unknown>): string {
  if (block.count === 0) {
    return '无';
  }
  if (block.truncated) {
    return `共 ${block.count} 条，此处只显示前 ${block.rows.length} 条，请按「查看全部」去原页处理`;
  }
  return `共 ${block.count} 条`;
}

/** 一批缺口行里要跳转的目标（`null` = 该行没有外跳） */
export function rowJump(row: { jump?: string | null }): string | null {
  const jump = row.jump ?? null;
  return jump && jump.trim() ? jump : null;
}
