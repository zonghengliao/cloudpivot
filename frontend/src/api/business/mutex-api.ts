import { del, get, post, put, type PageData } from '@/api/request';

/**
 * 角色互斥（批次 B · B7，SoD 职责分离）管理接口。
 *
 * # 契约来源
 *
 * 后端 `backend/src/business/mutex/{dto,handler}.rs`（接口挂在 `/business/mutex`）。
 *
 * | 方法 | 路径 | 权限码 |
 * |------|------|--------|
 * | GET    | `/api/business/mutex/page` | `system:role-mutex:list` |
 * | POST   | `/api/business/mutex` | `system:role-mutex:create` |
 * | PUT    | `/api/business/mutex/{id}` | `system:role-mutex:create` |
 * | DELETE | `/api/business/mutex/{id}` | `system:role-mutex:delete` |
 * | GET    | `/api/business/mutex/conflict` | `system:role-mutex:conflict` |
 * | POST   | `/api/business/mutex/scan` | `system:role-mutex:conflict` |
 *
 * # 四条不能违反的口径
 *
 * 1. `role_a` / `role_b` 由后端按 `role_code` 字典序规范化，**前端不排序、不假设方向**。
 * 2. 编辑只能改 `reason` / `enabled`（换角色对 = 删了重建）。
 * 3. 「停用」在库里是软删（`DELETE` 语义），恢复启用请用 `PUT` + `enabled: true`。
 * 4. 创建时已有用户双持 → `409 + 61010`（消息含人数与示例用户名）；确认后可带
 *    `?force=true` 强制落库。
 */

/** 一条互斥规则 */
export interface MutexRes {
  /** 规则 ID */
  id: string;
  /** 角色 A（字典序较小侧）ID */
  role_a_id: string;
  /** 角色 A 编码 */
  role_a_code: string;
  /** 角色 A 名称 */
  role_a_name: string;
  /** 角色 B ID */
  role_b_id: string;
  /** 角色 B 编码 */
  role_b_code: string;
  /** 角色 B 名称 */
  role_b_name: string;
  /** 互斥理由 */
  reason: string;
  /** 是否生效 */
  enabled: boolean;
  /** 创建时间 */
  created_at: string;
  /** 创建人 */
  create_by: string | null;
}

/** 规则分页查询参数 */
export type MutexPageQuery = {
  /** 页码，从 1 开始 */
  page: number;
  /** 每页条数 */
  page_size: number;
  /** 只看与某角色相关的规则 */
  role_id?: string;
  /** 角色编码 / 名称 / 理由模糊匹配 */
  keyword?: string;
  /** 1 只看生效 / 0 只看停用 / 缺省都看 */
  status?: number;
};

/** 新建规则参数 */
export type MutexCreateParams = {
  /** 角色 A */
  role_a_id: string;
  /** 角色 B */
  role_b_id: string;
  /** 互斥理由（必填） */
  reason: string;
};

/** 编辑规则参数（角色对不可改） */
export type MutexUpdateParams = {
  /** 新的理由；缺省不改 */
  reason?: string;
  /** 启停；缺省不改 */
  enabled?: boolean;
};

/** 一条被命中的互斥对 */
export interface MutexPairRes {
  /** 规则 ID */
  mutex_id: string;
  /** 互斥理由 */
  reason: string;
  /** 角色 A ID */
  role_a_id: string;
  /** 角色 A 编码 */
  role_a_code: string;
  /** 角色 A 名称 */
  role_a_name: string;
  /** 角色 B ID */
  role_b_id: string;
  /** 角色 B 编码 */
  role_b_code: string;
  /** 角色 B 名称 */
  role_b_name: string;
}

/** 违规用户 */
export interface MutexHolderRes {
  /** 用户 ID */
  user_id: string;
  /** 登录名 */
  username: string;
  /** 展示名（real_name 为空时等于 username） */
  display_name: string;
  /** 部门名称；无部门为空串 */
  dept_name: string;
}

/** 单用户互斥体检结果 */
export interface MutexConflictRes {
  /** 用户 ID */
  user_id: string;
  /** 登录名 */
  username: string;
  /** 展示名 */
  display_name: string;
  /** 已违规的互斥对（空数组 = 未违规） */
  conflicts: MutexPairRes[];
}

/** 全量扫描里的一组违规 */
export interface MutexScanItemRes {
  /** 规则 ID */
  mutex_id: string;
  /** 互斥理由 */
  reason: string;
  /** 角色 A 编码 */
  role_a_code: string;
  /** 角色 A 名称 */
  role_a_name: string;
  /** 角色 B 编码 */
  role_b_code: string;
  /** 角色 B 名称 */
  role_b_name: string;
  /** 违规用户数 */
  total: number;
  /** 违规用户样本 */
  sample: MutexHolderRes[];
}

/** 全量存量扫描结果 */
export interface MutexScanRes {
  /** 扫描的生效规则数 */
  scanned_pairs: number;
  /** 违规规则数 */
  violated_pairs: number;
  /** 违规人次 */
  violated_holders: number;
  /** 违规明细 */
  items: MutexScanItemRes[];
}

/** 分页查询互斥规则 */
export function pageMutexes(query: MutexPageQuery): Promise<PageData<MutexRes>> {
  return get<PageData<MutexRes>>('/api/business/mutex/page', query);
}

/**
 * 新建互斥规则。
 *
 * 失败码：
 * - `61012` 两个角色相同；
 * - `61011` 该对的互斥关系已存在（含反向）；
 * - `61010` 已有用户同时持有这两个角色（`force` 为 true 时跳过阻断）。
 */
export function createMutex(data: MutexCreateParams, force = false): Promise<MutexRes> {
  const url = force ? '/api/business/mutex?force=true' : '/api/business/mutex';
  return post<MutexRes>(url, data);
}

/** 修改互斥规则（只能改理由与启停） */
export function updateMutex(id: string, data: MutexUpdateParams): Promise<MutexRes> {
  return put<MutexRes>(`/api/business/mutex/${id}`, data);
}

/** 删除互斥规则（软删 = 停用） */
export function deleteMutex(id: string): Promise<unknown> {
  return del<unknown>(`/api/business/mutex/${id}`);
}

/** 单用户互斥体检 */
export function getMutexConflict(userId: string): Promise<MutexConflictRes> {
  return get<MutexConflictRes>('/api/business/mutex/conflict', { user_id: userId });
}

/** 存量全量扫描（只读，不落库） */
export function scanMutexes(): Promise<MutexScanRes> {
  return post<MutexScanRes>('/api/business/mutex/scan');
}

/** 角色互斥模块接口集合 */
export const mutexApi = {
  pageMutexes,
  createMutex,
  updateMutex,
  deleteMutex,
  getMutexConflict,
  scanMutexes,
};
