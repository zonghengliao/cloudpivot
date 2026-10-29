/**
 * 项目分组接口（批次 C+ · T7c）。
 *
 * 路径与后端 `backend/src/business/project/handler.rs` 的
 * `#[alun::get/post/put/delete("/business/project/...")]` 逐条对应
 * （注解不含 `/api`，前端必须写全 —— 漏了 `/api` 会命中 SPA fallback 返回 HTML，
 * 报「响应体格式不符合」）。
 *
 * # 字段映射（后端 `dto.rs` 的口径，别按库列名写）
 *
 * | 前端字段 | 说明 |
 * |----------|------|
 * | `code` | 项目编码（库列 `project_no`），归一化大写、未删唯一 |
 * | `name` | 项目名称 |
 * | `due_date` | 项目结束日（库列 `end_date`），不是任务的截止日 |
 * | `status` | `ACTIVE` / `ARCHIVED` |
 *
 * # 统计口径（重要）
 *
 * `task_total` / `completion_rate` / `stats.*` **只统计当前用户可见的任务**
 * （与任务列表同一条可见性：数据范围 ∪ 参与关系）。所以「项目负责人」看到的
 * 数字可能小于项目成员看不到的任务数 —— 这不是 bug，是「项目不是权限边界」。
 */
import { del, get, post, put, type PageData } from '@/api/request';

/**
 * 丢掉空串 / null / undefined 的查询参数。
 *
 * 后端把 `?status=` 当作「等于空串」的过滤条件（会返回 0 条），
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

/** 项目状态（与后端 `dto::STATUSES` 逐字一致） */
export type ProjectStatus = 'ACTIVE' | 'ARCHIVED';

/** 项目成员角色（本卡两者权限等价，`VIEWER` 为预留） */
export type ProjectMemberRole = 'MEMBER' | 'VIEWER';

/** 项目（列表项 / 详情头部共用，对应后端 `ProjectRes`） */
export interface ProjectItem {
  /** 主键 */
  id: string;
  /** 编码（未删唯一，大写） */
  code: string;
  /** 名称 */
  name: string;
  /** 说明（可空 → 后端下发空串） */
  description: string;
  /** 负责人 id（可空） */
  owner_id: string | null;
  /** 负责人姓名（空串 = 未指定） */
  owner_name: string;
  /** 归属部门 id */
  dept_id: string | null;
  /** 归属部门名 */
  dept_name: string;
  /** 状态 */
  status: ProjectStatus;
  /** 开始日 `YYYY-MM-DD` */
  start_date: string | null;
  /** 结束日 `YYYY-MM-DD` */
  due_date: string | null;
  /** **我可见的**任务总数 */
  task_total: number;
  /** **我可见的**已完成任务数 */
  task_done: number;
  /** 完成率 0–100（1 位小数；分母剔除已取消） */
  completion_rate: number;
  /** 成员数 */
  member_count: number;
  /** 创建时间（RFC3339） */
  created_at: string;
  /** 更新时间（RFC3339） */
  updated_at: string;
}

/** 项目统计（对应后端 `ProjectStatsRes`，全部按可见性裁剪） */
export interface ProjectStats {
  /** 任务总数 */
  total: number;
  /** 待开始 */
  todo: number;
  /** 进行中 */
  doing: number;
  /** 待验收 */
  review: number;
  /** 已完成 */
  done: number;
  /** 已取消 */
  cancelled: number;
  /** 完成率 0–100（分母剔除已取消） */
  completion_rate: number;
  /** 总预估工时（小时） */
  estimate_hours: number;
  /** 总实际工时（小时） */
  actual_hours: number;
}

/** 项目成员（对应后端 `ProjectMemberRes`） */
export interface ProjectMember {
  user_id: string;
  username: string;
  real_name: string;
  role: ProjectMemberRole;
  created_at: string;
}

/** 项目详情（对应后端 `ProjectDetailRes`：头部字段摊平 + stats + members） */
export interface ProjectDetail extends ProjectItem {
  stats: ProjectStats;
  members: ProjectMember[];
}

/** 分页查询参数（对应后端 `ProjectPageQuery`） */
export interface ProjectPageQuery {
  page?: number;
  page_size?: number;
  /** 名称 **或** 编码关键词 */
  name?: string;
  status?: ProjectStatus | '';
  /** 负责人 id */
  owner_id?: string;
}

/** 新建项目（对应后端 `ProjectCreateReq`） */
export interface ProjectCreateReq {
  name: string;
  code: string;
  description?: string | null;
  owner_id?: string | null;
  start_date?: string | null;
  due_date?: string | null;
}

/**
 * 编辑项目（对应后端 `ProjectUpdateReq`）。
 *
 * **字段缺席 = 不改；显式 `null` = 清空**（可清空字段：description / owner_id /
 * start_date / due_date）。表单里被清空的字段必须显式传 `null`。
 */
export interface ProjectUpdateReq {
  name?: string;
  code?: string;
  description?: string | null;
  owner_id?: string | null;
  status?: ProjectStatus;
  start_date?: string | null;
  due_date?: string | null;
}

/** 写操作回执（对应后端 `ProjectSaveRes`） */
export interface ProjectSaveResult {
  id: string;
  code: string;
  name: string;
  status: ProjectStatus;
}

/** 删除回执（对应后端 `ProjectDeleteRes`；**任务不会被删除**，只解绑） */
export interface ProjectDeleteResult {
  deleted: boolean;
  /** 被解绑（`project_id` 置 NULL）的任务数 */
  unbound_tasks: number;
  /** 被清理的成员行数 */
  removed_members: number;
}

/** 成员增删回执（对应后端 `ProjectMemberChangeRes`） */
export interface ProjectMemberChangeResult {
  /** `false` = 幂等命中（已经是 / 本来不是成员） */
  changed: boolean;
  members: ProjectMember[];
}

/** 任务归入 / 移出回执（对应后端 `TaskProjectRes`） */
export interface TaskProjectResult {
  task_id: string;
  project_id: string | null;
  project_code: string | null;
  project_name: string | null;
}

/** 项目分页（`GET /api/business/project/page`） */
export function pageProjects(query: ProjectPageQuery): Promise<PageData<ProjectItem>> {
  return get<PageData<ProjectItem>>('/api/business/project/page', asParams(query));
}

/** 项目详情（`GET /api/business/project/{id}`，含统计与成员） */
export function getProject(id: string): Promise<ProjectDetail> {
  return get<ProjectDetail>(`/api/business/project/${id}`);
}

/** 新建项目（`POST /api/business/project`） */
export function createProject(data: ProjectCreateReq): Promise<ProjectSaveResult> {
  return post<ProjectSaveResult>('/api/business/project', data);
}

/** 编辑项目（`PUT /api/business/project/{id}`） */
export function updateProject(id: string, data: ProjectUpdateReq): Promise<ProjectSaveResult> {
  return put<ProjectSaveResult>(`/api/business/project/${id}`, data);
}

/** 删除项目（`DELETE /api/business/project/{id}`；任务不删，`project_id` 置空） */
export function deleteProject(id: string): Promise<ProjectDeleteResult> {
  return del<ProjectDeleteResult>(`/api/business/project/${id}`);
}

/** 项目成员列表（`GET /api/business/project/{id}/members`） */
export function listProjectMembers(id: string): Promise<ProjectMember[]> {
  return get<ProjectMember[]>(`/api/business/project/${id}/members`);
}

/**
 * 项目下任务分页（`GET /api/business/project/{id}/tasks`）。
 *
 * **走 T1 的可见性**：后端先把 `project_id` 当过滤条件塞进「数据范围 ∪ 参与关系」的
 * 唯一可见性实现，所以「项目负责人 / 项目成员」看到的仍是自己有权看的那些任务。
 * 出参形状与 `/api/business/task/page` 完全一致（同一套 `TaskCard`）。
 */
export function pageProjectTasks(id: string, query: { page?: number; page_size?: number } = {}): Promise<PageData<import('./task-api').TaskCard>> {
  return get<PageData<import('./task-api').TaskCard>>(`/api/business/project/${id}/tasks`, asParams(query));
}

/** 加项目成员（`POST /api/business/project/{id}/members`，幂等） */
export function addProjectMember(id: string, userId: string, role?: ProjectMemberRole): Promise<ProjectMemberChangeResult> {
  return post<ProjectMemberChangeResult>(`/api/business/project/${id}/members`, { user_id: userId, role });
}

/** 移除项目成员（`DELETE /api/business/project/{id}/members/{user_id}`） */
export function removeProjectMember(id: string, userId: string): Promise<ProjectMemberChangeResult> {
  return del<ProjectMemberChangeResult>(`/api/business/project/${id}/members/${userId}`);
}

/**
 * 任务归入 / 移出项目（`PUT /api/business/task/{id}/project`）。
 *
 * `projectId = null` 表示移出（后端要求**显式**给字段：缺席 → 400）。
 */
export function assignTaskProject(taskId: string, projectId: string | null): Promise<TaskProjectResult> {
  return put<TaskProjectResult>(`/api/business/task/${taskId}/project`, { project_id: projectId });
}
