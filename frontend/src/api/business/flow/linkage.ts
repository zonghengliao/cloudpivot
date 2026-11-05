/**
 * 跨单据联动规则的**配置管理面**接口（卡 t_d0daa4f0 / P1-FIX-5A）。
 *
 * 契约逐字抄自 `backend/src/business/flow/linkage/admin.rs`：
 *
 * | # | 方法 | 路径 | 权限码 | 落点 |
 * |---|------|------|--------|------|
 * | 1 | GET    | `/business/flow/linkage`                  | `business:flow:linkage:list`   | `list_rules`（`RuleListRes`） |
 * | 2 | PUT    | `/business/flow/linkage`                  | `business:flow:linkage:manage` | `upsert_rule`（`RuleRes`） |
 * | 3 | PUT    | `/business/flow/linkage/toggle`           | `business:flow:linkage:manage` | `toggle_rule`（`RuleRes`） |
 * | 4 | DELETE | `/business/flow/linkage?id=&confirm=true` | `business:flow:linkage:manage` | `delete_rule`（空） |
 * | 5 | POST   | `/business/flow/linkage/preview`          | `business:flow:linkage:list`   | `preview_impact`（`PreviewRes`） |
 *
 * # 三条不许自己发明的口径
 *
 * 1. **动作是白名单选择**：动作码只来自 `GET` 下发的 `actions`（后端 `spec::ACTIONS`），
 *    前端不维护第二份动作表；`implemented = false` 的动作在下拉里置灰（后端也会 400）。
 * 2. **参数表单由后端声明驱动**：`params[].kind` 决定控件（text / uuid / enum / bool / int /
 *    decimal / uuid_list），必填与范围也用后端下发的值 —— 前端不自己猜某个动作吃哪些键。
 * 3. **唯一键不可变**：`(trigger_entity, trigger_status, action_code)` 决定一行；编辑时三项只读，
 *    保存即覆盖（后端 `ON CONFLICT DO UPDATE`）。
 *
 * 写接口一律要 `confirm: true`（后端的二次确认硬门）：页面把它做成弹窗里的确认勾选，
 * 不允许「点保存就生效」。
 */
import { del, get, post, put } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`，前端必须写全） */
const BASE = '/api/business/flow/linkage';

// ==================== 字典（后端下发，逐字抄 admin.rs 的 DTO） ====================

/** 参数类型（`ParamKindRes.kind`） */
export type LinkageParamKindName =
  | 'text'
  | 'uuid'
  | 'enum'
  | 'bool'
  | 'int'
  | 'decimal'
  | 'uuid_list';

/** 参数类型声明（`ParamKindRes`） */
export interface LinkageParamKind {
  /** 类型：决定用哪种控件 */
  kind: LinkageParamKindName;
  /** `text` 的长度上限 */
  max_len?: number;
  /** `int` 的下界 */
  min?: number;
  /** `int` 的上界 */
  max?: number;
  /** `uuid_list` 的条数上限 */
  max_items?: number;
  /** `enum` 的候选值（逐字命中，全大写） */
  values?: string[];
}

/** 一个动作参数（`ParamRes`） */
export interface LinkageParam {
  /** 键名（落 `action_params`） */
  key: string;
  /** 类型声明 */
  kind: LinkageParamKind;
  /** 是否必填 */
  required: boolean;
  /** 人读说明 */
  note: string;
}

/** 一个白名单动作（`ActionRes`） */
export interface LinkageAction {
  /** 动作码（落 `action_code`） */
  code: string;
  /** 中文名 */
  label: string;
  /** 事务归属：`IN_TX` 同事务 / `AFTER_COMMIT` 提交后 */
  phase: 'IN_TX' | 'AFTER_COMMIT' | string;
  /** 失败等级 */
  severity: 'CRITICAL' | 'NON_CRITICAL' | string;
  /** 是否已接管（`false` = 占位，禁止配置） */
  implemented: boolean;
  /** 参数声明 */
  params: LinkageParam[];
  /** 复用的既有实现（预览里的「影响」说明） */
  reuse: string;
}

/** 一个触发点（`TriggerRes`） */
export interface LinkageTrigger {
  /** 触发单据类型（落 `trigger_entity`） */
  entity: string;
  /** 中文名 */
  label: string;
  /** 允许的触发状态 */
  statuses: string[];
}

// ==================== 规则 ====================

/** 一条联动规则（`RuleRes`） */
export interface LinkageRule {
  /** 主键 */
  id: string;
  /** 规则名 */
  name: string;
  /** 触发单据类型 */
  trigger_entity: string;
  /** 触发单据中文名 */
  trigger_entity_label: string;
  /** 触发状态 */
  trigger_status: string;
  /** 白名单动作码 */
  action_code: string;
  /** 动作中文名 */
  action_label: string;
  /** 事务归属 */
  phase: string;
  /** 失败等级 */
  severity: string;
  /** 是否已接管 */
  implemented: boolean;
  /** 动作参数 */
  action_params: Record<string, unknown>;
  /** 是否启用 */
  enabled: boolean;
  /** 执行顺序 */
  seq: number;
  /** 创建时间（RFC 3339） */
  created_at: string;
  /** 更新时间（RFC 3339） */
  updated_at: string;
  /** 自身问题（动作不在白名单 / 未接管 / 参数不合法）；空 = 健康 */
  problems: string[];
}

/** `GET /business/flow/linkage` 的响应（`RuleListRes`） */
export interface LinkageListRes {
  /** 规则（含禁用行） */
  rules: LinkageRule[];
  /** 规则条数 */
  total: number;
  /** 触发点字典 */
  triggers: LinkageTrigger[];
  /** 动作字典（白名单） */
  actions: LinkageAction[];
  /** 参数里禁止出现的键（禁配 SQL） */
  forbidden_keys: string[];
}

/** 新增 / 编辑请求体（`RuleUpsertReq`） */
export interface LinkageRuleUpsertReq {
  /** 规则名 */
  name: string;
  /** 触发单据类型 */
  trigger_entity: string;
  /** 触发状态 */
  trigger_status: string;
  /** 白名单动作码 */
  action_code: string;
  /** 动作参数（省略 = `{}`） */
  action_params?: Record<string, unknown>;
  /** 是否启用 */
  enabled?: boolean;
  /** 执行顺序 */
  seq?: number;
  /** 二次确认（**必须** `true`） */
  confirm: true;
}

/** 启停请求体（`RuleToggleReq`） */
export interface LinkageRuleToggleReq {
  /** 规则主键 */
  id: string;
  /** 目标状态 */
  enabled: boolean;
  /** 二次确认（**必须** `true`） */
  confirm: true;
}

/** 删除查询参数（`RuleDeleteQuery`） */
export interface LinkageRuleDeleteQuery {
  /** 规则主键 */
  id: string;
  /** 二次确认（**必须** `true`） */
  confirm: true;
}

// ==================== 预览影响 ====================

/** 预览里的一步（`PreviewStepRes`） */
export interface LinkagePreviewStep {
  /** 规则主键 */
  rule_id: string;
  /** 规则名 */
  name: string;
  /** 执行顺序 */
  seq: number;
  /** 动作码 */
  action_code: string;
  /** 动作中文名 */
  action_label: string;
  /** 事务归属 */
  phase: string;
  /** 失败等级 */
  severity: string;
  /** 动作参数 */
  action_params: Record<string, unknown>;
  /** 复用的既有实现 */
  effect: string;
  /** 这一行的问题 */
  problems: string[];
}

/** 草稿校验结果（`DraftCheckRes`） */
export interface LinkageDraftCheck {
  /** 校验是否通过 */
  ok: boolean;
  /** 失败原因（`ok = true` 时为空串） */
  message: string;
  /** 动作中文名 */
  action_label: string;
  /** 事务归属 */
  phase: string;
  /** 失败等级 */
  severity: string;
  /** 复用的既有实现 */
  effect: string;
  /** 同一触发点 + 同一动作是否已有存活规则（保存会覆盖它） */
  overrides_existing: boolean;
}

/** `POST /business/flow/linkage/preview` 的响应（`PreviewRes`） */
export interface LinkagePreviewRes {
  /** 触发单据类型 */
  trigger_entity: string;
  /** 触发单据中文名 */
  trigger_entity_label: string;
  /** 触发状态 */
  trigger_status: string;
  /** 该触发点当前启用的规则数 */
  enabled_count: number;
  /** 按 seq 排好的执行链 */
  steps: LinkagePreviewStep[];
  /** 是否存在「关键动作 + 同事务」 */
  rollback_risk: boolean;
  /** 提交后执行的动作数 */
  after_commit_count: number;
  /** 结论与提醒 */
  warnings: string[];
  /** 「什么时候才会跑」的说明 */
  next_run_hint: string;
  /** 草稿校验结果 */
  draft: LinkageDraftCheck | null;
}

/** 预览请求体（`PreviewReq`） */
export interface LinkagePreviewReq {
  /** 触发单据类型 */
  trigger_entity: string;
  /** 触发状态 */
  trigger_status: string;
  /** 待保存的草稿（给了就一并校验） */
  draft?: {
    /** 动作码 */
    action_code: string;
    /** 动作参数 */
    action_params?: Record<string, unknown>;
  };
}

// ==================== 函数 ====================

/**
 * 查规则 + 配置字典。
 *
 * `trigger_entity` 为空时后端返回全部规则：一次请求就能撑起「实体下拉 + 动作下拉 + 规则表」。
 * 传了未知实体 → 400（后端不静默返回空列表）。
 */
export function listLinkageRules(triggerEntity?: string): Promise<LinkageListRes> {
  const params: Record<string, unknown> = {};
  if (triggerEntity && triggerEntity.trim()) {
    params.trigger_entity = triggerEntity.trim();
  }
  return get<LinkageListRes>(BASE, params);
}

/**
 * 新增 / 编辑一条规则。
 *
 * 异常：400（二次确认缺失 / 名称空 / 未知触发点或状态 / 动作不在白名单 / 未接管 / 参数不合法）、
 * 403（非超管 / 无 manage 码）。
 */
export function saveLinkageRule(body: LinkageRuleUpsertReq): Promise<LinkageRule> {
  return put<LinkageRule>(BASE, body);
}

/** 启停一条规则（原行再 upsert，只改 `enabled`）。 */
export function toggleLinkageRule(body: LinkageRuleToggleReq): Promise<LinkageRule> {
  return put<LinkageRule>(`${BASE}/toggle`, body);
}

/** 软删一条规则（= 这条联动不再执行）。 */
export function deleteLinkageRule(query: LinkageRuleDeleteQuery): Promise<void> {
  return del<void>(BASE, { ...query });
}

/**
 * 预览一个触发点的执行链（含草稿校验）。
 *
 * 异常：400（未知触发点 / 状态不在该单据的状态集里）。
 */
export function previewLinkage(body: LinkagePreviewReq): Promise<LinkagePreviewRes> {
  return post<LinkagePreviewRes>(`${BASE}/preview`, body);
}
