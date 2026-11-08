/**
 * 附件必传规则管理 · **纯逻辑**（ATT-09）。
 *
 * 这里只有不碰 DOM / 不发请求的函数，方便单测把「界面上存得进去 = 后端收得下」
 * 钉死。判定口径与后端 `rule_admin::dto` **逐条对齐**：
 *
 * | 规则 | 后端 | 这里 |
 * |------|------|------|
 * | 空白 → 不限格式（`null`） | `parse_allow_exts(None/空白)` | `parseAllowExts` |
 * | 每个 token `[a-z0-9]{1,10}`（不许 `*` / 空格 / 点号前缀可留） | 同 | 同 |
 * | 去重保序、小写、中文逗号也认 | 同 | 同 |
 * | 最多 20 个 | `EXT_TOKEN_MAX_COUNT` | `EXT_TOKEN_MAX_COUNT` |
 * | `min_count ≥ 1`、`max_count` 留空或 `≥ min_count` | `validate` | `validateCounts` |
 *
 * **前端校验只是提前告知**，真正的闸门在后端（后端仍然会 400）。所以这里不追求
 * 「比后端更严」，只追求「同一份错误在点保存之前就能看见」。
 */

/** 单个扩展名最大长度（与后端 `EXT_TOKEN_MAX_CHARS` 一致） */
export const EXT_TOKEN_MAX_CHARS = 10;

/** 一条规则最多写多少个扩展名（与后端 `EXT_TOKEN_MAX_COUNT` 一致） */
export const EXT_TOKEN_MAX_COUNT = 20;

/** allow_exts 解析结果 */
export interface AllowExtsResult {
  /** 归一后的值（`null` = 不限格式） */
  value: string | null;
  /** 校验失败的文案（`null` = 通过） */
  error: string | null;
}

/**
 * 解析 `allow_exts` 输入框（逗号分隔）。
 *
 * @param raw 输入框原文（允许空 = 不限格式）
 * @returns 归一后的值 + 错误文案；两者互斥（`error` 非空时 `value` 为 `null`）
 */
export function parseAllowExts(raw: string | null | undefined): AllowExtsResult {
  const text = (raw ?? '').trim();
  if (!text) {
    return { value: null, error: null };
  }
  const out: string[] = [];
  for (const part of text.split(/[,，]/)) {
    const token = part.trim().replace(/^\.+/, '').toLowerCase();
    if (!token) {
      continue;
    }
    if (token.length > EXT_TOKEN_MAX_CHARS || !/^[a-z0-9]+$/.test(token)) {
      return {
        value: null,
        error: `扩展名「${token}」不合法：只允许字母 / 数字，长度 1–${EXT_TOKEN_MAX_CHARS}（不要写点号前缀与通配符）`,
      };
    }
    if (!out.includes(token)) {
      out.push(token);
    }
  }
  if (out.length === 0) {
    return {
      value: null,
      error: '只写了分隔符：要么留空表示不限格式，要么写出具体扩展名',
    };
  }
  if (out.length > EXT_TOKEN_MAX_COUNT) {
    return {
      value: null,
      error: `最多 ${EXT_TOKEN_MAX_COUNT} 个扩展名（当前 ${out.length} 个）`,
    };
  }
  return { value: out.join(','), error: null };
}

/** 数量区间校验结果 */
export interface CountsResult {
  /** 错误文案（`null` = 通过） */
  error: string | null;
}

/**
 * 校验数量区间：`min_count` 至少 1；`max_count` 留空（`null` / 0）或 ≥ `min_count`。
 *
 * @param minCount 至少几个
 * @param maxCount 最多几个（`null` / `undefined` = 不限）
 */
export function validateCounts(
  minCount: number,
  maxCount: number | null | undefined,
): CountsResult {
  if (!Number.isInteger(minCount) || minCount < 1) {
    return {
      error: `min_count 至少要 1（当前 ${minCount}）：0 等于不校验，会让单据全部放行`,
    };
  }
  if (maxCount === null || maxCount === undefined) {
    return { error: null };
  }
  if (!Number.isInteger(maxCount) || maxCount < 1) {
    return { error: `max_count 要么留空（不限），要么至少 1（当前 ${maxCount}）` };
  }
  if (maxCount < minCount) {
    return {
      error: `max_count（${maxCount}）不能小于 min_count（${minCount}）：这样的规则永远不可能成立`,
    };
  }
  return { error: null };
}

/**
 * 把表格行折叠成一句「要求摘要」，给列表的 tooltip 用。
 *
 * 只做拼接，不重算判定：文案与后端 `requirement` 同源（页面上显示什么，
 * 提交被拦时就是什么）。
 *
 * @param requirement 后端返回的完整要求文案
 * @param enabled 是否启用
 */
export function requirementSummary(requirement: string, enabled: boolean): string {
  return enabled ? requirement : `${requirement}（当前已禁用，不参与校验）`;
}
