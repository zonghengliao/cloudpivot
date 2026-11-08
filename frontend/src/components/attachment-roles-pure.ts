/**
 * 单据附件的**分栏纯逻辑**（ATT-04：按 `file_role` 分组）。
 *
 * # 为什么单独一层
 *
 * 「按角色分组 / 空分组保留 / 未知值兜底 / 组内换位后重排整单顺序」全是可枚举的规则，
 * 放在 `.vue` 里只能靠截图验；放这里可以被 `pnpm test:unit`（`node --test`）直接跑。
 * 同款做法见 `attachment-panel-pure.ts`（ATT-03）与 `attachment-preview-pure.ts`（ATT-01）。
 *
 * # 词表只有一份
 *
 * 分组的 `code` / `label` 来自**接口下发的 `roles`**（后端 `FileRole::label`），
 * 本文件不维护角色中文名，只维护「怎么把 items 塞进分组」的规则。老实例不返回
 * `roles` 时才回落到调用方给的本地词表（`FILE_ROLE_OPTIONS`）。
 *
 * # 不变式（单测钉死）
 *
 * 1. `sum(groups[].count) === items.length` —— 每条附件恰好出现在一个分组里；
 * 2. 分组顺序 = 词表顺序（空分组也在，用于显示「暂无 XX，点击上传」）；
 * 3. 词表里没有的 `file_role`（未知值 / 兼容值）→ 有 `DEFAULT` 分组就并入它，
 *    否则丢弃（绝不新增一个没有中文名的分组）；
 * 4. `moveWithinGroup` 返回的整单顺序 = 各分组按词表顺序拼接（与后端
 *    `sort_order` 的落库口径一致：界面上看到什么，库里就存什么）。
 */

/** 分组用的最小角色契约（`DocAttachmentRoleGroup` 的超集） */
export interface RoleOption {
  code: string;
  label: string;
}

/** 一条附件的分栏契约（`DocAttachmentItem` 的超集） */
export interface RoleTagged {
  file_role: string;
}

/** 一个渲染分组 */
export interface AttachmentRoleBucket<T> {
  /** 角色字面值（`CONTRACT_SCAN` / … / `DEFAULT`） */
  code: string;
  /** 分组标题（中文） */
  label: string;
  /** 该组的附件（保持传入顺序 = 当前排序口径下的顺序） */
  items: T[];
  /** 条数（= items.length，便于模板直接用） */
  count: number;
}

/** 兜底分组的角色值（后端缺省值同口径） */
export const DEFAULT_ROLE_CODE = 'DEFAULT';

/**
 * 解析「该显示哪些分组」。
 *
 * 优先用接口下发的 `roles`（后端唯一真相，空分组也在里面）；老实例 / 接口没给时
 * 回落本地词表（`FILE_ROLE_OPTIONS`），保证面板在旧后端上也能按角色分栏。
 *
 * @param roles 接口返回的 `roles`
 * @param fallback 本地词表（`FILE_ROLE_OPTIONS`）
 */
export function resolveRoleCatalog(
  roles: readonly RoleOption[] | null | undefined,
  fallback: readonly RoleOption[],
): RoleOption[] {
  const source = (roles ?? []).filter((role) => role && typeof role.code === 'string' && role.code);
  if (source.length > 0) {
    return source.map((role) => ({ code: role.code, label: role.label || role.code }));
  }
  return fallback.map((role) => ({ code: role.code, label: role.label }));
}

/**
 * 把附件按 `file_role` 分进各个分组（**纯函数**，不改入参）。
 *
 * @param items 附件列表（顺序 = 当前排序口径）
 * @param catalog 分组词表（顺序 = 渲染顺序，空分组也会出现在结果里）
 *
 * 未知 `file_role`（词表里没有）的处理：
 *
 * - 词表里有 `DEFAULT` → 并入 `DEFAULT`（宁可显示在「其他附件」里，也不能让附件凭空消失）；
 * - 词表里没有 `DEFAULT`（调用方只传了若干角色）→ 丢弃，避免出现没有中文名的分组。
 */
export function groupByRole<T extends RoleTagged>(
  items: readonly T[],
  catalog: readonly RoleOption[],
): AttachmentRoleBucket<T>[] {
  const buckets: AttachmentRoleBucket<T>[] = catalog.map((role) => ({
    code: role.code,
    label: role.label,
    items: [],
    count: 0,
  }));
  const byCode = new Map(buckets.map((bucket) => [bucket.code, bucket]));
  const fallback = byCode.get(DEFAULT_ROLE_CODE);

  for (const item of items) {
    const bucket = byCode.get(item.file_role) ?? fallback;
    if (!bucket) {
      continue;
    }
    bucket.items.push(item);
    bucket.count = bucket.items.length;
  }
  return buckets;
}

/**
 * 空分组的引导文案（卡面验收②：「暂无 XX，点击上传」）。
 *
 * 抽成函数而不是在模板里拼 `暂无${label}，点击上传`：这句话会被单测与 e2e 断言，
 * 模板里的字符串一旦改动没有任何失败信号。
 */
export function emptyRoleHint(label: string): string {
  return `暂无${label}，点击上传`;
}

/**
 * 组内上移 / 下移（返回**整单的平铺顺序**，不是只返回该组）。
 *
 * # 为什么要返回整单
 *
 * 后端 `PUT /attachment/sort` 要求提交该单据附件的**完整 id 顺序**（ATT-03 的决定：
 * 部分提交会与其他组的序号撞号）。分组展示下「组内换位」必须换算回整单顺序：
 * 各分组按词表顺序拼接，组内保持移动后的顺序。
 *
 * # 与「按时间」模式的关系
 *
 * 手动排序建立后，整单顺序 = 分组顺序 × 组内顺序，因此同一组内的相对位置与界面一致；
 * 跨组的位置由词表决定（用户在界面上也拖不过去）。
 *
 * @param items 当前整单列表（顺序 = 当前排序口径）
 * @param catalog 分组词表（同 `groupByRole`）
 * @param code 目标分组
 * @param from 组内源下标
 * @param to 组内目标下标（越界 / 相等 → 原顺序返回）
 */
export function moveWithinGroup<T extends RoleTagged>(
  items: readonly T[],
  catalog: readonly RoleOption[],
  code: string,
  from: number,
  to: number,
): T[] {
  const groups = groupByRole(items, catalog);
  const target = groups.find((group) => group.code === code);
  if (!target) {
    return [...items];
  }
  const list = target.items;
  if (
    !Number.isInteger(from) ||
    !Number.isInteger(to) ||
    from < 0 ||
    from >= list.length ||
    to < 0 ||
    to >= list.length ||
    from === to
  ) {
    return [...items];
  }
  const [moved] = list.splice(from, 1);
  list.splice(to, 0, moved);
  return groups.flatMap((group) => group.items);
}

/** 整单平铺顺序（分组顺序 × 组内顺序）：提交排序 / 预览左右切换的统一口径 */
export function flattenGroups<T>(groups: readonly AttachmentRoleBucket<T>[]): T[] {
  return groups.flatMap((group) => group.items);
}
