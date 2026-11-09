/**
 * 主数据编码的**前端口径**（镜像后端 `seq::resolve_manual_code` + `validate_manual_code`）。
 *
 * # 为什么单独一片
 *
 * 供应商 / 客户 / 物料 / 仓库 / 资产分类 / 费用类型六个主数据表单都有同一件事：
 * 「编码留空 = 后端自动取号，填了 = 原样保留」。前端如果各自写一份正则，迟早会
 * 出现「后端能存的码，前端先拦了」——最典型的就是取号器产出的 `AC-01` / `ET-01`
 * 带连字符，旧正则只允许 `[A-Z0-9_]`，**用户点完「生成编码」自己保存不过去**。
 *
 * 所以规则只在这里写一次：
 *
 * | 口径 | 值 | 依据 |
 * |---|---|---|
 * | 最大长度 | 40 | 库列 `VARCHAR(40)`；后端 `seq::MAX_MANUAL_CODE_LEN` |
 * | 允许字符 | 字母 / 数字 / `-` `_` `.` | 后端 `seq::is_manual_code_char` |
 * | 大小写 | **原样保留**（只 trim） | 后端六类统一「不转大写」（E2E08-FIX-2 ①） |
 * | 留空 | `null`（不是 `''`） | 后端 `resolve_manual_code(None)` = 交给取号器 |
 *
 * # 注意：**不校验**具体形态
 *
 * `SUP-0001` / `MAT-0001` / `WH-01` / `AC-01` / `ET-01` 只是自动取号的产物，
 * 手工编码允许企业自有体系（`CAT-BUILDING`、`ADMIN-FEE`、`YB-001-02`、`CK-001`）。
 * 谁在这里加一条「必须以 SUP- 开头」，就是替用户做了他没答应的事。
 *
 * # 大小写：六类一致，别再分家（E2E08-FIX-2 ①）
 *
 * 历史上前端只对资产分类 / 费用类型调了 `toUpperCase()`，另四类原样提交 ——
 * 同一张「主数据编码」规则表里两种行为，对接方按一类经验写另一类会踩坑。
 * 现在统一**原样保留**：有些企业自有编码区分大小写，替用户改大小写等于改他的编码；
 * 后端也不再把 AC / ET 归一成大写。
 */

/** 编码最大长度（与库列 `VARCHAR(40)` / 后端 `MAX_MANUAL_CODE_LEN` 同口径）。 */
export const MASTER_CODE_MAX_LEN = 40;

/**
 * 手工编码允许的字符集（**大小写都收**）：字母 / 数字 / `-` `_` `.`。
 *
 * 第 1 位不限（后端不要求字母开头）：`1A-01` 这类企业码后端照收。
 */
export const MASTER_CODE_PATTERN = /^[A-Za-z0-9._-]{1,40}$/;

/** 六个主数据表单共用的编码提示文案（占位符 / help 用，避免各页各写一份）。 */
export const MASTER_CODE_HINT = '留空则保存时自动生成；可手填企业自有编码（字母 / 数字 / - _ .，大小写按填写原样保存）';

/**
 * 表单值 → 编码入参：去首尾空白，**不改大小写**，空则 `null`。
 *
 * `null` 的语义是「没填」——后端据此自动取号；`''` 后端也当没填，但看不出意图。
 */
export function toMasterCodeInput(raw: string | null | undefined): string | null {
  const text = (raw ?? '').trim();
  return text === '' ? null : text;
}

/**
 * 校验手工编码；`null` = 通过（**留空也算通过**，由后端取号）。
 *
 * 返回的文案与后端 400 逐字对齐（「编码最长 40 个字符（当前 N 个）」「编码里不能出现 …」），
 * 免得同一条错误前后端两种说法。大小写不参与校验（原样保留）。
 */
export function masterCodeError(raw: string | null | undefined): string | null {
  const text = (raw ?? '').trim();
  if (text === '') {
    return null;
  }
  const len = [...text].length;
  if (len > MASTER_CODE_MAX_LEN) {
    return `编码最长 ${MASTER_CODE_MAX_LEN} 个字符（当前 ${len} 个）`;
  }
  const bad = [...text].find((ch) => !/[A-Za-z0-9._-]/.test(ch));
  if (bad !== undefined) {
    return `编码里不能出现「${bad}」：只允许字母、数字与 - _ .（企业自有编码可直接沿用）`;
  }
  return null;
}
