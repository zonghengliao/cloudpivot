/**
 * 采购明细页的**纯逻辑**（卡 t_6f023cee / BUG-SYS-01）—— 不 import vue，
 * 可被 `tsx --test` 直接单测。
 *
 * # 字段口径（硬红线：以 `dto.rs` + 迁移里的列名为准）
 *
 * `biz_purchase_order_item` 的列 → `dto.rs::PurchaseOrderItemRes`：
 * `order_id / line_no / material_id / unit_id / qty / price / tax_rate /
 *  amount / tax_amount / with_tax_amount / received_qty / remark`。
 *
 * - `price` 是**不含税单价**（后端的 `price_incl` 只在建单/改单回参里给，
 *   列表接口没有）—— 所以「含税单价」这一列本页不做，免得前端拿
 *   `price * (1+rate)` 反算出与库里不一致的钱；
 * - `tax_rate` 的量纲是 **0–100**（不是 0–1），13% 存的是 `13.0000`；
 * - `received_qty` 是收货过账回写的累计值，可能大于 `qty`（超收由后端拦，
 *   前端只如实显示，不截断成 100%）。
 *
 * # 为什么筛选原本只有两个 id，现在多一个 keyword
 *
 * 后端 `dto.rs::PurchaseOrderItemFilter` 原本只有 `order_id` / `material_id` 两个**精确**
 * 匹配条件 + 分页；没有 keyword、没有日期区间、没有排序参数。
 * 前端多造一个筛选框就等于发一个后端不认的参数（看起来能查，实际没过滤）。
 *
 * 卡 E2E01-FIX-2 起后端补了 `keyword`：`ILIKE '%…%'` 命中**任一**即算 ——
 * 采购单号 / 物料编码 / 物料名称（一次 JOIN 两张表，见
 * `model::purchase_order_item_page`）。前端这才有搜索框可加。
 */

import type { PurchaseOrderItemLine } from '@/api/business/erp';
import { toNumber } from '../shared/money';

/** 页面筛选状态（`''` = 全部） */
export interface ItemFilterState {
  /** 所属采购单 id */
  order_id: string;
  /** 物料 id */
  material_id: string;
  /** 关键词（单号 / 物料编码 / 物料名称，模糊、大小写不敏感） */
  keyword: string;
}

/** 空筛选 */
export function emptyItemFilter(): ItemFilterState {
  return { order_id: '', material_id: '', keyword: '' };
}

/** 筛选是否有内容 */
export function isItemFiltered(filter: ItemFilterState): boolean {
  return Boolean(filter.order_id) || Boolean(filter.material_id) || Boolean(filter.keyword.trim());
}

/**
 * 组装 `/purchase-order-item/page` 的查询入参。
 *
 * 空串一律不发：后端把 `?order_id=` 当「order_id 等于空串」过滤，
 * 直接查 0 行（`asParams` 也会兜，但入参对象本身应当干净，单测才好断言）。
 *
 * `keyword` 额外**去首尾空格**：`"  DEMO-P1 "` 与 `"DEMO-P1"` 是同一个搜索
 * （后端 `utils::like_pattern` 也会 trim，两边口径一致）。
 */
export function buildItemQuery(
  filter: ItemFilterState,
  page = 1,
  pageSize = 20,
): {
  order_id?: string;
  material_id?: string;
  keyword?: string;
  page: number;
  page_size: number;
} {
  const query: {
    order_id?: string;
    material_id?: string;
    keyword?: string;
    page: number;
    page_size: number;
  } = {
    page,
    page_size: pageSize,
  };
  if (filter.order_id) {
    query.order_id = filter.order_id;
  }
  if (filter.material_id) {
    query.material_id = filter.material_id;
  }
  const keyword = filter.keyword.trim();
  if (keyword) {
    query.keyword = keyword;
  }
  return query;
}

/**
 * 数量 / 单价文本：去掉 Decimal 字符串末尾的 0。
 *
 * 后端出 JSON 是 `"1.5000"` / `"0.0000"`，原样渲染会让「1.5 个」显示成
 * 「1.5000 个」—— 列表里 4 位小数纯噪音。整数位保留（`"2.0000"` → `"2"`）。
 */
export function decimalText(value: number | string | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '—';
  }
  if (typeof value === 'number') {
    return Number.isFinite(value) ? String(value) : '—';
  }
  const raw = value.trim();
  if (!raw) {
    return '—';
  }
  if (!raw.includes('.')) {
    return raw;
  }
  const trimmed = raw.replace(/0+$/, '').replace(/\.$/, '');
  return trimmed || '0';
}

/**
 * 税率文本（量纲 0–100）。
 *
 * `"13.0000"` → `"13%"`；`"0"` → `"0%"`（不是「—」：0% 是免税，
 * 是一个有意义的税档，留白会被误读成「没填」）。
 */
export function taxRateText(value: number | string | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '—';
  }
  const text = decimalText(value);
  return text === '—' ? '—' : `${text}%`;
}

/**
 * 已收货进度文案：`已收 2/5`。
 *
 * `received_qty` 缺席（老数据 / 未过账）时按 0 处理，但**不**把 qty 也算 0 ——
 * 前端只显示事实，不做业务推断。
 */
export function receivedText(
  row: Pick<PurchaseOrderItemLine, 'received_qty' | 'qty'>,
): string {
  const received = decimalText(row.received_qty);
  const qty = decimalText(row.qty);
  return `已收 ${received === '—' ? '0' : received}/${qty}`;
}

/**
 * 是否已收满（用于行内「已收满」小标签）。
 *
 * 用整数分比较（`toNumber` 后的 1e-6 容差）而不是字符串相等：
 * `"5.0000" >= "5"` 在字符串比较下是 false，但业务上就是收满了。
 */
export function isFullyReceived(
  row: Pick<PurchaseOrderItemLine, 'received_qty' | 'qty'>,
): boolean {
  const received = toNumber(row.received_qty);
  const qty = toNumber(row.qty);
  if (qty <= 0) {
    return false;
  }
  return received + 1e-6 >= qty;
}

/** 主数据没加载完时的兜底：短 id（不留白、也不假装是名字） */
export function fallbackLabel(id: string | null | undefined): string {
  if (!id) {
    return '—';
  }
  return id.length > 8 ? `${id.slice(0, 8)}…` : id;
}

/**
 * 从 `id → label` 的 Map 里取名字，取不到退回短 id。
 *
 * 参数化成 Map 而不是「自己拉一遍列表」：页面拉一次主数据建两张 Map，
 * 翻页时不必重新解析，纯函数也就能单测。
 */
export function labelOf(map: ReadonlyMap<string, string>, id: string | null | undefined): string {
  if (!id) {
    return '—';
  }
  const hit = map.get(id);
  return hit && hit.trim() ? hit : fallbackLabel(id);
}
