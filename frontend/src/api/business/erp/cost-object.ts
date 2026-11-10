/**
 * 成本对象（生产批）API 层（ARCH-9 §5.2 #1–#5，后端 `cost_object_{dto,handler,model}.rs`）。
 *
 * # 为什么单独成文件
 *
 * 与 `expense.ts` / `asset.ts` 同款理由：`erp/index.ts` 是仓储域的几千行总契约，
 * 成本对象是独立后端模块（自己的 handler / DTO / 迁移 V0370 段），按模块边界切开，
 * 页面只 `import '@/api/business/erp/cost-object'`，不与正在改 `index.ts` 的人互相踩。
 *
 * **词表与契约类型在 `cost-object-kinds.ts`**（那里不 import 请求层，纯逻辑单测可直接引用），
 * 本文件原样 `export *` 出来 —— 页面仍然只认 `@/api/business/erp/cost-object` 一个入口。
 *
 * # 字段名逐字抄后端（不是猜的）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 对象号 | `object_no`（`CB-yyyyMMdd-NNNNNN`） | `cost_object_dto.rs::CostObjectListRes` |
 * | 计划 / 完工 / 在制数量 | `plan_qty` / `completed_qty` / `wip_qty` | 同上（**字符串**） |
 * | 归集总成本 | `cost_total`（料+工+费实时汇总） | 同上（字符串，无归集时是 `"0"`） |
 * | 归集汇总 | `collect_summary{mat,labor,oh,total}` | `CollectSummary` |
 * | 成本计算单 | `period_items[]`（按期倒序） | `CostPeriodItemRes` |
 * | 车间名 | `workshop_dept_name`（实时 join 优先、建单快照兜底） | `CostObjectListRes` |
 * | 状态 | `status` ∈ `OPEN / CLOSED / CANCELLED` | `ck_biz_cost_object_status` |
 * | 投料方式 | `material_input_mode` ∈ `ONE_TIME / AS_PROGRESS` | `ck_biz_cost_object_input` |
 * | 可去状态 | `next_statuses[]` + `legal_edges[[from,to]]` | `CostObjectDetailRes` |
 *
 * # 五个容易踩的点
 *
 * 1. **路径写全 `/api`**（后端注解不含前缀，漏了会命中 SPA fallback 返回 HTML，
 *    报「响应体格式不符合」）；
 * 2. **金额 / 数量一律字符串**（numeric 四位小数 `"100.0000"`）。类型上就声明 `string`，
 *    展示走 `shared/money.ts` 的整数分工具，不要 `parseFloat` 后累加；
 * 3. **列表没有 `start_date` / `due_date`**：后端 `CostObjectListRes` 只给 §5.2 #1 那 15 个
 *    字段 —— 列表页不能显示「开工日期」列，要看日期去详情；
 * 4. **PUT 是部分更新 + 三态**：字段**缺失 = 不改**、显式 `null` = **清空**（可空字段）。
 *    所以编辑只能带「真改过」的字段（见 `cost-object-logic.ts::toUpdatePayload`），
 *    整表回传会把后端没返回的字段当清空写掉；
 * 5. **`DELETE` 后端未实现**（B1-8 卡面验收只做 5 个接口，软删接口未提供）：
 *    权限码 `business:erp:cost-object:delete` 已种，但**没有 HTTP 路由**，
 *    本层刻意不导出 `deleteCostObject` —— 导出了页面就会挂一个必 404 的按钮。
 *
 * # 关批返回的 409 里有什么
 *
 * 5 条前置（§4.1）任一不过 → HTTP 409 + 业务码 `66055`，`msg` 是**逐条列好的人话**
 * （`成本对象 CB-… 不能关批：[DRAFT_LABOR] 还有 3 条草稿工时台账…；[WIP_NOT_ZERO] …`）。
 * 结构化明细只在后端日志 / 单测里，前端**原样展示 `msg` 即可**（不要解析中文）。
 * 前置 ⑤ 需要「带在制关批」时传 `force_wip: true`（两段确认的第二段，见页面）。
 */

import { get, post, put, type PageData } from '@/api/request';

import type {
  CostObjectClosePayload,
  CostObjectCreatePayload,
  CostObjectDetail,
  CostObjectPageQuery,
  CostObjectRow,
  CostObjectUpdatePayload,
} from './cost-object-kinds';

export * from './cost-object-kinds';

/** 接口前缀（后端注解不含 `/api`） */
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

/** 列表分页（`GET /business/erp/cost-object`，权限 `business:erp:cost-object:list`） */
export function fetchCostObjectPage(
  query: CostObjectPageQuery,
): Promise<PageData<CostObjectRow>> {
  return get<PageData<CostObjectRow>>(`${BASE}/cost-object`, asParams(query));
}

/** 详情（`GET /business/erp/cost-object/{id}`，权限 `cost-object:list`） */
export function fetchCostObjectDetail(id: string): Promise<CostObjectDetail> {
  return get<CostObjectDetail>(`${BASE}/cost-object/${id}`);
}

/** 新建（`POST /business/erp/cost-object`，权限 `cost-object:create`）→ 直接回详情 */
export function createCostObject(payload: CostObjectCreatePayload): Promise<CostObjectDetail> {
  return post<CostObjectDetail>(`${BASE}/cost-object`, payload);
}

/** 编辑（`PUT /business/erp/cost-object/{id}`，权限 `cost-object:update`）→ 回详情 */
export function updateCostObject(
  id: string,
  payload: CostObjectUpdatePayload,
): Promise<CostObjectDetail> {
  return put<CostObjectDetail>(`${BASE}/cost-object/${id}`, payload);
}

/**
 * 关批（`POST /business/erp/cost-object/{id}/close`，权限 `cost-object:close`）。
 *
 * 失败口径：5 条前置任一不过 ⇒ **HTTP 409 + `code=66055`**，`msg` 逐条列出原因；
 * 状态不是 `OPEN` ⇒ 409（业务码 `RESOURCE_CONFLICT`）。页面把 `msg` 原样展示
 * （`cost-object-logic.ts::closeErrorText` 会再补一段「去哪修」的指引）。
 */
export function closeCostObject(
  id: string,
  payload: CostObjectClosePayload = {},
): Promise<CostObjectDetail> {
  return post<CostObjectDetail>(`${BASE}/cost-object/${id}/close`, payload);
}
