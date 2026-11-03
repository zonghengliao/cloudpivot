/**
 * 状态机边配置的**管理面**接口（【D3】流程配置化 · 阶段 3 出口 / 卡 t_a392a5da）。
 *
 * 契约逐字抄自 `backend/src/business/flow/edge_admin.rs`：
 *
 * | # | 方法 | 路径 | 权限码 | 落点 |
 * |---|------|------|--------|------|
 * | 1 | GET    | `/business/flow/state-edge`                | `business:flow:state-edge:list`   | `list_state_edges`（`StateEdgeListRes`） |
 * | 2 | PUT    | `/business/flow/state-edge`                | `business:flow:state-edge:manage` | `upsert_state_edge`（`EdgeChangeRes`） |
 * | 3 | DELETE | `/business/flow/state-edge`                | `business:flow:state-edge:manage` | `delete_state_edge`（`EdgeChangeRes`） |
 *
 * # 三条不许自己发明的口径
 *
 * 1. **路径**：后端注解 `#[nik::put("/business/flow/state-edge")]` —— 改配置是 **PUT 同一个 URL**，
 *    不是 `POST /state-edge/change`（那是一条不存在的路径，真机实测 404；
 *    `scripts/verify/deployB_e2e.mjs` 的探针与 2026-11-03 的 `probe_state_edge.mjs` 都验证过）。
 * 2. **`PUT` 不会直接落库**：后端先按内核口径「试写 + 回滚」做 dry-run，再起一张
 *    `flow_edge_config` 审批单，审批通过的回调才调 `state_machine::upsert_edge_config`。
 *    所以前端拿到的 `applied = false` 是**正常成功**（已送审），不是失败。
 * 3. **边表 / 状态名 / 文案一律来自后端**：`EdgeRes.from|to|label`、`entity_label` 全部由
 *    `GET` 下发；前端**不允许**再维护一份「哪个状态能到哪个状态」的映射（那是第二份状态机）。
 *
 * # 字段名为什么这么长
 *
 * `PUT` 的请求体在 Rust 侧是 `EdgeUpsertReq`（`serde` 默认字段名）：
 * `entity / from_status / to_status / enabled / requires_reason / label / auth_code / confirm / reason`，
 * 与响应里的 `from / to` **不是**同一套命名（响应是 `EdgeRes.from`）。
 * 看错一个字段名就是「点了变更但什么都没改」，编译期抓不到 —— 所以这里把两套都写全并注明来源。
 */
import { del, get, put } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`，前端写全） */
const BASE = '/api/business/flow/state-edge';

// ==================== 响应（逐字抄 edge_admin.rs） ====================

/**
 * 一条边（`EdgeRes`）。
 *
 * BASE 侧 / 有效侧共用同一形状；`configured` 只在「当前有存活配置行」时为 `true`。
 */
export interface StateEdge {
  /** 变更前状态（如 `DRAFT`） */
  from: string;
  /** 变更后状态（如 `PENDING_POST`） */
  to: string;
  /** 显示文案（配置覆盖后的值；前端直接展示，不要自己拼） */
  label: string;
  /** 是否硬约束边（`true` = 配置禁不掉，`mandatory` 只能为 true） */
  mandatory: boolean;
  /** 走这条边是否必须填原因（BASE ⊙ 配置，**只能加不能减**） */
  requires_reason: boolean;
  /** 配置追加的权限码（BASE 边恒 `null`） */
  auth_code: string | null;
  /** 当前是否有存活配置行 */
  configured: boolean;
  /** 配置是否启用（无边配置时恒 `true`） */
  enabled: boolean;
}

/** 一行存活配置（`ConfigRowRes`，`biz_flow_state_edge` 的原样投影） */
export interface StateEdgeConfigRow {
  /** 实体类型 */
  entity: string;
  /** 变更前状态 */
  from: string;
  /** 变更后状态 */
  to: string;
  /** 是否启用 */
  enabled: boolean;
  /** 是否要求原因 */
  requires_reason: boolean;
  /** 显示文案（`null` = 用 BASE 默认） */
  label: string | null;
  /** 追加的权限码 */
  auth_code: string | null;
}

/** 一张在跑的配置变更审批单（`PendingChangeRes`） */
export interface StateEdgePendingChange {
  /** 审批单 id */
  instance_id: string;
  /** 审批单号（`FLW-…`，可直接在审批中心搜） */
  instance_no: string;
  /** 标题 */
  title: string;
  /** 动作：`UPSERT` / `DELETE` */
  action: string;
  /** 实体类型 */
  entity: string;
  /** 变更前状态 */
  from: string;
  /** 变更后状态 */
  to: string;
  /** 会变成什么样（后端生成的人话，例：`禁用（变成 false）`） */
  summary: string;
  /** 发起人姓名 */
  initiator_name: string | null;
  /** 发起时间（RFC 3339） */
  started_at: string;
}

/** 一个实体的「BASE 边 + 当前配置 + 合并后的有效边」（`EntityEdgesRes`） */
export interface EntityStateEdges {
  /** 实体类型（`STOCK_OUT` / …） */
  entity: string;
  /** 实体中文名（后端下发，前端不用再维护映射） */
  entity_label: string;
  /** 代码里的 BASE 边（真相，配置删不掉） */
  base_edges: StateEdge[];
  /** 当前存活配置行 */
  configs: StateEdgeConfigRow[];
  /** 合并后的有效边（前端按钮该照这份显示） */
  effective_edges: StateEdge[];
  /** 在跑的配置变更审批单 */
  pending_changes: StateEdgePendingChange[];
}

/** `GET /business/flow/state-edge` 的响应（`StateEdgeListRes`） */
export interface StateEdgeListRes {
  /** 各实体的边配置视图（传了 `entity` 就只有一条） */
  list: EntityStateEdges[];
  /** `list` 长度 */
  total: number;
}

/** `PUT` / `DELETE` 的响应（`EdgeChangeRes`）：要么「已送审」，要么「已直接生效」 */
export interface StateEdgeChangeRes {
  /** 是否**已经生效**（`false` = 已送审，等审批 —— 这是正常路径，不是错误） */
  applied: boolean;
  /** 审批单 id（`applied = false` 时必有） */
  instance_id: string | null;
  /** 审批单号（同上） */
  instance_no: string | null;
  /** 审批单状态（`PENDING` / `APPROVED`） */
  instance_status: string | null;
  /** 给人看的结论（含下一步该干什么） */
  message: string;
}

// ==================== 请求（逐字抄 EdgeUpsertReq / EdgeDeleteQuery） ====================

/**
 * `PUT /business/flow/state-edge` 的请求体（`EdgeUpsertReq`）。
 *
 * `confirm` 是后端的**二次确认**硬门（缺了直接 400 / 10001）：语义是「改状态机 = 改全公司的
 * 审批路径」。前端把它做成「变更弹窗里点『提交审批』」这一下，不用再让用户勾一次 —— 但值必须显式传 `true`。
 */
export interface StateEdgeChangeReq {
  /** 实体类型（`STOCK_OUT` / …） */
  entity: string;
  /** 变更前状态 */
  from_status: string;
  /** 变更后状态 */
  to_status: string;
  /** 是否启用（`false` = 禁用这条可选边；mandatory 边禁不掉） */
  enabled: boolean;
  /** 是否要求原因（只能加不能减） */
  requires_reason: boolean;
  /** 显示文案（`null` / 空串 = 用 BASE 的默认文案） */
  label: string | null;
  /** 追加的权限码 */
  auth_code: string | null;
  /** 二次确认（**必须** `true`） */
  confirm: true;
  /** 变更理由（写进审批单标题 / 审计留痕） */
  reason: string | null;
}

/** `DELETE /business/flow/state-edge` 的查询参数（`EdgeDeleteQuery`） */
export interface StateEdgeDeleteQuery {
  /** 实体类型 */
  entity: string;
  /** 变更前状态 */
  from_status: string;
  /** 变更后状态 */
  to_status: string;
  /** 二次确认（**必须** `true`） */
  confirm: true;
  /** 删除理由（写进审批单标题 / 审计留痕） */
  reason?: string;
}

// ==================== 函数 ====================

/**
 * 查状态机边配置。
 *
 * 不传 `entity` 时后端返回 9 个实体全量（一次就能撑起「实体下拉 + 边表」，不必按实体逐个请求）。
 *
 * 异常：400 + `65005`（`entity` 不是 9 个实体之一，后端不静默返回空列表）/ 500（数据库）。
 *
 * @param entity 可选实体类型；空串 / 不传 = 全部
 */
export function listStateEdges(entity?: string): Promise<StateEdgeListRes> {
  const params: Record<string, unknown> = {};
  // 空串必须不发：后端把 `?entity=` 当「NotEmpty 校验失败」处理（会 400）
  if (entity && entity.trim()) {
    params.entity = entity.trim();
  }
  return get<StateEdgeListRes>(BASE, params);
}

/**
 * 改一条边的配置 —— **走审批，不直写**。
 *
 * 后端行为（`upsert_state_edge`）：
 * 1. dry-run 校验（非法补丁在这一步就 400，不会「审批通过了才发现配不了」）；
 * 2. 起一张 `flow_edge_config` 审批单，`form_data` = 待变更内容；
 * 3. 审批通过才由回调调内核 `upsert_edge_config` 落库。
 *
 * 所以 `applied = false` + `instance_no` 是**成功送审**；只有 `applied = true` 才表示
 * 「没有已发布流程 + 持 `business:erp:skip-approval` + 理由 ≥10 字」的显式跳过。
 *
 * 异常：400（二次确认缺失 / 未知实体 / 不是 BASE 边 / 禁用 mandatory / 放宽 requires_reason）、
 * 403（非超管 / 无 manage 码）、409（同一条边已有在跑的审批单，报文带审批单号）。
 */
export function changeStateEdge(body: StateEdgeChangeReq): Promise<StateEdgeChangeRes> {
  return put<StateEdgeChangeRes>(BASE, body);
}

/**
 * 删掉一条边的配置（语义 = 回到 `BASE_EDGES` 行为，**同样走审批**）。
 *
 * 异常：400（二次确认缺失 / 未知实体 / 不是 BASE 边）、404（本来就没有配置 ——
 * 重复删除不产生审批单）、409（同一条边已有在跑的审批单）。
 */
export function deleteStateEdge(query: StateEdgeDeleteQuery): Promise<StateEdgeChangeRes> {
  return del<StateEdgeChangeRes>(BASE, { ...query });
}
