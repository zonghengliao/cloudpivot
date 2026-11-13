/**
 * 请假单 API 层（ARCH-9 §5.2 #26~#31 · HR-25 后端 / HR-28 前端）。
 *
 * 后端：`backend/src/business/hr/leave_handler.rs`（HR-25）+ `leave_dto.rs`（HR-24 契约）。
 * 出参契约与词表在 `./leave-kinds.ts`（纯逻辑、可单测），本文件 `export *` 出来 ——
 * 页面只认 `@/api/business/hr/leave` 一个入口。
 *
 * | # | 方法 | 路径 | 权限码 | 页面用在哪 |
 * |:--:|---|---|---|---|
 * | 26 | GET  | `/leave/page` | `business:hr:leave:list` | 「全部请假」tab（`mine=true` 只在有 list 码时用） |
 * | 27 | GET  | `/leave/{id}` | `business:hr:leave:detail` | 详情抽屉（单据 + 审批轨迹） |
 * | 28 | POST | `/leave` | `business:hr:leave:create` | 新建（恒为 `DRAFT`） |
 * | 29 | PUT  | `/leave/{id}` | `business:hr:leave:update` | 改草稿（部分更新） |
 * | 30 | POST | `/leave/{id}/submit` | `business:hr:leave:submit` | 提交审批（起 `hr_leave` 实例） |
 * | 31 | POST | `/leave/{id}/cancel` | `business:hr:leave:cancel` | 删除 / 撤回 / 销假（一条路径三件事） |
 * | 47 | GET  | `/leave/my` | **登录即可**（`#[nik::auth_only]`） | 「我的请假」tab（员工自助） |
 *
 * # 路径必须写全 `/api`
 *
 * 后端注解不含前缀（`[router].prefix` 注入）；漏了会命中 SPA fallback 返回 HTML，
 * 报「响应体格式不符合」——本仓最高频的前端踩坑。
 *
 * # 「我的请假」为什么有两个来源（卡 t_0af7ec46 解耦后）
 *
 * 解耦前页面码兼作 `leave:list`，所以「我的」tab 用 `/leave/page?mine=true` 就够了。
 * `V0401` 之后员工只有 `leave:page` + `leave:create`，`/leave/page` 对他 403 ——
 * 于是「我的」tab 按**有没有 `leave:list`** 分流：
 *
 * * 有 `leave:list`（人事 / 超管）：仍走 `/leave/page?mine=true`（能连带看到自己的审批态）；
 * * 没有（员工自助）：走 `/leave/my`（登录即可、只出本人，`MyLeaveRow` 投影）。
 */

import { get, post, put, type PageData } from '@/api/request';

import type {
  LeaveCancelResult,
  LeaveCreatePayload,
  LeaveDetail,
  LeaveListRow,
  LeavePageQuery,
  LeaveSubmitPayload,
  LeaveUpdatePayload,
  MyLeaveRow,
} from './leave-kinds';

export * from './leave-kinds';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/hr';

/**
 * 丢掉空串 / null / undefined / 空数组。
 *
 * 与 `hr/employee.ts::asParams` 同口径：`buildLeaveQuery` 已经做过一轮清洗，
 * 这里再兜一次是为了防止调用方手工拼 query 时把 `undefined` 带成字符串 `"undefined"`。
 */
function asParams(query: LeavePageQuery): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === '') {
      continue;
    }
    out[key] = value;
  }
  return out;
}

/**
 * 请假单分页（`GET /leave/page`，权限 `business:hr:leave:list`）。
 *
 * 「全部请假」必有它；「我的请假」只在**持有 `leave:list`** 时才走它（带 `mine=true`）。
 * 没有 list 码的人请用 {@link fetchMyLeaves}（`/leave/my`，登录即可）——
 * 否则会拿到 403（解耦后员工自助的真实形状，见文件头）。
 */
export function fetchLeavePage(query: LeavePageQuery = {}): Promise<PageData<LeaveListRow>> {
  return get<PageData<LeaveListRow>>(`${BASE}/leave/page`, asParams(query));
}

/**
 * 我的请假（`GET /leave/my`，**登录即可**，只出本人）。
 *
 * 出参是**自助投影** `MyLeaveRow`（不含员工/部门快照，不含审批实例 id）——
 * 用 `myLeaveToListRow` 转成列表行后与「全部请假」共用同一张表。
 *
 * 员工自助在解耦后的唯一入口：他不再持有 `leave:list`，`/leave/page` 会 403。
 */
export function fetchMyLeaves(query: { page?: number; page_size?: number } = {}): Promise<PageData<MyLeaveRow>> {
  return get<PageData<MyLeaveRow>>(`${BASE}/leave/my`, asParams(query));
}

/** 请假单详情（单据 + 审批实例时间线），权限 `business:hr:leave:detail` */
export function fetchLeaveDetail(id: string): Promise<LeaveDetail> {
  return get<LeaveDetail>(`${BASE}/leave/${id}`);
}

/** 新建请假单（恒为 `DRAFT`；单号与员工快照由服务端定），权限 `business:hr:leave:create` */
export function createLeave(payload: LeaveCreatePayload): Promise<LeaveListRow> {
  return post<LeaveListRow>(`${BASE}/leave`, payload);
}

/** 改草稿（部分更新；非草稿回 409 + 68012），权限 `business:hr:leave:update` */
export function updateLeave(id: string, payload: LeaveUpdatePayload): Promise<LeaveListRow> {
  return put<LeaveListRow>(`${BASE}/leave/${id}`, payload);
}

/** 提交审批（发起 `hr_leave` 流程；无流程回 409 + 66033），权限 `business:hr:leave:submit` */
export function submitLeave(id: string, payload: LeaveSubmitPayload = {}): Promise<LeaveListRow> {
  return post<LeaveListRow>(`${BASE}/leave/${id}/submit`, payload);
}

/**
 * 作废 / 撤回 / 删除（一条路径三件事，`action` 由后端按当前状态决定）。
 *
 * **三条路都要求 `reason` ≥5 字**（后端 `leave_dto::cancel_of` 在状态分流之前统一校验），
 * 所以这里 `reason` 是必填参数而不是可选 —— 类型层就把「忘了传原因」挡住。
 */
export function cancelLeave(id: string, reason: string): Promise<LeaveCancelResult> {
  return post<LeaveCancelResult>(`${BASE}/leave/${id}/cancel`, { reason });
}
