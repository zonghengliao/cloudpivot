/**
 * 工作任务接口（批次 C+ · T1 后端 6 个接口）。
 *
 * 路径与后端 `backend/src/business/task/handler.rs` 的
 * `#[alun::get/post/put/delete("/business/task/...")]` 逐条对应（注解不含 `/api`，前端写全）。
 *
 * # 三条取值口径（照后端 `dto.rs`，别自己发明）
 *
 * 1. **枚举一律大写字面量**：`status` ∈ `TODO|DOING|REVIEW|DONE|CANCELLED`，
 *    `priority` ∈ `P0|P1|P2|P3`，`source` ∈ `SELF|ASSIGNED|TRANSFERRED`；
 * 2. **日期是 `YYYY-MM-DD`**（`NaiveDate`，无时区），**时间是 RFC3339**（`DateTime<Utc>`）；
 * 3. **列表卡片与详情是两个形状**：`TaskCardRes` 没有 `description` / `creator_name` /
 *    `estimate_hours`，这些只在详情（`TaskDetailRes`）里下发 —— 列表页不要指望它们。
 *
 * # 写接口的「缺席 ≠ 清空」
 *
 * 编辑用 `TaskUpdateReq`：字段**缺席 = 保持原值**，显式 `null` = 清空。
 * 所以编辑表单提交时，被清空的可空字段必须显式传 `null`，不能省掉。
 *
 * # T2（状态机）已接入，进度接口仍缺
 *
 * `PATCH /business/task/{id}/status` 已按后端 `TaskStatusChangeReq` 落地
 * （`changeTaskStatus`，T7b 看板拖拽用）；`PATCH /business/task/{id}/progress`
 * 尚无页面调用方，**不预置**（避免对着假路径写死）。
 */
import service, { ApiError, del, get, patch, post, put, type ApiResult, type PageData } from '@/api/request';

/**
 * 把强类型查询对象交给 axios 的 `params`。
 *
 * 丢掉空串 / null / undefined：后端把 `?status=` 当作「等于空串」的过滤条件
 * （实测公告 / 帮助接口都返回 0 条），页面里 `query.status = ''` 表示「不过滤」，
 * 必须根本不发这个参数。
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

/** 任务状态（与后端 `dto::STATUSES` 逐字一致） */
export type TaskStatus = 'TODO' | 'DOING' | 'REVIEW' | 'DONE' | 'CANCELLED';

/** 任务优先级 */
export type TaskPriority = 'P0' | 'P1' | 'P2' | 'P3';

/** 任务来源 */
export type TaskSource = 'SELF' | 'ASSIGNED' | 'TRANSFERRED';

/** 任务卡（列表 / 子任务 / 我的任务共用，对应后端 `TaskCardRes`） */
export interface TaskCard {
  /** 主键 */
  id: string;
  /** 任务编号 `TSK-YYYYMM-NNNNNN` */
  task_no: string;
  /** 标题 */
  title: string;
  /** 状态 */
  status: string;
  /** 优先级 */
  priority: string;
  /** 进度 0~100 */
  progress: number;
  /** 是否需要验收 */
  need_review: boolean;
  /** 负责人 id */
  owner_id: string;
  /** 负责人姓名 */
  owner_name: string;
  /** 归属部门 id */
  dept_id: string | null;
  /** 来源 */
  source: string;
  /** 计划开始日 `YYYY-MM-DD` */
  start_date: string | null;
  /** 截止日 `YYYY-MM-DD` */
  due_date: string | null;
  /** 子任务数 */
  child_count: number;
  /** 已完成子任务数（E6a；`status = 'DONE'` 且未软删） */
  child_done_count: number;
  /** 所属项目 id（T7c；`null` = 未归入项目） */
  project_id: string | null;
  /** 项目编码（空串 = 无项目 / 项目已删） */
  project_code: string;
  /** 项目名称（空串 = 无项目 / 项目已删） */
  project_name: string;
  /** 创建时间 RFC3339 */
  created_at: string;
  /** 最后更新时间 RFC3339 */
  updated_at: string;
}

/**
 * 任务分页（对应后端 `TaskPageRes`）。
 *
 * 是 `PageData<TaskCard>` 的**超集**：多一个 `member_scope_truncated`
 * —— 为 true 表示「我参与」的任务 id 超过后端 2000 上限，本视图只按数据范围过滤，
 * 页面必须提示用户「这不是全部任务」。
 */
export interface TaskPage extends PageData<TaskCard> {
  /** 「我参与」id 数组是否被截断（超上限 → 本视图只含数据范围内任务） */
  member_scope_truncated: boolean;
}

/** 协作人 / 关注者（对应后端 `TaskMemberRes`） */
export interface TaskMember {
  /** 用户 id */
  user_id: string;
  /** 用户姓名 */
  user_name: string;
  /** 角色 `COLLABORATOR` / `FOLLOWER` */
  role: string;
  /** 加入时间 */
  created_at: string;
}

/** 任务附件绑定（对应后端 `TaskAttachmentRes`；不含下载地址） */
export interface TaskAttachment {
  /** 绑定行 id */
  id: string;
  /** 文件 id */
  file_id: string;
  /** 原始文件名 */
  file_name: string;
  /** 文件大小（字节） */
  file_size: number;
  /** 展示顺序 */
  sort_order: number;
}

/** 任务详情（对应后端 `TaskDetailRes`：`TaskCardRes` 扁平展开 + 详情补充字段） */
export interface TaskDetail extends TaskCard {
  /** 描述（纯文本） */
  description: string | null;
  /** 创建人 id */
  creator_id: string;
  /** 创建人姓名 */
  creator_name: string;
  /** 分派人 id（自建任务为空） */
  assigner_id: string | null;
  /** 分派人姓名 */
  assigner_name: string;
  /** 部门名称 */
  dept_name: string;
  /** 父任务 id */
  parent_id: string | null;
  /** 父任务编号（面包屑用） */
  parent_task_no: string | null;
  /** 所属项目 id */
  project_id: string | null;
  /** 完成时间 */
  done_at: string | null;
  /** 预估工时 */
  estimate_hours: number | null;
  /** 实际工时 */
  actual_hours: number | null;
  /** 取消原因 */
  cancel_reason: string | null;
  /** 打回原因 */
  reject_reason: string | null;
  /** 审批流实例（批次 D 预留，本批恒为 null） */
  approval_instance_id: string | null;
  /** 子任务（已按可见性过滤） */
  children: TaskCard[];
  /** 协作人 / 关注者 */
  members: TaskMember[];
  /** 附件 */
  attachments: TaskAttachment[];
}

/** 分页查询（对应后端 `TaskPageQuery`） */
export interface TaskPageQuery {
  /** 页码（从 1 开始） */
  page?: number;
  /** 每页条数 */
  page_size?: number;
  /** 状态过滤 */
  status?: string;
  /** 优先级过滤 */
  priority?: string;
  /** 负责人过滤（用户 id） */
  owner_id?: string;
  /**
   * 项目过滤（T7c；`biz_task.project_id`）。
   *
   * 只是**过滤条件**，不改变可见性 —— 可见性恒为「数据范围 ∪ 参与关系」，
   * 所以「按项目筛选」永远不会看到项目里别人的任务。
   */
  project_id?: string;
  /** 截止日下界（含）`YYYY-MM-DD` */
  due_from?: string;
  /** 截止日上界（含）`YYYY-MM-DD` */
  due_to?: string;
  /** 关键词（标题 / 描述模糊） */
  keyword?: string;
}

/** 新建任务（对应后端 `TaskCreateReq`；只有 `title` 必填） */
export interface TaskCreateReq {
  /** 标题（1–200 字符） */
  title: string;
  /** 描述 */
  description?: string | null;
  /** 优先级（缺省 P2） */
  priority?: string;
  /** 是否需验收（缺省 false） */
  need_review?: boolean;
  /** 负责人（缺省 = 自己） */
  owner_id?: string | null;
  /** 父任务 */
  parent_id?: string | null;
  /** 所属项目 */
  project_id?: string | null;
  /** 计划开始日 */
  start_date?: string | null;
  /** 截止日（缺省 = 本周五） */
  due_date?: string | null;
  /** 预估工时（小时） */
  estimate_hours?: number | null;
}

/**
 * 编辑任务（对应后端 `TaskUpdateReq`）。
 *
 * **字段缺席 = 不改；显式 `null` = 清空** —— 页面上把描述 / 截止日 / 工时清空后，
 * 必须显式传 `null`，省掉字段会被后端理解成「保持原值」。
 */
export interface TaskUpdateReq {
  /** 标题 */
  title?: string;
  /** 描述（`null` = 清空） */
  description?: string | null;
  /** 优先级 */
  priority?: string;
  /** 是否需验收 */
  need_review?: boolean;
  /** 负责人 */
  owner_id?: string | null;
  /** 父任务（`null` = 摘成顶层） */
  parent_id?: string | null;
  /** 所属项目（`null` = 移出项目） */
  project_id?: string | null;
  /** 计划开始日（`null` = 清空） */
  start_date?: string | null;
  /** 截止日（`null` = 清空） */
  due_date?: string | null;
  /** 预估工时（`null` = 清空） */
  estimate_hours?: number | null;
}

/** 创建 / 编辑回执（对应后端 `TaskSaveRes`） */
export interface TaskSaveResult {
  /** 主键 */
  id: string;
  /** 任务编号 */
  task_no: string;
  /** 状态 */
  status: string;
  /** 优先级 */
  priority: string;
  /** 进度 */
  progress: number;
  /** 截止日 */
  due_date: string | null;
  /** 负责人 id */
  owner_id: string;
}

/** 删除回执（对应后端 `TaskDeleteRes`） */
export interface TaskDeleteResult {
  /** 被软删的任务 id */
  id: string;
  /** 是否真的删掉了一行（false = 早已软删 / 并发重复删除） */
  deleted: boolean;
}

/** 「我的任务」各组条数（对应后端 `MyCounts`） */
export interface MyTaskCounts {
  /** 逾期 */
  overdue: number;
  /** 今日 */
  today: number;
  /** 本周 */
  this_week: number;
  /** 以后 */
  later: number;
  /** 无期限 */
  no_due: number;
}

/** 「我的任务」五分组（对应后端 `MyTasksRes`；「今天」按 CST 计算） */
export interface MyTasks {
  /** 逾期 */
  overdue: TaskCard[];
  /** 今日 */
  today: TaskCard[];
  /** 本周 */
  this_week: TaskCard[];
  /** 以后 */
  later: TaskCard[];
  /** 无期限 */
  no_due: TaskCard[];
  /** 各组条数 */
  counts: MyTaskCounts;
  /** 参与统计的任务总数 */
  total: number;
}

/** 任务分页列表（`GET /business/task/page`，权限 `business:task:read`） */
export function pageTasks(query: TaskPageQuery): Promise<TaskPage> {
  return get<TaskPage>('/api/business/task/page', asParams(query));
}

/** 我的任务（`GET /business/task/my`，按逾期/今日/本周/以后/无期限分组） */
export function listMyTasks(): Promise<MyTasks> {
  return get<MyTasks>('/api/business/task/my');
}

/** 任务详情（`GET /business/task/{id}`） */
export function getTask(id: string): Promise<TaskDetail> {
  return get<TaskDetail>(`/api/business/task/${id}`);
}

/** 新建任务（`POST /business/task`，权限 `business:task:create`） */
export function createTask(data: TaskCreateReq): Promise<TaskSaveResult> {
  return post<TaskSaveResult>('/api/business/task', data);
}

/** 编辑任务（`PUT /business/task/{id}`，权限 `business:task:update`） */
export function updateTask(id: string, data: TaskUpdateReq): Promise<TaskSaveResult> {
  return put<TaskSaveResult>(`/api/business/task/${id}`, data);
}

/** 删除任务（`DELETE /business/task/{id}`，软删，权限 `business:task:delete`） */
export function deleteTask(id: string): Promise<TaskDeleteResult> {
  return del<TaskDeleteResult>(`/api/business/task/${id}`);
}

// ======================================================================
// E6a（子任务：新建 + 列表）
//
// 路径与后端 `handler.rs` 的注解逐条对应（注解不含 `/api`，这里写全）：
//   POST /api/business/task/{id}/subtask   权限 `business:task:subtask`
//   GET  /api/business/task/{id}/subtasks  权限 `business:task:read`
//
// 三条口径（照后端 `dto.rs`，别自己发明）：
//   1. 子任务字段与新建任务一样，**`parent_id` 由 URL 带**，body 里不传（传了也被忽略）；
//   2. `priority` / `due_date` / `need_review` 缺省 = **继承父任务**（不是继承默认值），
//      所以前端「想让子任务跟父任务走」就不要传这三个字段；
//   3. 列表的 `total` / `progress` / `progress_locked` 按**全部**子任务算（聚合口径），
//      而 `list` 只含**我有权看**的子任务 —— 两者数量可能不等，前端不要用 `list.length` 当 total。
// ======================================================================

/** 新建子任务（对应后端 `TaskSubtaskCreateReq`；只有 `title` 必填） */
export interface TaskSubtaskCreateReq {
  /** 标题（1–200 字符） */
  title: string;
  /** 描述 */
  description?: string | null;
  /** 优先级（缺省继承父任务） */
  priority?: string | null;
  /** 负责人（缺省 = 我自己） */
  owner_id?: string | null;
  /** 计划开始日 `YYYY-MM-DD` */
  start_date?: string | null;
  /** 截止日（缺省继承父任务） */
  due_date?: string | null;
  /** 预估工时（小时） */
  estimate_hours?: number | null;
  /** 是否需验收（缺省继承父任务） */
  need_review?: boolean | null;
}

/** 子任务列表（对应后端 `TaskSubtaskListRes`） */
export interface TaskSubtaskList {
  /** 父任务 id */
  parent_id: string;
  /** 父任务当前状态（终态时前端应禁用「新建子任务」） */
  parent_status: string;
  /** 父任务进度（= 子任务加权汇总） */
  progress: number;
  /** 进度是否由子任务锁定（true = 父任务进度手改会被后端 409 / 63012 拒） */
  progress_locked: boolean;
  /** 子任务总数（**全部**，含我看不见的） */
  total: number;
  /** 我有权看的子任务卡（形状 = `TaskCard`） */
  list: TaskCard[];
}

/** 新建子任务（`POST /business/task/{id}/subtask`，权限 `business:task:subtask`） */
export function createSubtask(parentId: string, data: TaskSubtaskCreateReq): Promise<TaskSaveResult> {
  return post<TaskSaveResult>(`/api/business/task/${parentId}/subtask`, data);
}

/** 子任务列表（`GET /business/task/{id}/subtasks`，权限 `business:task:read`） */
export function listSubtasks(parentId: string): Promise<TaskSubtaskList> {
  return get<TaskSubtaskList>(`/api/business/task/${parentId}/subtasks`);
}

// ======================================================================
// T4（依赖 + 只读甘特图）
//
// 路径与后端 `handler.rs` 的注解逐条对应（注解不含 `/api`，这里写全）：
//   GET    /api/business/task/{id}/dependencies
//   POST   /api/business/task/{id}/dependencies
//   DELETE /api/business/task/{id}/dependencies/{dep_id}
//   GET    /api/business/task/gantt
//
// 三条口径（照后端 `dto.rs`，别自己发明）：
//   1. `dep_id` 是**对端任务 id**（前置组 = 前置 id，后继组 = 后继 id），删除按 pair 双向匹配；
//   2. 甘特只读：`bar.offset_days` / `bar.length_days` 由**后端**算好，前端只乘列宽，不自己算日期差；
//   3. 时间窗上限 180 天（超了后端 400 + 63105），默认今天起 30 天。
// ======================================================================

/** 依赖项（前置 / 后继共用一条形状） */
export interface TaskDependencyItem {
  /** 对端任务 id（删除时用它） */
  dep_id: string;
  task_no: string;
  title: string;
  status: string;
  priority: string;
  progress: number;
  owner_id: string;
  owner_name: string;
  start_date: string | null;
  due_date: string | null;
  /** 本版恒 `FS` */
  dep_type: string;
  /** 滞后天数（只存不算排程） */
  lag_days: number;
  /** 对端是否仍在阻塞本任务（只有前置组会为 true） */
  blocking: boolean;
  created_at: string;
}

/** 依赖列表（两组 + 未完成前置数） */
export interface TaskDependencies {
  task_id: string;
  prerequisites: TaskDependencyItem[];
  successors: TaskDependencyItem[];
  blocking_count: number;
}

/** 加依赖回执（`created=false` = 幂等命中，依赖已存在） */
export interface TaskDependencyAddResult extends TaskDependencies {
  depends_on_id: string;
  created: boolean;
  lag_days: number;
}

/** 删依赖回执 */
export interface TaskDependencyRemoveResult extends TaskDependencies {
  dep_id: string;
  removed: boolean;
}

/** 甘特查询参数 */
export interface TaskGanttQuery {
  /** 本版不支持（传了后端 400） */
  board_id?: string;
  project_id?: string;
  assignee_id?: string;
  /** `YYYY-MM-DD` */
  start?: string;
  /** `YYYY-MM-DD`（缺省 start + 29 天，跨度上限 180 天） */
  end?: string;
  /** `my` = 只看我负责的；`all` / 省略 = 我的可见范围 */
  scope?: 'my' | 'all';
}

/** 甘特时间窗（含今天与今天在窗口里的偏移） */
export interface GanttRange {
  start: string;
  end: string;
  days: number;
  today: string;
  /** 今天不在窗口内 → null（不画今天线） */
  today_offset: number | null;
}

/** 甘特任务条位置（后端算好的天数） */
export interface GanttBar {
  offset_days: number;
  length_days: number;
}

/** 甘特任务 */
export interface GanttTask {
  id: string;
  task_no: string;
  title: string;
  status: string;
  priority: string;
  progress: number;
  owner_id: string;
  owner_name: string;
  start_date: string | null;
  due_date: string | null;
  parent_id: string | null;
  /** P1 才有，本版恒 false */
  is_milestone: boolean;
  /** 被未完成前置阻塞（画斜纹） */
  blocked: boolean;
  /** 逾期（due_date < 今天且未终态） */
  overdue: boolean;
  bar: GanttBar | null;
}

/** 甘特依赖边：`from` = 前置，`to` = 后继 */
export interface GanttDependency {
  from: string;
  to: string;
}

/** 甘特数据 */
export interface TaskGantt {
  range: GanttRange;
  tasks: GanttTask[];
  dependencies: GanttDependency[];
  /** 任务数超 500 → 已截断 */
  truncated: boolean;
  member_scope_truncated: boolean;
}

/** 依赖列表（`GET /api/business/task/{id}/dependencies`） */
export function listDependencies(id: string): Promise<TaskDependencies> {
  return get<TaskDependencies>(`/api/business/task/${id}/dependencies`);
}

/** 加前置依赖（`POST /api/business/task/{id}/dependencies`；成环 → 400 + 63101） */
export function addDependency(
  id: string,
  data: { depends_on_id: string; lag_days?: number },
): Promise<TaskDependencyAddResult> {
  return post<TaskDependencyAddResult>(`/api/business/task/${id}/dependencies`, data);
}

/** 解除依赖（`DELETE /api/business/task/{id}/dependencies/{dep_id}`，`dep_id` = 对端任务 id） */
export function removeDependency(id: string, depId: string): Promise<TaskDependencyRemoveResult> {
  return del<TaskDependencyRemoveResult>(`/api/business/task/${id}/dependencies/${depId}`);
}

/** 甘特图数据（`GET /api/business/task/gantt`，**只读**；时间窗 ≤ 180 天） */
export function fetchGantt(query: TaskGanttQuery): Promise<TaskGantt> {
  return get<TaskGantt>('/api/business/task/gantt', asParams(query));
}
// ======================================================================
// T7b 接入位（看板拖拽改状态）—— 后端 T2 已交付：PATCH /status（10 条白名单流转）
// ======================================================================

/**
 * 任务域错误码（与后端 `backend/src/shared/error_code.rs` 逐字一致）。
 *
 * 前端只有「按码给文案」这一条路：HTTP 状态码在 `api/request.ts` 里已被
 * 规整成 `ApiError`（只保留业务码），所以判据只能是业务码。
 */
export const TASK_ERROR_CODE = {
  /** 409：`from → to` 不在白名单里（跳级 / 同状态 / 终态直取） */
  TRANSITION_INVALID: 63010,
  /** 403：当前身份无权推这个流转（如 `REVIEW → DONE` 只有验收人能推） */
  STATUS_FORBIDDEN: 63011,
  /** 400：打回（`REVIEW → DOING`）必须填原因，且 ≥5 字 */
  REJECT_REASON_REQUIRED: 63013,
  /** 409：还有未完成子任务，父任务不能完成 */
  HAS_OPEN_CHILDREN: 63006,
  /** 404：任务不存在 / 不可见 */
  NOT_FOUND: 63001,
  /** 400：改期后开始日早于未完成前置的截止日（后端点名前置：编号 + 标题 + 截止日） */
  SCHEDULE_PREDECESSOR: 63110,
  /** 400：改期后截止日晚于后继的计划开始日（后端点名后继） */
  SCHEDULE_SUCCESSOR: 63111,
  /** 400：子任务跑出父任务区间，或父任务把子任务甩出区间（后端点名父子） */
  SCHEDULE_PARENT_RANGE: 63112,
  /**
   * 409：任务挂了审批单且实例 `PENDING`，`REVIEW → DONE / DOING` 被门闸拦下
   * （后端 `TASK_APPROVAL_PENDING`，批次 D·D3a）。
   *
   * 前端必须**给跳出任务页的入口**：审批结论只能在审批中心里下，
   * 在任务详情上反复点按钮只会一直撞同一道门。
   */
  APPROVAL_PENDING: 65030,
} as const;

/**
 * 改状态请求（对应后端 `TaskStatusChangeReq`）。
 *
 * 字段名是 **`to`**（目标状态），不是 `status` —— 后端用 serde `alias` 也收
 * `status`，但**两个都传会 400**（duplicate field），所以前端只传 `to`。
 */
export interface TaskStatusChangeReq {
  /** 目标状态（5 态大写） */
  to: TaskStatus;
  /** 原因：`REVIEW → DOING` 必填（≥5 字，进 `reject_reason`）；`→ CANCELLED` 可选；其它只进评论 */
  reason?: string;
}

/** 父任务重算结果（对应后端 `ParentProgressRes`；顶层任务为空数组） */
export interface ParentProgress {
  id: string;
  previous: number;
  progress: number;
  mode: string;
  all_done: boolean;
}

/** 改状态回执（对应后端 `TaskStatusRes`） */
export interface TaskStatusChangeResult {
  id: string;
  task_no: string;
  /** 流转前状态 */
  from: string;
  /** 流转后状态（= 库里的现值） */
  status: string;
  /** 流转后进度（无子任务时可能被联动改动） */
  progress: number;
  /** 完成时间（进入 DONE 后有值；重开后为 null） */
  done_at: string | null;
  /** 本次写入的状态变更评论 id（时间线定位用） */
  comment_id: string;
  /** 父任务重算结果（顶层任务为空数组） */
  parent_progress: ParentProgress[];
}

/**
 * 改任务状态（`PATCH /api/business/task/{id}/status`，权限 `business:task:status`）。
 *
 * # 参数
 * - `id`：任务 id（UUID）
 * - `to`：目标状态
 * - `reason`：原因（仅打回 / 取消时有意义；空白串按「没填」处理，不会发出该字段）
 *
 * # 失败口径（由后端判定，前端只负责翻译）
 * - 409 + `63010` 非法流转；403 + `63011` 无权；400 + `63013` 打回缺原因。
 */
export function changeTaskStatus(id: string, to: TaskStatus, reason?: string): Promise<TaskStatusChangeResult> {
  const body: TaskStatusChangeReq = { to };
  const text = reason?.trim();
  if (text) {
    body.reason = text;
  }
  return patch<TaskStatusChangeResult>(`/api/business/task/${id}/status`, body);
}

// ======================================================================
// T6b：评论 / 附件 / 工时（补齐 T1「建了表没功能」的三张表）
// ======================================================================

/**
 * 评论类型（与后端 `biz_task_comment.comment_type` 的 CHECK 逐字一致）。
 *
 * ⚠️ **人写的评论是 `PLAIN`**：卡面把它写作 `COMMENT`，但库里（`V0108`）的值是
 * `PLAIN`，`V0112` 的 CHECK 也只有下面这 6 个值 —— 前端判据必须按库里来。
 *
 * `readonly = true` 的四类（`STATUS_CHANGE` / `ASSIGN` / `PROGRESS_AUTO` /
 * `AI_PROGRESS`）是**系统生成**的时间线事实，不可改删；`AI_REPORT` 是 AI 写的
 * 执行报告，管理员可按普通评论的规则清理。
 */
export type TaskCommentType = 'PLAIN' | 'AI_REPORT' | 'STATUS_CHANGE' | 'ASSIGN' | 'PROGRESS_AUTO' | 'AI_PROGRESS';

/** 单条评论（对应后端 `TaskCommentItemRes`） */
export interface TaskCommentItem {
  /** 评论 id */
  id: string;
  /** 评论类型 */
  comment_type: TaskCommentType;
  /** 作者 id */
  user_id: string;
  /** 作者姓名 */
  user_name: string;
  /** 正文（Markdown 原文，前端渲染 + 净化） */
  content: string;
  /** 被 @ 的用户 id */
  mentions: string[];
  /** 回复的上级评论 */
  parent_comment_id: string | null;
  /** 发表时间（RFC3339） */
  created_at: string;
  /** 是否系统生成（只读） */
  readonly: boolean;
  /** 当前用户能否删除（**服务端算好的结论**，前端不要自己推导） */
  can_delete: boolean;
}

/** 评论分页（对应后端 `TaskCommentsRes`） */
export interface TaskComments {
  list: TaskCommentItem[];
  total: number;
  page: number;
  page_size: number;
}

/** 评论分页查询 */
export interface TaskCommentQuery {
  page?: number;
  page_size?: number;
}

/**
 * 评论列表（`GET /api/business/task/{id}/comments`）。
 *
 * 时间**正序**（时间线的阅读方向）；看不见任务时后端返回 404（不暴露 id 是否存在）。
 */
export function listTaskComments(id: string, query: TaskCommentQuery = {}): Promise<TaskComments> {
  return get<TaskComments>(`/api/business/task/${id}/comments`, asParams(query));
}

/**
 * 删除评论（`DELETE /api/business/task/{id}/comments/{commentId}`）。
 *
 * 软删：列表不再返回。作者本人可删自己的；删别人的需要 `business:task:comment:delete`；
 * **系统生成的评论一律 403**（`readonly = true` 的那些，前端不该显示删除按钮）。
 */
export function deleteTaskComment(id: string, commentId: string): Promise<{ id: string; deleted: boolean }> {
  return del<{ id: string; deleted: boolean }>(`/api/business/task/${id}/comments/${commentId}`);
}

/**
 * 发表评论请求（对应后端 `TaskCommentReq`）。
 *
 * 字段名是 **`mentions`**（T5b 定下的，与 `biz_task_comment.mentions` 列同名）；
 * 卡面早期写的 `mentioned_user_ids` 在代码里并不存在 —— 以代码为准。
 */
export interface TaskCommentCreateReq {
  /** 正文（Markdown 原文；后端存原文，前端渲染 + 净化） */
  content: string;
  /** 被 @ 的用户 id（**每个人都收到强制通知**） */
  mentions?: string[];
  /** 回复的上级评论 id */
  parent_comment_id?: string;
}

/** 发表评论回执（对应后端 `TaskCommentRes`） */
export interface TaskCommentCreateResult {
  id: string;
  comment_type: string;
  user_id: string;
  user_name: string;
  created_at: string;
  /** 实际生效的被 @ 用户（只有真正收到强制通知的账号） */
  mentions?: string[];
  /** 被忽略的 `mentions`（账号不存在 / 已停用 / 已删除；后端忽略而不是报错） */
  mentions_ignored?: string[];
}

/**
 * 发表评论（`POST /api/business/task/{id}/comment`）。
 *
 * 这条写接口是 T5b 建的（同时接受 AI 任务令牌），T6b 只是把它接到详情页的发表框上。
 * `mentions` 里的人会收到**强制**站内通知（修正 8：被 @ = 强制，不看偏好开关）。
 */
export function addTaskComment(id: string, data: TaskCommentCreateReq): Promise<TaskCommentCreateResult> {
  return post<TaskCommentCreateResult>(`/api/business/task/${id}/comment`, data);
}

/**
 * 发表评论（`POST /api/business/task/${id}/comments`）—— **人写评论的唯一入口**。
 *
 * # 为什么与 `addTaskComment` 并存
 *
 * 单数路径 `/comment` 是 T5b 给 **AI 任务令牌**开的（`no_auth` + 任务级令牌校验）；
 * 复数路径 `/comments` 是 REST 语义的人类入口（与 `GET /comments`、
 * `DELETE /comments/{id}` 同前缀，权限 `business:task:read`），T6b 补漏补的。
 * 后端两个 handler 共用同一份写入实现，差别只有正文上限（人 5000 超长 400 /
 * AI 8000 超长截断）。
 *
 * # 服务端的四条约束（前端只做提示，判定只有后端一份）
 *
 * - `content` trim 后 1–5000 字，超长 **400**；
 * - `mentions` 里不存在的账号**被忽略**，回执 `mentions_ignored` 里逐个列出；
 * - `parent_comment_id` 必须属于同一任务，跨任务 **400**；
 * - `comment_type` 只能是 `PLAIN`，伪造 `AI_REPORT` 等一律 **400**。
 */
export function postTaskComment(
  id: string,
  data: TaskCommentCreateReq,
): Promise<TaskCommentCreateResult> {
  return post<TaskCommentCreateResult>(`/api/business/task/${id}/comments`, data);
}

/** 卡面里的名字（`postComment`）与 {@link postTaskComment} 是同一个函数。 */
export const postComment = postTaskComment;

/** 单条附件（对应后端 `TaskAttachmentItemRes`） */
export interface TaskAttachmentItem {
  /** 绑定行 id（删除 / 下载都用它，**不是** file_id） */
  id: string;
  /** 文件 id（`sys_file.id`） */
  file_id: string;
  /** 原始文件名 */
  file_name: string;
  /** 大小（字节） */
  file_size: number;
  /** MIME */
  mime_type: string;
  /** 来源：`HUMAN` 人上传 / `AI` AI 产出 */
  source: 'HUMAN' | 'AI';
  /** 产出的 AI profile（仅 AI 有值） */
  ai_profile: string | null;
  /** 上传人 id */
  uploader_id: string | null;
  /** 上传人姓名 */
  uploader_name: string;
  /** 上传时间（RFC3339） */
  created_at: string;
  /** 当前用户能否删除（上传者 / 管理员；服务端判定） */
  can_delete: boolean;
}

/** 附件列表（对应后端 `TaskAttachmentsRes`） */
export interface TaskAttachments {
  list: TaskAttachmentItem[];
  /** 当前数量 */
  total: number;
  /** 单任务上限（50） */
  max_count: number;
}

/** 附件上传回执（对应后端 `TaskUploadRes`） */
export interface TaskAttachmentUploadResult {
  attachment_id: string;
  file_id: string;
  file_name: string;
  file_size: number;
  /** `HUMAN` / `AI` */
  source: string;
  /** 命中的存储策略分类（`task` / `task_ai`） */
  category: string;
}

/** 附件列表（`GET /api/business/task/{id}/attachments`） */
export function listTaskAttachments(id: string): Promise<TaskAttachments> {
  return get<TaskAttachments>(`/api/business/task/${id}/attachments`);
}

/**
 * 上传附件（`POST /api/business/task/{id}/attachment`，multipart 字段名 `file`）。
 *
 * 走 A 批次的存储策略（人类 → `task` 50 MB 白名单；AI → `task_ai`）。
 * **第 51 个会被后端 400 拒**（单任务上限 50）。
 */
export function uploadTaskAttachment(id: string, file: File): Promise<TaskAttachmentUploadResult> {
  const formData = new FormData();
  formData.append('file', file);
  return service.post<ApiResult<TaskAttachmentUploadResult>>(`/api/business/task/${id}/attachment`, formData).then((response) => {
    const body = response.data;
    if (!body || typeof body.code !== 'number' || body.code !== 0) {
      throw new ApiError(typeof body?.code === 'number' ? body.code : -1, body?.msg || '上传失败');
    }
    return body.data;
  });
}

/** 删除附件（`DELETE /api/business/task/{id}/attachments/{attId}`，软删 + 文件标记删除） */
export function deleteTaskAttachment(
  id: string,
  attId: string,
): Promise<{ id: string; file_id: string; deleted: boolean }> {
  return del<{ id: string; file_id: string; deleted: boolean }>(`/api/business/task/${id}/attachments/${attId}`);
}

/**
 * 下载附件（`GET /api/business/task/{id}/attachments/{attId}/download`）。
 *
 * 必须走 `service` 取 Blob（**不能**直接 `<a href>`）：接口要 Bearer 鉴权，
 * 裸链接触发的是浏览器导航，不带 `Authorization` → 401。后端在出流之前
 * **已校验任务可见性**（红线），越权拿 att_id 只会得到 404。
 */
export async function downloadTaskAttachment(id: string, attId: string): Promise<Blob> {
  const response = await service.get<Blob>(`/api/business/task/${id}/attachments/${attId}/download`, {
    responseType: 'blob',
  });
  return response.data;
}

/** 单条工时（对应后端 `TaskWorklogItemRes`） */
export interface TaskWorklogItem {
  id: string;
  user_id: string;
  user_name: string;
  /** 工时日期（`YYYY-MM-DD`） */
  work_date: string;
  /** 工时（小时） */
  hours: number;
  description: string | null;
  created_at: string;
  /** 当前用户能否删除（本人 / 管理员） */
  can_delete: boolean;
}

/** 工时列表（对应后端 `TaskWorklogsRes`） */
export interface TaskWorklogs {
  list: TaskWorklogItem[];
  /** 记录条数 */
  total: number;
  /** 明细合计 */
  sum_hours: number;
  /** 任务卡实际工时（**服务端汇总**；没有记录时为 null） */
  actual_hours: number | null;
  /** 单任务记录上限（500） */
  max_records: number;
}

/** 工时登记请求（**没有** actual_hours：汇总只由服务端算） */
export interface TaskWorklogCreateReq {
  /** 工时（0.25–24） */
  hours: number;
  /** 日期（`YYYY-MM-DD`，不得晚于今天） */
  work_date: string;
  /** 工作说明（可空） */
  note?: string;
}

/** 工时登记回执 */
export interface TaskWorklogSaveResult {
  id: string;
  work_date: string;
  hours: number;
  sum_hours: number;
  actual_hours: number | null;
}

/** 工时删除回执 */
export interface TaskWorklogDeleteResult {
  id: string;
  deleted: boolean;
  sum_hours: number;
  actual_hours: number | null;
}

/** 工时列表（`GET /api/business/task/{id}/worklogs`，含合计） */
export function listTaskWorklogs(id: string): Promise<TaskWorklogs> {
  return get<TaskWorklogs>(`/api/business/task/${id}/worklogs`);
}

/** 登记工时（`POST /api/business/task/{id}/worklogs`；只有负责人 / 协作人 / 有 worklog 权限的人） */
export function addTaskWorklog(id: string, data: TaskWorklogCreateReq): Promise<TaskWorklogSaveResult> {
  return post<TaskWorklogSaveResult>(`/api/business/task/${id}/worklogs`, data);
}

/** 删除工时（`DELETE /api/business/task/{id}/worklogs/{wlId}`，本人 / 管理员） */
export function deleteTaskWorklog(id: string, wlId: string): Promise<TaskWorklogDeleteResult> {
  return del<TaskWorklogDeleteResult>(`/api/business/task/${id}/worklogs/${wlId}`);
}

// ======================================================================
// T7a 接入位（甘特拖拽改期）—— 后端已交付：PATCH /business/task/{id}/schedule
// ======================================================================

/**
 * 改期请求（对应后端 `TaskScheduleReq`）。
 *
 * # 两种形态**二选一**（后端 `mode()` 会拒绝两种同时传 / 都不传）
 *
 * - 拖条形**中部**：`{ shift_days: 3 }` → 起止日一起 +3 天；
 * - 拖**左边缘**：`{ start_date: '2026-11-04' }` → 只改开始日；
 * - 拖**右边缘**：`{ due_date: '2026-11-08' }` → 只改截止日。
 */
export interface TaskScheduleReq {
  /** 新的计划开始日（`YYYY-MM-DD`） */
  start_date?: string;
  /** 新的截止日（`YYYY-MM-DD`） */
  due_date?: string;
  /** 平移天数（可负；0 会被后端 400 拒绝，前端拖动 0 格时不该发请求） */
  shift_days?: number;
}

/** 改期结果（对应后端 `TaskScheduleRes`；日期是**贴库值**） */
export interface TaskScheduleResult {
  id: string;
  task_no: string;
  old_start_date: string | null;
  old_due_date: string | null;
  start_date: string | null;
  due_date: string | null;
  /** 实际平移天数（显式形态下按开始日位移算） */
  shift_days: number;
  /** `SCHEDULE` 类型评论 id（时间线上点得进去） */
  comment_id: string;
  /** 变更日志是否落库 */
  changelog_written: boolean;
}

/**
 * 改期（`PATCH /api/business/task/{id}/schedule`）。
 *
 * # 后端会重跑四类校验（修正 10），失败一律 400 + 点名冲突对象
 *
 * | 码 | 含义 | 前端文案 |
 * |----|------|----------|
 * | `63004` | 形态 / 日期非法 | 直接用后端 `msg`（已含具体日期） |
 * | `63110` | 违反前置依赖 | `改期违反依赖：前置 TSK-xxx「标题」（截止 xx）尚未完成` |
 * | `63111` | 违反后继依赖 | `改期违反依赖：后继 TSK-xxx「标题」计划 xx 开始` |
 * | `63112` | 父子区间冲突 | `子任务日期必须在父任务范围内…`（后端已点名父子） |
 *
 * 前端**不自己拼冲突文案**：后端 `msg` 里已经有 `task_no + 标题 + 日期`，
 * 自己再拼一遍就等于把「点名哪个冲突对象」的逻辑写了两份（迟早不一致）。
 */
export function scheduleTask(id: string, data: TaskScheduleReq): Promise<TaskScheduleResult> {
  return patch<TaskScheduleResult>(`/api/business/task/${id}/schedule`, data);
}
