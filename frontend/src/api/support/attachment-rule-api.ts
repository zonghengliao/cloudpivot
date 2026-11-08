import { del, get, post, put } from '@/api/request';

/**
 * 附件必传规则管理接口（**ATT-09**）。
 *
 * 后端：`backend/src/business/attachment/rule_admin/`，权限码前缀
 * `support:attachment-rule:*`，**只有超管能用**（非超管 403）。
 *
 * # 为什么下拉数据也从后端取
 *
 * `doc_type` / `required_at` / `file_role` 三份词表在 Rust 侧各有一个唯一真相
 * （`DocKind::ALL` / `RequiredAt::ALL` / `FileRole::ALL`）。前端自己抄一份就会出现
 * 「界面能选、保存 400」的漂移（ATT-06 就因为常量少 5 个值踩过一次）。
 */

/** 下拉选项（`{ value, label }`） */
export interface RuleOptionRes {
  /** 落库字面值（`INVOICE` / `SUBMIT` / `INVOICE_SCAN` …） */
  value: string;
  /** 中文标签 */
  label: string;
}

/** 下拉数据源（`GET /api/support/attachment-rule/options`） */
export interface RuleOptionsRes {
  /** 单据类型（12 个） */
  doc_types: RuleOptionRes[];
  /** 触发动作（提交 / 确认 / 生效） */
  required_ats: RuleOptionRes[];
  /** 文件角色（6 个分栏 + 兼容值 SIGN_RECEIPT） */
  file_roles: RuleOptionRes[];
}

/** 一条附件必传规则 */
export interface AttachmentRuleRes {
  /** 规则主键 */
  id: string;
  /** 单据类型字面值 */
  doc_type: string;
  /** 单据类型中文名 */
  doc_type_label: string;
  /** 指定文件角色（`null` = 任意角色都算） */
  file_role: string | null;
  /** 文件角色中文名 */
  file_role_label: string | null;
  /** 触发动作字面值 */
  required_at: string;
  /** 触发动作中文名 */
  required_at_label: string;
  /** 至少要几个命中白名单的附件 */
  min_count: number;
  /** 最多几个（`null` = 不限） */
  max_count: number | null;
  /** 扩展名白名单原文（小写、逗号分隔；`null` = 不限格式） */
  allow_exts: string | null;
  /** 扩展名白名单数组（后端拆好，页面直接画 tag） */
  allow_exts_list: string[];
  /** 附加条件片段（`null` = 恒适用） */
  condition_sql: string | null;
  /** 人话名（直接进错误文案） */
  remark: string | null;
  /** 是否启用（禁用后**即时**不参与校验） */
  enabled: boolean;
  /** 完整要求文案（与提交被拦时的 400 报文同源） */
  requirement: string;
  /** 创建时间 */
  created_at: string;
  /** 最后更新时间 */
  updated_at: string;
}

/** 规则列表（含已禁用；不含已软删） */
export interface AttachmentRuleListRes {
  /** 规则 */
  list: AttachmentRuleRes[];
  /** 条数 */
  total: number;
}

/** 新建 / 编辑请求体（PUT 是**全量替换**：可空字段传 `null` = 置空） */
export interface AttachmentRuleSaveParams {
  /** 单据类型（`DocKind` 字面值） */
  doc_type: string;
  /** 文件角色（`null` = 任意角色都算） */
  file_role: string | null;
  /** 触发动作（`SUBMIT` / `CONFIRM` / `ACTIVATE`） */
  required_at: string;
  /** 至少要几个（缺省 1） */
  min_count: number;
  /** 最多几个（`null` = 不限） */
  max_count: number | null;
  /** 扩展名白名单（逗号分隔；`null` = 不限格式） */
  allow_exts: string | null;
  /** 附加条件片段（`null` = 恒适用） */
  condition_sql: string | null;
  /** 人话名（可空） */
  remark: string | null;
  /** 是否启用 */
  enabled: boolean;
}

/** 启停请求体 */
export interface AttachmentRuleToggleParams {
  /** 目标状态 */
  enabled: boolean;
}

/** 删除结果 */
export interface AttachmentRuleDeleteRes {
  /** 规则主键 */
  id: string;
  /** 是否真的删掉了 */
  deleted: boolean;
}

/** 规则列表（含已禁用） */
export function listAttachmentRules(): Promise<AttachmentRuleListRes> {
  return get<AttachmentRuleListRes>('/api/support/attachment-rule/list');
}

/** 下拉数据源（doc_type / required_at / file_role） */
export function getAttachmentRuleOptions(): Promise<RuleOptionsRes> {
  return get<RuleOptionsRes>('/api/support/attachment-rule/options');
}

/** 新建规则 */
export function createAttachmentRule(
  data: AttachmentRuleSaveParams,
): Promise<AttachmentRuleRes> {
  return post<AttachmentRuleRes>('/api/support/attachment-rule', data);
}

/** 全量编辑规则 */
export function updateAttachmentRule(
  id: string,
  data: AttachmentRuleSaveParams,
): Promise<AttachmentRuleRes> {
  return put<AttachmentRuleRes>(`/api/support/attachment-rule/${id}`, data);
}

/** 启用 / 禁用（即时生效，不重启） */
export function setAttachmentRuleEnabled(
  id: string,
  data: AttachmentRuleToggleParams,
): Promise<AttachmentRuleRes> {
  return put<AttachmentRuleRes>(`/api/support/attachment-rule/${id}/enabled`, data);
}

/** 软删规则 */
export function deleteAttachmentRule(id: string): Promise<AttachmentRuleDeleteRes> {
  return del<AttachmentRuleDeleteRes>(`/api/support/attachment-rule/${id}`);
}

/** 附件规则管理接口集合 */
export const attachmentRuleApi = {
  listAttachmentRules,
  getAttachmentRuleOptions,
  createAttachmentRule,
  updateAttachmentRule,
  setAttachmentRuleEnabled,
  deleteAttachmentRule,
};
