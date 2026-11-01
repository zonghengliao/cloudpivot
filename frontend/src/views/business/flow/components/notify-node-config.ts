/**
 * `config.notify` 的类型 / 默认值 / 校验（P-FLOW-2 · 设计 §2.6 / §2.7 R19）.
 *
 * # 为什么校验判据放在这个文件而不是组件里
 *
 * 后端 `node_config::parse_notify_config` 才是唯一的判定方（会返回 `400 + 65034/65036`）。
 * 前端这份**只是把同一个判据提前到「刚改完就能看见」**：判据一多，写在组件里就会与
 * 后端各写一份、早晚漂移；放在纯函数里能被单测钉住，组件只负责渲染。
 *
 * 每一条的错误码与文案口径都与后端对齐：
 *
 * | 判据 | 后端 | 前端这里 |
 * |------|------|----------|
 * | `targets` 1–5 项 | `65034` | {@link notifyIssues} |
 * | `when` 三值 | `65034` | 下拉框只有三项（选不出别的） |
 * | 模板二选一 | `65034` | {@link notifyIssues} |
 * | 某来源缺必备键 | `65034` | {@link targetIssue} |
 * | `template_code` 格式 / 内联超长 | `65036` | {@link notifyIssues} |
 * | `ON_TIMEOUT` 必须有 `timeout_hours` | `65031` | 由 `NodeProps` 的「超时小时」输入框保证 |
 */

/** 触发时机（`NotifyWhen`） */
export type NotifyWhen = 'ON_ENTER' | 'ON_LEAVE' | 'ON_TIMEOUT';

/** 收件人来源（`targets[].type`，10 值枚举） */
export type NotifyTargetType =
  | 'USER'
  | 'ROLE'
  | 'DEPT_LEADER'
  | 'INITIATOR'
  | 'INITIATOR_SUPERIOR'
  | 'TASK_OWNER'
  | 'TASK_CREATOR'
  | 'TASK_ASSIGNER'
  | 'FORM_VAR'
  | 'ADMIN';

/** 通知级别（`URGENT` 落库映射成 `ERROR`，见 `node_config.rs::NotifyLevel`） */
export type NotifyLevel = 'INFO' | 'WARN' | 'URGENT';

/** 0 收件人时的策略 */
export type SkipIfEmpty = 'PASS' | 'WARN' | 'ERROR';

/** 通道（位掩码在提交时算好） */
export type NotifyChannel = 'INBOX' | 'EMAIL' | 'WEBHOOK' | 'SMS';

/** 模板来源 */
export type NotifyTemplateMode = 'code' | 'inline';

/** `targets[]` 的一项 */
export interface NotifyTarget {
  /** 来源 */
  type: NotifyTargetType;
  /** `USER`：用户 id 列表 */
  user_ids: string[];
  /** `ROLE`：角色编码列表（**不是 role id**：后端按 code 查，跨环境稳定） */
  role_codes: string[];
  /** `DEPT_LEADER`：部门 id 列表 */
  dept_ids: string[];
  /** `INITIATOR_SUPERIOR`：上溯层数 */
  levels: number;
  /** `FORM_VAR`：表单变量名 */
  form_var_key: string | null;
}

/** 面板状态（= `config.notify` 的强类型形态） */
export interface NotifyConfig {
  /** 触发时机 */
  when: NotifyWhen;
  /** 收件人（1–5 项） */
  targets: NotifyTarget[];
  /** 是否追加「上一审批节点的最后签字人」 */
  prev_approver: boolean;
  /** 模板二选一：引用还是内联 */
  template_mode: NotifyTemplateMode;
  /** 引用模式的模板编码 */
  template_code: string | null;
  /** 内联标题 */
  inline_title: string;
  /** 内联正文 */
  inline_content: string;
  /** 内联正文格式 */
  inline_format: 'TEXT' | 'HTML';
  /** 通道 */
  channels: NotifyChannel[];
  /** 级别 */
  level: NotifyLevel;
  /** 是否强制送达（跳过站内信偏好） */
  force: boolean;
  /** 0 收件人策略 */
  skip_if_empty: SkipIfEmpty;
}

/** 目标条数上限（`node_config.rs::MAX_NOTIFY_TARGETS`） */
export const MAX_NOTIFY_TARGETS = 5;
/** 内联标题上限（`node_config.rs::MAX_NOTIFY_TITLE_CHARS`） */
export const MAX_NOTIFY_TITLE_CHARS = 200;
/** 内联正文上限（`node_config.rs::MAX_NOTIFY_CONTENT_CHARS`） */
export const MAX_NOTIFY_CONTENT_CHARS = 4000;
/** 模板编码上限（`MAX_NOTIFY_TEMPLATE_CODE_CHARS`） */
export const MAX_NOTIFY_TEMPLATE_CODE_CHARS = 64;

/** 时机选项（`hint` 直接写进 UI，避免用户猜「离开时」到底是什么时候） */
export const WHEN_OPTIONS: Array<{ value: NotifyWhen; label: string; hint: string }> = [
  {
    value: 'ON_ENTER',
    label: '进入时',
    hint: '流程走到这个节点就发，节点立即通过并继续往下走（不等人）。',
  },
  {
    value: 'ON_LEAVE',
    label: '离开时',
    hint: '节点通过之后发。通知节点本身不停留，所以与「进入时」的差别只是发送时点相对节点通过的先后。',
  },
  {
    value: 'ON_TIMEOUT',
    label: '超时后',
    hint: '节点先挂起，等「超时小时」到了才发，然后继续往下走（必须同时填超时小时）。',
  },
];

/** 收件人来源选项 */
export const TARGET_TYPE_OPTIONS: Array<{ value: NotifyTargetType; label: string }> = [
  { value: 'INITIATOR', label: '发起人' },
  { value: 'USER', label: '指定用户' },
  { value: 'ROLE', label: '角色' },
  { value: 'DEPT_LEADER', label: '部门负责人' },
  { value: 'INITIATOR_SUPERIOR', label: '发起人主管' },
  { value: 'FORM_VAR', label: '表单变量（存用户 id）' },
  { value: 'TASK_OWNER', label: '任务负责人（仅任务验收流程）' },
  { value: 'TASK_CREATOR', label: '任务创建人（仅任务验收流程）' },
  { value: 'TASK_ASSIGNER', label: '任务分派人（仅任务验收流程）' },
  { value: 'ADMIN', label: '平台管理员' },
];

/** 级别选项 */
export const LEVEL_OPTIONS: Array<{ value: NotifyLevel; label: string }> = [
  { value: 'INFO', label: '普通' },
  { value: 'WARN', label: '提醒' },
  { value: 'URGENT', label: '紧急' },
];

/** 通道选项 */
export const CHANNEL_OPTIONS: Array<{ value: NotifyChannel; label: string }> = [
  { value: 'INBOX', label: '站内信' },
  { value: 'EMAIL', label: '邮件' },
  { value: 'WEBHOOK', label: 'Webhook' },
  { value: 'SMS', label: '短信' },
];

/** 0 收件人策略选项 */
export const SKIP_OPTIONS: Array<{ value: SkipIfEmpty; label: string; hint: string }> = [
  { value: 'PASS', label: '静默通过', hint: '流程继续，只记日志。' },
  { value: 'WARN', label: '通过 + 告警', hint: '流程继续，日志里留一条 WARN（默认）。' },
  { value: 'ERROR', label: '报错回滚', hint: '配置错误才这么选：解析不出收件人时整笔回滚。' },
];

/** 默认面板状态（新建通知节点时用） */
export const DEFAULT_NOTIFY_CONFIG: NotifyConfig = {
  when: 'ON_ENTER',
  targets: [{ type: 'INITIATOR', user_ids: [], role_codes: [], dept_ids: [], levels: 1, form_var_key: null }],
  prev_approver: false,
  template_mode: 'inline',
  template_code: null,
  inline_title: '{{instance.title}} 已到「{{node.name}}」',
  inline_content: '单号：{{instance.biz_no}}｜流程：{{instance.title}}｜发起人：{{initiator.name}}',
  inline_format: 'TEXT',
  channels: ['INBOX'],
  level: 'INFO',
  force: false,
  skip_if_empty: 'WARN',
};

/** 空白 target（新增一行用） */
export function emptyTarget(type: NotifyTargetType = 'INITIATOR'): NotifyTarget {
  return { type, user_ids: [], role_codes: [], dept_ids: [], levels: 1, form_var_key: null };
}

/** 从节点反解 `config.notify`（缺 / 坏 → 默认值，面板永远可编辑） */
export function notifyConfigOf(node: { config?: unknown } | null | undefined): NotifyConfig {
  const raw = (node?.config ?? null) as Record<string, unknown> | null;
  const source = (raw?.notify ?? null) as Record<string, unknown> | null;
  if (!source || typeof source !== 'object') {
    return { ...DEFAULT_NOTIFY_CONFIG, targets: DEFAULT_NOTIFY_CONFIG.targets.map((item) => ({ ...item })) };
  }
  const text = (value: unknown): string | null => {
    const out = String(value ?? '');
    return out === '' ? null : out;
  };
  const inline = (source.inline ?? null) as Record<string, unknown> | null;
  const hasCode = text(source.template_code) !== null;
  return {
    when: asWhen(source.when),
    targets:
      Array.isArray(source.targets) && source.targets.length > 0
        ? source.targets.map((item) => asTarget(item))
        : [emptyTarget()],
    prev_approver: source.prev_approver === true,
    template_mode: hasCode ? 'code' : 'inline',
    template_code: text(source.template_code),
    inline_title: String(inline?.title ?? ''),
    inline_content: String(inline?.content ?? ''),
    inline_format: String(inline?.content_format ?? 'TEXT').toUpperCase() === 'HTML' ? 'HTML' : 'TEXT',
    channels: asChannels(source.channels),
    level: asLevel(source.level),
    force: source.force === true,
    skip_if_empty: asSkip(source.skip_if_empty),
  };
}

/**
 * 把面板状态包成 `patch-node` 的载荷（只带 `config` 一个字段，撤销粒度最小）。
 *
 * # 三条「必须落成 null / 必须删键」的规矩
 *
 * 1. 模板二选一：引用模式**不带** `inline`，内联模式**不带** `template_code`
 *    （后端对「两个都给」是 65034，对「都不给」也是 65034）；
 * 2. 每种来源只带自己那个键（`USER`→`user_ids`、`ROLE`→`role_codes`…）—— 带别的键后端 65034；
 * 3. 空数组直接落成空数组而不是省略：后端对 `USER` 带 `user_ids: []` 会报「必须填」，
 *    这正是我们想在**发布前**就看见的问题，不要靠省略键糊过去。
 */
export function notifyConfigPatch(config: NotifyConfig): { config: { notify: Record<string, unknown> } } {
  const targets = config.targets.map((item) => {
    const base: Record<string, unknown> = { type: item.type };
    if (item.type === 'USER') {
      base.user_ids = item.user_ids;
    } else if (item.type === 'ROLE') {
      base.role_codes = item.role_codes;
    } else if (item.type === 'DEPT_LEADER') {
      base.dept_ids = item.dept_ids;
    } else if (item.type === 'INITIATOR_SUPERIOR') {
      base.levels = Math.min(10, Math.max(1, Number(item.levels) || 1));
    } else if (item.type === 'FORM_VAR') {
      base.form_var_key = item.form_var_key ?? '';
    }
    return base;
  });
  const notify: Record<string, unknown> = {
    when: config.when,
    targets,
    prev_approver: config.prev_approver,
    channels: config.channels.length > 0 ? config.channels : ['INBOX'],
    level: config.level,
    force: config.force,
    skip_if_empty: config.skip_if_empty,
  };
  if (config.template_mode === 'code') {
    notify.template_code = config.template_code ?? '';
  } else {
    notify.inline = {
      title: config.inline_title,
      content: config.inline_content,
      content_format: config.inline_format,
    };
  }
  return { config: { notify } };
}

/** 单个 target 的必填问题（`null` = 没问题） */
export function targetIssue(target: NotifyTarget): string | null {
  switch (target.type) {
    case 'USER':
      return target.user_ids.length === 0 ? '请至少选一个用户' : null;
    case 'ROLE':
      return target.role_codes.length === 0 ? '请至少选一个角色' : null;
    case 'DEPT_LEADER':
      return target.dept_ids.length === 0 ? '请至少选一个部门' : null;
    case 'FORM_VAR':
      return target.form_var_key && target.form_var_key.trim() !== ''
        ? null
        : '请填表单里存用户 id 的字段名';
    case 'INITIATOR_SUPERIOR':
      return target.levels >= 1 && target.levels <= 10 ? null : '主管层数必须在 1–10 之间';
    default:
      return null;
  }
}

/**
 * 整块配置的问题清单（文案与后端 `parse_notify_config` 对齐）。
 *
 * 返回 `[]` = 后端发布校验也不会因这条配置报错（模板存在性除外 —— 那要查库）。
 */
export function notifyIssues(config: NotifyConfig): string[] {
  const issues: string[] = [];
  if (config.targets.length === 0) {
    issues.push('通知给谁：至少要有一个收件人来源');
  }
  if (config.targets.length > MAX_NOTIFY_TARGETS) {
    issues.push(`收件人来源最多 ${MAX_NOTIFY_TARGETS} 个（当前 ${config.targets.length} 个）`);
  }
  config.targets.forEach((target, index) => {
    const issue = targetIssue(target);
    if (issue) {
      issues.push(`第 ${index + 1} 个收件人：${issue}`);
    }
  });
  if (config.template_mode === 'code') {
    const code = (config.template_code ?? '').trim();
    if (code === '') {
      issues.push('请选择一个通知模板');
    } else if (code.length > MAX_NOTIFY_TEMPLATE_CODE_CHARS) {
      issues.push(`模板编码最多 ${MAX_NOTIFY_TEMPLATE_CODE_CHARS} 个字符`);
    } else if (!/^[A-Z][A-Z0-9_]*$/.test(code)) {
      issues.push('模板编码只允许大写字母 / 数字 / 下划线，且以大写字母开头');
    }
  } else {
    const title = config.inline_title.trim();
    const content = config.inline_content.trim();
    if (title === '') {
      issues.push('标题不能为空');
    } else if (title.length > MAX_NOTIFY_TITLE_CHARS) {
      issues.push(`标题最多 ${MAX_NOTIFY_TITLE_CHARS} 个字符（渲染后要落 sys_notice.title）`);
    }
    if (content === '') {
      issues.push('正文不能为空');
    } else if (content.length > MAX_NOTIFY_CONTENT_CHARS) {
      issues.push(`正文最多 ${MAX_NOTIFY_CONTENT_CHARS} 个字符`);
    }
  }
  if (config.channels.length === 0) {
    issues.push('至少选一个投递通道（默认站内信）');
  }
  return issues;
}

/**
 * **样例预览**用的假数据。
 *
 * # 说清楚这不是「真渲染」
 *
 * 真正的渲染在引擎事务里做（`render_paths`，缺变量是 400），需要实例 / 表单 / 上一审批人。
 * 配置阶段这些都还不存在，所以这里用一份**标注清楚的样例**：用户看的是「模板长什么样」，
 * 不是「这条通知会发给谁」。UI 上必须写明「样例预览」四个字，不能让人误以为是真结果。
 */
export const PREVIEW_SAMPLES: Record<string, string> = {
  'instance.biz_no': 'PO202611010001',
  'instance.title': '华东金属 · 304 不锈钢板采购',
  'instance.biz_type': 'PURCHASE_ORDER',
  'initiator.name': '张三',
  'node.name': '通知发起人',
  'node.key': 'n_notify',
  'form.supplier_name': '华东金属',
  'form.amount': '125000',
};

/** 用样例数据渲染一段内联模板（未知变量显示成 `〔path〕`，**不是**空串） */
export function renderSample(template: string, samples: Record<string, string> = PREVIEW_SAMPLES): string {
  return template.replace(/\{\{\s*([A-Za-z0-9_.]+)\s*\}\}/g, (_match, path: string) => {
    const value = samples[path];
    return value === undefined ? `〔${path}〕` : value;
  });
}

/** 时机 → 人话（列表 / 摘要用） */
export function whenLabel(when: NotifyWhen): string {
  return WHEN_OPTIONS.find((item) => item.value === when)?.label ?? when;
}

/** 收件人来源 → 人话 */
export function targetTypeLabel(type: NotifyTargetType): string {
  return TARGET_TYPE_OPTIONS.find((item) => item.value === type)?.label ?? type;
}

function asWhen(value: unknown): NotifyWhen {
  const raw = String(value ?? '').trim().toUpperCase();
  return raw === 'ON_LEAVE' || raw === 'ON_TIMEOUT' ? (raw as NotifyWhen) : 'ON_ENTER';
}

function asLevel(value: unknown): NotifyLevel {
  const raw = String(value ?? '').trim().toUpperCase();
  if (raw === 'WARN') {
    return 'WARN';
  }
  if (raw === 'URGENT' || raw === 'ERROR') {
    return 'URGENT';
  }
  return 'INFO';
}

function asSkip(value: unknown): SkipIfEmpty {
  const raw = String(value ?? '').trim().toUpperCase();
  return raw === 'PASS' || raw === 'ERROR' ? (raw as SkipIfEmpty) : 'WARN';
}

function asChannels(value: unknown): NotifyChannel[] {
  if (!Array.isArray(value)) {
    return ['INBOX'];
  }
  const allowed: NotifyChannel[] = ['INBOX', 'EMAIL', 'WEBHOOK', 'SMS'];
  const out = value
    .map((item) => String(item ?? '').trim().toUpperCase() as NotifyChannel)
    .filter((item) => allowed.includes(item));
  return out.length > 0 ? out : ['INBOX'];
}

function asTarget(value: unknown): NotifyTarget {
  const raw = (value ?? {}) as Record<string, unknown>;
  const type = String(raw.type ?? '').trim().toUpperCase() as NotifyTargetType;
  const known = TARGET_TYPE_OPTIONS.some((item) => item.value === type);
  const strings = (source: unknown): string[] =>
    Array.isArray(source) ? source.map((item) => String(item ?? '')).filter((item) => item !== '') : [];
  return {
    type: known ? type : 'INITIATOR',
    user_ids: strings(raw.user_ids),
    role_codes: strings(raw.role_codes),
    dept_ids: strings(raw.dept_ids),
    levels: Math.min(10, Math.max(1, Number(raw.levels) || 1)),
    form_var_key: String(raw.form_var_key ?? '') === '' ? null : String(raw.form_var_key),
  };
}
