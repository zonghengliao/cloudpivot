/**
 * 凭证来源 → 前端路由映射表（**唯一真相**）—— VTRACE-1。
 *
 * # 解决什么问题
 *
 * `biz_voucher` 早就有 `source_type / source_id / source_no`，后端也有
 * `find_voucher_by_source()` / `reverse_by_source()`；但前端一直不知道该把
 * 「来源单据号」跳到哪个页面 —— 于是凭证详情里的来源单据只能显示成纯文本。
 * 本文件补的就是这一公里：**类型 → 目标页 + 用哪个参数**。
 *
 * # 表里的键 = 真正的凭证来源
 *
 * 键的类型是 {@link VoucherSourceType}（`@/api/business/erp/doc-kinds`，它是
 * Rust `VoucherSourceType` 枚举的逐字镜像，两边由后端单测
 * `frontend_ts_mirror_matches_rust` 钉死）**再加** {@link EXTRA_VOUCHER_SOURCES}。
 * 后者只放「后端确实写进 `biz_voucher.source_type`、但还没进枚举」的值 ——
 * 目前只有一个：`FX_REVALUATION`（`backend/src/business/erp/fx_revaluation.rs:58`
 * 直接落库，`doc_kind.rs` 的 `VOUCHER_SOURCE_VALUES` 里还没有它）。
 *
 * **不要在这里写别的词的映射。** 卡面（VTRACE-1 body）列的
 * `PURCHASE / RETURN / MATERIAL / LABOR / OVERHEAD / ALLOC / LABOR_TICKET /
 * OVERHEAD_TICKET / IMPORT / PRODUCTION / OTHER / DEPRECIATION_REVERSAL` 都**不是**
 * `biz_voucher.source_type` 的取值，它们属于别的轴（见 `doc-kinds.ts` 七轴表）：
 *
 * | 卡面词 | 实际落库列 | 出处 |
 * |---|---|---|
 * | `PURCHASE` / `RETURN` / `PRODUCTION` | `biz_material_lot.source_type`、`biz_material_serial.source_type` | `lot_dto.rs:58-66`、`serial_model.rs:157` |
 * | `MATERIAL` / `LABOR` / `OVERHEAD` / `ALLOC` | 成本归集行（`biz_cost_*` 归集来源） | `cost_collect_dto.rs:45`、`cost_labor_collect_dto.rs:53` |
 * | `LABOR_TICKET` / `OVERHEAD_TICKET` / `IMPORT` / `DEPRECIATION_REVERSAL` | 同上（成本归集的来源子类型） | `cost_labor_dto.rs:70-76`、`cost_overhead_dto.rs:84-88` |
 * | `EXPENSE` | 成本归集来源；**凭证来源**是 `EXPENSE_REPORT` / `EXPENSE_PAYMENT` | `cost_overhead_collect_dto.rs:53`、`expense_bridge.rs:51-53` |
 *
 * 给它们硬造一条凭证跳转 = 跳到错页面，所以不做；写在这里是为了下一个人不用再查一遍。
 *
 * # 三种落地程度（{@link DeepLink}）
 *
 * 目标页**有没有真读这个 query** 是逐个页面核实过的，不靠「看起来像」：
 *
 * - `detail` —— 页面读到参数会直接打开该单据（`in_id` / `year` / `batch_no`）；
 * - `filter` —— 页面只把它当筛选条件（`out_no` 走列表 keyword）；
 * - `none` —— 页面还没读这个参数，`resolveSourceRoute` **不往 query 里塞值**
 *   （塞了就是个静默无效的假参数），只返回列表页路径；
 * - `page-missing` —— 目标页连路由都还没注册（**当前无成员**：最后两个 `PAYROLL_ACCRUAL` /
 *   `PAYROLL_PAYMENT` 指向的薪资列表页已由卡 t_fc256a30 落地，改成了 `filter`），
 *   这种情况下 `resolveSourceRoute` 返回 `null` —— **宁可不给链接，不给 404 链接**。
 *
 * 页面补深链后，把对应行的 `deepLink` 从 `none` 改成 `filter`/`detail` 即可，
 * 这张表是唯一要动的地方（`pnpm test:unit` 里的 vtrace-source-route 会跟着绿）。
 *
 * # 用法
 *
 * ```ts
 * const route = resolveSourceRoute(voucher.source_type, voucher.source_id, voucher.source_no)
 * if (route) router.push({ path: route.path, query: route.query })
 * // route === null ⇒ 保持纯文本（MANUAL / 来源单据缺失 / 目标页未落地）
 * ```
 */

import type { VoucherSourceType } from '@/api/business/erp/doc-kinds';

/** 后端已落库、但还没进 `VoucherSourceType` 枚举的来源值（改动前先看模块头注释）。 */
export const EXTRA_VOUCHER_SOURCES = ['FX_REVALUATION'] as const;

/** 额外来源（不是 Rust 枚举里的成员）。 */
export type ExtraVoucherSource = (typeof EXTRA_VOUCHER_SOURCES)[number];

/** 这张表覆盖的全部来源键。 */
export type SourceDocType = VoucherSourceType | ExtraVoucherSource;

/**
 * 目标页读参数的深度（见文件头「三种落地程度」）。
 *
 * - `detail`：直接打开单据详情
 * - `filter`：把参数当筛选条件
 * - `none`：暂不读（不往 query 塞值）
 * - `page-missing`：目标页还没注册路由 ⇒ 整条返回 `null`
 */
export type DeepLink = 'detail' | 'filter' | 'none' | 'page-missing';

/** 一行映射。 */
export interface SourceRouteSpec {
  /** 目标页路由 path（`/business/erp/...`，与 `router/index.ts` 逐字一致）。 */
  readonly path: string;
  /** 用凭证哪个字段当参数值：`id` = `source_id`(uuid)、`no` = `source_no`、`none` = 不传。 */
  readonly by: 'id' | 'no' | 'none';
  /** 目标页的查询参数名（即便 `deepLink='none'` 也写上 —— 这是「该补哪个参数」的清单）。 */
  readonly queryKey: string | null;
  /** 目标页对参数的支持程度。 */
  readonly deepLink: DeepLink;
  /** 从原始值里提炼参数（返回 `null` = 这一行这次跳不了）。 */
  readonly format?: (raw: string) => string | null;
  /** 人在界面上看到的目标名（tooltip / aria-label）。 */
  readonly label: string;
  /** 为什么是这个页面 / 为什么暂不可跳。 */
  readonly note: string;
}

/**
 * 来源 → 目标页。**顺序 = `VoucherSourceType::ALL` 顺序 + 额外项**，便于对表。
 */
export const SOURCE_ROUTE_TABLE = {
  // ── ① 库存 / 成本链 ────────────────────────────────────────────────
  /** `voucher.rs:1553` `source_id = biz_stock_in.id`；页面读 `?in_id=`（`stockin/index.vue:2002`）。 */
  STOCK_IN: {
    path: '/business/erp/stock-in',
    by: 'id',
    queryKey: 'in_id',
    deepLink: 'detail',
    label: '收货入库单',
    note: '收货单详情直开（in_id）；从详情里可再上溯采购订单',
  },
  /** `voucher.rs:2696` `source_id = biz_stock_out.id`；页面只把 `?out_no=` 当 keyword（`stock-out/index.vue:1375`）。 */
  STOCK_OUT: {
    path: '/business/erp/stock-out',
    by: 'no',
    queryKey: 'out_no',
    deepLink: 'filter',
    label: '出库单',
    note: '出库页按单号筛（半深链：定位到那一行，不自动开详情）',
  },
  /** `payable_payment.rs:1196` `source_id = biz_payable_payment.id`、`source_no = payment_no`。 */
  PAYMENT: {
    path: '/business/erp/payable',
    by: 'no',
    queryKey: 'payment_no',
    deepLink: 'none',
    label: '应付单（付款记录）',
    note: '付款台账在应付页内；该页当前只认 `payable_no`，付款单号深链待补',
  },
  /** `stocktake_model.rs:1234` `source_id = biz_stock_take.id`。 */
  STOCK_TAKE: {
    path: '/business/erp/stock-take',
    by: 'id',
    queryKey: 'take_id',
    deepLink: 'none',
    label: '盘点单',
    note: '盘点页未读 take_id ⇒ 只落到列表',
  },
  /** `serial_model.rs:1450` `source_id = biz_material_serial.id`、`source_no = serial_no`。 */
  SERIAL_BATCH: {
    path: '/business/erp/serial',
    by: 'no',
    queryKey: 'serial_no',
    deepLink: 'none',
    label: '序列号档案',
    note: '序列号页未读 serial_no ⇒ 只落到列表',
  },
  /** 同 `SERIAL_BATCH`，来源是「序列号出库结转成本」。 */
  SERIAL_OUT: {
    path: '/business/erp/serial',
    by: 'no',
    queryKey: 'serial_no',
    deepLink: 'none',
    label: '序列号档案',
    note: '序列号页未读 serial_no ⇒ 只落到列表',
  },
  /** 同 `SERIAL_BATCH`，来源是「序列号报废」。 */
  SERIAL_SCRAP: {
    path: '/business/erp/serial',
    by: 'no',
    queryKey: 'serial_no',
    deepLink: 'none',
    label: '序列号档案',
    note: '序列号页未读 serial_no ⇒ 只落到列表',
  },

  // ── ② 结转 / 期初 / 年末 ───────────────────────────────────────────
  /** `opening.rs:808` `source_id = None`、`source_no = "OPENING-<期间码>"` —— **只能按单号跳**。 */
  OPENING: {
    path: '/business/erp/opening',
    by: 'no',
    queryKey: 'period',
    deepLink: 'none',
    /** `OPENING-2026-01` → `2026-01`（期初页按期间看）。 */
    format: (no) => trimPrefix(no, 'OPENING-'),
    label: '期初余额',
    note: '期初页未读 period ⇒ 只落到列表',
  },
  /** `year_end.rs:1301` `source_id = None`、`source_no = "YEAR-END-<年份>"`。 */
  YEAR_END: {
    path: '/business/erp/year-end',
    by: 'no',
    queryKey: 'year',
    deepLink: 'detail',
    /** `YEAR-END-2026` → `2026`；年结页 `?year=` 只预选年度（`year-end/index.vue:369`）。 */
    format: (no) => {
      const year = trimPrefix(no, 'YEAR-END-');
      return year && /^[0-9]{4}$/.test(year) ? year : null;
    },
    label: '年结结转',
    note: '年结页按年度预选（?year=）',
  },

  // ── ③ 手工 / 无来源 ───────────────────────────────────────────────
  /** 手工凭证：**没有来源单据**，永远不跳（`doc_kind.rs` 里 `Manual` 也不映射业务对象）。 */
  MANUAL: {
    path: '',
    by: 'none',
    queryKey: null,
    deepLink: 'none',
    label: '手工凭证',
    note: '手工录入，无来源单据 ⇒ 不可跳',
  },

  // ── ④ 固定资产 / 折旧 ─────────────────────────────────────────────
  /** `asset_handler.rs:600` `source_id = biz_fixed_asset.id`。 */
  FIXED_ASSET_ACQ: {
    path: '/business/erp/asset',
    by: 'id',
    queryKey: 'asset_id',
    deepLink: 'none',
    label: '资产卡片',
    note: '资产页未读 asset_id ⇒ 只落到列表',
  },
  /** `asset_bridge.rs:395` `source_id = biz_fixed_asset.id`（处置：出售 / 报废 / 盘亏）。 */
  FIXED_ASSET_DISPOSAL: {
    path: '/business/erp/asset',
    by: 'id',
    queryKey: 'asset_id',
    deepLink: 'none',
    label: '资产卡片',
    note: '资产页未读 asset_id ⇒ 只落到列表',
  },
  /** `depreciation_model.rs:456` `source_id = biz_depreciation_batch.id`。 */
  DEPRECIATION: {
    path: '/business/erp/depreciation',
    by: 'id',
    queryKey: 'batch_id',
    deepLink: 'none',
    label: '折旧批次',
    note: '折旧页未读 batch_id ⇒ 只落到列表',
  },

  // ── ⑤ 费用报销 ───────────────────────────────────────────────────
  /** `expense_bridge.rs:433` `source_id = biz_expense_report.id`（入账）。 */
  EXPENSE_REPORT: {
    path: '/business/erp/expense',
    by: 'id',
    queryKey: 'report_id',
    deepLink: 'none',
    label: '费用报销单',
    note: '报销页未读 report_id ⇒ 只落到列表',
  },
  /** `expense_bridge.rs:466` `source_id = biz_expense_report.id`、`source_no = payment_no`（付款）。 */
  EXPENSE_PAYMENT: {
    path: '/business/erp/expense',
    by: 'id',
    queryKey: 'report_id',
    deepLink: 'none',
    label: '费用报销单（付款）',
    note: '付款凭证也挂报销单；报销页未读 report_id ⇒ 只落到列表',
  },

  // ── ⑥ 销售链 ─────────────────────────────────────────────────────
  /** `receipt.rs:1639` `source_id = biz_receipt.id`。 */
  RECEIPT: {
    path: '/business/erp/receipt',
    by: 'id',
    queryKey: 'receipt_id',
    deepLink: 'none',
    label: '收款单',
    note: '收款页未读 receipt_id ⇒ 只落到列表',
  },
  /** `receipt_refund.rs:2229` `source_id = biz_receipt_refund.id`。 */
  RECEIPT_REFUND: {
    path: '/business/erp/receipt-refund',
    by: 'id',
    queryKey: 'refund_id',
    deepLink: 'none',
    label: '收款退款单',
    note: '退款页未读 refund_id ⇒ 只落到列表',
  },
  /** `voucher.rs:3073` `source_id = biz_invoice.id`。 */
  INVOICE: {
    path: '/business/erp/invoice',
    by: 'id',
    queryKey: 'invoice_id',
    deepLink: 'none',
    label: '销售发票',
    note: '发票页未读 invoice_id ⇒ 只落到列表',
  },
  /** `commission_handler.rs:909` `source_id = biz_commission_settlement.id`、`source_no = 结算单号`。 */
  COMMISSION: {
    path: '/business/erp/commission',
    by: 'no',
    queryKey: 'settlement_no',
    deepLink: 'none',
    label: '佣金结算单',
    note: '佣金页未读 settlement_no ⇒ 只落到列表',
  },

  // ── ⑦ 成本核算 ───────────────────────────────────────────────────
  /** `cost_settle_voucher.rs:89` `source_id = biz_cost_period.id`、`source_no = CS-yyyyMM-NNNN`。 */
  COST_PERIOD: {
    path: '/business/erp/cost-period',
    by: 'no',
    queryKey: 'batch_no',
    deepLink: 'detail',
    label: '成本核算批次',
    note: '成本核算页读 `?batch_no=` 直接打开该批次的五步向导（COST-B3-11 落地，2026-11-11）',
  },

  // ── ⑨ 人力资源（ARCH-9-2 §4.6，HR-06）──────────────────────────────
  /**
   * `salary_bridge` 过账生成的**计提**凭证：`source_id = biz_salary_sheet.id`、
   * `source_no = SAL-yyyyMM-NNNN`（与发放凭证共用同一张薪资单）。
   */
  PAYROLL_ACCRUAL: {
    path: '/business/hr/salary',
    by: 'no',
    queryKey: 'sheet_no',
    deepLink: 'filter',
    label: '薪资单（工资计提）',
    note: '薪资列表页读 `?sheet_no=` 当单号筛选并高亮（卡 t_fc256a30 落地，2026-11-13）',
  },
  /** 同一次过账的**发放**凭证（`source_id` 相同，靠摘要 + 科目区分）。 */
  PAYROLL_PAYMENT: {
    path: '/business/hr/salary',
    by: 'no',
    queryKey: 'sheet_no',
    deepLink: 'filter',
    label: '薪资单（工资发放）',
    note: '薪资列表页读 `?sheet_no=` 当单号筛选并高亮（卡 t_fc256a30 落地，2026-11-13）',
  },

  // ── ⑧ 额外：后端已在写、枚举还没收录 ──────────────────────────────
  /** `fx_revaluation.rs:496` `source_id = None`、`source_no = 期间码`（结账流程内触发）。 */
  FX_REVALUATION: {
    path: '/business/erp/period',
    by: 'no',
    queryKey: 'period_code',
    deepLink: 'none',
    label: '会计期间（期末调汇）',
    note: '重估随期间结账触发（无独立页面）；期间页未读 period_code ⇒ 只落到列表',
  },
} as const satisfies Readonly<Record<SourceDocType, SourceRouteSpec>>;

/** 单个来源的解析结果。 */
export interface SourceRoute {
  /** 目标页 path。 */
  path: string;
  /** 目标页可用的查询参数（`deepLink='none'` 时恒为空对象）。 */
  query: Record<string, string>;
  /** 支持程度，供调用方决定「给链接 / 给灰字 / 给 tooltip」。 */
  deepLink: DeepLink;
  /** 目标名的中文（tooltip：「收货入库单」）。 */
  label: string;
}

/** `no` 必须以 `prefix` 开头才摘前缀；不匹配返回 `null`（宁可不跳，不猜）。 */
function trimPrefix(raw: string, prefix: string): string | null {
  const text = (raw ?? '').trim();
  return text.startsWith(prefix) ? text.slice(prefix.length).trim() || null : null;
}

/**
 * 解析「凭证来源 → 前端路由」。
 *
 * @param type  `biz_voucher.source_type`（未知值返回 `null`，绝不猜页面）
 * @param id    `biz_voucher.source_id`（uuid；`by='id'` 的映射必填）
 * @param no    `biz_voucher.source_no`（单号；`by='no'` 的映射必填）
 * @returns `{path, query, deepLink, label}`；不可跳时 `null`
 *
 * 返回 `null` 的四种情形（调用方一律渲染纯文本，不给假链接）：
 * 1. `MANUAL` —— 手工凭证没有来源单据；
 * 2. 未知 / 空 `type`；
 * 3. 目标页尚未落地（`deepLink='page-missing'`）；
 * 4. 该映射需要的参数缺失（`by='id'` 但 `id` 为空 / `by='no'` 但 `no` 为空或格式不对）。
 */
export function resolveSourceRoute(
  type: string | null | undefined,
  id?: string | null,
  no?: string | null,
): SourceRoute | null {
  const key = (type ?? '').trim().toUpperCase();
  if (!key) {
    return null;
  }
  if (!isSourceDocType(key)) {
    return null;
  }
  const spec: SourceRouteSpec = SOURCE_ROUTE_TABLE[key];
  if (spec.deepLink === 'page-missing' || spec.by === 'none' || !spec.queryKey) {
    // MANUAL / 页面未落地：不给链接（`page-missing` 连列表页都不给，避免 404）。
    return null;
  }
  const raw = (spec.by === 'id' ? id : no) ?? null;
  if (!raw || !String(raw).trim()) {
    return null;
  }
  let value = String(raw).trim();
  if (spec.format) {
    const formatted = spec.format(value);
    if (!formatted) {
      return null;
    }
    value = formatted;
  }
  return {
    path: spec.path,
    // 页面没读这个参数时不塞（静默无效的假参数比没有更坏）。
    query: spec.deepLink === 'none' ? {} : { [spec.queryKey]: value },
    deepLink: spec.deepLink,
    label: spec.label,
  };
}

/** 类型守卫：`type` 是不是本表覆盖的来源（含额外项）。 */
export function isSourceDocType(type: string): type is SourceDocType {
  return Object.prototype.hasOwnProperty.call(SOURCE_ROUTE_TABLE, type);
}
