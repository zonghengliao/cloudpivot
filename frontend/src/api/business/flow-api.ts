/**
 * 审批流接口（批次 D · D1 定义侧 12 个接口 + D2 实例侧契约）。
 *
 * 路径与后端 `backend/src/business/flow/handler.rs` 的
 * `#[nik::get/post/put/delete("/business/flow/...")]` 逐条对应（注解不含 `/api`，前端写全）。
 *
 * # 定义侧（D1 已交付，可直接联调）
 *
 * | # | 方法 | 路径 | 权限码 |
 * |---|------|------|--------|
 * | 1 | GET | `/business/flow/def/page` | `business:flow:read` |
 * | 2 | GET | `/business/flow/def/{id}` | `business:flow:read` |
 * | 3 | POST | `/business/flow/def` | `business:flow:create` |
 * | 4 | PUT | `/business/flow/def/{id}` | `business:flow:update` |
 * | 5 | DELETE | `/business/flow/def/{id}` | `business:flow:delete` |
 * | 6 | POST | `/business/flow/def/{id}/publish` | `business:flow:publish` |
 * | 7 | POST | `/business/flow/def/{id}/offline` | `business:flow:publish` |
 * | 8 | GET | `/business/flow/def/{id}/versions` | `business:flow:read` |
 * | 9 | GET | `/business/flow/version/{vid}` | `business:flow:read` |
 * | 10 | PUT | `/business/flow/version/{vid}/graph` | `business:flow:update` |
 * | 11 | POST | `/business/flow/version/{vid}/validate` | `business:flow:update` |
 * | 12 | POST | `/business/flow/def/{id}/draft` | `business:flow:update` |
 *
 * # 实例侧（D2 已交付，D4a 补齐列表字段与 `/instance/my`）
 *
 * 所有函数都打真接口，**不许在前端造数据**：接口失败就渲染错误态 / 空态（红线）。
 *
 * # 三条取值口径（照后端 `dto.rs`，别自己发明）
 *
 * 1. **节点类型大写**：`START|APPROVAL|COUNTERSIGN|CONDITION|PARALLEL|CC|END`；
 * 2. **版本状态**：`DRAFT|PUBLISHED|ARCHIVED`；流程状态：`DRAFT|PUBLISHED|DISABLED`
 *    （注意不是 `OFFLINE`：D1 的 `dto::DEF_STATUSES` 里停用叫 `DISABLED`）；
 * 3. **时间一律 RFC3339**（`DateTime<Utc>`），格式化交给页面。
 */
import { del, get, post, put, type PageData } from '@/api/request';

/**
 * 把强类型查询对象交给 axios 的 `params`。
 *
 * 丢掉空串 / null / undefined：后端把 `?status=` 当作「等于空串」的过滤条件，
 * 页面里 `query.status = ''` 表示「不过滤」，必须根本不发这个参数。
 */
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

// ==================== 枚举字面量 ====================

/** 七种节点类型（与后端 `graph::NodeType` 逐字一致） */
export type FlowNodeType =
  | 'START'
  | 'APPROVAL'
  | 'COUNTERSIGN'
  | 'CONDITION'
  | 'PARALLEL'
  | 'CC'
  | 'END'
  | 'TASK_NODE'
  | 'NOTIFY_NODE';

/** 审批人来源（与后端 `graph::AssigneeKind` 逐字一致） */
export type FlowAssigneeKind =
  | 'USER'
  | 'ROLE'
  | 'INITIATOR'
  | 'INITIATOR_SUPERIOR'
  | 'DEPT_LEADER'
  | 'TASK_OWNER'
  | 'TASK_CREATOR'
  | 'TASK_ASSIGNER'
  | 'FORM_VAR'
  | 'ADMIN';

/** 可「落行」的审批人来源（只有这三种能进 `biz_flow_node_assignee`） */
export type FlowAssigneeRowKind = 'USER' | 'ROLE' | 'DEPT_LEADER';

/** 会签模式（后端 `graph::ApprovalMode`） */
export type FlowApprovalMode = 'ALL' | 'RATIO' | 'ANY';

/** 网关模式（后端 `graph::GatewayMode`；并行网关分叉 / 条件节点汇聚都用它） */
export type FlowGatewayMode = 'AND' | 'OR';

/** 空审批人策略（后端 `dto::EMPTY_ASSIGNEE_POLICIES`） */
export type FlowEmptyAssignee = 'SKIP' | 'ERROR' | 'FALLBACK_ADMIN';

/** 自审策略（后端 `dto::SELF_APPROVAL_POLICIES`） */
export type FlowSelfApproval = 'ALLOW' | 'SKIP' | 'FORBID';

/** 超时动作（后端 `dto::TIMEOUT_ACTIONS`；`AUTO_APPROVE` 需流程级 `allow_auto_approve`） */
export type FlowTimeoutAction = 'REMIND' | 'AUTO_APPROVE' | 'AUTO_REJECT' | 'ESCALATE';

/** 流程定义状态（`DISABLED` 是停用，不叫 OFFLINE） */
export type FlowDefStatus = 'DRAFT' | 'PUBLISHED' | 'DISABLED';

/** 版本状态 */
export type FlowVersionStatus = 'DRAFT' | 'PUBLISHED' | 'ARCHIVED';

// ==================== 定义侧类型（D1） ====================

/** 节点审批人（响应 `AssigneeRes`） */
export interface FlowAssignee {
  /** `USER` / `ROLE` / `DEPT_LEADER` */
  type: string;
  /** 目标 id */
  ref_id: string;
  /** 顺序 */
  sort_order: number;
}

/** 图节点（响应 `GraphNodeRes`；`x`/`y` 是画布逻辑坐标，落库就是这个） */
export interface FlowNode {
  /** 节点 key（图内唯一） */
  key: string;
  /** 名称 */
  name: string;
  /** 七种类型之一 */
  type: string;
  /** 画布 x */
  x: number;
  /** 画布 y */
  y: number;
  /** 排序 */
  sort_order: number;
  /** 审批人来源（非审批类节点为 null） */
  assignee_type: string | null;
  /** 主管层数（`INITIATOR_SUPERIOR` 用） */
  superior_levels: number;
  /** `FORM_VAR` 变量名 */
  form_var_key: string | null;
  /** 会签模式 */
  approval_mode: string | null;
  /** 会签阈值 (0,1] */
  approve_ratio: number | null;
  /** 一票否决 */
  reject_veto: boolean;
  /** 空审批人策略 */
  empty_assignee: string;
  /** 自审策略 */
  self_approval: string;
  /** 是否允许机器人审批 */
  allow_robot: boolean;
  /** 汇聚模式 */
  join_mode: string;
  /** 分叉模式 */
  fork_mode: string;
  /** 超时小时 */
  timeout_hours: number | null;
  /** 超时动作 */
  timeout_action: string;
  /** 提前提醒小时 */
  remind_before_hours: number | null;
  /** 最多提醒次数 */
  max_remind_times: number;
  /** 通过后抄送 */
  cc_on_approved: boolean;
  /** 扩展配置 */
  config: unknown;
  /** 可枚举审批人 */
  assignees: FlowAssignee[];
}

/** 图连线（响应 `GraphEdgeRes`） */
export interface FlowEdge {
  /** 边 key */
  key: string;
  /** 起点 node key */
  from: string;
  /** 终点 node key */
  to: string;
  /** 条件 DSL（结构见架构 §3.4） */
  condition: unknown;
  /** 条件标签 */
  label: string | null;
  /** 优先级 */
  priority: number;
  /** 是否兜底分支 */
  is_default: boolean;
}

/** 流程定义（响应 `DefRes`） */
export interface FlowDef {
  /** 主键 */
  id: string;
  /** 编码（小写 + 数字 + 下划线） */
  flow_code: string;
  /** 名称 */
  flow_name: string;
  /** 业务类型 */
  biz_type: string;
  /** 分类（空串 = 无） */
  category: string;
  /** 说明（空串 = 无） */
  description: string;
  /** `DRAFT` / `PUBLISHED` / `DISABLED` */
  status: string;
  /** 当前发布版本 id（未发布 → null） */
  current_version_id: string | null;
  /** 当前发布版本号（未发布 → 0） */
  current_version_no: number;
  /** 是否该业务类型的默认流程 */
  is_default: boolean;
  /** 是否允许超时自动通过 */
  allow_auto_approve: boolean;
  /** 当前版本节点数 */
  node_count: number;
  /** 当前版本边数 */
  edge_count: number;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/** 创建流程回执（`DefSaveRes`） */
export interface FlowDefSaveRes {
  /** 流程 id */
  id: string;
  /** 初始草稿版本 id */
  version_id: string;
  /** 版本号（新建 = 1） */
  version_no: number;
  /** 节点数 */
  node_count: number;
  /** 边数 */
  edge_count: number;
}

/** 版本摘要（`VersionSummaryRes`） */
export interface FlowVersionSummary {
  /** 版本 id */
  id: string;
  /** 版本号 */
  version_no: number;
  /** `DRAFT` / `PUBLISHED` / `ARCHIVED` */
  version_status: string;
  /** 是否当前（发布）版本 */
  is_current: boolean;
  /** 节点数 */
  node_count: number;
  /** 边数 */
  edge_count: number;
  /** 图 hash（未发布 → null） */
  graph_hash: string | null;
  /** 变更备注 */
  change_note: string | null;
  /** 发布时间 */
  published_at: string | null;
  /** 发布人 id */
  published_by: string | null;
  /** 发布人姓名（空串 = 未知 / 机器人） */
  published_by_name: string;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/** 版本详情（`VersionRes`：摘要 + 整图） */
export interface FlowVersion extends FlowVersionSummary {
  /** 所属流程 id */
  def_id: string;
  /** 所属流程编码 */
  flow_code: string;
  /** 所属流程名称 */
  flow_name: string;
  /** 是否可编辑（只有 DRAFT 为 true —— 前端据此禁用画布） */
  editable: boolean;
  /** 快照是否已生成（PUBLISHED 才有） */
  has_snapshot: boolean;
  /** 节点 */
  nodes: FlowNode[];
  /** 连线 */
  edges: FlowEdge[];
}

/** 一条校验不通过项（`IssueRes`；`nodes` 是节点 key，画布高亮就用它） */
export interface FlowIssue {
  /** 规则编号 R1–R15 */
  rule: string;
  /** 业务错误码 65xxx */
  code: number;
  /** 中文文案 */
  message: string;
  /** 涉及节点 key */
  nodes: string[];
}

/** 只校验不保存（`ValidateRes`） */
export interface FlowValidateRes {
  /** 版本 id */
  version_id: string;
  /** 版本号 */
  version_no: number;
  /** 全部通过 → true */
  passed: boolean;
  /** 全部不通过项（不是第一个） */
  issues: FlowIssue[];
  /** 节点数 */
  node_count: number;
  /** 边数 */
  edge_count: number;
}

/** 保存整图回执（`GraphSaveRes`） */
export interface FlowGraphSaveRes {
  /** 版本 id */
  version_id: string;
  /** 版本号 */
  version_no: number;
  /** 节点数 */
  node_count: number;
  /** 边数 */
  edge_count: number;
  /** 不阻断保存、但发布会拦的提示 */
  warnings: string[];
}

/** 发布回执（`PublishRes`） */
export interface FlowPublishRes {
  /** 流程 id */
  def_id: string;
  /** 发布后的版本 id */
  version_id: string;
  /** 版本号 */
  version_no: number;
  /** 流程状态（发布后 = PUBLISHED） */
  status: string;
  /** 图 hash（sha256） */
  graph_hash: string;
  /** 节点数 */
  node_count: number;
  /** 边数 */
  edge_count: number;
  /** 发布时间 */
  published_at: string;
}

/** 停用回执（`OfflineRes`） */
export interface FlowOfflineRes {
  /** 流程 id */
  id: string;
  /** 停用后的状态（DISABLED） */
  status: string;
  /** 当前版本号 */
  current_version_no: number;
}

/** 删除回执（`DefDeleteRes`） */
export interface FlowDefDeleteRes {
  /** 是否已删除 */
  deleted: boolean;
  /** 编码 */
  flow_code: string;
}

/** 另存草稿回执（`DraftRes`） */
export interface FlowDraftRes {
  /** 流程 id */
  def_id: string;
  /** 新草稿版本 id */
  version_id: string;
  /** 新版本号 */
  version_no: number;
  /** 复制来源版本号 */
  source_version_no: number;
  /** 节点数 */
  node_count: number;
  /** 边数 */
  edge_count: number;
}

/** 定义分页查询 */
export interface FlowDefPageQuery {
  /** 页码（1 起） */
  page?: number;
  /** 每页条数（上限 100） */
  page_size?: number;
  /** 编码 / 名称关键词 */
  keyword?: string;
  /** 状态 `DRAFT|PUBLISHED|DISABLED` */
  status?: string;
  /** 业务类型 */
  biz_type?: string;
}

/** 新建流程请求（`DefCreateReq`） */
export interface FlowDefCreateReq {
  /** 编码（必填，小写 + 数字 + 下划线，≥2 字符） */
  flow_code: string;
  /** 名称（必填） */
  flow_name: string;
  /** 业务类型（必填） */
  biz_type: string;
  /** 分类 */
  category?: string | null;
  /** 说明 */
  description?: string | null;
  /** 是否该业务类型的默认流程 */
  is_default?: boolean;
  /** 是否允许超时自动通过 */
  allow_auto_approve?: boolean;
}

/**
 * 改流程元信息（`DefUpdateReq`）。
 *
 * 后端用 `double_option` 区分「缺席」与「显式 null」：字段**缺席 = 不改**，
 * 显式 `null` = 清空。所以清空分类 / 说明时必须显式传 `null`，不能省字段。
 */
export interface FlowDefUpdateReq {
  /** 名称（缺席 = 不改） */
  flow_name?: string;
  /** 业务类型（缺席 = 不改） */
  biz_type?: string;
  /** 分类（缺席 = 不改；`null` = 清空） */
  category?: string | null;
  /** 说明（缺席 = 不改；`null` = 清空） */
  description?: string | null;
  /** 是否默认 */
  is_default?: boolean;
  /** 是否允许超时自动通过 */
  allow_auto_approve?: boolean;
}

/** 节点审批人（请求） */
export interface FlowAssigneeReq {
  /** 来源（只能 USER / ROLE / DEPT_LEADER） */
  type: FlowAssigneeRowKind;
  /** 目标 id */
  ref_id: string;
  /** 顺序（缺省按数组下标） */
  sort_order?: number;
}

/**
 * 图节点（请求 `GraphNodeReq`）。
 *
 * 字段名与架构 §10.4.3 的样例一致 —— 只有 `type` 例外（JS 关键字，后端 `#[serde(rename)]`）。
 */
export interface FlowNodeReq {
  /** 节点 key */
  key: string;
  /** 名称 */
  name: string;
  /** 类型 */
  type: FlowNodeType;
  /** 画布 x */
  x: number;
  /** 画布 y */
  y: number;
  /** 排序 */
  sort_order?: number;
  /** 审批人来源 */
  assignee_type?: string | null;
  /** 主管层数 */
  superior_levels?: number;
  /** FORM_VAR 变量名 */
  form_var_key?: string | null;
  /** 会签模式 */
  approval_mode?: string | null;
  /** 会签阈值 */
  approve_ratio?: number | null;
  /** 一票否决 */
  reject_veto?: boolean;
  /** 空审批人策略 */
  empty_assignee?: string;
  /** 自审策略 */
  self_approval?: string;
  /** 是否允许机器人 */
  allow_robot?: boolean;
  /** 汇聚模式 */
  join_mode?: string;
  /** 分叉模式 */
  fork_mode?: string;
  /** 超时小时 */
  timeout_hours?: number | null;
  /** 超时动作 */
  timeout_action?: string;
  /** 提前提醒小时 */
  remind_before_hours?: number | null;
  /** 最多提醒次数 */
  max_remind_times?: number;
  /** 通过后抄送 */
  cc_on_approved?: boolean;
  /** 扩展配置 */
  config?: unknown;
  /** 可枚举审批人 */
  assignees?: FlowAssigneeReq[];
}

/** 图连线（请求 `GraphEdgeReq`） */
export interface FlowEdgeReq {
  /** 边 key */
  key: string;
  /** 起点 key */
  from: string;
  /** 终点 key */
  to: string;
  /** 条件 DSL */
  condition?: unknown;
  /** 条件标签 */
  label?: string | null;
  /** 优先级 */
  priority?: number;
  /** 是否兜底分支 */
  is_default?: boolean;
}

/** 保存整图请求（`GraphSaveReq`；整图覆盖，只写 DRAFT） */
export interface FlowGraphSaveReq {
  /** 节点 */
  nodes: FlowNodeReq[];
  /** 连线 */
  edges: FlowEdgeReq[];
  /** 变更备注 */
  change_note?: string | null;
}

/** 另存草稿请求（`DraftReq`） */
export interface FlowDraftReq {
  /** 从哪个版本复制（缺省 = 当前发布版本；没发布过 → 最新版本） */
  source_version_id?: string | null;
  /** 变更备注 */
  change_note?: string | null;
}

// ==================== 定义侧函数（D1，可联调） ====================

/**
 * 流程定义分页（`GET /business/flow/def/page`）。
 *
 * # 参数
 * - `query`：页码 / 关键词 / 状态 / 业务类型
 */
export function pageFlowDefs(query: FlowDefPageQuery = {}): Promise<PageData<FlowDef>> {
  return get<PageData<FlowDef>>('/api/business/flow/def/page', asParams(query));
}

/**
 * 流程定义详情（`GET /business/flow/def/{id}`）。
 *
 * # 参数
 * - `id`：流程 id（UUID；非法值后端回 400 + 65005）
 */
export function getFlowDef(id: string): Promise<FlowDef> {
  return get<FlowDef>(`/api/business/flow/def/${id}`);
}

/**
 * 新建流程（`POST /business/flow/def`）。
 *
 * 后端同时建 v1 草稿 + START/END 两个默认节点 + 一条边，返回草稿版本 id。
 */
export function createFlowDef(body: FlowDefCreateReq): Promise<FlowDefSaveRes> {
  return post<FlowDefSaveRes>('/api/business/flow/def', body);
}

/** 改流程元信息（`PUT /business/flow/def/{id}`）；空对象会被后端 400 拒掉。 */
export function updateFlowDef(id: string, body: FlowDefUpdateReq): Promise<FlowDef> {
  return put<FlowDef>(`/api/business/flow/def/${id}`, body);
}

/** 删除流程（`DELETE /business/flow/def/{id}`，软删；有在跑审批单 → 409 + 65021）。 */
export function deleteFlowDef(id: string): Promise<FlowDefDeleteRes> {
  return del<FlowDefDeleteRes>(`/api/business/flow/def/${id}`);
}

/**
 * 发布（`POST /business/flow/def/{id}/publish`）。
 *
 * 后端跑 R1–R15，全部通过才写快照并切 `current_version_id`；
 * 有任一不通过 → 400 + 65xxx，`msg` 是汇总文案（前 5 条 + 共 N 条）。
 */
export function publishFlowDef(id: string): Promise<FlowPublishRes> {
  return post<FlowPublishRes>(`/api/business/flow/def/${id}/publish`);
}

/** 停用（`POST /business/flow/def/{id}/offline`；不改在跑实例）。 */
export function offlineFlowDef(id: string): Promise<FlowOfflineRes> {
  return post<FlowOfflineRes>(`/api/business/flow/def/${id}/offline`);
}

/** 版本列表（`GET /business/flow/def/{id}/versions`，按版本号倒序）。 */
export function listFlowVersions(id: string): Promise<{ list: FlowVersionSummary[] }> {
  return get<{ list: FlowVersionSummary[] }>(`/api/business/flow/def/${id}/versions`);
}

/** 版本详情 + 整图（`GET /business/flow/version/{vid}`）。 */
export function getFlowVersion(vid: string): Promise<FlowVersion> {
  return get<FlowVersion>(`/api/business/flow/version/${vid}`);
}

/**
 * 保存整图（`PUT /business/flow/version/{vid}/graph`）。
 *
 * **只允许 DRAFT 版本**：PUBLISHED 直接 409 + 65022（服务端硬拒，不是前端隐藏按钮）。
 * 保存期只做结构校验（key 格式 / 唯一、from/to 存在、无自环、无重复边），
 * 发布级问题进 `warnings` 返回、不阻断。
 */
export function saveFlowGraph(vid: string, body: FlowGraphSaveReq): Promise<FlowGraphSaveRes> {
  return put<FlowGraphSaveRes>(`/api/business/flow/version/${vid}/graph`, body);
}

/**
 * 只校验不保存（`POST /business/flow/version/{vid}/validate`）。
 *
 * 校验的是**库里已保存的图**，所以编辑器必须先保存再校验（页面里「校验」按钮就是保存 + 校验）。
 * 返回 200（校验本身成功），不通过与否看 `passed`。
 */
export function validateFlowVersion(vid: string): Promise<FlowValidateRes> {
  return post<FlowValidateRes>(`/api/business/flow/version/${vid}/validate`);
}

/**
 * 另存为新草稿（`POST /business/flow/def/{id}/draft`）。
 *
 * 「编辑已发布版本」的唯一正确姿势：复制一份 version_no = max + 1 的新 DRAFT，
 * 原 PUBLISHED 版本一行不动（架构 §3.6.3）。
 */
export function createFlowDraft(id: string, body: FlowDraftReq = {}): Promise<FlowDraftRes> {
  return post<FlowDraftRes>(`/api/business/flow/def/${id}/draft`, body);
}

// ==================== 实例侧类型（D2 已交付，2026-10-29 对齐） ====================

/**
 * 审批任务（`TaskRes`）。
 *
 * 列表接口（todo / done）一次 JOIN 取齐展示字段：`flow_name` / `initiator_name` /
 * `current_node_name` / `arrived_at` 都由后端下发（D4a 补齐），卡片不用再点进详情才知道节点中文名。
 */
export interface FlowTaskItem {
  /** 待办 id */
  id: string;
  /** 实例 id */
  instance_id: string;
  /** 单据号（列表场景填） */
  instance_no: string;
  /** 单据标题（列表场景填） */
  title: string;
  /** 流程名称（如「财务审批」） */
  flow_name: string;
  /** 发起人姓名（发起人已删 → 空串） */
  initiator_name: string;
  /** 当前节点中文名（如「主管审批」；**卡片显示它，不显示 node_key**） */
  current_node_name: string;
  /** 到达时间（= `created_at`；待办列表按它倒序） */
  arrived_at: string;
  /** 节点 key */
  node_key: string;
  /** 轮次（退回一次 +1） */
  round: number;
  /** 种类 `APPROVAL` / `RESUBMIT` */
  task_kind: string;
  /** 审批人 id */
  assignee_id: string;
  /** 审批人姓名 */
  assignee_name: string;
  /** 审批人来源（8 种之一） */
  assignee_source: string;
  /** 任务状态 `PENDING|APPROVED|REJECTED|SKIPPED|TERMINATED|TRANSFERRED|DELEGATED` */
  status: string;
  /**
   * 所属审批实例的状态（P11）。
   * 与 `status`（待办自身状态）是两个维度：任务取消/删除时两者应当一起终态，
   * 但历史脏数据里可能出现「待办还 PENDING + 实例已终态」的孤儿组合 ——
   * 前端据此把这种行挡在待办列表外（或标成「该审批已结束」），不给点不动的按钮。
   */
  instance_status: string;
  /** 是否加签 */
  is_added_sign: boolean;
  /** 是否自审 */
  is_self_approval: boolean;
  /** 是否系统代点 */
  is_auto: boolean;
  /** 意见 */
  comment: string;
  /** 转办来源行 */
  transferred_from: string;
  /** 创建时间 */
  created_at: string;
  /** 完成时间 */
  completed_at: string | null;
}

/** 抄送条目（`CcRes`） */
export interface FlowCcItem {
  /** 抄送行 id */
  id: string;
  /** 实例 id */
  instance_id: string;
  /** 单据号 */
  instance_no: string;
  /** 单据标题 */
  title: string;
  /** 流程名称 */
  flow_name: string;
  /** 发起人姓名（已删 → 空串） */
  initiator_name: string;
  /** 节点中文名（节点行缺失 → 空串） */
  current_node_name: string;
  /** 抄送到达时间（= `created_at`；抄送列表按它倒序） */
  arrived_at: string;
  /** 节点 key */
  node_key: string;
  /** 轮次 */
  round: number;
  /** 是否已读 */
  is_read: boolean;
  /** 读取时间 */
  read_at: string | null;
  /** 创建时间 */
  created_at: string;
}

/** 审批单摘要（`InstanceRes`） */
export interface FlowInstance {
  /** 主键 */
  id: string;
  /** 单据号 `FLW-YYYYMM-NNNNNN` */
  instance_no: string;
  /** 流程编码 */
  flow_code: string;
  /** 流程名称 */
  flow_name: string;
  /** 绑定的版本号（实例绑版本） */
  version_no: number;
  /** 业务类型 */
  biz_type: string;
  /** 业务 id（空串 = 无） */
  biz_id: string;
  /** 业务单号（空串 = 无） */
  biz_no: string;
  /** 标题 */
  title: string;
  /** 状态 `RUNNING|APPROVED|REJECTED|WITHDRAWN|VOID|CANCELLED` */
  status: string;
  /** 发起人 id */
  initiator_id: string;
  /** 发起人姓名 */
  initiator_name: string;
  /** 发起人部门 id（空串 = 无） */
  initiator_dept_id: string;
  /** 表单数据（发起时冻结在 `biz_flow_instance.form_data`；无表单 → `{}`） */
  form_data?: Record<string, unknown>;
  /** 自动通过的节点数（审计口径，详情页要显著提示） */
  auto_passed_nodes: number;
  /** 最大退回轮次（打回次数 = round_total - 1） */
  round_total: number;
  /** 结束说明 */
  result_comment: string;
  /** 发起时间 */
  started_at: string;
  /** 结束时间 */
  ended_at: string | null;
  /** 创建时间 */
  created_at: string;
  /** 我在本单上的待办 id（没有 → 空串） */
  my_task_id: string;
}

/** 节点进度（`NodeProgressRes`） */
export interface FlowNodeProgress {
  /** 节点 key */
  node_key: string;
  /** 节点名（**这里才有中文名**，列表接口没有） */
  node_name: string;
  /** 节点类型 */
  node_type: string;
  /** 轮次 */
  round: number;
  /** 节点实例状态 `PENDING|ACTIVE|APPROVED|REJECTED|SKIPPED|TERMINATED` */
  status: string;
  /** 会签模式 */
  approval_mode: string;
  /** 会签阈值（0 = 非按比例） */
  approve_ratio: number;
  /** 应到人数 */
  approver_count: number;
  /** 已同意 */
  approved_count: number;
  /** 已驳回 */
  rejected_count: number;
  /** 是否自动通过 */
  is_auto_passed: boolean;
  /** 条件命中的边 */
  taken_edge_key: string;
  /** 激活时间 */
  activated_at: string | null;
  /** 结束时间 */
  ended_at: string | null;
  /** 耗时（毫秒） */
  duration_ms: number | null;
}

/** 审批单详情（`InstanceDetailRes`） */
export interface FlowInstanceDetail {
  /** 实例摘要 */
  instance: FlowInstance;
  /** 节点进度（按 round / 到达顺序） */
  nodes: FlowNodeProgress[];
  /** 审批时间线（含 SKIPPED / TRANSFERRED 留痕） */
  timeline: FlowTaskItem[];
  /** 抄送 */
  cc: FlowCcItem[];
  /** 我在本单上的 PENDING 待办（没有 → null） */
  my_task: FlowTaskItem | null;
  /** 自动通过节点数（与 `instance.auto_passed_nodes` 同值） */
  auto_passed_nodes: number;
}

/** 审批动作回执（`ActionRes`） */
export interface FlowActionRes {
  /** 实例 id */
  instance_id: string;
  /** 动作后的实例状态 */
  instance_status: string;
  /** 当前节点状态 */
  node_status: string;
  /** 待办是否处理完 */
  done: boolean;
  /** 是否幂等命中（重复提交 / 23505 兜底） */
  idempotent: boolean;
  /** 本次激活的下游节点 key */
  next_nodes: string[];
  /** 自动通过节点数 */
  auto_passed_nodes: number;
  /** 应到人数 */
  approver_count: number;
  /** 已同意 */
  approved_count: number;
  /** 已驳回 */
  rejected_count: number;
}

/** 发起审批请求（`InstanceCreateReq`；`def_id` / `flow_code` / `biz_type` 三选一） */
export interface FlowInstanceCreateReq {
  /** 流程定义 id */
  def_id?: string;
  /** 流程编码 */
  flow_code?: string;
  /** 业务类型（未给前两者时取该类型的默认发布流程） */
  biz_type?: string;
  /** 业务单据 id（给了会做「同一单据只能有一个在跑实例」校验） */
  biz_id?: string;
  /** 业务单号（展示用） */
  biz_no?: string;
  /** 标题（必填） */
  title: string;
  /** 表单数据（条件表达式的作用域，发起时冻结） */
  form_data?: Record<string, unknown>;
}

/** 实例分页查询（`InstancePageQuery`） */
export interface FlowInstancePageQuery {
  /** 页码 */
  page?: number;
  /** 每页条数 */
  page_size?: number;
  /** 单据号 / 标题关键词 */
  keyword?: string;
  /** 状态 */
  status?: string;
  /** 流程编码 */
  flow_code?: string;
  /** 业务类型 */
  biz_type?: string;
}

// ==================== 实例侧函数（D2 已交付，可直接联调） ====================

/**
 * 发起审批（`POST /business/flow/instance`）。
 *
 * 只有 `PUBLISHED` 的流程能发起（草稿 → 400 + 65005，不是 404）。
 * 返回 `ActionRes`，**拿 `instance_id` 跳详情**（没有 `instance_no`，要展示单据号请再查一次详情）。
 */
export function createFlowInstance(body: FlowInstanceCreateReq): Promise<FlowActionRes> {
  return post<FlowActionRes>('/api/business/flow/instance', body);
}

/** 审批单分页（`GET /business/flow/instance/page`，可见性由后端按参与关系过滤）。 */
export function pageFlowInstances(query: FlowInstancePageQuery = {}): Promise<PageData<FlowInstance>> {
  return get<PageData<FlowInstance>>('/api/business/flow/instance/page', asParams(query));
}

/**
 * 我发起的（`GET /business/flow/instance/my?page=1&page_size=20`）。
 *
 * 口径是 `initiator_id = 我`、按发起时间倒序（D4a）。**不是**可见性并集：
 * 抄送我的 / 待我审的单子不会混进来，也不需要前端再按 `initiator_id` 过滤。
 */
export function listMyInstances(query: { page?: number; page_size?: number } = {}): Promise<PageData<FlowInstance>> {
  return get<PageData<FlowInstance>>('/api/business/flow/instance/my', asParams(query));
}

/** 审批单详情（`GET /business/flow/instance/{id}`：实例 + 节点进度 + 时间线 + 抄送 + 我的待办）。 */
export function getFlowInstance(id: string): Promise<FlowInstanceDetail> {
  return get<FlowInstanceDetail>(`/api/business/flow/instance/${id}`);
}

/** 撤回（`POST /business/flow/instance/{id}/withdraw`；只允许发起人、且零人已审 → 否则 409 + 65023）。 */
export function withdrawFlowInstance(id: string, reason?: string): Promise<FlowActionRes> {
  return post<FlowActionRes>(`/api/business/flow/instance/${id}/withdraw`, { reason: reason ?? null });
}

/** 我的待办（`GET /business/flow/task/todo`，`APPROVAL` + `RESUBMIT`，只返回 PENDING）。 */
export function listTodoTasks(query: { page?: number; page_size?: number } = {}): Promise<PageData<FlowTaskItem>> {
  return get<PageData<FlowTaskItem>>('/api/business/flow/task/todo', asParams(query));
}

/** 我的已办（`GET /business/flow/task/done`，含被转办出去的，按处理时间倒序）。 */
export function listDoneTasks(query: { page?: number; page_size?: number } = {}): Promise<PageData<FlowTaskItem>> {
  return get<PageData<FlowTaskItem>>('/api/business/flow/task/done', asParams(query));
}

/** 抄送我的（`GET /business/flow/task/cc`；**路径是 task/cc，不是 cc/my**）。 */
export function listMyCc(query: { page?: number; page_size?: number } = {}): Promise<PageData<FlowCcItem>> {
  return get<PageData<FlowCcItem>>('/api/business/flow/task/cc', asParams(query));
}

/** 同意（`POST /business/flow/task/{id}/approve`，意见可选；会签时只记自己那一票）。 */
export function approveFlowTask(id: string, body: { comment?: string } = {}): Promise<FlowActionRes> {
  return post<FlowActionRes>(`/api/business/flow/task/${id}/approve`, body);
}

/**
 * 驳回（`POST /business/flow/task/{id}/reject`）。
 *
 * `comment` 必填（≥5 字）；`return_to` ∈ `PREV`（默认）/ `INITIATOR` / `NONE` / 节点 key。
 * 返回 409 + 65020 表示「已被别人处理」——前端按「已处理，刷新」处理，不是错误弹窗。
 */
export function rejectFlowTask(
  id: string,
  body: { comment: string; return_to?: string },
): Promise<FlowActionRes> {
  return post<FlowActionRes>(`/api/business/flow/task/${id}/reject`, body);
}

/** 转办（`POST /business/flow/task/{id}/transfer`）。 */
export function transferFlowTask(id: string, body: { to_user_id: string; reason?: string }): Promise<FlowActionRes> {
  return post<FlowActionRes>(`/api/business/flow/task/${id}/transfer`, body);
}

/** 加签（`POST /business/flow/task/{id}/add-sign`，≤ 10 人）。 */
export function addSignFlowTask(id: string, body: { user_ids: string[] }): Promise<FlowActionRes> {
  return post<FlowActionRes>(`/api/business/flow/task/${id}/add-sign`, body);
}

/** 批量审批（`POST /business/flow/task/batch`，≤ 50 条，逐条独立事务）。 */
export function batchFlowTasks(body: {
  action: 'APPROVE' | 'REJECT';
  task_ids: string[];
  comment?: string;
  return_to?: string;
}): Promise<{ total: number; succeeded: number; failed: number; items: Array<{ task_id: string; ok: boolean; code: number; message: string }> }> {
  return post<{ total: number; succeeded: number; failed: number; items: Array<{ task_id: string; ok: boolean; code: number; message: string }> }>(
    '/api/business/flow/task/batch',
    body,
  );
}

// ==================== 任务节点预演（P-FLOW-1A 接口 / P-FLOW-1C 前端接入） ====================

/**
 * 预演请求里的指派配置（后端 `PreviewAssigneeReq`）。
 *
 * 形状与 `biz_flow_node` 的列 + `biz_flow_node_assignee` 表同款（决策 D3：
 * 不造第二套指派规则，预演走**同一个** `assignee::resolve`）。
 * 本卡只填得起 `assignee_type` / `role_codes` / `user_ids` / `empty_assignee` / `allow_robot`
 * 这几项（面板上能编辑的那几个），其余留空 = 用后端缺省。
 */
export interface TaskPreviewAssigneeReq {
  /** 来源类型（`USER` / `ROLE` / `INITIATOR` …） */
  assignee_type?: string | null;
  /** `ROLE` 的角色编码 */
  role_codes?: string[];
  /** `USER` 的用户 id */
  user_ids?: string[];
  /** 主管层数 */
  superior_levels?: number | null;
  /** `FORM_VAR` 的变量名 */
  form_var_key?: string | null;
  /** 空负责人策略 */
  empty_assignee?: string | null;
  /** 是否允许机器人 */
  allow_robot?: boolean | null;
}

/**
 * 预演请求（后端 `TaskPreviewReq`）。
 *
 * `strict` 的两种模式：`false`（默认）缺变量渲染成可见占位 `〔form.x〕` + warnings；
 * `true` 与运行期一致，缺变量直接 `400 + 65039`。
 */
export interface TaskPreviewReq {
  /** 完整 `config.task`（与保存时同一形状） */
  config: Record<string, unknown>;
  /** 指派配置（不给就只回模板结果） */
  assignee?: TaskPreviewAssigneeReq | null;
  /** 模拟变量（一层嵌套：`{form:{…}, instance:{…}}`） */
  vars?: Record<string, unknown> | null;
  /** 决定 `task.*` 是否可用 */
  biz_type?: string | null;
  /** `{{node.name}}` 的值 */
  node_name?: string | null;
  /** 算相对期限的基准时刻（默认 now） */
  due_base?: string | null;
  /** 严格模式 */
  strict?: boolean | null;
}

/** 预演渲染出的任务（后端 `PreviewTaskRes`） */
export interface PreviewTaskRes {
  /** 渲染后的标题 */
  title: string;
  /** 渲染后的描述 */
  description: string;
  /** 优先级 */
  priority: string;
  /** 截止日 `yyyy-MM-dd`（没配 → null） */
  due_date: string | null;
  /** 是否需验收 */
  need_review: boolean;
  /** 完成后怎么走 */
  on_complete: string;
}

/** 预演解析出的人（后端 `PreviewAssigneeRes`；第一个是负责人，其余 `member=true`） */
export interface PreviewAssigneeRes {
  /** 用户 id */
  user_id: string;
  /** 姓名 */
  name: string;
  /** 来源 */
  source: string;
  /** 来源角色 / 部门 id */
  source_ref: string;
  /** 是否机器人 */
  is_robot: boolean;
  /** 是否协作人 */
  member: boolean;
}

/** 预演回执（后端 `TaskPreviewRes`；**400 时前端拿到的是 ApiError，不是这个结构**） */
export interface TaskPreviewRes {
  /** 是否通过 */
  ok: boolean;
  /** 渲染结果（失败时也可能有部分结果） */
  task: PreviewTaskRes | null;
  /** 解析出的人 */
  assignees: PreviewAssigneeRes[];
  /** 告警（缺变量的可见占位等；**绝不含「已自动填充默认值」这类假安慰**） */
  warnings: string[];
  /** 错误 */
  errors: string[];
  /** 排障痕迹 */
  trace: string[];
}

/**
 * 任务节点模板预演（`POST /business/flow/node/task/preview`，设计 §3.3）。
 *
 * **不发布、不建实例**：拿一份 `config.task` + 一份模拟变量，看渲染出的任务长什么样。
 * 属性面板底部的实时预览调它（防抖 300ms）。
 *
 * 权限是 `business:flow:tasknode:read`（后端 1A 的取舍，不是设计 §3.3 写的 `flow:read`）。
 *
 * # 异常情况
 *
 * - `400 + 65034`：`config` 形状 / 交叉约束非法
 * - `400 + 65035`：`need_review × on_complete` 矩阵违例（`on_complete=APPROVE` 自 P-FLOW-1B 起合法，
 *   只有「该 biz_type 下没有已发布默认流程」才会在**发布期**返回这个码）
 * - `400 + 65036`：模板语法错
 * - `400 + 65037`：变量路径不在白名单 / `task.*` 用在非 `TASK_ACCEPT`
 * - `400 + 65039`：严格模式缺变量 / 渲染后标题超 200 字符
 * - `400 + 65018`：指派人解析出 0 人且 `empty_assignee=ERROR`
 */
export function previewTaskNode(body: TaskPreviewReq): Promise<TaskPreviewRes> {
  return post<TaskPreviewRes>('/api/business/flow/node/task/preview', body);
}
