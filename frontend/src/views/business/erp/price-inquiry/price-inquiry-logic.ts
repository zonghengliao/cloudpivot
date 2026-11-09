/**
 * 询比价页面纯逻辑（P2-01）。
 *
 * # 为什么单独一个 ts 文件
 *
 * 页面（`index.vue`）只负责画画，**取数 / 组装 / 校验 / 比价表口径**都放这里：
 * ① `vue-tsc` 对 ts 文件的检查比 .vue 更严，口径写错更容易当场红；
 * ② 这些函数没有 Vue 依赖，`tests/unit` 里能直接跑；
 * ③ 后端 `price_inquiry_dto.rs` 的三条规则（≥2 家 / 中标必须对得上报价 / 按行定标）
 *    在前端**同口径预校验**，用户不用等 409 才知道少填了理由。
 *
 * # 金额与数量一律字符串提交
 *
 * 后端是 `Decimal`（JSON 字符串）。`t-input-number` 给的是 number，
 * `String(number)` 在 0.1+0.2 这类场景会掉精度 —— 所以数量/单价入参在
 * `build*Payload` 里统一 `String()` 转换，**不做本地加减乘除**（金额只由服务端算）。
 */
import type {
  AwardPayload,
  CompareRow,
  CreatePriceInquiryPayload,
  DecimalText,
  InquiryStatus,
  PriceInquiryDetail,
  PriceInquiryFilter,
  PriceQuoteRow,
  QuotePayload,
  ToPoPayload,
} from '@/api/business/erp/price-inquiry';

/** 列表默认每页条数（与后端 `page_size` 上限一致，超过会被夹到 200） */
export const PAGE_SIZE = 20;
/** 单次询价单明细上限（与后端 `MAX_LINES` 一致） */
export const MAX_LINES = 200;
/** 备注上限（与后端 `MAX_REMARK_LEN` 一致） */
export const MAX_REMARK = 500;
/** 单源例外理由最短长度（与后端 `MIN_AWARD_REASON_LEN` / `ck_biz_piq_award_reason` 一致） */
export const MIN_SINGLE_SOURCE_REASON = 10;
/** 默认税率（%，物料的 `tax_rate` 是 0–1 的小数，这里只是兜底展示值） */
export const DEFAULT_TAX_PERCENT = 13;

/**
 * `t-input-number` 的值 → number（`null` = 没填）。
 *
 * 为什么不用 `number | null` 直接标注草稿字段：TDesign 的
 * `TdInputNumberProps.modelValue` 是 `number | string`，`v-model` 绑 `number | null`
 * 当场红（`noUnusedLocals` + `strict` 的项目里这是硬错误）。所以草稿字段统一
 * `number | string`，校验时再收敛：**空串 / 非数字都算没填**，不能把 `''` 当 0
 * （`Number('') === 0`，直接 `Number()` 会把「没填单价」洗成「0 元报价」）。
 */
export function toNumberOrNull(value: number | string | null | undefined): number | null {
  if (value === null || value === undefined) {
    return null;
  }
  if (typeof value === 'string' && value.trim() === '') {
    return null;
  }
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

// ==================== 筛选 ====================

/** 列表筛选态（日期用两个输入框，避免 range-picker 的 `string[] | number[]` 联合类型） */
export interface FilterState {
  /** 关键字：单号 / 采购员 / 物料编码 */
  keyword: string;
  /** 状态 */
  status: InquiryStatus | '';
  /** 询价日期起 */
  date_from: string;
  /** 询价日期止 */
  date_to: string;
}

/** 空筛选 */
export function emptyFilter(): FilterState {
  return { keyword: '', status: '', date_from: '', date_to: '' };
}

/** 筛选态 → 后端 query（空串一律不传：后端把 `?keyword=` 当「等于空串」过滤，会查出 0 行） */
export function buildQuery(filter: FilterState, page: number, pageSize = PAGE_SIZE): PriceInquiryFilter {
  const query: PriceInquiryFilter = { page, page_size: pageSize };
  if (filter.keyword.trim()) {
    query.keyword = filter.keyword.trim();
  }
  if (filter.status) {
    query.status = filter.status;
  }
  if (filter.date_from) {
    query.date_from = filter.date_from;
  }
  if (filter.date_to) {
    query.date_to = filter.date_to;
  }
  return query;
}

/** 是否处于筛选态（决定「空表格」的文案：没有数据 vs 没有命中） */
export function isFiltered(filter: FilterState): boolean {
  return Boolean(filter.keyword.trim() || filter.status || filter.date_from || filter.date_to);
}

/** 起点晚于终点时给出提示（后端会返回 400；前端先拦，少一次请求） */
export function dateRangeError(filter: FilterState): string | null {
  if (filter.date_from && filter.date_to && filter.date_from > filter.date_to) {
    return '询价日期起不能晚于止';
  }
  return null;
}

// ==================== 建单 ====================

/** 建单行草稿（数量 `t-input-number` 给 `number | string`，提交时统一转字符串） */
export interface CreateLineDraft {
  /** 本地行键（不是后端 id） */
  key: number;
  /** 物料 id */
  material_id: string;
  /** 数量 */
  qty: number | string;
  /** 需求日期 */
  demand_date: string;
  /** 行备注 */
  remark: string;
}

/** 建单表单 */
export interface CreateForm {
  /** 询价日期（空 = 后端取今天，北京时间） */
  inquiry_date: string;
  /** 收货仓库（必填：转采购单时要落 `biz_purchase_order.warehouse_id`） */
  warehouse_id: string;
  /** 备注 */
  remark: string;
  /** 明细 */
  items: CreateLineDraft[];
}

let lineSeed = 0;

/** 新明细行（行键自增，vue 的 :key 才不会复用错行；数量默认 1，价格相关不预填） */
export function newCreateLine(): CreateLineDraft {
  lineSeed += 1;
  return { key: lineSeed, material_id: '', qty: 1, demand_date: '', remark: '' };
}

/** 空建单表单（默认 1 行 + 今天；日期交后端算，避免时区差一天） */
export function emptyCreateForm(): CreateForm {
  return { inquiry_date: '', warehouse_id: '', remark: '', items: [newCreateLine()] };
}

/** 建单校验：返回第一条错误文案（`null` = 通过） */
export function validateCreateForm(form: CreateForm): string | null {
  if (!form.warehouse_id) {
    return '请选择收货仓库（转采购订单时要用它落采购单的收货仓库）';
  }
  if (form.remark.length > MAX_REMARK) {
    return `备注最多 ${MAX_REMARK} 字`;
  }
  if (form.items.length === 0) {
    return '至少要有 1 行明细';
  }
  if (form.items.length > MAX_LINES) {
    return `单次最多 ${MAX_LINES} 行明细`;
  }
  for (const [index, line] of form.items.entries()) {
    if (!line.material_id) {
      return `第 ${index + 1} 行还没选物料`;
    }
    const qty = toNumberOrNull(line.qty);
    if (qty === null || !(qty > 0)) {
      return `第 ${index + 1} 行数量必须大于 0`;
    }
  }
  const materials = form.items.map((line) => line.material_id).filter(Boolean);
  if (new Set(materials).size !== materials.length) {
    return '同一物料只能出现一次（要两批就拆成两张询价单，或写在行备注里）';
  }
  return null;
}

/** 建单表单 → 请求体（单位由后端按物料带出，这里不传） */
export function buildCreatePayload(form: CreateForm): CreatePriceInquiryPayload {
  return {
    inquiry_date: form.inquiry_date || null,
    warehouse_id: form.warehouse_id,
    remark: form.remark.trim() || null,
    items: form.items.map((line) => ({
      material_id: line.material_id,
      qty: String(toNumberOrNull(line.qty) ?? 0),
      demand_date: line.demand_date || null,
      remark: line.remark.trim() || null,
    })),
  };
}

// ==================== 报价 ====================

/** 报价行草稿（不含税单价；含税价由后端算） */
export interface QuoteLineDraft {
  /** 明细 id */
  item_id: string;
  /** 行号（展示） */
  line_no: number;
  /** 物料编码（展示） */
  material_code: string;
  /** 物料名称（展示） */
  material_name: string;
  /** 询价数量（展示） */
  qty: DecimalText;
  /** 不含税单价（`t-input-number`：number | string，'' = 没填） */
  price_excl: number | string;
  /** 税率 % */
  tax_rate: number | string;
  /** 交期天数（'' = 不承诺） */
  delivery_days: number | string;
  /** 备注 */
  remark: string;
}

/** 一家供应商的报价表单 */
export interface QuoteForm {
  /** 供应商 */
  supplier_id: string;
  /** 报价行 */
  items: QuoteLineDraft[];
}

/**
 * 物料的默认税率（%）。
 *
 * 主数据 `biz_material.tax_rate` 是 **0–1 的小数**（`0.13`），而报价接口要的是
 * **百分数**（`0–100`，后端 `check_tax` 卡这个区间）。直接透传会把 13% 变成 0.13%，
 * 含税价差 100 倍 —— 这里统一换算，识别不出来就落 `DEFAULT_TAX_PERCENT`。
 */
export function defaultTaxPercent(materialTaxRate: DecimalText | null | undefined): number {
  const raw = Number(materialTaxRate ?? '');
  if (!Number.isFinite(raw) || raw <= 0) {
    return DEFAULT_TAX_PERCENT;
  }
  const percent = raw <= 1 ? raw * 100 : raw;
  return Math.min(100, Math.round(percent * 100) / 100);
}

/**
 * 按明细生成报价表单；已有报价（改价场景）回填原值。
 *
 * `existing` 传同一家供应商上次提交的报价（后端 `(item_id, supplier_id)` 唯一，
 * 重复提交 = 改价），否则留空等用户填。
 */
export function emptyQuoteForm(
  detail: PriceInquiryDetail,
  existing: PriceQuoteRow[] = [],
): QuoteForm {
  const byItem = new Map(existing.map((quote) => [quote.item_id, quote]));
  return {
    supplier_id: '',
    items: detail.items.map((item) => {
      const quote = byItem.get(item.id);
      return {
        item_id: item.id,
        line_no: item.line_no,
        material_code: item.material_code,
        material_name: item.material_name,
        qty: item.qty,
        price_excl: quote ? Number(quote.price_excl) : '',
        tax_rate: quote ? Number(quote.tax_rate) : DEFAULT_TAX_PERCENT,
        delivery_days: quote?.delivery_days ?? '',
        remark: quote?.remark ?? '',
      };
    }),
  };
}

/** 某家供应商的已有报价（改价回填用） */
export function quotesOfSupplier(detail: PriceInquiryDetail, supplierId: string): PriceQuoteRow[] {
  return detail.quotes.filter((quote) => quote.supplier_id === supplierId);
}

/** 报价校验：返回第一条错误文案（`null` = 通过） */
export function validateQuoteForm(form: QuoteForm): string | null {
  if (!form.supplier_id) {
    return '请选择报价供应商';
  }
  if (form.items.length === 0) {
    return '询价单没有明细，无法报价';
  }
  for (const line of form.items) {
    const price = toNumberOrNull(line.price_excl);
    if (price === null) {
      return `第 ${line.line_no} 行还没填不含税单价`;
    }
    if (price < 0) {
      return `第 ${line.line_no} 行单价不能为负`;
    }
    const tax = toNumberOrNull(line.tax_rate);
    if (tax === null || tax < 0 || tax > 100) {
      return `第 ${line.line_no} 行税率必须在 0–100 之间`;
    }
    const days = toNumberOrNull(line.delivery_days);
    if (days !== null && (days < 0 || !Number.isInteger(days))) {
      return `第 ${line.line_no} 行交期必须是非负整数天`;
    }
  }
  return null;
}

/** 报价表单 → 请求体（含税价不传：后端按 `price_excl × (1 + tax_rate/100)` 算） */
export function buildQuotePayload(form: QuoteForm): QuotePayload {
  return {
    supplier_id: form.supplier_id,
    items: form.items.map((line) => ({
      item_id: line.item_id,
      price_excl: String(toNumberOrNull(line.price_excl) ?? 0),
      tax_rate: String(toNumberOrNull(line.tax_rate) ?? 0),
      delivery_days: toNumberOrNull(line.delivery_days),
      remark: line.remark.trim() || null,
    })),
  };
}

// ==================== 比价表 ====================

/** 比价表的一个供应商列 */
export interface CompareSupplier {
  /** 供应商 id */
  supplier_id: string;
  /** 供应商名 */
  supplier_name: string;
  /** 参与报价的明细行数（用来排序：报得多的排前面） */
  quoted_lines: number;
}

/**
 * 比价表的供应商列（横向对比的「列头」）。
 *
 * 只列**真的报过价**的供应商（没报价的供应商不该占一列空格子）；
 * 排序：报价行数多 → 供应商名，保证同一张单每次渲染列序一致（截图对得上）。
 */
export function compareSuppliersOf(detail: PriceInquiryDetail): CompareSupplier[] {
  const seen = new Map<string, CompareSupplier>();
  for (const row of detail.comparison) {
    for (const quote of row.quotes) {
      const current = seen.get(quote.supplier_id);
      if (current) {
        current.quoted_lines += 1;
      } else {
        seen.set(quote.supplier_id, {
          supplier_id: quote.supplier_id,
          supplier_name: quote.supplier_name,
          quoted_lines: 1,
        });
      }
    }
  }
  return [...seen.values()].sort(
    (left, right) =>
      right.quoted_lines - left.quoted_lines || left.supplier_name.localeCompare(right.supplier_name, 'zh'),
  );
}

/** 取某行 × 某供应商的报价（没有就是 `undefined`，页面显示「未报价」） */
export function quoteOf(row: CompareRow, supplierId: string) {
  return row.quotes.find((quote) => quote.supplier_id === supplierId);
}

/** 一行里报价家数不足 2 的明细（定标时需要单源理由） */
export function singleSourceLines(detail: PriceInquiryDetail): CompareRow[] {
  return detail.comparison.filter((row) => row.bidder_count < 2);
}

/** 是否必须填单源理由 */
export function needsSingleSourceReason(detail: PriceInquiryDetail): boolean {
  return singleSourceLines(detail).length > 0;
}

/** 单源理由是否够长（后端卡 ≥10 字，前端同口径先拦） */
export function reasonIsLongEnough(reason: string): boolean {
  return reason.trim().length >= MIN_SINGLE_SOURCE_REASON;
}

// ==================== 定标 ====================

/** 定标选择：明细 id → 供应商 id */
export type AwardPick = Record<string, string>;

/** 已定标的选择（重复打开详情时回填；没定标过 = 空对象，不预选任何供应商） */
export function awardPickOf(detail: PriceInquiryDetail): AwardPick {
  const pick: AwardPick = {};
  for (const row of detail.comparison) {
    const awarded = row.quotes.find((quote) => quote.is_awarded);
    if (awarded) {
      pick[row.item_id] = awarded.supplier_id;
    }
  }
  return pick;
}

/**
 * 「一键选最低价」：每行选 `is_lowest` 的那家。
 *
 * 只做**建议**用，永远可以改 —— 最低价不等于最优（交期、账期、质量都可能更重要），
 * 所以按钮文案写「全部选最低价」而不是「智能定标」。
 */
export function lowestPicksOf(detail: PriceInquiryDetail): AwardPick {
  const pick: AwardPick = {};
  for (const row of detail.comparison) {
    const lowest = row.quotes.find((quote) => quote.is_lowest);
    if (lowest) {
      pick[row.item_id] = lowest.supplier_id;
    }
  }
  return pick;
}

/** 还没选供应商的明细行（后端要求每条明细都有中标供应商） */
export function unpickedLines(detail: PriceInquiryDetail, pick: AwardPick): CompareRow[] {
  return detail.comparison.filter((row) => !pick[row.item_id]);
}

/** 选中的供应商在该行没有报价的行（后端 409「中标供应商没给这条明细报价」） */
export function invalidPicks(detail: PriceInquiryDetail, pick: AwardPick): CompareRow[] {
  return detail.comparison.filter((row) => {
    const supplierId = pick[row.item_id];
    return Boolean(supplierId) && !row.quotes.some((quote) => quote.supplier_id === supplierId);
  });
}

/** 定标校验：返回第一条错误文案（`null` = 通过） */
export function validateAward(
  detail: PriceInquiryDetail,
  pick: AwardPick,
  reason: string,
): string | null {
  if (detail.header.status === 'AWARDED' || detail.header.status === 'CONVERTED') {
    return '这张询价单已经定标过了（重复定标会被后端 409 挡住）';
  }
  if (detail.comparison.length === 0) {
    return '询价单没有明细，无法定标';
  }
  const unpicked = unpickedLines(detail, pick);
  if (unpicked.length > 0) {
    return `还有 ${unpicked.length} 行没选中标供应商（第 ${unpicked.map((row) => row.line_no).join('、')} 行）`;
  }
  const invalid = invalidPicks(detail, pick);
  if (invalid.length > 0) {
    return `第 ${invalid.map((row) => row.line_no).join('、')} 行选的供应商没报过价，请先录入它的报价`;
  }
  if (needsSingleSourceReason(detail) && !reasonIsLongEnough(reason)) {
    const lines = singleSourceLines(detail).map((row) => row.line_no);
    return `第 ${lines.join('、')} 行不足 2 家报价，单源采购必须填写例外理由（至少 ${MIN_SINGLE_SOURCE_REASON} 字）`;
  }
  return null;
}

/** 定标请求体：按明细逐行表达（支持一行一个供应商 → 转单时能拆成多张采购单） */
export function buildAwardPayload(pick: AwardPick, reason: string): AwardPayload {
  const items = Object.entries(pick)
    .filter(([, supplierId]) => Boolean(supplierId))
    .map(([itemId, supplierId]) => ({ item_id: itemId, supplier_id: supplierId }));
  const payload: AwardPayload = { items };
  const reasonText = reason.trim();
  if (reasonText) {
    payload.single_source_reason = reasonText;
  }
  return payload;
}

/** 定标结果的预览：按供应商分组后会生成几张采购单（与后端 `group_by_supplier` 同口径） */
export function awardSummaryOf(detail: PriceInquiryDetail, pick: AwardPick): {
  supplier_id: string;
  supplier_name: string;
  line_nos: number[];
}[] {
  const grouped = new Map<string, { supplier_id: string; supplier_name: string; line_nos: number[] }>();
  for (const row of detail.comparison) {
    const supplierId = pick[row.item_id];
    if (!supplierId) {
      continue;
    }
    const quote = row.quotes.find((item) => item.supplier_id === supplierId);
    const current = grouped.get(supplierId);
    if (current) {
      current.line_nos.push(row.line_no);
    } else {
      grouped.set(supplierId, {
        supplier_id: supplierId,
        supplier_name: quote?.supplier_name ?? supplierId,
        line_nos: [row.line_no],
      });
    }
  }
  return [...grouped.values()].sort((left, right) =>
    left.supplier_name.localeCompare(right.supplier_name, 'zh'),
  );
}

// ==================== 转采购订单 ====================

/** 转单弹窗表单 */
export interface ToPoForm {
  /** 下单日期（空 = 后端今天） */
  order_date: string;
  /** 收货仓库覆盖（空 = 用询价单的仓库） */
  warehouse_id: string;
  /** 采购单备注前缀 */
  remark: string;
}

/** 空转单表单 */
export function emptyToPoForm(): ToPoForm {
  return { order_date: '', warehouse_id: '', remark: '' };
}

/** 转单表单 → 请求体 */
export function buildToPoPayload(form: ToPoForm): ToPoPayload {
  return {
    order_date: form.order_date || null,
    warehouse_id: form.warehouse_id || null,
    remark: form.remark.trim() || null,
  };
}

/** 转单按钮的可用性 + 不可用原因（`can_to_po` 来自后端，前端只做展示） */
export function toPoStateOf(detail: PriceInquiryDetail): { enabled: boolean; hint: string } {
  if (detail.header.status === 'CONVERTED') {
    return { enabled: false, hint: '已转采购订单，不能重复转（幂等闸）' };
  }
  if (detail.header.status !== 'AWARDED') {
    return { enabled: false, hint: '请先定标：转单要用中标供应商与中标单价' };
  }
  if (!detail.can_to_po) {
    return { enabled: false, hint: detail.to_po_blocked_reason ?? '当前状态不能转单' };
  }
  return { enabled: true, hint: '按中标供应商分组生成采购订单（同一供应商的多行合成一张）' };
}

// ==================== 展示 ====================

/** 金额 / 数量展示：`null` → `—`，字符串原样（不四舍五入，后端给几位就显示几位） */
export function decimalText(value: DecimalText | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '—';
  }
  return String(value);
}

/** 采购单引用文案：`PO-…（3 行）` */
export function poRefsText(refs: { po_no: string; line_count: number }[] | null | undefined): string {
  if (!refs || refs.length === 0) {
    return '—';
  }
  return refs.map((ref) => `${ref.po_no}（${ref.line_count} 行）`).join('、');
}

/** 表格空态文案 */
export function emptyListText(filtered: boolean): string {
  return filtered ? '没有符合条件的询价单' : '还没有询价单，点「新建询价单」开始';
}
