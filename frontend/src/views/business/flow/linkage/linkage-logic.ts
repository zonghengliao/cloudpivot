/**
 * 联动规则配置页的**纯逻辑**（卡 t_d0daa4f0 / P1-FIX-5A）。
 *
 * 拆出来的理由与其它页面一致：表单与列表里「怎么把后端下发的声明变成控件、怎么把控件值拼回
 * `action_params`、哪几行该被筛掉」全是可判定的纯函数；`.vue` 只负责画。
 * 单测见 `tests/unit/flow-linkage.test.ts`。
 *
 * # 三条硬口径（都来自后端契约，不是前端自创）
 *
 * 1. **参数键只能来自动作声明**：`buildParamPayload` 的键集合 = `action.params` 的键集合，
 *    前端不可能拼出 `sql` 这类键（后端 `ck_flow_linkage_no_sql_key` 与 `spec::FORBIDDEN_KEYS`
 *    是同一份禁令的第二、三层）。
 * 2. **必填与类型由后端声明判定**：空值 + `required` = 错误；`int` / `decimal` 走 `Number`，
 *    `bool` 走布尔，`uuid_list` 用逗号 / 空格 / 换行切分后过滤空项。
 * 3. **未接管动作不可选**：`implemented = false` 的动作后端直接 400，前端下拉置灰。
 */
import type {
  LinkageAction,
  LinkageParam,
  LinkagePreviewRes,
  LinkageRule,
  LinkageTrigger,
} from '@/api/business/flow/linkage';

/** 下拉选项（`t-select` 的形状） */
export interface LinkageOption {
  /** 展示文案 */
  label: string;
  /** 取值 */
  value: string;
  /** 是否禁用（未接管动作 / 编辑态不可变的键） */
  disabled?: boolean;
}

/** 参数表单里的单个值：文本类统一用字符串（输入框原样），布尔用布尔 */
export type ParamFormValue = string | boolean;

/** 参数表单（键 = 动作声明的 key） */
export type ParamForm = Record<string, ParamFormValue>;

/** 事务归属的中文名 */
const PHASE_LABELS: Record<string, string> = {
  IN_TX: '同事务执行',
  AFTER_COMMIT: '提交后执行',
};

/** 失败等级的中文名 */
const SEVERITY_LABELS: Record<string, string> = {
  CRITICAL: '关键（失败回滚）',
  NON_CRITICAL: '非关键（失败跳过）',
};

/** 参数类型的中文名 */
const KIND_LABELS: Record<string, string> = {
  text: '文本',
  uuid: 'UUID',
  enum: '枚举',
  bool: '布尔',
  int: '整数',
  decimal: '十进制数',
  uuid_list: 'UUID 列表',
};

/** 事务归属中文名（未知值原样返回，不吞） */
export function phaseLabel(phase: string): string {
  return PHASE_LABELS[phase] ?? (phase || '—');
}

/** 失败等级中文名（未知值原样返回，不吞） */
export function severityLabel(severity: string): string {
  return SEVERITY_LABELS[severity] ?? (severity || '—');
}

/** 参数类型中文名 */
export function kindLabel(kind: string): string {
  return KIND_LABELS[kind] ?? (kind || '—');
}

/** 事务归属的标签颜色 */
export function phaseTheme(phase: string): 'primary' | 'warning' | 'default' {
  if (phase === 'IN_TX') {
    return 'primary';
  }
  if (phase === 'AFTER_COMMIT') {
    return 'warning';
  }
  return 'default';
}

/** 失败等级的标签颜色 */
export function severityTheme(severity: string): 'danger' | 'success' | 'default' {
  if (severity === 'CRITICAL') {
    return 'danger';
  }
  if (severity === 'NON_CRITICAL') {
    return 'success';
  }
  return 'default';
}

/**
 * 触发点下拉：**未接线的触发点标注出来**（P1-FIX-5C）。
 *
 * 为什么不直接置灰：白名单 15 个触发点里当前只有一小部分真的接了线，剩下的如果一律禁选，
 * 管理员就没法为「下周要上线的接线」先备好配置。所以这里**标注 + 强制二次确认**
 * （见 `index.vue` 的保存流程与 `allow_unwired`），而不是简单禁选 —— 但**绝不允许静默保存**。
 *
 * 标注文案用「未接线 · 配了不会触发」，这是给管理员的最终结论，不是技术术语。
 */
export function triggerOptions(triggers: LinkageTrigger[]): LinkageOption[] {
  return triggers.map((trigger) => ({
    label: `${trigger.label}（${trigger.entity}）${trigger.wired ? '' : ' · 未接线'}`,
    value: trigger.entity,
    // 未接线的项**可选**（允许预置配置），但视觉上必须能分辨：文案已带「· 未接线」
  }));
}

/** 按实体取触发点声明（配置页判断「是否接线」的唯一来源） */
export function findTrigger(
  triggers: LinkageTrigger[],
  entity: string,
): LinkageTrigger | undefined {
  return triggers.find((item) => item.entity === entity);
}

/**
 * 该触发点是否**真的接线**。
 *
 * 未知实体一律 `false`（fail-closed）：查不到就当没接线，宁可多一道确认，
 * 也不能因为「字典里没有」而放一条永远不会触发的规则过去。
 */
export function isWiredEntity(triggers: LinkageTrigger[], entity: string): boolean {
  return findTrigger(triggers, entity)?.wired === true;
}

/**
 * 未接线触发点的说明文字（已接线 / 查不到 → 空串）。
 *
 * 文案直接来自后端 `TriggerSpec.wiring`：为什么没接、接的时候要动哪个文件哪个函数。
 * 前端不维护第二份解释，避免两边说法打架。
 */
export function unwiredNote(triggers: LinkageTrigger[], entity: string): string {
  const trigger = findTrigger(triggers, entity);
  if (!trigger || trigger.wired) {
    return '';
  }
  return trigger.wiring.trim();
}

/**
 * 一条规则的「未接线」问题（不含其它配置问题）。
 *
 * 后端把「触发点未接线」也塞进了 `problems`（所以 `isHealthy` 会红），但列表里
 * 这两类问题要分开显示：未接线是**接线进度**问题（配了也不会触发），
 * 其它是**这条规则自己写错了**（动作不在白名单 / 参数非法）。
 */
export function ruleWiringProblems(rule: LinkageRule, triggers: LinkageTrigger[]): string[] {
  if (isWiredEntity(triggers, rule.trigger_entity)) {
    return [];
  }
  const prefix = `触发点 ${rule.trigger_entity} 未接线`;
  return rule.problems.filter((item) => item.startsWith(prefix));
}

/** 一条规则的**其它**问题（排除「未接线」那一类，列表里分开显示） */
export function ruleOtherProblems(rule: LinkageRule, triggers: LinkageTrigger[]): string[] {
  const wiring = ruleWiringProblems(rule, triggers);
  return rule.problems.filter((item) => !wiring.includes(item));
}

/**
 * 某个实体下单个状态的**中文名**（P1-FIX-12A）。
 *
 * 字典只有后端一份（`TriggerRes.status_labels`）；查不到就原样返回编码 ——
 * 历史脏状态 / 字典里没有的值宁可显示编码，也不能显示空白或「未知」。
 */
export function statusLabelOf(
  triggers: LinkageTrigger[],
  entity: string,
  status: string,
): string {
  const trigger = triggers.find((item) => item.entity === entity);
  const hit = (trigger?.status_labels ?? []).find((item) => item.code === status);
  return hit?.label || status;
}

/**
 * 某个实体的状态下拉（后端下发的状态集，前端不自己维护状态表）。
 *
 * 文案是「中文（编码）」（P1-FIX-12A）：中文给管理员看，括号里的编码留给
 * 「日志 / 库里是哪个值」的排障场景 —— 两样都得在，否则对不上号。
 * `value` 永远是编码（落库、后端校验都认它）。
 */
export function statusOptions(triggers: LinkageTrigger[], entity: string): LinkageOption[] {
  const trigger = triggers.find((item) => item.entity === entity);
  const labelMap = new Map((trigger?.status_labels ?? []).map((item) => [item.code, item.label]));
  return (trigger?.statuses ?? []).map((status) => {
    const label = labelMap.get(status);
    return {
      label: label && label !== status ? `${label}（${status}）` : status,
      value: status,
    };
  });
}

/**
 * 列表里一行规则的**触发状态显示名**。
 *
 * 后端行级已下发 `trigger_status_label`（查不到回落编码），所以这里直接用它，
 * 不再在前端建第二份映射；老响应 / 脏数据缺字段时才退回编码。
 */
export function ruleTriggerStatusText(rule: LinkageRule): string {
  return rule.trigger_status_label || rule.trigger_status;
}

/** 已实现（`implemented = true`）的动作个数 —— 「白名单说明」里的 X */
export function implementedActionCount(actions: LinkageAction[]): number {
  return actions.filter((action) => action.implemented).length;
}

/**
 * 「目标动作（白名单）」的说明文案。
 *
 * 用户会问「动作能不能自己新增」：答案是**不能**，动作码来自后端编译期白名单
 * （`spec::ACTIONS`），新增动作 = 改代码 + 上线。能自由配的只有每个动作的参数。
 * X 取 `implemented = true` 的个数（占位动作虽在下拉里但置灰，不该算进「能用」）。
 */
export function actionWhitelistNote(actions: LinkageAction[]): string {
  return `动作由系统内置（当前 ${implementedActionCount(actions)} 个已实现），不能自行新增；如需新动作（如发短信、调外部接口）需开发。每个动作的参数可自由配置。`;
}

/**
 * 动作下拉：**未接管的动作置灰**（选了下拉也保存不了，后端 400）。
 *
 * 文案带上事务归属，让人一眼看出「这条是回滚级还是只发通知」。
 */
export function actionOptions(actions: LinkageAction[]): LinkageOption[] {
  return actions.map((action) => ({
    label: `${action.label}（${action.code} · ${phaseLabel(action.phase)}）${
      action.implemented ? '' : ' · 未接管'
    }`,
    value: action.code,
    disabled: !action.implemented,
  }));
}

/** 按动作码取声明 */
export function findAction(actions: LinkageAction[], code: string): LinkageAction | undefined {
  return actions.find((action) => action.code === code);
}

/**
 * 把一条已有规则的 `action_params` 铺进表单。
 *
 * 布尔 → `boolean`；`uuid_list` → 逗号串；其余（数字 / 文本 / 枚举）→ 字符串。
 * 声明里没有的键忽略（历史脏行不把垃圾带进表单，保存时自然被清掉）。
 */
export function paramsToForm(action: LinkageAction | undefined, params: Record<string, unknown>): ParamForm {
  const form: ParamForm = {};
  if (!action) {
    return form;
  }
  for (const param of action.params) {
    const raw = params[param.key];
    if (param.kind.kind === 'bool') {
      form[param.key] = raw === true;
      continue;
    }
    if (param.kind.kind === 'uuid_list') {
      form[param.key] = Array.isArray(raw) ? raw.map((item) => String(item)).join(', ') : '';
      continue;
    }
    form[param.key] = raw === undefined || raw === null ? '' : String(raw);
  }
  return form;
}

/** 空表单（新建规则 / 切换动作时用） */
export function emptyForm(action: LinkageAction | undefined): ParamForm {
  return paramsToForm(action, {});
}

/** 参数表单的校验与拼装结果 */
export interface ParamBuildResult {
  /** 拼好的 `action_params`（有错误时只含合法项，调用方不该提交） */
  payload: Record<string, unknown>;
  /** 错误（中文，直接展示；空 = 通过） */
  errors: string[];
}

/**
 * 表单 → `action_params`。
 *
 * - 必填项为空 → 错误（后端同样会 400，这里提前拦是为了少一次往返）；
 * - 可选空项**不写进 payload**（后端 `validate_params` 对可选键缺省是放行的；
 *   写 `""` 反而可能触发布类型错误）；
 * - 数字类用 `Number`，非数字 → 错误；
 * - `int` 额外判整数与区间（区间来自后端声明）。
 */
export function buildParamPayload(action: LinkageAction | undefined, form: ParamForm): ParamBuildResult {
  const payload: Record<string, unknown> = {};
  const errors: string[] = [];
  if (!action) {
    return { payload, errors };
  }
  for (const param of action.params) {
    const value = form[param.key];
    if (param.kind.kind === 'bool') {
      // 布尔永远有值（false 也是值）：显式写进 payload
      payload[param.key] = value === true;
      continue;
    }
    const text = value === undefined ? '' : String(value).trim();
    if (text === '') {
      if (param.required) {
        errors.push(`参数「${param.key}」（${param.note}）必填`);
      }
      continue;
    }
    const converted = convertParam(param, text, errors);
    if (converted !== undefined) {
      payload[param.key] = converted;
    }
  }
  return { payload, errors };
}

/** 单个参数的类型转换（失败 push 错误并返回 undefined） */
function convertParam(param: LinkageParam, text: string, errors: string[]): unknown {
  switch (param.kind.kind) {
    case 'int':
    case 'decimal': {
      const value = Number(text);
      if (!Number.isFinite(value)) {
        errors.push(`参数「${param.key}」必须是数字（当前「${text}」）`);
        return undefined;
      }
      if (param.kind.kind === 'int' && !Number.isInteger(value)) {
        errors.push(`参数「${param.key}」必须是整数（当前「${text}」）`);
        return undefined;
      }
      const min = param.kind.min;
      const max = param.kind.max;
      if (param.kind.kind === 'int') {
        if (min !== undefined && value < min) {
          errors.push(`参数「${param.key}」不能小于 ${min}`);
          return undefined;
        }
        if (max !== undefined && value > max) {
          errors.push(`参数「${param.key}」不能大于 ${max}`);
          return undefined;
        }
      }
      return value;
    }
    case 'enum':
      if (param.kind.values && param.kind.values.length > 0 && !param.kind.values.includes(text)) {
        errors.push(`参数「${param.key}」必须是 ${param.kind.values.join(' / ')} 之一（当前「${text}」）`);
        return undefined;
      }
      return text;
    case 'uuid_list': {
      const items = text
        .split(/[,\s]+/)
        .map((item) => item.trim())
        .filter((item) => item !== '');
      if (param.kind.max_items !== undefined && items.length > param.kind.max_items) {
        errors.push(`参数「${param.key}」最多 ${param.kind.max_items} 项（当前 ${items.length} 项）`);
        return undefined;
      }
      return items;
    }
    case 'text':
      if (param.kind.max_len !== undefined && text.length > param.kind.max_len) {
        errors.push(`参数「${param.key}」最长 ${param.kind.max_len} 个字符（当前 ${text.length}）`);
        return undefined;
      }
      return text;
    case 'uuid':
    case 'bool':
    default:
      return text;
  }
}

/** 参数摘要（列表页的一列：`biz_type=STOCK_IN, mode=SHADOW`；空 = `—`） */
export function formatParams(params: Record<string, unknown>): string {
  const entries = Object.entries(params).filter(([, value]) => {
    if (value === null || value === undefined || value === '') {
      return false;
    }
    return !(Array.isArray(value) && value.length === 0);
  });
  if (entries.length === 0) {
    return '—';
  }
  return entries
    .map(([key, value]) => `${key}=${Array.isArray(value) ? value.join('|') : String(value)}`)
    .join(', ');
}

/** 列表筛选条件 */
export interface RuleFilter {
  /** 源单据类型（空 = 全部） */
  entity: string;
  /** 关键词（规则名 / 动作码 / 动作中文名，大小写不敏感） */
  keyword: string;
  /** 只看启用的 */
  onlyEnabled: boolean;
}

/** 列表筛选（纯前端：规则总量小，配置页不需要后端分页） */
export function filterRules(rules: LinkageRule[], filter: RuleFilter): LinkageRule[] {
  const keyword = filter.keyword.trim().toLowerCase();
  return rules.filter((rule) => {
    if (filter.entity && rule.trigger_entity !== filter.entity) {
      return false;
    }
    if (filter.onlyEnabled && !rule.enabled) {
      return false;
    }
    if (!keyword) {
      return true;
    }
    const haystack = [rule.name, rule.action_code, rule.action_label, rule.trigger_status]
      .join(' ')
      .toLowerCase();
    return haystack.includes(keyword);
  });
}

/** 预览弹窗里的提醒分组：把后端的 `warnings` 与「执行链为空」合成一份给人看的清单 */
export function previewHighlights(preview: LinkagePreviewRes | null): string[] {
  if (!preview) {
    return [];
  }
  const list = [...preview.warnings];
  if (preview.rollback_risk) {
    list.push('有回滚风险：同事务的关键动作失败会让整笔状态变更回滚');
  }
  if (preview.steps.length > 0 && preview.after_commit_count === preview.steps.length) {
    list.push('全部动作都在提交后执行：失败不会影响单据本身');
  }
  return list;
}

/**
 * 预览的结论主题：**未接线优先于一切**（P1-FIX-5C）。
 *
 * 未接线时即使草稿校验通过，也必须用 `warning`/`error` 的颜色说「不生效」，
 * 否则「可以通过校验」的绿条会盖过真正重要的结论。
 */
export function previewAlertTheme(preview: LinkagePreviewRes | null): 'success' | 'warning' | 'info' {
  if (!preview) {
    return 'info';
  }
  return preview.trigger_wired ? 'success' : 'warning';
}

/** 草稿校验结果 → 弹窗里的 `t-alert` 主题 */
export function draftAlertTheme(draft: { ok: boolean } | null): 'success' | 'error' | 'info' {
  if (!draft) {
    return 'info';
  }
  return draft.ok ? 'success' : 'error';
}

/** 规则是否健康（无 problems） */
export function isHealthy(rule: LinkageRule): boolean {
  return rule.problems.length === 0;
}

/** 时间展示（`2026-11-05 19:30`；解析失败原样返回） */
export function formatDateTime(raw: string): string {
  if (!raw) {
    return '—';
  }
  const date = new Date(raw);
  if (Number.isNaN(date.getTime())) {
    return raw;
  }
  const pad = (value: number): string => String(value).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(
    date.getHours(),
  )}:${pad(date.getMinutes())}`;
}
