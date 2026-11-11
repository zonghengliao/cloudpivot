/**
 * 单据详情页「查看凭证」的**唯一取数口径** —— VTRACE-4。
 *
 * # 解决什么问题
 *
 * `biz_voucher` 有 `source_type / source_id`，后端也有了按来源反查的接口
 * （`GET /erp/voucher/by-source`，VTRACE-3）；缺的是「**这张单据该拿哪些
 * (source_type, source_id) 去查**」。这件事**不能**让六个页面各写一遍：
 *
 * | 单据 | 凭证挂在哪 | 本文件怎么给 |
 * |---|---|---|
 * | 收货入库 | 本单（`STOCK_IN` / `biz_stock_in.id`） | {@link stockInSources} |
 * | 出库单 | 本单（`STOCK_OUT` / `biz_stock_out.id`） | {@link stockOutSources} |
 * | 采购订单 | **派生**：本单生成的收货单（`STOCK_IN`） | {@link purchaseOrderSources} |
 * | 销售订单 | **派生**：发货过账生成的销售出库单（`STOCK_OUT`） | {@link salesOrderSources} |
 * | 费用报销 | **多来源**：入账 `EXPENSE_REPORT` + 付款 `EXPENSE_PAYMENT`，**同一个 id** | {@link expenseReportSources} |
 * | 折旧批次 | 本单（`DEPRECIATION` / `biz_depreciation_batch.id`） | {@link depreciationSources} |
 *
 * ## 为什么采购单 / 销售订单要「派生」
 *
 * `PURCHASE_ORDER` / `SALE_ORDER` **不是** `biz_voucher.source_type` 的取值
 * （见 `doc-kinds.ts` ⑤「凭证来源」轴；映射表 `source-route.ts` 里也不该有它们）。
 * 采购单过账时不直接产生凭证 —— 是它生成的**收货单**过账才产生（`stockin_handler_post.rs:955`
 * 一次事务里生成凭证 + 应付单）；销售订单同理，凭证在发货过账生成的**销售出库单**上。
 * 所以「采购单详情 → 查看凭证」必须顺着 `source_order_id` 找到收货单再查，
 * 否则查出来永远是 0 张。
 *
 * ## 为什么报销单是多来源
 *
 * 审批通过时生成入账凭证（`source_type = EXPENSE_REPORT`），之后每笔付款再生成
 * 付款凭证（`source_type = EXPENSE_PAYMENT`），**两张的 `source_id` 都是报销单 id**
 * （`expense_bridge.rs:433` / `:466`）。只查一个类型必然漏另一张，所以这里返回两条来源。
 *
 * # 本文件只做纯计算
 *
 * 不发请求、不碰路由：查询与展示在 `VoucherTraceButton.vue`，跳转复用
 * `source-route.ts` 的目标页（凭证页深链 `?voucher_no=`，与应付页 `openVoucherDoc` 同口径）。
 * 于是「六张单据口径一致」由类型 + 单测钉住，而不是靠人记得同步改六处。
 */

import type { PendingReceiveStockIn, VoucherRow } from '@/api/business/erp';
import type { Shipment } from '@/api/business/erp/shipment';
import { SOURCE_ROUTE_TABLE, isSourceDocType, type SourceDocType } from './source-route';
import { formatMoney } from './money';

/**
 * 一条「该去反查凭证的来源」。
 *
 * `type` + `id` 是**请求参数**（`biz_voucher.source_type / source_id` 的匹配键），
 * `label` / `docNo` 只用于**弹窗里给人看**（「这 N 张分别来自哪张单」）。
 */
export interface VoucherSourceRef {
  /** `biz_voucher.source_type`（只能是 `source-route.ts` 覆盖的值域，否则后端 400） */
  readonly type: SourceDocType;
  /** `biz_voucher.source_id`（对应业务单据主键；空串的行会被 {@link normalizeSources} 丢掉） */
  readonly id: string;
  /** 来源单据号（展示用；`null` = 该来源没有单号，比如部分 OMS 导出） */
  readonly docNo: string | null;
  /** 来源单据的中文名（展示用，如「收货入库单」） */
  readonly label: string;
}

/** `docNo` 空值的统一处理（`undefined` / 空白串一律 `null`，避免弹窗里出现空列）。 */
function docNoOf(raw: string | null | undefined): string | null {
  const text = (raw ?? '').trim();
  return text || null;
}

/** `id` 必须非空 —— 空 id 打过去后端会 400（uuid 解析失败），不如这里就不查。 */
function idOf(raw: string | null | undefined): string {
  return (raw ?? '').trim();
}

// ==================== 六个单据的取数口径 ====================

/**
 * 收货入库单：凭证直接挂本单（`STOCK_IN`）。
 *
 * 口径出处：`voucher.rs:1553` `source_type = SOURCE_STOCK_IN`、`source_id = biz_stock_in.id`。
 * 过账事务里同时建应付单（`stockin_handler_post.rs:955`），但应付单**不另生成**凭证 ——
 * 它的 `voucher_id` 指的就是这张入库凭证，所以这里只有一条来源，不会出现「应付凭证」。
 */
export function stockInSources(
  id: string | null | undefined,
  inNo?: string | null,
): VoucherSourceRef[] {
  const key = idOf(id);
  return key ? [{ type: 'STOCK_IN', id: key, docNo: docNoOf(inNo), label: STOCK_IN_LABEL }] : [];
}

/** 出库单：凭证直接挂本单（`STOCK_OUT`），只有过账（成本结转）才会产生。 */
export function stockOutSources(
  id: string | null | undefined,
  outNo?: string | null,
): VoucherSourceRef[] {
  const key = idOf(id);
  return key ? [{ type: 'STOCK_OUT', id: key, docNo: docNoOf(outNo), label: STOCK_OUT_LABEL }] : [];
}

/**
 * 采购订单（**派生型**）：本单生成的每一张收货单 → 一条 `STOCK_IN` 来源。
 *
 * 入参就是详情页「收货进度」那块已经在用的 `pending.stock_ins`（`pending-items` 接口下发，
 * 未软删、新单在前）——**不再多发一次请求**。一张采购单分多次收货就有多张凭证，
 * 全部列出（采购单本身永远查不到凭证）。
 */
export function purchaseOrderSources(stockIns: readonly PendingReceiveStockIn[]): VoucherSourceRef[] {
  return normalizeSources(
    stockIns.map((row) => ({
      type: 'STOCK_IN' as const,
      id: row.id,
      docNo: row.in_no,
      label: STOCK_IN_LABEL,
    })),
  );
}

/**
 * 销售订单（**派生型**）：本单发货过账生成的销售出库单 → 一条 `STOCK_OUT` 来源。
 *
 * `stock_out_id` 为 `null` = 这张发货单还没过账（没扣库存、没结转成本 ⇒ 也不会有凭证），
 * 直接跳过，不发无意义的查询。
 */
export function salesOrderSources(shipments: readonly Shipment[]): VoucherSourceRef[] {
  return normalizeSources(
    shipments.map((row) => ({
      type: 'STOCK_OUT' as const,
      // 未过账为 null ⇒ 归一成空串，由 normalizeSources 丢掉（不发无意义查询）
      id: row.stock_out_id ?? '',
      docNo: row.stock_out_no,
      label: STOCK_OUT_LABEL,
    })),
  );
}

/**
 * 费用报销单（**多来源型**）：入账凭证 + 每笔付款的付款凭证。
 *
 * 两条来源的 `id` 相同（都是 `biz_expense_report.id`），只有 `source_type` 不同 ——
 * 所以调用方会并发查两次，`mergeVoucherRows` 再按凭证 id 去重（同一张凭证不会出现两行）。
 */
export function expenseReportSources(
  id: string | null | undefined,
  reportNo?: string | null,
): VoucherSourceRef[] {
  const key = idOf(id);
  if (!key) {
    return [];
  }
  const docNo = docNoOf(reportNo);
  return [
    { type: 'EXPENSE_REPORT', id: key, docNo, label: EXPENSE_REPORT_LABEL },
    { type: 'EXPENSE_PAYMENT', id: key, docNo, label: EXPENSE_PAYMENT_LABEL },
  ];
}

/** 折旧批次：凭证挂本批次（`DEPRECIATION` / `biz_depreciation_batch.id`）。 */
export function depreciationSources(
  batchId: string | null | undefined,
  batchNo?: string | null,
): VoucherSourceRef[] {
  const key = idOf(batchId);
  return key ? [{ type: 'DEPRECIATION', id: key, docNo: docNoOf(batchNo), label: DEPRECIATION_LABEL }] : [];
}

// ==================== 归集 / 去重 / 排序 ====================

const STOCK_IN_LABEL = '收货入库单';
const STOCK_OUT_LABEL = '销售出库单';
const EXPENSE_REPORT_LABEL = '费用报销单（入账）';
const EXPENSE_PAYMENT_LABEL = '费用报销单（付款）';
const DEPRECIATION_LABEL = '折旧批次';

/** 来源的稳定键（`type + id`）：用于「参数没变就不重复请求」的判定。 */
export function sourceKey(source: VoucherSourceRef): string {
  return `${source.type}:${source.id}`;
}

/** `sources` 的稳定键串（`sources` 是数组，直接 watch 引用会因每次 computed 重建而抖动）。 */
export function sourcesKey(sources: readonly VoucherSourceRef[]): string {
  return normalizeSources(sources)
    .map(sourceKey)
    .sort()
    .join('|');
}

/**
 * 丢掉空 id / 同类型同 id 的重复项（保持入参顺序）。
 *
 * 报销单的 `EXPENSE_REPORT` / `EXPENSE_PAYMENT` id 相同但 type 不同，**不会被这里合并** ——
 * 它们要各查一次才拿得全。
 */
export function normalizeSources(sources: readonly VoucherSourceRef[]): VoucherSourceRef[] {
  const seen = new Set<string>();
  const out: VoucherSourceRef[] = [];
  for (const source of sources) {
    const id = idOf(source.id);
    if (!id) {
      continue;
    }
    const key = `${source.type}:${id}`;
    if (seen.has(key)) {
      continue;
    }
    seen.add(key);
    out.push({ type: source.type, id, docNo: docNoOf(source.docNo), label: source.label });
  }
  return out;
}

/**
 * 排序键：`posted_at DESC NULLS LAST, created_at DESC, voucher_no DESC`
 * —— 与后端 `voucher_query::vouchers_by_source` **逐字一致**。
 *
 * 后端只保证「每次请求内」有序；多来源拼起来之后要在这里再排一次，否则
 * 「报销单入账 + 付款」混排时最新的一笔可能被压到第二页位置。
 */
function compareVoucher(a: VoucherRow, b: VoucherRow): number {
  const aPosted = a.posted_at ?? '';
  const bPosted = b.posted_at ?? '';
  if (aPosted !== bPosted) {
    // 空串（NULL）排最后：`NULLS LAST`
    if (!aPosted) {
      return 1;
    }
    if (!bPosted) {
      return -1;
    }
    return aPosted < bPosted ? 1 : -1;
  }
  const aCreated = a.created_at ?? '';
  const bCreated = b.created_at ?? '';
  if (aCreated !== bCreated) {
    return aCreated < bCreated ? 1 : -1;
  }
  if ((a.voucher_no ?? '') === (b.voucher_no ?? '')) {
    return 0;
  }
  return (a.voucher_no ?? '') < (b.voucher_no ?? '') ? 1 : -1;
}

/**
 * 把多次「按来源反查」的结果合成一张清单。
 *
 * - **按凭证 id 去重**：同一张凭证理论上只挂一条来源，但红冲 / 补录的历史数据可能
 *   出现同 id 两行（比如两个来源都指向同一张凭证），去重比出现两条一样的好；
 * - **按后端同一套排序键重排**（见 {@link compareVoucher}）；
 * - 入参是 `unknown`：某个来源查询失败时调用方传 `null` / 抛错都当空数组，不连累其它来源。
 */
export function mergeVoucherRows(groups: ReadonlyArray<readonly VoucherRow[] | null | undefined>): VoucherRow[] {
  const seen = new Set<string>();
  const out: VoucherRow[] = [];
  for (const group of groups) {
    for (const row of group ?? []) {
      if (!row?.id || seen.has(row.id)) {
        continue;
      }
      seen.add(row.id);
      out.push(row);
    }
  }
  return out.sort(compareVoucher);
}

/** 清单里是否有红字凭证（红冲产生的，按钮 / 弹窗提示用）。 */
export function hasRedVoucher(rows: readonly VoucherRow[]): boolean {
  return rows.some((row) => row.is_red);
}

/**
 * 按钮文案：`查看凭证 (N)`；N > 0 才调用（N = 0 时按钮整只不渲染）。
 *
 * 含红字时**文案不变**（括号里的 N 已经把红字算进去了），红字在弹窗里用标签区分 ——
 * 按钮文字再长出「含红字」会被挤到下一位（六个页面挤的都是同一行工具条）。
 */
export function voucherTraceButtonText(count: number): string {
  return `查看凭证 (${count})`;
}

/**
 * 凭证页深链：`/business/erp/voucher?voucher_no=PZ-…`（与该页 `openByQuery` 逐字对应）。
 *
 * 与应付页 `payable-logic.ts::voucherDocPath` **同一口径**（那边是 VTRACE 之前就有的先例），
 * 不重复造第二个拼接函数 —— 这里只是把它搬进溯源域，免得单据页去 import 应付页的模块。
 */
export function voucherDocPathFor(voucherNo: string | null | undefined): string | null {
  const no = (voucherNo ?? '').trim();
  return no ? `/business/erp/voucher?voucher_no=${encodeURIComponent(no)}` : null;
}

/** 来源类型的中文名（弹窗「来源」列）：未知类型原样返回，不编造。 */
export function sourceTypeLabel(type: string | null | undefined): string {
  const key = (type ?? '').trim().toUpperCase();
  if (!key) {
    return '—';
  }
  return isSourceDocType(key) ? SOURCE_ROUTE_TABLE[key].label : key;
}

/**
 * 金额展示：借方=贷方（自动凭证恒平），平则只显示一个。
 *
 * 走 `shared/money.ts::formatMoney`（整数分 + 两位小数 + 千分位），与凭证列表同一口径 ——
 * 直接渲染后端给的 `"3616.0000"` 会在这张弹窗里和凭证列表里显示成两种样子。
 */
export function amountText(row: Pick<VoucherRow, 'total_debit' | 'total_credit'>): string {
  const debit = String(row.total_debit ?? '').trim();
  const credit = String(row.total_credit ?? '').trim();
  if (!debit && !credit) {
    return '—';
  }
  const debitText = formatMoney(row.total_debit);
  const creditText = formatMoney(row.total_credit);
  return debitText === creditText ? debitText : `${debitText} / ${creditText}`;
}
