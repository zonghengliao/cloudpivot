/**
 * then 目标选择器（`then` 子树）的纯逻辑层 —— 与后端 `business/flow/expression.rs::parse_target`
 * **逐字对齐**（键名 / 上限 / 合法来源）。
 *
 * # 为什么单独一个 .ts
 *
 * 「生成 JSON / 反解 JSON / 校验」全在这里，**不 import Vue、不碰 DOM**，
 * 这样 `pnpm test:unit`（`tsx --test`）能直接跑单测，页面与单测共用同一份逻辑，杜绝漂移
 * —— 与 `condition-tree.ts`（P1-B1）同一套做法。
 *
 * # 与后端的契约（`expression.rs`，一个字母都不能改）
 *
 * `then` 是**数组**，1–5 项（`MAX_EXPR_TARGETS`）；每一项是一个目标对象，
 * 后端 `deny_unknown_keys` 会拒掉任何多余键，所以每种来源只落它自己的键：
 *
 * ```json
 * {"type":"USER","user_ids":["<uuid>", "…"]}            // 1–20 个 uuid（=「写死的人」，即卡面的 STATIC）
 * {"type":"ROLE","role_codes":["FINANCE_DIRECTOR"]}     // 1–10 个**编码**，不是 id（D-E4）
 * {"type":"DEPT_LEADER","dept_ids":["<uuid>"]}          // 可空；空 = 发起人所在部门
 * {"type":"INITIATOR"}                                  // 发起人本人
 * {"type":"INITIATOR_SUPERIOR","levels":2}              // 上溯层数 1–10，缺省 1（= 卡面的 SUPERVISOR）
 * {"type":"TASK_OWNER"} / {"type":"TASK_CREATOR"} / {"type":"TASK_ASSIGNER"}
 * {"type":"FORM_VAR","form_var_key":"owner_id"}         // 表单字段名，值是 uuid
 * {"type":"ADMIN"}                                      // 平台超管
 * ```
 *
 * # 卡面口径的三处对齐（重要）
 *
 * 卡面写的 `SUPERVISOR / INITIATOR_DEPT / STATIC` 在**库、后端、前端 TS 类型里都不存在**
 * （`biz_flow_node.assignee_type` 的 CHECK、`graph::AssigneeKind`、`expression::Target` 三处一致）。
 * 硬编这三个名字，生成的 JSON 会被 `parse_target` 以「来源不支持」400 掉。所以：
 *
 * | 卡面 | 实际落地 |
 * |---|---|
 * | `SUPERVISOR` | `INITIATOR_SUPERIOR` + `levels` |
 * | `INITIATOR_DEPT` | `DEPT_LEADER` + 空 `dept_ids`（后端语义：空 = 发起人部门） |
 * | `STATIC` | `USER` + 多个 `user_ids` |
 *
 * 语义一一对应，只是键名按后端真实枚举走 —— 与 P1-A1（EXPRESSION DSL）的口径一致。
 *
 * # `PREV_APPROVER`
 *
 * 能力 B，P2 才开放。后端在**解析期**就硬拒（`65033`），因此这里：
 * ① 下拉里灰显、不可选；② 旧数据里若真有，加载出来必须红框 + 提示，且**不写进产出 JSON**
 * （避免用户点保存时拿到一个必拒的 400）。
 */

/** 后端 `expression::TARGET_TYPES`（10 种来源，逐字一致，顺序也照抄）。 */
export const TARGET_TYPES = [
  'USER',
  'ROLE',
  'DEPT_LEADER',
  'INITIATOR',
  'INITIATOR_SUPERIOR',
  'TASK_OWNER',
  'TASK_CREATOR',
  'TASK_ASSIGNER',
  'FORM_VAR',
  'ADMIN',
] as const;

/** 合法来源（10 种之一）。 */
export type TargetType = (typeof TARGET_TYPES)[number];

/** 本期不开放的保留词（D-E8：绝不能让它落到「存得进去、跑起来 501」）。 */
export const RESERVED_PREV_APPROVER = 'PREV_APPROVER';

// ==================== 上限（`expression.rs` 的常量，别在这里改小） ====================

/** 单条规则的 then 目标数上限。 */
export const MAX_TARGETS = 5;
/** `user_ids` / `dept_ids` 单项上限。 */
export const MAX_USER_IDS = 20;
/** `role_codes` 单项上限。 */
export const MAX_ROLE_CODES = 10;
/** `levels` 上限（与 `biz_flow_node.superior_levels` 的 1..10 一致）。 */
export const MAX_SUPERIOR_LEVELS = 10;
/** `form_var_key` 字符数上限。 */
export const MAX_FORM_VAR_CHARS = 64;

// ==================== 类型 ====================

/**
 * 编辑期的一个目标（camelCase 只是本地字段名，落盘时由 [`toTargetJson`] 转成后端的 snake_case 键）。
 *
 * `type` 故意用 `string` 而不是 `TargetType`：旧数据 / 别人手改库里可能塞进来
 * `PREV_APPROVER` 或别的不认识的值，**编辑器不该因此白屏**，要能渲染成一行红框。
 */
export interface ThenTarget {
  /** 来源（正常是 10 种之一；可能是保留词或不认识的值） */
  type: string;
  /** `USER` 的用户 id 列表（= 卡面的 STATIC 写死列表） */
  userIds: string[];
  /** `ROLE` 的角色**编码**列表（D-E4） */
  roleCodes: string[];
  /** `DEPT_LEADER` 的部门 id 列表（空 = 发起人部门） */
  deptIds: string[];
  /** `INITIATOR_SUPERIOR` 的上溯层数 */
  levels: number;
  /** `FORM_VAR` 的表单字段名 */
  formVarKey: string;
}

/** 一条校验问题：`index` 是第几个目标（`-1` = 整组问题，比如空数组）。 */
export interface TargetIssue {
  /** 目标下标（`-1` = 整组） */
  index: number;
  /** 中文原因 */
  message: string;
}

/** 需要什么控件。 */
export type TargetInput = 'users' | 'roles' | 'dept' | 'levels' | 'formVar' | 'none';

/** 下拉里的一项（含保留词的「P2 支持」标记）。 */
export interface SourceOption {
  /** 落盘值 */
  value: string;
  /** 中文名 */
  label: string;
  /** 一句话说明（下拉里灰字） */
  hint: string;
  /** 不可选（保留词） */
  disabled: boolean;
  /** 是不是保留词（要挂「P2 支持」标签） */
  reserved: boolean;
}

// ==================== 来源表 ====================

/** 来源下拉（顺序：最常用在上；保留词永远在最后一行、灰显）。 */
export const SOURCE_OPTIONS: SourceOption[] = [
  {
    value: 'USER',
    label: '指定用户',
    hint: '选具体的人（可多选，写死 uuid）',
    disabled: false,
    reserved: false,
  },
  {
    value: 'ROLE',
    label: '按角色',
    hint: '按 role_code 引用（跨环境导出不失效）',
    disabled: false,
    reserved: false,
  },
  {
    value: 'INITIATOR_SUPERIOR',
    label: '发起人上级',
    hint: '按层数上溯发起人的主管',
    disabled: false,
    reserved: false,
  },
  {
    value: 'INITIATOR',
    label: '发起人本人',
    hint: '谁发起谁审（自审策略另配）',
    disabled: false,
    reserved: false,
  },
  {
    value: 'DEPT_LEADER',
    label: '部门负责人',
    hint: '不选部门 = 发起人所在部门的负责人',
    disabled: false,
    reserved: false,
  },
  {
    value: 'TASK_OWNER',
    label: '关联任务负责人',
    hint: '只有任务类业务能解析',
    disabled: false,
    reserved: false,
  },
  {
    value: 'TASK_CREATOR',
    label: '关联任务创建人',
    hint: '只有任务类业务能解析',
    disabled: false,
    reserved: false,
  },
  {
    value: 'TASK_ASSIGNER',
    label: '关联任务分派人',
    hint: '自建任务没有分派人 → 空结果走策略',
    disabled: false,
    reserved: false,
  },
  {
    value: 'FORM_VAR',
    label: '表单字段里的人',
    hint: '填表单字段名，它存的是 uuid',
    disabled: false,
    reserved: false,
  },
  {
    value: 'ADMIN',
    label: '平台管理员',
    hint: '所有 SUPER_ADMIN',
    disabled: false,
    reserved: false,
  },
  {
    value: RESERVED_PREV_APPROVER,
    label: '上一节点审批人',
    hint: '本期不支持（P2 开放）',
    disabled: true,
    reserved: true,
  },
];

/** 能不能选这个来源（保留词 / 不认识的值 → false）。 */
export function isSelectableSource(type: string): boolean {
  const option = SOURCE_OPTIONS.find((item) => item.value === normalizeType(type));
  return option !== undefined && !option.disabled;
}

/** 是不是 10 种合法来源之一。 */
export function isTargetType(type: string): boolean {
  return (TARGET_TYPES as readonly string[]).includes(normalizeType(type));
}

/** 是不是保留词 `PREV_APPROVER`。 */
export function isReserved(type: string): boolean {
  return normalizeType(type) === RESERVED_PREV_APPROVER;
}

/** 取来源的中文名（不认识的直接回原值，方便用户看懂旧数据里是什么）。 */
export function sourceLabel(type: string): string {
  const raw = (type ?? '').trim();
  return SOURCE_OPTIONS.find((item) => item.value === normalizeType(raw))?.label ?? (raw || '（未选）');
}

/** 该来源要什么控件。 */
export function inputKind(type: string): TargetInput {
  switch (normalizeType(type)) {
    case 'USER':
      return 'users';
    case 'ROLE':
      return 'roles';
    case 'DEPT_LEADER':
      return 'dept';
    case 'INITIATOR_SUPERIOR':
      return 'levels';
    case 'FORM_VAR':
      return 'formVar';
    default:
      return 'none';
  }
}

/** 无需输入时，右边显示「谁会被派到」的一句话。 */
export function autoHint(type: string): string {
  switch (normalizeType(type)) {
    case 'INITIATOR':
      return '运行时取发起人';
    case 'TASK_OWNER':
      return '运行时取关联任务负责人';
    case 'TASK_CREATOR':
      return '运行时取关联任务创建人';
    case 'TASK_ASSIGNER':
      return '运行时取关联任务分派人';
    case 'ADMIN':
      return '运行时取平台超管';
    default:
      return '运行时解析';
  }
}

/** 统一大小写 + 去空白（后端 `parse` 也做 `trim().to_ascii_uppercase()`）。 */
function normalizeType(type: string): string {
  return (type ?? '').trim().toUpperCase();
}

// ==================== 构造 ====================

/**
 * 造一个空目标。
 *
 * `DEPT_LEADER` 默认 `deptIds: []`（= 发起人部门），`INITIATOR_SUPERIOR` 默认 1 级 ——
 * 都是后端认可的「合法且有意义」的初值，新加一行不会立刻是红框。
 */
export function createTarget(type: string = 'USER'): ThenTarget {
  return {
    type: normalizeType(type),
    userIds: [],
    roleCodes: [],
    deptIds: [],
    levels: 1,
    formVarKey: '',
  };
}

// ==================== 单项校验 ====================

/** UUID 文本（后端 `Uuid::parse_str` 的宽松形态：32–36 位十六进制，可带连字符）。 */
export function isUuid(value: string): boolean {
  const text = (value ?? '').trim();
  if (/^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/.test(text)) {
    return true;
  }
  return /^[0-9a-fA-F]{32}$/.test(text);
}

/**
 * 表单字段名判定 —— 与后端 `is_form_var_key` 同一套规则：
 * 首字符字母或下划线，其余字母/数字/下划线，总长 ≤ 64。
 */
export function formVarKeyError(key: string): string | null {
  const text = (key ?? '').trim();
  if (!text) return '请填表单字段名（它执行时存的是审批人 uuid）';
  if (!/^[A-Za-z_]/.test(text) || !/^[A-Za-z_][A-Za-z0-9_]*$/.test(text)) {
    return '字段名要以字母或下划线开头，只含字母 / 数字 / 下划线';
  }
  if (text.length > MAX_FORM_VAR_CHARS) {
    return `字段名最长 ${MAX_FORM_VAR_CHARS} 个字符`;
  }
  return null;
}

/** 去空、去重（保序）—— 后端拒「空字符串」和重复项没有意义，这里先收口。 */
export function cleanList(list: readonly string[] | undefined): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const item of list ?? []) {
    const text = typeof item === 'string' ? item.trim() : '';
    if (!text || seen.has(text)) continue;
    seen.add(text);
    out.push(text);
  }
  return out;
}

/**
 * 单个目标的问题列表（空数组 = 合法）。`toTargetJson` 与 `validateTargets` 共用这一份判定。
 */
export function targetIssues(target: ThenTarget): string[] {
  const type = normalizeType(target.type);
  if (!type) return ['请先选审批人来源'];

  if (isReserved(type)) {
    return [
      '「上一节点审批人」（PREV_APPROVER）本版本未开放（P2），发布会被拒（65033）；请改成其它来源',
    ];
  }
  if (!isTargetType(type)) {
    return [`审批人来源「${type}」不支持（允许 ${TARGET_TYPES.join(' / ')}）`];
  }

  switch (type) {
    case 'USER': {
      const ids = cleanList(target.userIds);
      if (ids.length === 0) return ['请至少选 1 个人'];
      if (ids.length > MAX_USER_IDS) return [`指定用户最多 ${MAX_USER_IDS} 个（当前 ${ids.length} 个）`];
      const bad = ids.find((id) => !isUuid(id));
      if (bad) return [`用户 id「${bad}」不是合法 UUID`];
      return [];
    }
    case 'ROLE': {
      // D-E4：存 role_code。role_id 是 gen_random_uuid()，跨环境复制数据库就会指错角色。
      const codes = cleanList(target.roleCodes);
      if (codes.length === 0) return ['请至少选 1 个角色'];
      if (codes.length > MAX_ROLE_CODES) return [`指定角色最多 ${MAX_ROLE_CODES} 个（当前 ${codes.length} 个）`];
      return [];
    }
    case 'DEPT_LEADER': {
      const ids = cleanList(target.deptIds);
      // 空数组 = 发起人部门，后端明确认可，不算错
      if (ids.length > MAX_USER_IDS) return [`指定部门最多 ${MAX_USER_IDS} 个（当前 ${ids.length} 个）`];
      const bad = ids.find((id) => !isUuid(id));
      if (bad) return [`部门 id「${bad}」不是合法 UUID`];
      return [];
    }
    case 'INITIATOR_SUPERIOR': {
      const levels = Number(target.levels);
      if (!Number.isInteger(levels) || levels < 1 || levels > MAX_SUPERIOR_LEVELS) {
        return [`上溯层数必须是 1–${MAX_SUPERIOR_LEVELS} 的整数（当前 ${String(target.levels)}）`];
      }
      return [];
    }
    case 'FORM_VAR': {
      const error = formVarKeyError(target.formVarKey);
      return error === null ? [] : [error];
    }
    default:
      // INITIATOR / TASK_OWNER / TASK_CREATOR / TASK_ASSIGNER / ADMIN：无需输入
      return [];
  }
}

/** 整组校验（含「空数组」「超 5 个」两条整组规则）。 */
export function validateTargets(targets: readonly ThenTarget[]): TargetIssue[] {
  const issues: TargetIssue[] = [];
  if (targets.length === 0) {
    issues.push({
      index: -1,
      message: '「命中后由谁审批」不能为空：至少选 1 个审批人（否则条件命中后无人可派）',
    });
    return issues;
  }
  if (targets.length > MAX_TARGETS) {
    issues.push({
      index: -1,
      message: `单条规则的审批人最多 ${MAX_TARGETS} 个（当前 ${targets.length} 个）`,
    });
  }
  targets.forEach((target, index) => {
    for (const message of targetIssues(target)) {
      issues.push({ index, message: `第 ${index + 1} 个审批人：${message}` });
    }
  });
  return issues;
}

/** 下标 → 第一条问题（行内红框 + 行尾提示用；`-1` 是整组问题，单独取）。 */
export function issueByIndex(issues: readonly TargetIssue[]): Record<number, string> {
  const map: Record<number, string> = {};
  for (const issue of issues) {
    if (issue.index < 0) continue;
    if (!(issue.index in map)) map[issue.index] = issue.message;
  }
  return map;
}

// ==================== 生成 JSON ====================

/**
 * 单个目标 → 后端要的 JSON。
 *
 * 不合法（保留词 / 缺值 / 超限 / 不认识）返回 `null` —— **不把必拒的对象抛给调用方**，
 * 问题由 [`validateTargets`] 在界面上红字说明。
 */
export function toTargetJson(target: ThenTarget): Record<string, unknown> | null {
  if (targetIssues(target).length > 0) return null;
  const type = normalizeType(target.type);
  switch (type) {
    case 'USER':
      // 卡面的 STATIC：写死的 uuid 列表，就是 USER 的 user_ids
      return { type: 'USER', user_ids: cleanList(target.userIds) };
    case 'ROLE':
      return { type: 'ROLE', role_codes: cleanList(target.roleCodes) };
    case 'DEPT_LEADER': {
      const ids = cleanList(target.deptIds);
      // 空数组 = 发起人部门；后端允许省略 dept_ids，省掉更干净
      return ids.length === 0 ? { type: 'DEPT_LEADER' } : { type: 'DEPT_LEADER', dept_ids: ids };
    }
    case 'INITIATOR_SUPERIOR':
      return { type: 'INITIATOR_SUPERIOR', levels: Number(target.levels) };
    case 'FORM_VAR':
      return { type: 'FORM_VAR', form_var_key: target.formVarKey.trim() };
    case 'INITIATOR':
    case 'TASK_OWNER':
    case 'TASK_CREATOR':
    case 'TASK_ASSIGNER':
    case 'ADMIN':
      // 无需输入：只落 type 一个键（后端 deny_unknown_keys 会拒多余键）
      return { type };
    default:
      return null;
  }
}

/** 整组 → `then` 数组（跳过不合法的项，`[]` 表示一个都没填好）。 */
export function toThenJson(targets: readonly ThenTarget[]): Array<Record<string, unknown>> {
  const out: Array<Record<string, unknown>> = [];
  for (const target of targets) {
    const json = toTargetJson(target);
    if (json !== null) out.push(json);
  }
  return out;
}

/** 目标 → 一句话（只读态 / 折叠展示用）。 */
export function describeTarget(target: ThenTarget): string {
  const type = normalizeType(target.type);
  if (isReserved(type) || !isTargetType(type)) {
    return `${sourceLabel(type)}（本期不支持）`;
  }
  switch (type) {
    case 'USER': {
      const ids = cleanList(target.userIds);
      return ids.length === 0 ? '指定用户（未选人）' : `指定用户：${ids.length} 人`;
    }
    case 'ROLE': {
      const codes = cleanList(target.roleCodes);
      return codes.length === 0 ? '按角色（未选）' : `按角色：${codes.join('、')}`;
    }
    case 'DEPT_LEADER': {
      const ids = cleanList(target.deptIds);
      return ids.length === 0 ? '发起人部门负责人' : `部门负责人：${ids.length} 个部门`;
    }
    case 'INITIATOR_SUPERIOR':
      return `发起人上级（上溯 ${Number(target.levels) || 1} 级）`;
    case 'FORM_VAR':
      return `表单字段：${target.formVarKey.trim() || '（未填）'}`;
    default:
      return sourceLabel(type);
  }
}

// ==================== 反解 JSON ====================

/**
 * `then` 原文 → 编辑期目标数组。
 *
 * **不抛异常**：先做整体形状检查（必须是数组、长度 0–5），再逐项宽松解析 ——
 * 不认识的值留在 `type` 里、缺的键补空，把问题记进 `issues`。
 * 编辑器不该因为一条脏数据白屏（P1-B1 的 `fromJson` 同一套原则）。
 */
export function fromThenJson(raw: unknown): { targets: ThenTarget[]; issues: TargetIssue[] } {
  const issues: TargetIssue[] = [];
  if (raw === null || raw === undefined) return { targets: [], issues };
  if (!Array.isArray(raw)) {
    issues.push({ index: -1, message: '审批人（then）必须是数组，如 [{"type":"ROLE","role_codes":["FINANCE_DIRECTOR"]}]' });
    return { targets: [], issues };
  }
  if (raw.length > MAX_TARGETS) {
    issues.push({ index: -1, message: `审批人超过 ${MAX_TARGETS} 个（当前 ${raw.length} 个），多余的已忽略` });
  }
  const targets: ThenTarget[] = [];
  raw.slice(0, MAX_TARGETS).forEach((item, index) => {
    if (item === null || typeof item !== 'object' || Array.isArray(item)) {
      issues.push({ index, message: `第 ${index + 1} 个审批人：必须是 JSON 对象` });
      targets.push(createTarget(''));
      return;
    }
    const map = item as Record<string, unknown>;
    const type = typeof map.type === 'string' ? normalizeType(map.type) : '';
    const target = createTarget(type);
    target.userIds = strList(map.user_ids);
    target.roleCodes = strList(map.role_codes);
    target.deptIds = strList(map.dept_ids);
    target.levels = typeof map.levels === 'number' || typeof map.levels === 'string' ? toLevels(map.levels) : 1;
    target.formVarKey = typeof map.form_var_key === 'string' ? map.form_var_key : '';
    targets.push(target);

    // 旧字段名 `role_ids`（上一版草案）：后端会专门报这条，前端也提前挑明
    if (map.role_ids !== undefined) {
      issues.push({
        index,
        message: `第 ${index + 1} 个审批人：请用 role_codes（角色编码）而不是 role_ids（角色 id），id 跨环境会失配`,
      });
    }
    // 形状问题直接给出，别等用户点保存
    for (const message of targetIssues(target)) {
      issues.push({ index, message: `第 ${index + 1} 个审批人：${message}` });
    }
  });
  return { targets, issues };
}

/** 任意值 → 字符串数组（非数组 → 空；元素非字符串 → 跳过）。 */
function strList(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value
    .filter((item): item is string => typeof item === 'string')
    .map((item) => item.trim())
    .filter((item) => item !== '');
}

/** 任意值 → 层数（非法值原样保留成 number，交给 `targetIssues` 报错）。 */
function toLevels(value: number | string): number {
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : 1;
}
