/**
 * 取价来源的展示口径（SO-34）：**唯一一份**译名与配色，价格页与销售订单页共用。
 *
 * 后端 `source` 五值见 `price_dto::SOURCES`（顺序 = 优先级从高到低）：
 * `AGREED`（客户协议价）→ `TIER_CUSTOMER`（客户专属阶梯）→ `TIER_COMMON`（通用阶梯）
 * → `STANDARD`（物料标准售价）→ `NONE`（未找到，让销售手填）。
 *
 * 为什么单独成文件：同一份枚举在两处出现时，最怕「订单页说『协议价』、价格页说『客户价』」——
 * 销售看着两个名字会以为是两套机制。翻译只写一次，改口径只改这里。
 */
import type { PriceSource } from '@/api/business/erp/customer-price';

/** 来源 → 中文名（与 ARCH-7 §15.3.3 的层级叫法一致） */
export const PRICE_SOURCE_LABELS: Record<PriceSource, string> = {
  AGREED: '协议价',
  TIER_CUSTOMER: '阶梯价（客户专属）',
  TIER_COMMON: '阶梯价（通用）',
  STANDARD: '标准价',
  NONE: '未找到',
};

/** 来源 → TDesign 主题色（协议价最"贵重"用 success，未找到用 warning 提醒手填） */
export const PRICE_SOURCE_THEMES: Record<PriceSource, 'success' | 'primary' | 'default' | 'warning'> = {
  AGREED: 'success',
  TIER_CUSTOMER: 'primary',
  TIER_COMMON: 'primary',
  STANDARD: 'default',
  NONE: 'warning',
};

/** 来源 → 中文名（未知值原样回显，别显示 undefined） */
export function priceSourceLabel(source: string | null | undefined): string {
  if (!source) {
    return '—';
  }
  return PRICE_SOURCE_LABELS[source as PriceSource] ?? source;
}

/** 来源 → 主题色（未知值回落 default） */
export function priceSourceTheme(
  source: string | null | undefined,
): 'success' | 'primary' | 'default' | 'warning' {
  if (!source) {
    return 'default';
  }
  return PRICE_SOURCE_THEMES[source as PriceSource] ?? 'default';
}

/** 只有 `AGREED` / `TIER_*` / `STANDARD` 才算"带出了价"；`NONE` 等于没价 */
export function hasPrice(source: string | null | undefined): boolean {
  return source !== null && source !== undefined && source !== 'NONE';
}
