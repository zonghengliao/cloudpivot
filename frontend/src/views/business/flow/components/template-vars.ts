/**
 * 模板变量：白名单 + 占位符解析 + 插入（P-FLOW-1C · 设计 §4.2 / §2.5）。
 *
 * # 为什么是纯逻辑文件
 *
 * 三处要**同一份判据**：输入框下方的实时标红（`TaskNodeConfig.vue`）、
 * 变量选择器列哪些项（`TemplateVarPicker.vue`）、单测（`tests/unit/template-vars.test.ts`）。
 * 判据写在 `.vue` 里就没法单测，也必然漂移 —— 所以全部收在这里，组件只做渲染。
 *
 * # 与后端逐字对齐的三个常量
 *
 * | 前端 | 后端 | 含义 |
 * |---|---|---|
 * | {@link FIXED_FIELDS}（10） | `graph.rs::FIXED_FIELDS` | 条件 DSL 与模板共用的固定字段 |
 * | {@link TEMPLATE_ONLY_FIELDS}（4） | `graph.rs::TEMPLATE_ONLY_FIELDS` | **只给模板**用的 4 个根 |
 * | {@link MAX_VAR_SEGMENT}（64） | `notice/render.rs::MAX_VAR_NAME_LEN` | 单个路径段字符上限 |
 *
 * 单测直接 `readFileSync` 读 `.rs` 源码比对，后端改一个字段这里先红。
 *
 * # 两套插入文本，按模式切换（设计 §4.2 第 3 条，红线）
 *
 * - 内联模板 / `TASK_NODE` 标题 → 插入**点号**路径 `{{instance.biz_no}}`（走后端 `render_paths`）；
 * - 「引用 `template_code`」模式 → 选择器**根本不显示**（那些 `sys_notice_template.variables`
 *   是无点号名，走旧 `render`，插点号会 422）。这条约束由
 *   {@link varGroups} 的调用方（`TemplateVarPicker.vue`）用 `mode` 决定，本文件不提供点号→下划线的静默转换。
 *
 * # `task.*` 的可用条件
 *
 * `EvalContext.task` 只在 `biz_type = 'TASK_ACCEPT'` 时有值（后端 `graph.rs:1729` 注释），
 * 所以非 `TASK_ACCEPT` 流程里：**选择器不显示这一组**（{@link varGroups}）
 * **且校验判非法**（{@link validateTemplateVars}）—— 两处都要，只做一处等于没做。
 */

/** 固定字段白名单（逐字复制自 `backend/src/business/flow/graph.rs::FIXED_FIELDS`）。 */
export const FIXED_FIELDS = [
  'initiator.id',
  'initiator.name',
  'initiator.dept_id',
  'instance.biz_type',
  'instance.biz_no',
  'instance.title',
  'task.status',
  'task.priority',
  'task.estimate_hours',
  'task.title',
] as const;

/** 固定字段的字面量类型 */
export type FixedField = (typeof FIXED_FIELDS)[number];

/**
 * 只给**模板**用的 4 个字段根（`graph.rs::TEMPLATE_ONLY_FIELDS`）。
 *
 * 条件 DSL 不认它们：条件的求值时机是「我该走哪条边」，此时当前节点还没结束
 * （`node.name` 变成自我指涉），`prev.approver` 在第一个 START 之后根本不存在。
 */
export const TEMPLATE_ONLY_FIELDS = [
  'node.name',
  'node.key',
  'prev.approver_name',
  'prev.approver_id',
] as const;

/** 单个路径段字符上限（`notice/render.rs::MAX_VAR_NAME_LEN`）。 */
export const MAX_VAR_SEGMENT = 64;

/** 任务验收流程的业务类型 —— `task.*` 只在这类流程里有值。 */
export const TASK_ACCEPT_BIZ_TYPE = 'TASK_ACCEPT';

/** `form.*` 的活动字段名文法（`graph::field_allowed` 的 rest 部分）。 */
const FORM_FIELD_GRAMMAR = /^[A-Za-z_][A-Za-z0-9_]*$/;

/** 一条校验结论 */
export interface VarIssue {
  /** 出问题的路径（语法错时是原文片段） */
  path: string;
  /** 中文原因（**不阻止输入，只阻止保存**） */
  reason: string;
  /** 关联的后端错误码：`65036` 语法 / `65037` 白名单（UI 标码用） */
  code: number;
}

/** 模板里的一个占位符（`start`/`end` 是**含花括号**的原文区间） */
export interface Placeholder {
  /** 变量路径（已 trim） */
  path: string;
  /** 起始下标（`{{` 的第一个 `{`） */
  start: number;
  /** 结束下标（`}}` 之后一位） */
  end: number;
  /** 原文片段（含花括号与可能的空格） */
  raw: string;
}

/** `{{path}}` 的扫描结果 */
export interface ScanResult {
  /** 全部占位符（按出现顺序） */
  placeholders: Placeholder[];
  /** 语法问题（未闭合 / 变量名非法 / 多于一层点号） */
  syntax: VarIssue[];
}

/**
 * 单个路径段的文法检查（与后端 `render.rs::validate_var_segment` 同口径）。
 *
 * 返回中文原因；合法返回 `null`。
 */
function segmentError(segment: string, full: string, index: number): string | null {
  const which = index === 0 ? '根' : '叶';
  if (segment.length === 0) {
    return `占位符变量名非法：${full}（${which}段为空，点号两侧都不能为空）`;
  }
  if (segment.length > MAX_VAR_SEGMENT) {
    return `占位符变量名超过 ${MAX_VAR_SEGMENT} 个字符（${which}段「${segment.slice(0, 12)}…」有 ${segment.length} 个）`;
  }
  const first = segment[0] ?? '';
  if (!/^[A-Za-z_]$/.test(first)) {
    return `占位符变量名非法：${full}（${which}段首字符必须是字母或下划线）`;
  }
  if (!/^[A-Za-z0-9_]+$/.test(segment)) {
    return `占位符变量名非法：${full}（只允许字母、数字、下划线与一个点号；不支持表达式 / 过滤器）`;
  }
  return null;
}

/**
 * 变量路径的文法检查（**只看拼写**，不看白名单）。
 *
 * 口径与后端 `notice/render.rs::validate_var_path` 逐条一致：
 * 最多一个点号、每段 `[A-Za-z_][A-Za-z0-9_]{0,63}`。
 *
 * 返回中文原因；合法返回 `null`。
 */
export function pathGrammarError(path: string): string | null {
  const value = (path ?? '').trim();
  if (value.length === 0) {
    return '占位符是空的（写成 {{instance.biz_no}} 这样）';
  }
  const parts = value.split('.');
  if (parts.length > 2) {
    return `占位符变量名非法：${value}（最多允许一层点号，如 form.amount）`;
  }
  for (let index = 0; index < parts.length; index += 1) {
    const error = segmentError(parts[index] ?? '', value, index);
    if (error) {
      return error;
    }
  }
  return null;
}

/**
 * 扫描模板里的全部 `{{path}}`（单趟、不递归，与 `collect_placeholders_paths` 同语法）。
 *
 * 未闭合的 `{{` 之后的内容**不再解析**（后端的 `?` 也是直接返回错误），
 * 所以模板里出现一个未闭合占位符时只报一条。
 */
export function scanPlaceholders(template: string): ScanResult {
  const source = template ?? '';
  const placeholders: Placeholder[] = [];
  const syntax: VarIssue[] = [];
  let cursor = 0;
  while (cursor < source.length) {
    const open = source.indexOf('{{', cursor);
    if (open < 0) {
      break;
    }
    const close = source.indexOf('}}', open + 2);
    if (close < 0) {
      syntax.push({
        path: source.slice(open),
        reason: `占位符缺少收尾的 }}（第 ${open + 1} 字符处）`,
        code: 65036,
      });
      break;
    }
    const raw = source.slice(open, close + 2);
    const path = source.slice(open + 2, close).trim();
    const error = pathGrammarError(path);
    if (error) {
      syntax.push({ path: path || raw, reason: error, code: 65036 });
    } else {
      placeholders.push({ path, start: open, end: close + 2, raw });
    }
    cursor = close + 2;
  }
  return { placeholders, syntax };
}

/** 抽取模板里的全部变量路径（去重，保持出现顺序）。 */
export function extractPlaceholders(template: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const item of scanPlaceholders(template).placeholders) {
    if (!seen.has(item.path)) {
      seen.add(item.path);
      out.push(item.path);
    }
  }
  return out;
}

/** 是否 `TASK_ACCEPT` 流程（大小写 / 前后空白不敏感，与后端 `eq_ignore_ascii_case` 同口径）。 */
export function isTaskAccept(bizType: string | null | undefined): boolean {
  return (bizType ?? '').trim().toUpperCase() === TASK_ACCEPT_BIZ_TYPE;
}

/** 路径是否在模板白名单里（**只看拼写与名单，不看 `task.*` 的 biz_type 开关**）。 */
export function isTemplateFieldAllowed(path: string): boolean {
  const value = (path ?? '').trim();
  if ((FIXED_FIELDS as readonly string[]).includes(value)) {
    return true;
  }
  if ((TEMPLATE_ONLY_FIELDS as readonly string[]).includes(value)) {
    return true;
  }
  if (!value.startsWith('form.')) {
    return false;
  }
  const rest = value.slice('form.'.length);
  return FORM_FIELD_GRAMMAR.test(rest) && rest.length <= MAX_VAR_SEGMENT;
}

/**
 * 校验一份模板：语法 → 白名单 → `task.*` 的 biz_type 开关。
 *
 * 与后端两处判据逐条对齐：
 * - `graph.rs::template_issues`（发布期 R21）
 * - `flow/handler.rs::task_preview` 的 ④ 段（预演期）
 *
 * 返回**逐条**问题（空数组 = 通过）。**不阻止输入，只阻止保存**（设计 §4.2 第 4 条）。
 */
export function validateTemplateVars(template: string, bizType: string | null | undefined): VarIssue[] {
  const scan = scanPlaceholders(template);
  const issues: VarIssue[] = [...scan.syntax];
  const taskAccept = isTaskAccept(bizType);
  for (const item of scan.placeholders) {
    if (!isTemplateFieldAllowed(item.path)) {
      issues.push({
        path: item.path,
        reason: `{{${item.path}}} 不在白名单（允许 form.* / instance.* / initiator.* / node.* / prev.* / task.*）`,
        code: 65037,
      });
      continue;
    }
    if (item.path.startsWith('task.') && !taskAccept) {
      issues.push({
        path: item.path,
        reason: `{{${item.path}}} 只在 TASK_ACCEPT 流程里有值，本流程业务类型是 ${
          (bizType ?? '').trim() || '（未知）'
        }（运行期会渲染成「缺变量」）`,
        code: 65037,
      });
    }
  }
  return issues;
}

/**
 * 在文本的 `[start, end)` 处插入一个 `{{path}}`，返回新文本与**插入后光标应落的位置**。
 *
 * # 三条验收口径（设计 §4.2 第 1 条）
 *
 * 1. 光标在中间 → 插在光标处，光标落在 `}}` **之后**（`caret = start + token.length`）；
 * 2. 有选中文字 → **替换**选中段（`text.slice(0, start) + token + text.slice(end)`）；
 * 3. 越界的选区（组件失焦后拿到的旧下标）夹回文本长度，绝不抛错。
 */
export function insertPlaceholder(
  text: string,
  path: string,
  start: number,
  end: number,
): { text: string; caret: number } {
  const source = text ?? '';
  const token = `{{${(path ?? '').trim()}}}`;
  const size = source.length;
  const from = Math.min(Math.max(0, Number.isFinite(start) ? start : size), size);
  const to = Math.min(Math.max(from, Number.isFinite(end) ? end : from), size);
  return {
    text: `${source.slice(0, from)}${token}${source.slice(to)}`,
    caret: from + token.length,
  };
}

/** 变量选择器里的一项 */
export interface VarOption {
  /** 中文别名（设计 §2.5.1 的映射表） */
  label: string;
  /** 真实路径（插入的文本就是 `{{path}}`） */
  path: string;
}

/** 变量选择器里的一组 */
export interface VarGroup {
  /** 稳定 key（`t-collapse` / 测试定位用） */
  key: string;
  /** 组名 */
  label: string;
  /** 组内选项 */
  options: VarOption[];
}

/**
 * 变量选择器的分组菜单（设计 §4.2 的树）。
 *
 * # 参数
 * - `bizType`：本流程业务类型 —— **只有 `TASK_ACCEPT` 才给 `task.*` 组**；
 * - `formFields`：`form.*` 的候选字段名（**只放真实见过的**，没数据就是空数组，
 *   由组件显示「可手输」而不是伪造候选）；
 * - `includeTaskGroup`：强制开关（默认按 `bizType` 判，测试用）。
 */
export function varGroups(options: {
  bizType?: string | null;
  formFields?: string[];
  includeTaskGroup?: boolean;
} = {}): VarGroup[] {
  const bizType = options.bizType ?? null;
  const formFields = (options.formFields ?? [])
    .map((field) => (field ?? '').trim())
    .filter((field) => FORM_FIELD_GRAMMAR.test(field))
    .filter((field, index, all) => all.indexOf(field) === index);
  const groups: VarGroup[] = [
    {
      key: 'instance',
      label: '业务单据',
      options: [
        { label: '业务单号', path: 'instance.biz_no' },
        { label: '业务类型', path: 'instance.biz_type' },
        { label: '流程标题', path: 'instance.title' },
      ],
    },
    {
      key: 'initiator',
      label: '发起人',
      options: [
        { label: '姓名', path: 'initiator.name' },
        { label: '用户 ID', path: 'initiator.id' },
        { label: '部门 ID', path: 'initiator.dept_id' },
      ],
    },
    {
      key: 'node',
      label: '本节点',
      options: [
        { label: '节点名', path: 'node.name' },
        { label: '节点标识', path: 'node.key' },
      ],
    },
    {
      key: 'prev',
      label: '上一审批人',
      options: [
        { label: '姓名', path: 'prev.approver_name' },
        { label: '用户 ID', path: 'prev.approver_id' },
      ],
    },
    {
      key: 'form',
      label: '表单字段',
      options: formFields.map((field) => ({ label: field, path: `form.${field}` })),
    },
  ];
  if (options.includeTaskGroup ?? isTaskAccept(bizType)) {
    groups.push({
      key: 'task',
      label: '任务字段（仅任务验收流程）',
      options: [
        { label: '任务标题', path: 'task.title' },
        { label: '任务状态', path: 'task.status' },
        { label: '优先级', path: 'task.priority' },
        { label: '预估工时', path: 'task.estimate_hours' },
      ],
    });
  }
  return groups;
}
