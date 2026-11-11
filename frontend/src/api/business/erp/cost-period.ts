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
 * | 27 | POST | `/cost-period/calc` | `cost-period:calc` | 步③「重算」/「建批次后立刻算」（六段编排） |
 * | — | POST | `/cost-period` | `cost-period:calc` | 步③「建批次」（设计无独立建单接口，已具名偏差） |
 * | — | PUT | `/cost-period/{id}` | `cost-period:calc` | 步①改人工来源 / 备注（仅 `DRAFT`） |
 * | 31 | POST | `/cost-period/{id}/cancel` | `cost-period:submit` | `DRAFT` 作废（原因 ≥10 字） |
 * | 28 | POST | `/cost-period/{id}/submit` | `cost-period:submit` | 步④提交审批 |
 * | — | POST | `/cost-period/{id}/withdraw` | `cost-period:submit` | 审批中撤回（回 `DRAFT`） |
 * | 29 | POST | `/cost-period/{id}/settle` | `cost-period:settle` | 步⑤结算（无 body） |
 * | 30 | POST | `/cost-period/{id}/reverse` | `cost-period:reverse` | 红冲（`{reason}` ≥10 字） |
 * | 32 | GET | `/cost-period/{id}/cost-sheet?format=xlsx` | `cost-period:list` | 步③「导出 xlsx」（文件流） |
 *
 * # 路径必须写全 `/api`
 *
 * 后端注解不含前缀（`[router].prefix` 注入）；漏了会命中 SPA fallback 返回 HTML，
 * 报「响应体格式不符合」——本仓最高频的前端踩坑。
 *
 * # 计算入口（设计 §5.2 #27）
 *
 * `POST /cost-period/calc` = 料 → 工 → 费 → 池对账 → 分摊 → 计算单，后端一次事务跑完。
 * 期间没有批次时它**自己建** `DRAFT`（`CS-yyyyMM-NNNN`）；有 `DRAFT` 则原地重算（幂等）。
 * 前端只传 `period_code` —— 能传对象 / 金额就等于能改数。
 */

import service, { get, post, put, type PageData } from '@/api/request';

import type {
  CostPeriodCalcPayload,
  CostPeriodCalcResult,
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

/**
 * 计算成本（`POST /business/erp/cost-period/calc`，权限 `cost-period:calc`）。
 *
 * 后端一次事务跑完六段（料 → 工 → 费 → 池对账 → 分摊 → 计算单），并回带批次详情 +
 * 分段摘要。语义（设计 §5.2 #27）：
 *
 * - 期间**没有批次** ⇒ 建 `DRAFT` 再算（前端不必先调 `createCostPeriod`）；
 * - 期间有 `DRAFT` ⇒ 原地重算（幂等：同一批来源行重算金额不变）；
 * - 期间是 `SUBMITTED` / `APPROVED` / `SETTLED` / `CLOSED` ⇒ `409 + 66073`，报文点名
 *   「撤回 / 红冲 / 反结账」；
 * - 只有 `REVERSED` / `CANCELLED`（不占唯一键）⇒ 按「没有批次」处理，建新 `DRAFT` 再算。
 *
 * 任一段的业务校验不过 ⇒ 整笔回滚（期间保持调用前状态），报文带「第几段」+ 具体对象 / 单据。
 */
export function calcCostPeriod(payload: CostPeriodCalcPayload): Promise<CostPeriodCalcResult> {
  return post<CostPeriodCalcResult>(`${BASE}/cost-period/calc`, payload);
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

/**
 * 导出成本计算单 xlsx（`GET /business/erp/cost-period/{id}/cost-sheet?format=xlsx`）。
 *
 * 返回的是**文件字节流**（不走 `Res<T>` 统一响应体），必须 `responseType: 'blob'` ——
 * 少了这个参数 axios 会按 JSON 解析二进制，报「响应体格式不符合」。
 *
 * # 为什么没有前端传的期间 / 对象参数
 *
 * 设计 §5.2 #32 只按**批次 id** 导出整张计算单（对象 × 料工费 × 约当 × 单位成本 × 完工/在制）。
 * 导出口径（哪些行、什么合计）全在服务端，前端不参与挑数 —— 页面看到什么就导出什么。
 *
 * # 权限
 *
 * 与「看计算单」同一个权限码 `cost-period:list`：能看就能导（设计 §5.2 #32）。
 */
export async function exportCostSheet(id: string): Promise<Blob> {
  try {
    const response = await service.get<Blob>(`${BASE}/cost-period/${id}/cost-sheet`, {
      params: { format: 'xlsx' },
      responseType: 'blob',
      // 1000 行计算单在真库实测 1s 量级，但导出是一次性整表拉取（不受分页限制），
      // 默认 15s 对慢网 / 大对象太紧：给 60s，宁可等也不要把已经生成的字节流丢掉。
      timeout: 60000,
    });
    return response.data;
  } catch (error) {
    throw await blobError(error);
  }
}

/**
 * 把 blob 请求的失败还原成人话。
 *
 * `responseType: 'blob'` 时后端 4xx/5xx 的 JSON 报文也变成了 Blob，
 * 全局拦截器（`toApiError`）读不到 `msg`，只能给出「网络异常」这类通用话术。
 * 导出恰恰是**最需要看原文**的场景（403 权限码 / 409 找不到批次），
 * 所以在这里把错误体读回文本再解析；解析不出就交回原错误由页面兜底。
 */
async function blobError(error: unknown): Promise<Error> {
  const data = (error as { response?: { data?: unknown } }).response?.data;
  if (data instanceof Blob) {
    try {
      const text = await data.text();
      const body = JSON.parse(text) as { code?: number; msg?: string };
      if (body && typeof body.msg === 'string' && body.msg) {
        return new Error(body.msg);
      }
    } catch {
      // 不是 JSON（例如网关的 HTML 502）：保持原错误
    }
  }
  return error instanceof Error ? error : new Error(String(error));
}
