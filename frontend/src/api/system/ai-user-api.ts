import { del, get, post, put } from '@/api/request';

/**
 * AI 用户（云枢 × Hermes）接口。
 *
 * 对应后端 `business/aiagent`（卡 L1）：建号时由云枢自动创建 Hermes profile、
 * 写 SOUL.md、建 workspace；删除时先备份 SOUL.md 再清理 profile。
 *
 * 注意：AI 用户的**显示名 / 状态**仍走通用用户接口（`PUT /api/system/user/{id}`），
 * 但通用接口对 `ROBOT` 账号的「编辑 / 删除」会返回 409 + 64013 —— 那是防绕过
 * 不变式（`user_type=ROBOT` ⇔ `sys_ai_agent` 有配置行）的守卫，属预期行为。
 */

/** AI 用户详情（含 SOUL.md 全文） */
export interface AiUserRes {
  /** 用户 id（= sys_user.id） */
  user_id: string;
  /** 登录账号 = Hermes profile 名 */
  username: string;
  /** 显示名 */
  real_name: string;
  /** 账号类型，恒为 ROBOT */
  user_type: string;
  /** Hermes profile 名 */
  hermes_profile: string;
  /** workspace 绝对路径（服务器上的真实路径） */
  workspace_path: string;
  /** SOUL.md 绝对路径（保存前提示用） */
  soul_path: string;
  /** 职责 */
  agent_description: string;
  /** SOUL.md 全文（Markdown） */
  soul_md: string;
  /** 模型覆盖（任意 JSON 对象；null 表示用 profile 自身配置） */
  model_override: Record<string, unknown> | null;
  /** 上次探活记录的 gateway 状态：unknown / online / offline */
  gateway_status: string;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/** AI 用户探活结果 */
export interface AiUserStatusRes {
  /** 用户 id */
  user_id: string;
  /** profile 名 */
  hermes_profile: string;
  /** profile 目录是否存在 */
  profile_dir_exists: boolean;
  /** SOUL.md 是否存在 */
  soul_file_exists: boolean;
  /** workspace 目录是否存在 */
  workspace_exists: boolean;
  /** hermes CLI 是否可用 */
  cli_available: boolean;
  /** gateway 本次探测值 */
  gateway: string;
  /** 库里记录的上次探测值 */
  gateway_status: string;
  /** 整体健康 */
  healthy: boolean;
  /** 探测补充说明（命令失败原因等） */
  detail: string | null;
}

/** 创建 AI 用户参数 */
export type AiUserCreateParams = {
  /** 用户名 = profile 名（^[a-z][a-z0-9-]{1,30}$） */
  username: string;
  /** 显示名 */
  real_name?: string;
  /** 职责（≤255 字） */
  agent_description?: string;
  /** SOUL.md 全文（留空 → 服务端按模板生成） */
  soul_md?: string;
  /** 模型覆盖，如 { model: 'deepseek-v4.1-flash' } */
  model_override?: Record<string, unknown> | null;
  /** 克隆模板 profile 名（缺省用服务端配置） */
  clone_from?: string;
};

/** 编辑 AI 用户参数（字段缺省 = 不改） */
export type AiUserUpdateParams = {
  /** 职责（给了就同步 `hermes profile describe`） */
  agent_description?: string;
  /** SOUL.md 全文（给了就原子写磁盘） */
  soul_md?: string;
  /** 模型覆盖（null = 清空） */
  model_override?: Record<string, unknown> | null;
};

/** 删除 AI 用户结果 */
export interface AiUserDeleteRes {
  /** 被删除的用户 id */
  user_id: string;
  /** profile 名 */
  hermes_profile: string;
  /** SOUL.md 备份路径（磁盘上没有 SOUL.md 时为 null） */
  soul_backup_path: string | null;
  /** profile 目录是否已确认消失 */
  profile_removed: boolean;
  /** workspace 目录是否已确认消失 */
  workspace_removed: boolean;
}

/** 创建 AI 用户（建号 + profile + SOUL.md + workspace） */
export function createAiUser(data: AiUserCreateParams): Promise<AiUserRes> {
  return post<AiUserRes>('/api/system/user/ai', data);
}

/** 读 AI 用户配置（含 SOUL.md 全文） */
export function getAiUser(userId: string): Promise<AiUserRes> {
  return get<AiUserRes>(`/api/system/user/ai/${userId}`);
}

/** 编辑 AI 用户的职责 / SOUL.md / 模型 */
export function updateAiUser(userId: string, data: AiUserUpdateParams): Promise<AiUserRes> {
  return put<AiUserRes>(`/api/system/user/ai/${userId}`, data);
}

/** 探活：profile 目录 / SOUL.md / workspace / CLI / gateway */
export function getAiUserStatus(userId: string): Promise<AiUserStatusRes> {
  return get<AiUserStatusRes>(`/api/system/user/ai/${userId}/status`);
}

/** 删除 AI 用户（先备份 SOUL.md，再删 profile 与 workspace，最后软删账号） */
export function deleteAiUser(userId: string): Promise<AiUserDeleteRes> {
  return del<AiUserDeleteRes>(`/api/system/user/ai/${userId}`);
}

/** AI 用户模块接口集合 */
export const aiUserApi = {
  createAiUser,
  getAiUser,
  updateAiUser,
  getAiUserStatus,
  deleteAiUser,
};
