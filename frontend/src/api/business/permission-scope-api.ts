import { get, put } from '@/api/request';

/**
 * 数据权限（批次 B · B4）配置面接口。
 *
 * # 契约来源
 *
 * 后端 `backend/src/business/permission/{dto,scope_handler}.rs`，前缀 `/api` 由后端
 * `[router].prefix` 注入，本文件写全路径。
 *
 * | 方法 | 路径 | 权限码 |
 * |------|------|--------|
 * | GET  | `/api/business/permission/scope/role/{role_id}` | `business:permission:scope:read` |
 * | PUT  | `/api/business/permission/scope/role/{role_id}` | `business:permission:scope:write` |
 * | GET  | `/api/business/permission/scope/dept-tree` | `business:permission:scope:read` |
 * | GET  | `/api/business/permission/scope/self-check` | `business:permission:scope:self-check` |
 *
 * # 三条不能违反的口径
 *
 * 1. **中文标签随响应下发**（`data_scope_label` / `scope.label`），前端不为已保存的
 *    范围硬编码文案；只有「可选项」5 个标签在前端维护一份（后端没有枚举字典接口）。
 * 2. **部门树由后端按操作者范围裁剪**（`scope_limited` / `visible_dept_ids` 可对账），
 *    前端不做「拉全量再过滤」——那本身就是越权读取组织信息。
 * 3. `custom_dept_ids` 三态：`undefined`（不动）、`[]`（清空）、`[id…]`（覆盖）。
 */

/** 部门精简信息（自定义范围回显 / 越界部门点名） */
export interface DeptBriefRes {
  /** 部门 ID */
  id: string;
  /** 部门编码 */
  dept_code: string;
  /** 部门名称 */
  dept_name: string;
}

/** 一个角色的数据范围配置 */
export interface RoleScopeRes {
  /** 角色 ID */
  role_id: string;
  /** 角色编码 */
  role_code: string;
  /** 角色名称 */
  role_name: string;
  /** 角色状态：1 正常 / 0 停用 */
  status: number;
  /** 数据范围落库值（1..5） */
  data_scope: number;
  /** 数据范围稳定编码（ALL / DEPT / DEPT_AND_CHILD / SELF / CUSTOM） */
  data_scope_code: string;
  /** 数据范围中文标签（后端下发，直接展示） */
  data_scope_label: string;
  /** 自定义部门 ID 列表（非 5 时也回显库里的存量配置） */
  custom_dept_ids: string[];
  /** 自定义部门明细 */
  custom_depts: DeptBriefRes[];
  /** 该角色当前持有者数量（保存会让这些人的缓存失效） */
  holder_count: number;
}

/** 设置数据范围的请求体 */
export type SetRoleScopeParams = {
  /** 数据范围：1 全部 / 2 本部门 / 3 本部门及以下 / 4 仅本人 / 5 自定义部门 */
  data_scope: number;
  /** 自定义部门 ID 列表；不传 = 不动存量配置，传 [] = 清空 */
  custom_dept_ids?: string[];
};

/** 设置数据范围的结果 */
export interface SetRoleScopeRes {
  /** 角色 ID */
  role_id: string;
  /** 生效后的范围落库值 */
  data_scope: number;
  /** 生效后的范围编码 */
  data_scope_code: string;
  /** 生效后的范围标签 */
  data_scope_label: string;
  /** 生效后的自定义部门 ID */
  custom_dept_ids: string[];
  /** 生效后的自定义部门明细 */
  custom_depts: DeptBriefRes[];
  /** 本次被清数据范围缓存的用户数 */
  invalidated_users: number;
}

/** 自定义部门树节点（**已按操作者范围裁剪**） */
export interface ScopeDeptTreeNode {
  /** 部门 ID */
  id: string;
  /** 上级部门 ID（父节点不可见时为 null，即提升为根） */
  parent_id: string | null;
  /** 部门编码 */
  dept_code: string;
  /** 部门名称 */
  dept_name: string;
  /** 状态：1 正常 / 0 停用 */
  status: number;
  /** 排序值 */
  sort_order: number;
  /** 子部门 */
  children: ScopeDeptTreeNode[];
}

/** 自定义部门树响应 */
export interface ScopeDeptTreeRes {
  /** 裁剪后的节点总数 */
  total: number;
  /** 根节点个数 */
  root_count: number;
  /** 是否按操作者范围裁剪过（false = 操作者是超管或范围=全部） */
  scope_limited: boolean;
  /** 裁剪后保留的部门 ID */
  visible_dept_ids: string[];
  /** 树本身 */
  tree: ScopeDeptTreeNode[];
}

/** 数据范围摘要 */
export interface ScopeSummaryRes {
  /** 落库值 */
  data_scope: number;
  /** 稳定编码 */
  code: string;
  /** 中文标签 */
  label: string;
}

/** 来源角色（解释「我的范围是哪个角色给的」） */
export interface SourceRoleRes {
  /** 角色 ID */
  role_id: string;
  /** 角色编码 */
  role_code: string;
  /** 角色名称 */
  role_name: string;
  /** 该角色的范围落库值 */
  data_scope: number;
  /** 该角色的范围编码 */
  data_scope_code: string;
}

/** 一张登记表上最终注入的片段（不含参数值） */
export interface TableFilterRes {
  /** 表名 */
  table: string;
  /** 主体表别名 */
  alias: string;
  /** 片段原文 */
  where_sql: string;
  /** 绑定参数个数 */
  binds: number;
  /** 是否等价于「不过滤」 */
  is_noop: boolean;
  /** 是否发生安全侧降级 */
  degraded: boolean;
}

/** 当前用户的数据权限自检结果（只取页面用到的字段） */
export interface ScopeSelfCheckRes {
  /** 用户 ID */
  user_id: string;
  /** 登录名 */
  username: string;
  /** 姓名 */
  real_name: string;
  /** 是否超管 */
  is_super_admin: boolean;
  /** 所属部门 ID */
  own_dept_id: string | null;
  /** 所属部门名称 */
  own_dept_name: string | null;
  /** 生效范围摘要 */
  scope: ScopeSummaryRes;
  /** 可见部门数量 */
  dept_count: number;
  /** 可见部门 ID */
  dept_ids: string[];
  /** 可见部门明细 */
  depts: DeptBriefRes[];
  /** 是否额外可见「自己的行」 */
  self_only: boolean;
  /** 来源角色 */
  roles: SourceRoleRes[];
  /** 各登记表注入片段 */
  tables: TableFilterRes[];
}

/** 查询某个角色的数据范围配置 */
export function getRoleScope(roleId: string): Promise<RoleScopeRes> {
  return get<RoleScopeRes>(`/api/business/permission/scope/role/${roleId}`);
}

/**
 * 保存某个角色的数据范围。
 *
 * 失败码（前端必须原样展示后端消息，尤其是 61002 会点名越界部门）：
 * - `61001` 参数非法（范围越界 / 自定义为空 / 非 5 却带集合）；
 * - `61002` 超出操作者自身可见范围；
 * - `61003` 不能改自己持有角色的范围；
 * - `61004` 部门不存在或已删除。
 */
export function updateRoleScope(
  roleId: string,
  data: SetRoleScopeParams,
): Promise<SetRoleScopeRes> {
  return put<SetRoleScopeRes>(`/api/business/permission/scope/role/${roleId}`, data);
}

/** 查询自定义部门树（**后端已按操作者范围裁剪，不要在前端再过滤**） */
export function getScopeDeptTree(): Promise<ScopeDeptTreeRes> {
  return get<ScopeDeptTreeRes>('/api/business/permission/scope/dept-tree');
}

/** 当前用户的数据权限自检 */
export function getScopeSelfCheck(): Promise<ScopeSelfCheckRes> {
  return get<ScopeSelfCheckRes>('/api/business/permission/scope/self-check');
}

/** 5 种数据范围的可选项（值 + 稳定编码 + 文案；与后端 `ScopeKind` 一致） */
export const DATA_SCOPE_OPTIONS: ReadonlyArray<{
  /** 落库值 */
  value: number;
  /** 稳定编码 */
  code: string;
  /** 文案 */
  label: string;
  /** 一句话说明 */
  hint: string;
}> = [
  { value: 1, code: 'ALL', label: '全部数据', hint: '不受部门限制（最高权限，误配即静默泄露）' },
  { value: 2, code: 'DEPT', label: '本部门', hint: '仅本人所在部门的数据' },
  { value: 3, code: 'DEPT_AND_CHILD', label: '本部门及以下', hint: '本人所在部门 + 全部下级部门' },
  { value: 4, code: 'SELF', label: '仅本人', hint: '只看自己创建 / 归属自己的数据' },
  { value: 5, code: 'CUSTOM', label: '自定义部门', hint: '手工勾选部门集合（可选范围已被后端裁剪）' },
];

/** 数据权限模块接口集合 */
export const permissionScopeApi = {
  getRoleScope,
  updateRoleScope,
  getScopeDeptTree,
  getScopeSelfCheck,
};
