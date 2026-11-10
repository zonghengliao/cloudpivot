/**
 * 批次主数据 API 层（ARCH-9 §5.2 #7–#9 + `GET /lot/options`；后端 `lot_{dto,handler}.rs`）。
 *
 * 词表与契约类型在 `lot-kinds.ts`（那里不 import 请求层，纯逻辑单测可直接引用），
 * 本文件原样 `export *` 出去 —— 页面只认 `@/api/business/erp/lot` 一个入口。
 *
 * # 三个接口 + 一个下拉
 *
 * | 方法 | 路径 | 权限码 | 说明 |
 * |---|---|---|---|
 * | GET | `/api/business/erp/lot` | `business:erp:lot:list` | 分页 `{list,total,page,page_size}` |
 * | GET | `/api/business/erp/lot/options` | `business:erp:lot:list` | 按物料取批号（只返启用批，上限 200） |
 * | POST | `/api/business/erp/lot` | `business:erp:lot:create` | 新建（唯一键冲突 → 409 + `66056`） |
 * | PUT | `/api/business/erp/lot/{id}` | `business:erp:lot:update` | 部分更新（三态） |
 *
 * **没有 DELETE**：设计 §5.3 只给 3 个权限码，后端未实现软删接口 ——
 * 本层刻意不导出 `deleteLot`，导出了页面就会挂一个必 404 的按钮。
 */

import { get, post, put, type PageData } from '@/api/request';

import type { LotCreatePayload, LotOption, LotPageQuery, LotRow, LotUpdatePayload } from './lot-kinds';

export * from './lot-kinds';

/** 接口前缀（后端注解不含 `/api`，漏了会命中 SPA fallback 返回 HTML） */
const BASE = '/api/business/erp';

/** 丢掉空串 / null / undefined（后端把 `?keyword=` 当「等于空串」过滤，会查出 0 行） */
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

/** 分页列表（`GET /business/erp/lot`） */
export function fetchLotPage(query: LotPageQuery): Promise<PageData<LotRow>> {
  return get<PageData<LotRow>>(`${BASE}/lot`, asParams(query));
}

/**
 * 按物料取批号（`GET /business/erp/lot/options`，出库单 / 成本对象表单的下拉数据源）。
 *
 * 只返 `status = 1` 的启用批；物料不存在 → 后端 **400**（不是空列表），
 * 所以调用方要把错误照原样展示，别把「物料不存在」当成「还没建批」。
 */
export function fetchLotOptions(params: {
  /** 物料（必填） */
  material_id: string;
  /** 批号关键字（输入即筛） */
  keyword?: string;
}): Promise<LotOption[]> {
  return get<LotOption[]>(`${BASE}/lot/options`, asParams(params));
}

/** 新建（`POST /business/erp/lot`）→ 返回落库后的完整行 */
export function createLot(payload: LotCreatePayload): Promise<LotRow> {
  return post<LotRow>(`${BASE}/lot`, payload);
}

/**
 * 编辑（`PUT /business/erp/lot/{id}`，**部分更新**：只发真改过的字段）。
 *
 * 失败口径：批号撞唯一键 → **HTTP 409 + `code=66056`**，`msg` 是「批号『X』在物料『Y』下已存在…」
 * 的人话（不含 PG 约束名）。页面原样展示 `msg` 即可。
 */
export function updateLot(id: string, payload: LotUpdatePayload): Promise<LotRow> {
  return put<LotRow>(`${BASE}/lot/${id}`, payload);
}
