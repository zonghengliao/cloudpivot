/**
 * 成本核算批次 API 层（ARCH-9 §5.2 #25–#33 · COST-B3-11）。
 *
 * 后端：`backend/src/business/erp/cost_period_handler.rs`（+ `cost_period_dto.rs`），
 * 词表与契约类型在 `cost-period-kinds.ts`（不 import 请求层，纯逻辑单测可引用），
 * 本文件原样 `export *` 出来 —— 页面只认 `@/api/business/erp/cost-period` 一个入口。
 *
 * | # | 方法 | 路径 | 权限码 | 页面用在哪 |
 * |---|---|---|---|---|
 * | 25 | GET | `/cost-period` | `cost-period:list` | 列表 |
 * | 26 | GET | `/cost-period/{id}` | `cost-period:list` | 向导步②③④⑤（单头 + 计算单） |
 * | — | POST | `/cost-period` | `cost-period:calc` | 步③「建批次」（设计无独立建单接口，已具名偏差） |
 * | — | PUT | `/cost-period/{id}` | `cost-period:calc` | 步①改人工来源 / 备注（仅 `DRAFT`） |
 * | 31 | POST | `/cost-period/{id}/cancel` | `cost-period:submit` | `DRAFT` 作废（原因 ≥10 字） |
 * | 28 | POST | `/cost-period/{id}/submit` | `cost-period:submit` | 步④提交审批 |
 * | — | POST | `/cost-period/{id}/withdraw` | `cost-period:submit` | 审批中撤回（回 `DRAFT`） |
 * | 29 | POST | `/cost-period/{id}/settle` | `cost-period:settle` | 步⑤结算（无 body） |
 * | 30 | POST | `/cost-period/{id}/reverse` | `cost-period:reverse` | 红冲（`{reason}` ≥10 字） |
 *
 * # 路径必须写全 `/api`
 *
 * 后端注解不含前缀（`[router].prefix` 注入）；漏了会命中 SPA fallback 返回 HTML，
 * 报「响应体格式不符合」——本仓最高频的前端踩坑。
 *
 * # 为什么没有「试算」请求
 *
 * 设计 §5.2 #27 `POST /cost-period/calc`（六段串联的计算编排）**尚未落地**（属编排卡），
 * 所以本层不提供 `calc()` 假入口：步③ 只能「建批次 + 读已有计算单」。等编排卡上线后
 * 只需在本文件加一个 `calcCostPeriod()`，页面已经把按钮位留好（见 `index.vue` 的注释）。
 */

import { get, post, put, type PageData } from '@/api/request';

import type {
  CostPeriodCreatePayload,
  CostPeriodDetail,
  CostPeriodPageQuery,
  CostPeriodReasonPayload,
  CostPeriodRow,
  CostPeriodUpdatePayload,
} from './cost-period-kinds';

export * from './cost-period-kinds';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/** 丢掉空串 / null / undefined（`status` 的空串表示「全部状态」） */
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

/** 列表分页（`GET /business/erp/cost-period`，权限 `cost-period:list`） */
export function fetchCostPeriodPage(
  query: CostPeriodPageQuery = {},
): Promise<PageData<CostPeriodRow>> {
  return get<PageData<CostPeriodRow>>(`${BASE}/cost-period`, asParams(query));
}

/** 批次详情（单头 + 计算单 + 现算合计 + 可去状态） */
export function fetchCostPeriodDetail(id: string): Promise<CostPeriodDetail> {
  return get<CostPeriodDetail>(`${BASE}/cost-period/${id}`);
}

/**
 * 新建 `DRAFT` 批次（`POST /business/erp/cost-period`，权限 `cost-period:calc`）。
 *
 * 前置：期间合法 + 同期间没有活跃批次（`409 + 66072` 会点名已有批次号）。
 */
export function createCostPeriod(payload: CostPeriodCreatePayload): Promise<CostPeriodDetail> {
  return post<CostPeriodDetail>(`${BASE}/cost-period`, payload);
}

/** 改 `DRAFT` 批次（人工来源 / 备注；其它状态 409） */
export function updateCostPeriod(
  id: string,
  payload: CostPeriodUpdatePayload,
): Promise<CostPeriodDetail> {
  return put<CostPeriodDetail>(`${BASE}/cost-period/${id}`, payload);
}

/** 作废（`DRAFT → CANCELLED`，原因 ≥10 字） */
export function cancelCostPeriod(
  id: string,
  payload: CostPeriodReasonPayload,
): Promise<CostPeriodDetail> {
  return post<CostPeriodDetail>(`${BASE}/cost-period/${id}/cancel`, payload);
}

/**
 * 提交审批（`DRAFT → SUBMITTED`，同事务发起 `cost_settle_approve` 实例）。
 *
 * 没有已发布流程 ⇒ `409 + APPROVAL_FLOW_NOT_CONFIGURED`（后端**默认拒绝**，不静默直过）。
 */
export function submitCostPeriod(id: string): Promise<CostPeriodDetail> {
  return post<CostPeriodDetail>(`${BASE}/cost-period/${id}/submit`);
}

/** 撤回（终止在跑实例 + 回 `DRAFT`；实例已落终态时后端自愈回写） */
export function withdrawCostPeriod(id: string): Promise<CostPeriodDetail> {
  return post<CostPeriodDetail>(`${BASE}/cost-period/${id}/withdraw`);
}

/**
 * 结算（`APPROVED → SETTLED`：完工入库 + 三张凭证 + 6 条事务内断言）。
 *
 * 无 body：入库数量、单价、凭证金额全部由服务端按计算单推导。任一断言不平 ⇒ `409`，
 * **整笔回滚**、批次仍 `APPROVED`，修数据后可重试。
 */
export function settleCostPeriod(id: string): Promise<CostPeriodDetail> {
  return post<CostPeriodDetail>(`${BASE}/cost-period/${id}/settle`);
}

/**
 * 红冲（`SETTLED → REVERSED`：四张红字凭证 + 库存回冲 + 入库单作废 + 计算单回 `CALC`）。
 *
 * 只收 `reason`（≥10 字）：能传数量金额就等于允许「红冲一半」。库存退不回来 ⇒ `409 + 66078`。
 */
export function reverseCostPeriod(
  id: string,
  payload: CostPeriodReasonPayload,
): Promise<CostPeriodDetail> {
  return post<CostPeriodDetail>(`${BASE}/cost-period/${id}/reverse`, payload);
}
