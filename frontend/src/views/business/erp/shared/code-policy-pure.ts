/**
 * 主数据编码策略的**纯逻辑**部分（不 import 任何 API / vue，便于 `pnpm test:unit` 直接跑）。
 *
 * 有副作用的那一半（接口请求 + 进程内缓存）在 `./code-policy.ts`，两个文件的分工：
 *
 * | 文件 | 内容 | 能不能单测 |
 * |---|---|---|
 * | 本文件 | 兜底值 / 帮助文案 | 能（`node --test` 直接 import） |
 * | `code-policy.ts` | `fetchMasterDataCodePolicy()` + Promise 缓存 | 不能（会拉起 axios / `import.meta.env`） |
 */

/** 策略「允许手工指定编码」的兜底值：与后端 `code_policy::DEFAULT_ALLOW_MANUAL_CODE` 同口径。 */
export const DEFAULT_ALLOW_MANUAL_CODE = true;

/** 开关关闭时编码栏下方的提示（六个表单逐字一致，省得各页各写一份）。 */
export const MANUAL_CODE_FORBIDDEN_HINT =
  '编码由系统自动生成（管理员已关闭「允许手工指定编码」，保留空即可）';

/**
 * 编码栏的帮助文案。
 *
 * @param allowManual {@link DEFAULT_ALLOW_MANUAL_CODE} 一侧的生效值
 * @param editHint 编辑态 / 开关打开时的原文案（由各页传入，避免这里硬编码业务措辞）
 */
export function manualCodeHelp(allowManual: boolean, editHint: string): string {
  return allowManual ? editHint : MANUAL_CODE_FORBIDDEN_HINT;
}

/**
 * 编码输入框是否禁用：编辑态**或**开关关闭都禁用。
 *
 * 抽成函数而不是让模板写 `isEdit || !allowManualCode`：六个表单各写一遍迟早漏一个，
 * 而漏掉的那一个会变成「开关关了仍能填」的破口（后端还会 400，但体验已经错了）。
 */
export function manualCodeDisabled(isEdit: boolean, allowManual: boolean): boolean {
  return isEdit || !allowManual;
}
