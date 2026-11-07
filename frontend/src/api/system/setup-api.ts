/**
 * 首次启动向导 / 指派审批人接口（`backend/src/system/setup/handler.rs` 的前端镜像）。
 *
 * ```text
 * GET  /api/system/setup/check        关键角色谁还没人（向导清单）
 * POST /api/system/setup/assign       给角色追加成员（只增不减、幂等）
 * POST /api/system/setup/demo-users   一键创建演示账号（需 NIK_SEED_DEMO_USERS=true）
 * ```
 *
 * 路径必须写全 `/api` 前缀（漏了会命中 SPA fallback 返回 HTML，报「响应体格式不符合」）。
 */
import { get, post } from '@/api/request';

/** 一个关键角色的现状（`SetupRoleRes`）。 */
export interface SetupRoleStatus {
  /** 角色编码（`sys_role.role_code`） */
  role_code: string;
  /** 角色名称 */
  role_name: string;
  /** 角色是否在库（被删 / 没种则为 false） */
  exists: boolean;
  /** 角色 id */
  role_id: string | null;
  /** 在职成员数 */
  member_count: number;
  /** 其中「启用且非机器人」的人数 */
  usable_member_count: number;
  /** 建议的演示账号（一键建账号时用） */
  demo_username: string;
  /** 这个角色卡住哪条链 */
  why: string;
  /** 是否已就绪（`usable_member_count > 0`） */
  ready: boolean;
}

/** `GET /api/system/setup/check` 的响应。 */
export interface SetupCheckRes {
  /** 关键角色是否全部就绪 */
  all_ready: boolean;
  /** 演示账号开关（`NIK_SEED_DEMO_USERS`） */
  demo_users_enabled: boolean;
  /** 演示账号口令所在的环境变量名（后端不返回口令本身） */
  demo_password_env: string;
  /** 缺人的关键角色 */
  missing: SetupRoleStatus[];
  /** 全部关键角色现状 */
  roles: SetupRoleStatus[];
}

/** `POST /api/system/setup/assign` 的入参。 */
export interface SetupAssignParams {
  /** 角色编码（优先） */
  role_code?: string;
  /** 角色 id（role_code 缺失时用） */
  role_id?: string;
  /** 要加进角色的用户 id（只增不减） */
  user_ids: string[];
}

/** `POST /api/system/setup/assign` 的响应。 */
export interface SetupAssignRes {
  /** 角色编码 */
  role_code: string;
  /** 角色名称 */
  role_name: string;
  /** 本次新加进去的人 */
  added: string[];
  /** 本来就在角色里、跳过的（幂等证据） */
  skipped: string[];
  /** 指派完成后的在职成员数 */
  member_count: number;
}

/** `POST /api/system/setup/demo-users` 的响应。 */
export interface SetupDemoUsersRes {
  /** 开关是否打开 */
  enabled: boolean;
  /** 新建的登录名 */
  created: string[];
  /** 已存在、跳过的登录名 */
  existed: string[];
  /** 口令所在环境变量名 */
  password_env: string;
  /** 本次新建立的用户-角色绑定条数 */
  bindings_added: number;
}

/** 关键角色自检。 */
export function checkSetup(): Promise<SetupCheckRes> {
  return get<SetupCheckRes>('/api/system/setup/check');
}

/** 给角色追加成员（只增不减，幂等）。 */
export function assignSetupMembers(params: SetupAssignParams): Promise<SetupAssignRes> {
  return post<SetupAssignRes>('/api/system/setup/assign', params);
}

/** 一键创建演示账号（需 `NIK_SEED_DEMO_USERS=true`，否则 409）。 */
export function createDemoUsers(): Promise<SetupDemoUsersRes> {
  return post<SetupDemoUsersRes>('/api/system/setup/demo-users', {});
}
