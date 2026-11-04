/**
 * 文件管理「上传人」列的译名逻辑（P2-2B 缺陷②）。
 *
 * # 为什么单独一个文件
 *
 * `GET /business/storage/file/page` 的 `uploaded_by` 是 `sys_file.uploaded_by` 的 UUID 文本
 * （后端 `StorageFileRes::from_model` 直接 `id.to_string()`），页面原来把它原样渲染，
 * 于是列表里出现 `01a19012-7573-7602-9b64-6b2e74502477` 这种账号主键 —— 使用者看不出是谁上传的。
 *
 * 译名数据来自 `GET /api/system/user/page`（与本仓资产页 / 流程主数据译名同一条路子）。
 * 这里只放**纯函数**：去重、合并、回退规则都能在没有 DOM 的 `tsx --test` 里钉住，
 * 页面只负责发一次请求 + 渲染。
 *
 * # 回退规则（硬要求）
 *
 * 姓名 → 用户名 → **原 UUID**。解析不到就退回 UUID，绝不显示空白，也绝不改成手机号
 * （`sys_user.mobile` 已配脱敏，把上传人换成手机号属于越界）。
 */

/** 用户译名所需的字段子集（`sys_user.real_name` / `username`） */
export interface UploaderUserLike {
  /** 用户主键（= `sys_file.uploaded_by`） */
  id: string;
  /** 用户名（登录名） */
  username: string;
  /** 真实姓名（可能为空串 / null） */
  real_name?: string | null;
}

/** 一次拉多少用户做译名（与资产页 / 巡检页同口径；种子规模下够用） */
export const UPLOADER_PAGE_SIZE = 200;

/**
 * 上传人显示名：姓名 → 用户名 → 原 UUID。
 *
 * - `uploaded_by` 为空（历史文件 / 系统导入）→ `—`，不留白；
 * - 解析不到（用户被删 / 超出译名页范围 / 当前账号没有 `system:user:list`）→ **原 UUID**，
 *   用户至少还能拿它去对照；这比显示「未知」更能查问题。
 */
export function uploaderDisplayName(
  uploadedBy: string | null | undefined,
  names: ReadonlyMap<string, string>,
): string {
  const id = (uploadedBy ?? '').trim();
  if (!id) {
    return '—';
  }
  const hit = names.get(id);
  const label = (hit ?? '').trim();
  return label || id;
}

/**
 * 本页需要新解析的上传人 id（去重、剔除空串与已解析的）。
 *
 * 只解析**没解析过**的 id：翻页时不会把整张用户表再拉一遍（`uploaderNames` 在页面里跨页复用）。
 */
export function pendingUploaderIds(
  rows: ReadonlyArray<{ uploaded_by?: string | null }>,
  resolved: ReadonlyMap<string, string>,
): string[] {
  const out: string[] = [];
  for (const row of rows) {
    const id = (row.uploaded_by ?? '').trim();
    if (!id || resolved.has(id) || out.includes(id)) {
      continue;
    }
    out.push(id);
  }
  return out;
}

/**
 * 把用户分页结果并进「id → 显示名」表（返回**新** Map，便于触发 Vue 响应）。
 *
 * 显示名取 `real_name`，空则回落 `username`；两个都空（脏数据）就不写这条 ——
 * 让 `uploaderDisplayName` 继续回退 UUID，而不是在列里显示空串。
 */
export function mergeUserNames(
  names: ReadonlyMap<string, string>,
  users: ReadonlyArray<UploaderUserLike>,
): Map<string, string> {
  const merged = new Map(names);
  for (const user of users) {
    const id = (user.id ?? '').trim();
    if (!id) {
      continue;
    }
    const label = (user.real_name ?? '').trim() || (user.username ?? '').trim();
    if (label) {
      merged.set(id, label);
    }
  }
  return merged;
}
