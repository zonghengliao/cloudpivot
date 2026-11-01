/**
 * `config.task` 的类型 / 默认值 / **R18 双向联动**（P-FLOW-1C · 设计 §2.3 / §4.1）.
 *
 * # 为什么联动逻辑必须在 `task-node-config.ts` 而不是 `TaskNodeConfig.vue`
 *
 * 卡面验收第 2 条要求「R18 在 UI 上不可违反：4 种操作顺序各试一次，都产不出非法组合」。
 * 靠人手点 4 遍是一次性证据，靠纯函数则是**每次都跑**的回归：
 * 单测把 4 条顺序（外加所有枚举切换）全排列跑一遍，断言
 * {@link checkMatrix} 恒为 `null` —— 组件只是把这两个函数挂在 `@change` 上。
 *
 * # R18 的判据（`node_config.rs::check_matrix`，逐行对齐）
 *
 * | `on_complete` | `need_review` | 结果 |
 * |---|---|---|
 * | `DIRECT` | `false` | ✅ 推荐（干完即通过） |
 * | `DIRECT` | `true` | ✅ 允许（干完 → 待验收 → 验收通过） |
 * | `VERIFY` | `true` | ✅ 允许（`VERIFY` 是显式声明） |
 * | `VERIFY` | `false` | ❌ 矛盾：声明要验收却没开验收态（配置静默失效） |
 * | `APPROVE` | `true` | ❌「点两次提交」（发起人验收 + 审批人签字） |
 * | `APPROVE` | `false` | 1A 阶段闸拒绝（引擎分支在 P-FLOW-1B），1B 起 ✅ |
 *
 * # 三条 UI 纪律（设计 §4.1「关键交互纪律」1，照做别重新设计）
 *
 * 1. 选 `APPROVE` → `need_review` 自动置 `false` 并**禁用勾选**；
 * 2. 勾 `need_review` → `on_complete` 里的 `APPROVE` 项**灰掉**；
 * 3. 选 `VERIFY` → 自动勾 `need_review` 并**锁定**。
 *
 * 三条合起来的效果：**用户不可能配出 R18 违例**（{@link applyOnComplete} /
 * {@link applyNeedReview} 的返回值永远过 {@link checkMatrix}）。
 */

/** 任务完成后流程怎么走（`OnComplete`） */
export type OnComplete = 'DIRECT' | 'VERIFY' | 'APPROVE';

/** 打回时流程怎么走（`OnReject`：默认 `BACK_TO_PREV`） */
export type OnReject = 'BACK_TO_PREV' | 'TERMINATE' | 'STAY';

/** 取消时流程怎么走（`OnCancel`：默认 `TERMINATE`） */
export type OnCancel = 'TERMINATE' | 'BACK_TO_PREV' | 'STAY';

/** 期限模式 */
export type DueMode = 'RELATIVE' | 'ABSOLUTE' | 'FIELD';

/** 相对期限的单位 */
export type DueUnit = 'HOUR' | 'DAY' | 'WORKDAY';

/** 期限配置（三种模式互斥，多余键后端会 65034） */
export interface TaskDue {
  /** 模式 */
  mode: DueMode;
  /** 仅 `RELATIVE`：数量 */
  amount?: number | null;
  /** 仅 `RELATIVE`：单位 */
  unit?: DueUnit | null;
  /** 仅 `ABSOLUTE`：`yyyy-MM-dd` */
  date?: string | null;
  /** 仅 `FIELD`：`form.*` 路径 */
  field?: string | null;
}

/**
 * 「任务配置」面板的完整形状（与 `TASK_CONFIG_KEYS` 的 12 个键一一对应）。
 *
 * **不要在这里加键**：后端 `deny_unknown_keys` 会 400 + 65034，
 * 加了却没用上等于给用户一个「填了不生效」的输入框。
 */
export interface TaskConfig {
  /** 标题模板（必填，≤200 字符） */
  title_template: string;
  /** 描述模板（可空，≤8000 字符） */
  description_template: string | null;
  /** 优先级 `P0`–`P3` */
  priority: string;
  /** 期限（`null` = 不设期限） */
  due: TaskDue | null;
  /** 完成后需验收（`REVIEW` 态） */
  need_review: boolean;
  /** 完成后怎么走 */
  on_complete: OnComplete;
  /** 仅 `on_complete=APPROVE`：发起的审批流 `biz_type` */
  approve_biz_type: string | null;
  /** 打回时 */
  on_reject: OnReject;
  /** 取消时 */
  on_cancel: OnCancel;
  /** 详情页显示所属单据 */
  attach_biz: boolean;
  /** 「派不出人也不生成任务」的二次开关（默认 false） */
  allow_skip_when_no_assignee: boolean;
}

/** 新建 `TASK_NODE` 时的默认配置（与后端缺省值一致：P2 / 不设期限 / DIRECT / BACK_TO_PREV / TERMINATE / attach_biz=true） */
export const DEFAULT_TASK_CONFIG: TaskConfig = {
  title_template: '',
  description_template: null,
  priority: 'P2',
  due: null,
  need_review: false,
  on_complete: 'DIRECT',
  approve_biz_type: null,
  on_reject: 'BACK_TO_PREV',
  on_cancel: 'TERMINATE',
  attach_biz: true,
  allow_skip_when_no_assignee: false,
};

/** 优先级选项（`dto::PRIORITIES`） */
export const TASK_PRIORITY_OPTIONS = [
  { label: 'P0 最高', value: 'P0' },
  { label: 'P1 高', value: 'P1' },
  { label: 'P2 中', value: 'P2' },
  { label: 'P3 低', value: 'P3' },
] as const;

/** 完成方式选项（`ORDER` 决定下拉顺序；`APPROVE` 在 1A 会被后端拒，见 {@link stageGateError}） */
export const ON_COMPLETE_OPTIONS: Array<{ label: string; value: OnComplete; hint: string }> = [
  { label: '直接通过', value: 'DIRECT', hint: '负责人点完成，本节点即通过' },
  { label: '需验收', value: 'VERIFY', hint: '负责人点「提交验收」，流程发起人验收后节点通过' },
  { label: '完成后审批', value: 'APPROVE', hint: '完成即发起一条审批流，审批通过才算本节点完成（P-FLOW-1B 开放）' },
];

/** 打回时选项（默认「留在本节点」= 返工本来就该等） */
export const ON_REJECT_OPTIONS: Array<{ label: string; value: OnReject }> = [
  { label: '留在本节点（等返工后再完成）', value: 'STAY' },
  { label: '退回上一节点', value: 'BACK_TO_PREV' },
  { label: '终止流程', value: 'TERMINATE' },
];

/** 取消时选项（默认「终止流程」：活被取消了流程不该继续） */
export const ON_CANCEL_OPTIONS: Array<{ label: string; value: OnCancel }> = [
  { label: '终止流程', value: 'TERMINATE' },
  { label: '退回上一节点', value: 'BACK_TO_PREV' },
  { label: '留在本节点', value: 'STAY' },
];

/** 期限模式选项 */
export const DUE_MODE_OPTIONS: Array<{ label: string; value: DueMode }> = [
  { label: '相对', value: 'RELATIVE' },
  { label: '固定日期', value: 'ABSOLUTE' },
  { label: '取表单字段', value: 'FIELD' },
];

/** 相对期限单位选项（`WORKDAY` 按工作日算，跳过周六周日） */
export const DUE_UNIT_OPTIONS: Array<{ label: string; value: DueUnit }> = [
  { label: '工作日', value: 'WORKDAY' },
  { label: '自然日', value: 'DAY' },
  { label: '小时', value: 'HOUR' },
];

/** `on_complete` 的中文名（列表 / 徽标 / 副文案共用） */
export const ON_COMPLETE_LABELS: Record<OnComplete, string> = {
  DIRECT: '直接通过',
  VERIFY: '需验收',
  APPROVE: '完成后审批',
};

/** 把任意值收窄成 `OnComplete`（脏数据兜底 `DIRECT`） */
function asOnComplete(raw: unknown): OnComplete {
  const value = String(raw ?? '').trim().toUpperCase();
  return value === 'VERIFY' || value === 'APPROVE' ? value : 'DIRECT';
}

/** 把任意值收窄成 `OnReject`（脏数据兜底后端默认值 `BACK_TO_PREV`） */
function asOnReject(raw: unknown): OnReject {
  const value = String(raw ?? '').trim().toUpperCase();
  return value === 'STAY' || value === 'TERMINATE' ? value : 'BACK_TO_PREV';
}

/** 把任意值收窄成 `OnCancel`（脏数据兜底后端默认值 `TERMINATE`） */
function asOnCancel(raw: unknown): OnCancel {
  const value = String(raw ?? '').trim().toUpperCase();
  return value === 'BACK_TO_PREV' || value === 'STAY' ? value : 'TERMINATE';
}

/** 收窄期限配置（模式非法 → `null`，防止脏数据把面板画崩） */
function asDue(raw: unknown): TaskDue | null {
  if (!raw || typeof raw !== 'object') {
    return null;
  }
  const source = raw as Record<string, unknown>;
  const mode = String(source.mode ?? '').trim().toUpperCase();
  if (mode !== 'RELATIVE' && mode !== 'ABSOLUTE' && mode !== 'FIELD') {
    return null;
  }
  const toNumber = (value: unknown): number | null => {
    const parsed = Number(value);
    return value === null || value === undefined || Number.isNaN(parsed) ? null : parsed;
  };
  const toText = (value: unknown): string | null => {
    const text = String(value ?? '').trim();
    return text === '' ? null : text;
  };
  return {
    mode,
    amount: toNumber(source.amount),
    unit: (['HOUR', 'DAY', 'WORKDAY'] as const).includes(toText(source.unit) as DueUnit)
      ? (toText(source.unit) as DueUnit)
      : null,
    date: toText(source.date),
    field: toText(source.field),
  };
}

/**
 * 从节点上读出 `config.task` 并补齐默认值。
 *
 * **不改原对象**（撤销栈按「改前 / 改后」整键回退，就地改会让撤销回退到被污染的旧值）。
 */
export function taskConfigOf(node: { config?: unknown } | null | undefined): TaskConfig {
  const raw = (node?.config ?? null) as Record<string, unknown> | null;
  const source = (raw?.task ?? null) as Record<string, unknown> | null;
  if (!source || typeof source !== 'object') {
    return { ...DEFAULT_TASK_CONFIG };
  }
  const text = (value: unknown): string | null => {
    const out = String(value ?? '');
    return out === '' ? null : out;
  };
  return {
    title_template: String(source.title_template ?? ''),
    description_template: text(source.description_template),
    priority: String(source.priority ?? 'P2').trim().toUpperCase() || 'P2',
    due: asDue(source.due),
    need_review: source.need_review === true,
    on_complete: asOnComplete(source.on_complete),
    approve_biz_type: text(source.approve_biz_type),
    on_reject: asOnReject(source.on_reject),
    on_cancel: asOnCancel(source.on_cancel),
    attach_biz: source.attach_biz !== false,
    allow_skip_when_no_assignee: source.allow_skip_when_no_assignee === true,
  };
}

/**
 * 把面板状态包成 `patch-node` 的载荷（只带 `config` 一个字段，撤销粒度最小）。
 *
 * 空描述 / 空 `approve_biz_type` 一律落成 `null`：后端对「非 APPROVE 却带
 * approve_biz_type」是 **65034 拒绝**，前端不能靠「传空串糊过去」。
 */
export function taskConfigPatch(config: TaskConfig): { config: { task: Record<string, unknown> } } {
  const due = config.due
    ? {
        mode: config.due.mode,
        ...(config.due.mode === 'RELATIVE'
          ? { amount: config.due.amount ?? null, unit: config.due.unit ?? null }
          : {}),
        ...(config.due.mode === 'ABSOLUTE' ? { date: config.due.date ?? null } : {}),
        ...(config.due.mode === 'FIELD' ? { field: config.due.field ?? null } : {}),
      }
    : null;
  const task: Record<string, unknown> = {
    title_template: config.title_template,
    priority: config.priority,
    need_review: config.need_review,
    on_complete: config.on_complete,
    on_reject: config.on_reject,
    on_cancel: config.on_cancel,
    attach_biz: config.attach_biz,
    allow_skip_when_no_assignee: config.allow_skip_when_no_assignee,
  };
  if (config.description_template) {
    task.description_template = config.description_template;
  }
  if (due) {
    task.due = due;
  }
  if (config.on_complete === 'APPROVE' && config.approve_biz_type) {
    task.approve_biz_type = config.approve_biz_type;
  }
  return { config: { task } };
}

/**
 * 改「完成方式」→ 联动 `need_review`（R18 的 UI 表达，第 1 / 3 条）。
 *
 * - 选 `APPROVE`：`need_review` 强制 `false`（勾选框由 {@link needReviewLocked} 禁用）；
 * - 选 `VERIFY`：`need_review` 强制 `true`（并锁定）；
 * - 选 `DIRECT`：`need_review` 保持原值（DIRECT 与两种取值都合法）。
 */
export function applyOnComplete(config: TaskConfig, next: OnComplete): TaskConfig {
  if (next === 'APPROVE') {
    return { ...config, on_complete: 'APPROVE', need_review: false };
  }
  if (next === 'VERIFY') {
    return { ...config, on_complete: 'VERIFY', need_review: true, approve_biz_type: null };
  }
  return { ...config, on_complete: 'DIRECT', approve_biz_type: null };
}

/**
 * 改「完成后需验收」→ 反向联动 `on_complete`（R18 的 UI 表达，第 2 条）。
 *
 * - 勾上：当前是 `APPROVE` 时降级成 `VERIFY`（APPROVE+true 是 R18 违例）；
 * - 取消：当前是 `VERIFY` 时降级成 `DIRECT`（VERIFY+false 是 R18 违例）。
 *
 * 这两条兜底存在的原因：勾选框在 APPROVE / VERIFY 下是**禁用**的，
 * 正常点不到；但脏数据（旧快照、直接改库）会把非法组合读进来，
 * 用户勾一下就应该被**修正**，而不是继续往非法态走。
 */
export function applyNeedReview(config: TaskConfig, checked: boolean): TaskConfig {
  if (checked) {
    const base = config.on_complete === 'APPROVE'
      ? { ...config, on_complete: 'VERIFY' as OnComplete, approve_biz_type: null }
      : { ...config };
    return { ...base, need_review: true };
  }
  return {
    ...config,
    on_complete: config.on_complete === 'VERIFY' ? 'DIRECT' : config.on_complete,
    approve_biz_type: config.on_complete === 'VERIFY' ? null : config.approve_biz_type,
    need_review: false,
  };
}

/**
 * 「完成后需验收」勾选框是否禁用。
 *
 * `APPROVE`（审批已含确认环节，再开验收会让用户点两次提交）与
 * `VERIFY`（验收是它的定义，不许关）两种情况下禁用。
 */
export function needReviewLocked(config: TaskConfig): boolean {
  return config.on_complete === 'APPROVE' || config.on_complete === 'VERIFY';
}

/** 勾选框的 tooltip 文案（禁用时说明为什么，别只给一个灰框） */
export function needReviewTooltip(config: TaskConfig): string {
  if (config.on_complete === 'APPROVE') {
    return '审批已含确认环节，再开验收会让用户点两次提交';
  }
  if (config.on_complete === 'VERIFY') {
    return '完成方式选了「需验收」，验收态是它的定义，不能关（要免验收请改成「直接通过」）';
  }
  return '开启后负责人点「提交验收」，流程发起人验收通过才算本节点完成';
}

/** `on_complete` 里 `APPROVE` 项是否要灰掉（勾了验收就灰 —— 第 2 条纪律） */
export function approveOptionDisabled(config: TaskConfig): boolean {
  return config.need_review;
}

/**
 * R18 矩阵检查（与 `node_config.rs::check_matrix` 的前两条逐字同义）。
 *
 * 返回中文原因；合法返回 `null`。**`APPROVE` 的 1A 阶段闸不在这里**
 * （那是版本节奏，不是矩阵违例，见 {@link stageGateError}）。
 */
export function checkMatrix(config: TaskConfig): string | null {
  if (config.on_complete === 'VERIFY' && !config.need_review) {
    return '配置矛盾：on_complete=VERIFY 必须同时开 need_review（否则声明要验收，却允许负责人自己点完成直接通过）';
  }
  if (config.on_complete === 'APPROVE' && config.need_review) {
    return '配置矛盾：on_complete=APPROVE 与 need_review=true 不能同时开启（会让用户点两次：发起人验收 + 审批人签字）';
  }
  return null;
}

/**
 * P-FLOW-1A 的阶段闸：`on_complete=APPROVE` 会被后端 **65035 拒绝**。
 *
 * 为什么前端要提前说：这条拒绝**不是用户配错了**，是引擎分支还没上线
 * （`node_config.rs` 的 TODO(P-FLOW-1B)）。不说清的话，用户会以为是自己哪里填错，
 * 而预演返回的红字又只写着「将在 P-FLOW-1B 开放」。
 */
export function stageGateError(config: TaskConfig): string | null {
  if (config.on_complete === 'APPROVE') {
    return 'P-FLOW-1A 版本尚未开放「完成后审批」：引擎还没有「完成即发起审批流」这条分支，发布会被拒绝（65035）。请先选「直接通过」或「需验收」，或等 P-FLOW-1B。';
  }
  return null;
}

/** 期限配置的交叉约束检查（`parse_due` 的三条 + 日期格式），返回中文原因或 `null` */
export function dueIssue(due: TaskDue | null): string | null {
  if (!due) {
    return null;
  }
  if (due.mode === 'RELATIVE') {
    const amount = Number(due.amount);
    if (!Number.isFinite(amount) || amount < 1) {
      return '相对期限必须填数量（≥1）';
    }
    if (amount > 3650) {
      return '相对期限数量上限 3650（10 年）';
    }
    if (!due.unit) {
      return '相对期限必须选单位（工作日 / 自然日 / 小时）';
    }
    return null;
  }
  if (due.mode === 'ABSOLUTE') {
    if (!due.date || !/^\d{4}-\d{2}-\d{2}$/.test(due.date)) {
      return '固定日期必须是 yyyy-MM-dd';
    }
    return null;
  }
  if (!due.field) {
    return '取表单字段时必须选字段';
  }
  if (!due.field.startsWith('form.')) {
    return '期限只能取自表单字段（必须以 form. 开头）';
  }
  return null;
}
