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
 * # T2（状态机 + 进度）尚未就绪
 *
 * `PATCH /business/task/{id}/status`、`PATCH /business/task/{id}/progress`
 * 属 T2 交付物，本文件**先不实现**（避免对着假路径写死），只在文件末尾留 TODO。
 */
import { del, get, post, put, type PageData } from '@/api/request';

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
// T2 接入位（状态机 + 进度）—— **接口未就绪，先不实现**
//
// 设计文档 §二 / §三 定了两个接口，属 T2 交付物：
//   PATCH /api/business/task/{id}/status    { status, reason? }  8 条迁移规则
//   PATCH /api/business/task/{id}/progress  { progress, mode }   自动/手动 + 联动
// 卡片（t_f1b956b7）要求：按钮先画出来但 disabled + tooltip「状态流转功能即将上线」，
// 这里不放**假路径**（会让人误以为已接好）—— T2 合并后由后续小卡在此追加：
//
// export function updateTaskStatus(id: string, status: TaskStatus, reason?: string) {
//   return patch<TaskSaveResult>(`/api/business/task/${id}/status`, { status, reason });
// }
// export function updateTaskProgress(id: string, progress: number, mode: 'AUTO' | 'MANUAL') {
//   return patch<TaskSaveResult>(`/api/business/task/${id}/progress`, { progress, mode });
// }
// ======================================================================
