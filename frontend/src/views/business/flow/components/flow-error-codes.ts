/**
 * 后端错误码 / 文案的**唯一前端副本**（P1-B3 · 错误码对齐）。
 *
 * # 为什么要有这个文件
 *
 * 卡面要求：前端提示文案必须与后端 rust 里的 `msg` 字段**逐字一致**。
 * 做法不是「抄一遍然后祈祷别改」，而是把后端 `format!` 的**模板原文**（连同
 * `{MAX_CONDITION_DEPTH}` 这种常量占位、`{reason}` 这种具名占位）整段搬过来，
 * 再用 {@link fill} 做与 `format!` 同构的替换 —— 这样这里看到的字符串就是
 * `graph.rs` / `dto.rs` / `expression.rs` 里的那一段，一个字符都不改。
 *
 * `frontend/tests/unit/rule-validation.test.ts` 会**直接读后端 .rs 源码**，断言：
 * - 模板在本文件里 → 在 .rs 里也存在（`"<模板原文>"` 逐字命中）；
 * - 码值 / 上限 / `FIXED_FIELDS` 与后端常量逐项相等。
 * 后端改一个字、改一个数，前端单测先红。
 *
 * # 卡面口径 vs 后端真实分工（重要，别照卡面硬编）
 *
 * 卡面写「65016（条件树结构非法）/ 65033（then 目标非法）」，
 * 但后端实际分工是（`dto::validate_expression_nodes` + `graph.rs::expression_issues` R16）：
 *
 * | 情形 | 真实码 | 文案模板 |
 * |---|---|---|
 * | 条件树结构 / 上限 / 字段白名单 | `65016` | `边 {}（{} → {}）的条件表达式非法：{reason}` |
 * | 表达式（含 then 目标）结构非法 | `65016` | `节点 {}（{}）的审批人表达式不合法：{reason}` |
 * | 表达式引用了保留词 `PREV_APPROVER` | `65033` | `节点 {}（{}）的审批人表达式用了「上一节点的审批人」…` |
 *
 * 也就是说 **then 目标为空 / 超 5 个 / 来源不支持 落在 `65016`**，
 * `65033` 本期只有 `PREV_APPROVER` 一个场景（D-E8：发布期必须拒，不许「存进去、跑起来才 501」）。
 * 按卡面把 then 目标写成 65033 会让前端提示的码与后端真实返回的码不一致 —— 那正是本卡要消灭的错位。
 */

// ==================== 码值（`backend/src/shared/error_code.rs`） ====================

/** 条件表达式 / 审批人表达式**结构**非法（`error_code.rs::FLOW_CONDITION_INVALID`）。 */
export const FLOW_CONDITION_INVALID = 65016;
/** 审批人表达式不合法 —— 本期专用于保留词 `PREV_APPROVER`（`FLOW_EXPRESSION_INVALID`）。 */
export const FLOW_EXPRESSION_INVALID = 65033;

/** 本文件对齐的两种码（单测断言用）。 */
export const ALIGNED_CODES = [FLOW_CONDITION_INVALID, FLOW_EXPRESSION_INVALID] as const;

// ==================== 上限（`graph.rs` / `expression.rs`） ====================

/** `graph.rs::MAX_CONDITION_DEPTH`：条件树深上限。 */
export const MAX_CONDITION_DEPTH = 8;
/** `graph.rs::MAX_CONDITION_NODES`：条件树节点总数上限。 */
export const MAX_CONDITION_NODES = 64;
/** `graph.rs::MAX_CONDITION_ARGS`：单个 and/or 的 args 上限。 */
export const MAX_CONDITION_ARGS = 16;
/** `graph.rs::MAX_LITERAL_CHARS`：字符串字面量字符数上限。 */
export const MAX_LITERAL_CHARS = 200;
/** `graph.rs::MAX_LITERAL_ARRAY`：数组字面量项数上限。 */
export const MAX_LITERAL_ARRAY = 50;
/** `expression.rs::MAX_EXPR_RULES`：一个配置单里最多几条规则。 */
export const MAX_EXPR_RULES = 16;
/** `expression.rs::MAX_EXPR_TARGETS`：单条规则 then 目标数上限。 */
export const MAX_EXPR_TARGETS = 5;
/** `expression.rs::RESERVED_PREV_APPROVER`：本期不开放、必须在发布期拒掉的保留词。 */
export const RESERVED_PREV_APPROVER = 'PREV_APPROVER';

/**
 * 规则（分支）数上限 —— **纯前端闸**，后端没有对应常量。
 *
 * 卡面 §一① 要求「规则（分支）数 ≤ 32」。后端的 WHEN 子树只限深度 8 / 节点 64 / 每组 args 16，
 * 没有「叶子条数」这一条；前端加 32 是**比后端更严**的编辑期提示
 * （32 个叶子 + 分组 ≤ 64 节点，两个闸同一量级），因此**永远不会**造出一条「前端放行、后端 400」的表达式。
 * 反过来说：它命中时给不出后端文案（后端不会这么说），所以这类 issue 的 `code` 是 `null`。
 */
export const MAX_RULES = 32;

// ==================== 文案模板（逐字复制自 .rs，勿改） ====================

/**
 * 模板原文。**这里的字符串必须与 backend .rs 里的字面量一字不差**
 * （`{CONST}` = rust 的内联常量占位，`{}` = 顺序参数，`{name}` = 具名参数）。
 */
export const RUST_TEMPLATES = {
  /** `graph.rs` R10：边上条件非法的 65016 外壳。 */
  edgeCondition: '边 {}（{} → {}）的条件表达式非法：{reason}',
  /** `dto.rs::validate_expression_nodes` + `graph.rs` R16：审批人表达式非法的 65016 外壳。 */
  nodeExpression: '节点 {}（{}）的审批人表达式不合法：{reason}',
  /** `graph.rs` R16：保留词的 65033 外壳。 */
  prevApprover: '节点 {}（{}）的审批人表达式用了「上一节点的审批人」（PREV_APPROVER），本版本未开放（P2），请先改用其它来源',
  /** `assignee.rs::expression_plans` 运行期的 65033（库被直接改过时的最后一道）。 */
  prevApproverRuntime: '审批人表达式引用了「上一节点的审批人」（PREV_APPROVER），本版本未开放（P2），请先改用其它来源',
  /** `graph.rs::parse_condition_inner`：深度超限。 */
  reasonDepth: '条件嵌套层数超过 {MAX_CONDITION_DEPTH} 层（请把条件拆成几条边）',
  /** `graph.rs::parse_condition_inner`：节点数超限。 */
  reasonNodes: '条件表达式节点数超过 {MAX_CONDITION_NODES} 个（请把条件拆成几条边）',
  /** `graph.rs::parse_condition_inner`：args 超限。 */
  reasonArgs: '{op} 的 args 最多 {MAX_CONDITION_ARGS} 项',
  /** `graph.rs::parse_condition_inner`：字段不在白名单。 */
  reasonField: '字段 {field} 不在白名单内（允许 form.*、initiator.id/name/dept_id、instance.biz_type/biz_no/title、task.status/priority/estimate_hours）',
  /** `graph.rs::parse_condition_inner`：and/or 的 args 为空。 */
  reasonArgsEmpty: '{op} 的 args 不能为空',
  /** `graph.rs::parse_condition_inner`：not 的子节点数不对。 */
  reasonNotArgs: 'not 的 args 必须恰好 1 项',
  /** `graph.rs::parse_condition_inner`：cmp 缺字段。 */
  reasonFieldMissing: 'cmp 节点缺少 field',
  /** `graph.rs::parse_condition_inner`：in / nin 的右值不是数组。 */
  reasonArrayValue: '{} 的 value 必须是数组',
  /** `graph.rs::check_literal`：字符串字面量超长。 */
  reasonLiteralChars: '字符串字面量超过 {MAX_LITERAL_CHARS} 个字符',
  /** `graph.rs::check_literal`：数组字面量超长。 */
  reasonLiteralArray: '数组字面量最多 {MAX_LITERAL_ARRAY} 项',
  /** `expression.rs::parse_targets`：then 目标为空。 */
  reasonTargetEmpty: '{where_desc}的审批人不能为空（至少 1 个）',
  /** `expression.rs::parse_targets`：then 目标超限。 */
  reasonTargetsOver: '{where_desc}的审批人数超过 {MAX_EXPR_TARGETS} 个（当前 {} 个）',
  /** `expression.rs::parse_expression`：规则条数超限。 */
  reasonRulesOver: '规则数超过 {MAX_EXPR_RULES} 条（当前 {} 条）',
} as const;

/** 模板名 → 该模板所在的 .rs 文件（单测按这个清单去读源码比对）。 */
export const TEMPLATE_SOURCES: Record<keyof typeof RUST_TEMPLATES, string[]> = {
  edgeCondition: ['graph.rs'],
  nodeExpression: ['graph.rs', 'dto.rs'],
  prevApprover: ['graph.rs'],
  prevApproverRuntime: ['assignee.rs'],
  reasonDepth: ['graph.rs'],
  reasonNodes: ['graph.rs'],
  reasonArgs: ['graph.rs'],
  reasonField: ['graph.rs'],
  reasonArgsEmpty: ['graph.rs'],
  reasonNotArgs: ['graph.rs'],
  reasonFieldMissing: ['graph.rs'],
  reasonArrayValue: ['graph.rs'],
  reasonLiteralChars: ['graph.rs'],
  reasonLiteralArray: ['graph.rs'],
  reasonTargetEmpty: ['expression.rs'],
  reasonTargetsOver: ['expression.rs'],
  reasonRulesOver: ['expression.rs'],
};

// ==================== 与 `format!` 同构的替换 ====================

/** 具名占位（`{reason}` / `{op}` / `{MAX_CONDITION_DEPTH}` …）的取值来源。 */
export type TemplateScope = Record<string, string | number>;

/** 一个占位符：`{}`（顺序）或 `{name}`（具名）。 */
const PLACEHOLDER = /\{([A-Za-z_][A-Za-z0-9_]*)?\}/g;

/**
 * 按 rust `format!` 的语义填模板：`{}` 吃 `positional` 的下一个，`{name}` 从 `scope` 取。
 *
 * 与 rust 一样，**占位缺失就抛错**（而不是留个 `{reason}` 在界面上）——
 * 少填参数在 rust 里是编译错误，在这里就应该是测试失败。
 */
export function fill(
  template: string,
  scope: TemplateScope,
  positional: Array<string | number> = [],
): string {
  let cursor = 0;
  return template.replace(PLACEHOLDER, (_whole, name?: string) => {
    if (!name) {
      if (cursor >= positional.length) {
        throw new Error(`模板「${template}」的顺序参数不够（需要 ${cursor + 1} 个）`);
      }
      return String(positional[cursor++]);
    }
    if (!(name in scope)) {
      throw new Error(`模板「${template}」缺少具名参数 ${name}`);
    }
    return String(scope[name]);
  });
}

/** 模板用到的常量，集中一处，测试会拿它和 .rs 常量比对。 */
const CONSTS: TemplateScope = {
  MAX_CONDITION_DEPTH,
  MAX_CONDITION_NODES,
  MAX_CONDITION_ARGS,
  MAX_LITERAL_CHARS,
  MAX_LITERAL_ARRAY,
  MAX_EXPR_RULES,
  MAX_EXPR_TARGETS,
};

// ==================== 现成的取用函数（页面直接用这些） ====================

/** 边的条件树结构非法 → `65016` 全文（与后端 R10 同文）。 */
export function edgeConditionMessage(
  edge: { key: string; from: string; to: string },
  reason: string,
): string {
  return fill(RUST_TEMPLATES.edgeCondition, { ...CONSTS, reason }, [edge.key, edge.from, edge.to]);
}

/** 节点审批人表达式非法 → `65016` 全文（与后端保存期 / 发布期 R16 同文）。 */
export function nodeExpressionMessage(
  node: { key: string; name: string },
  reason: string,
): string {
  return fill(RUST_TEMPLATES.nodeExpression, { ...CONSTS, reason }, [node.key, node.name]);
}

/** 引用了 `PREV_APPROVER` → `65033` 全文（与后端 R16 同文）。 */
export function prevApproverMessage(node: { key: string; name: string }): string {
  return fill(RUST_TEMPLATES.prevApprover, CONSTS, [node.key, node.name]);
}

/** 深度超限原因（`parse_condition_inner` 原文）。 */
export function reasonDepth(): string {
  return fill(RUST_TEMPLATES.reasonDepth, CONSTS);
}

/** 节点数超限原因。 */
export function reasonNodes(): string {
  return fill(RUST_TEMPLATES.reasonNodes, CONSTS);
}

/** 单个分组的 args 超限原因。 */
export function reasonArgs(op: string): string {
  return fill(RUST_TEMPLATES.reasonArgs, { ...CONSTS, op });
}

/** 字段不在白名单原因。 */
export function reasonField(field: string): string {
  return fill(RUST_TEMPLATES.reasonField, { ...CONSTS, field });
}

/** and/or 的 args 为空原因。 */
export function reasonArgsEmpty(op: string): string {
  return fill(RUST_TEMPLATES.reasonArgsEmpty, { ...CONSTS, op });
}

/** `not` 的子节点数不对原因。 */
export function reasonNotArgs(): string {
  return fill(RUST_TEMPLATES.reasonNotArgs, CONSTS);
}

/** `cmp` 缺字段原因。 */
export function reasonFieldMissing(): string {
  return fill(RUST_TEMPLATES.reasonFieldMissing, CONSTS);
}

/** `in` / `nin` 的右值不是数组原因。 */
export function reasonArrayValue(cmp: string): string {
  return fill(RUST_TEMPLATES.reasonArrayValue, CONSTS, [cmp]);
}

/** 字符串字面量超长原因。 */
export function reasonLiteralChars(): string {
  return fill(RUST_TEMPLATES.reasonLiteralChars, CONSTS);
}

/** 数组字面量超限原因。 */
export function reasonLiteralArray(): string {
  return fill(RUST_TEMPLATES.reasonLiteralArray, CONSTS);
}

/** then 目标为空原因（`where_desc` 形如「第 1 条规则（r_1）」）。 */
export function reasonTargetEmpty(whereDesc: string): string {
  return fill(RUST_TEMPLATES.reasonTargetEmpty, { ...CONSTS, where_desc: whereDesc });
}

/** then 目标超限原因。 */
export function reasonTargetsOver(whereDesc: string, count: number): string {
  return fill(RUST_TEMPLATES.reasonTargetsOver, { ...CONSTS, where_desc: whereDesc }, [count]);
}

/** 规则条数超限原因。 */
export function reasonRulesOver(count: number): string {
  return fill(RUST_TEMPLATES.reasonRulesOver, CONSTS, [count]);
}

// ==================== P-FLOW-1C：TASK_NODE / NOTIFY_NODE 段（65034–65039） ====================
//
// 为什么这一段的形状与上面不同（**不是偷懒，是口径差异**）：
//
// 65016 / 65033 那两条是「一条边 / 一个节点的审批人表达式」这种**单一成因**的码，
// 后端只有一两个 `format!` 外壳，逐字搬模板 + `fill` 才划算。
//
// 65034–65039 每一条背后都是**几十个分支**（`node_config.rs` 里 `config_invalid()` 有 30+ 处调用、
// R21 有 4 种文案），把 30 条模板搬过来只会得到一份永远对不齐的副本。
// 所以这里的做法是：
//
// 1. **码值必须逐字对齐**（`TASK_NODE_CODES`，单测读 `error_code.rs` 断言）；
// 2. UI 优先显示**后端返回的原文**（`ApiError.message`），本地文案只在「没有后端文案」时兜底
//    （例如前端预检拦下的违规、或网络失败）；
// 3. 本地文案写的是**规则名 + 用户该怎么办**，不冒充后端 `msg`（{@link taskNodeErrorText} 里
//    `source` 字段把这件事标出来，避免以后有人以为它是逐字副本）。

/** 节点配置与节点类型不匹配 / `config.task` 形状非法（R17/R18 的一半） */
export const FLOW_NODE_CONFIG_MISMATCH = 65034;
/** 流程节点配置非法（R17/R18 矩阵；**不含** `on_complete=APPROVE`，1B 起它合法） */
export const FLOW_TASK_NODE_INVALID = 65035;
/** `NOTIFY_NODE` 的模板 / 占位符非法（R19） */
export const FLOW_NOTIFY_TEMPLATE_INVALID = 65036;
/** 模板变量路径不在白名单 / `task.*` 用在非 `TASK_ACCEPT` 流程（R21） */
export const FLOW_TEMPLATE_VAR_INVALID = 65037;
/** 流程任务本轮已完成（HTTP 409）：并发 / 重复点完成 → 前端按**幂等成功**展示（P-FLOW-1B） */
export const FLOW_TASK_ALREADY_DONE = 65020;
/** 流程任务状态与实例状态冲突（HTTP 409，P-FLOW-1B 用） */
export const FLOW_TASK_STATE_CONFLICT = 65038;
/** 预演参数非法：缺变量（`strict=true`）/ 渲染后标题超长 / 模板语法错 */
export const FLOW_PREVIEW_INVALID = 65039;

/** 本卡对齐的六个码（单测断言顺序与码值）。 */
export const TASK_NODE_CODES = [
  FLOW_NODE_CONFIG_MISMATCH,
  FLOW_TASK_NODE_INVALID,
  FLOW_NOTIFY_TEMPLATE_INVALID,
  FLOW_TEMPLATE_VAR_INVALID,
  FLOW_TASK_STATE_CONFLICT,
  FLOW_PREVIEW_INVALID,
] as const;

/** 一条码的本地文案（**兜底**用：有后端文案时永远显示后端原文） */
export interface TaskNodeCodeMeta {
  /** 码值 */
  code: number;
  /** 后端常量名（排障时按这个名字 grep `error_code.rs`） */
  name: string;
  /** 一句话标题 */
  title: string;
  /** 用户该怎么办 */
  hint: string;
  /** 关联的发布校验规则号（`null` = 不在发布校验里，只在运行期 / 预演出现） */
  rule: string | null;
}

/** 码 → 本地文案表 */
export const TASK_NODE_CODE_META: Record<number, TaskNodeCodeMeta> = {
  [FLOW_NODE_CONFIG_MISMATCH]: {
    code: FLOW_NODE_CONFIG_MISMATCH,
    name: 'FLOW_NODE_CONFIG_MISMATCH',
    title: '任务配置不完整或格式不对',
    hint: '检查标题模板是否为空、期限是否缺数量 / 日期 / 字段、完成方式与审批业务类型是否配套',
    rule: 'R17 / R18',
  },
  [FLOW_TASK_NODE_INVALID]: {
    code: FLOW_TASK_NODE_INVALID,
    name: 'FLOW_TASK_NODE_INVALID',
    title: '「需验收」与「完成方式」冲突',
    hint: '验收与审批不能同时开（会让用户点两次提交）；「完成后审批」要指定一条「已发布且设为默认」的审批流程，否则发布会返回本码',
    rule: 'R18',
  },
  [FLOW_NOTIFY_TEMPLATE_INVALID]: {
    code: FLOW_NOTIFY_TEMPLATE_INVALID,
    name: 'FLOW_NOTIFY_TEMPLATE_INVALID',
    title: '通知模板语法错误',
    hint: '占位符要写成 {{变量路径}} 且必须闭合；变量名只允许字母 / 数字 / 下划线与一个点号',
    rule: 'R19',
  },
  [FLOW_TEMPLATE_VAR_INVALID]: {
    code: FLOW_TEMPLATE_VAR_INVALID,
    name: 'FLOW_TEMPLATE_VAR_INVALID',
    title: '模板变量不在白名单内',
    hint: '只允许 form.* / instance.* / initiator.* / node.* / prev.* / task.*（task.* 仅任务验收流程）',
    rule: 'R21',
  },
  [FLOW_TASK_STATE_CONFLICT]: {
    code: FLOW_TASK_STATE_CONFLICT,
    name: 'FLOW_TASK_STATE_CONFLICT',
    title: '任务与流程状态冲突',
    hint: '所属流程已结束（CANCELLED / WITHDRAWN），这张任务不能再推进流程；请联系流程发起人重新发起',
    rule: null,
  },
  [FLOW_PREVIEW_INVALID]: {
    code: FLOW_PREVIEW_INVALID,
    name: 'FLOW_PREVIEW_INVALID',
    title: '预演失败',
    hint: '严格模式下缺变量会直接报错；先补上变量用的表单字段，或关掉严格模式看可见占位',
    rule: null,
  },
};

/**
 * 文案取用：**后端原文优先，本地兜底**（`source` 标明来源，避免把它当逐字副本）。
 *
 * # 参数
 * - `code`：`ApiError.code`（可能为 `null` / 0 = 网络层失败）
 * - `backendMessage`：后端 `msg`（拿得到就一定用它）
 */
export function taskNodeErrorText(
  code: number | null | undefined,
  backendMessage?: string | null,
): { text: string; source: 'backend' | 'local' | 'unknown'; meta: TaskNodeCodeMeta | null } {
  const message = (backendMessage ?? '').trim();
  const meta = typeof code === 'number' ? (TASK_NODE_CODE_META[code] ?? null) : null;
  if (message) {
    return { text: message, source: 'backend', meta };
  }
  if (meta) {
    return { text: `${meta.title}：${meta.hint}（${meta.code}）`, source: 'local', meta };
  }
  return { text: '未知错误，请稍后重试或查看后端日志', source: 'unknown', meta: null };
}
