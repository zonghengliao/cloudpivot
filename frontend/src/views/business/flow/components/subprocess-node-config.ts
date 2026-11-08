/**
 * `config.sub` 的类型 / 默认值 / 读写口径（P3-08 · ARCH-5 §4.14 子流程节点）。
 *
 * # 与后端逐字对齐的四个键（`flow::subprocess::parse_config`）
 *
 * | 键 | 取值 | 缺省 |
 * |---|---|---|
 * | `version_policy` | `PIN_CURRENT` / `FOLLOW_LATEST` | `PIN_CURRENT`（钉住第一次发起时的版本） |
 * | `fail_action` | `ABORT` / `CONTINUE` | `ABORT`（子流程没成 → 父流程终止） |
 * | `param_mapping` | `{ 子流程键: 父表单路径 }` | `{}`（不传参） |
 * | `result_mapping` | `{ 父表单路径: 子表单路径 }` | `{}`（不回写） |
 * | `spawn_task` | `boolean` | `false` |
 *
 * # 为什么键方向必须记牢（两个 mapping 相反）
 *
 * 后端 R27 要校验「`param_mapping` 的**键**在子流程表单里存在」——
 * 只有键是子侧，这句话才成立；`result_mapping` 恰好反过来（键是父侧路径，值是子侧路径）。
 * 前端如果把两个 mapping 做成同向，后端会稳定报 65045 或者静默映射错字段。
 *
 * # 为什么「子流程定义」不在这里
 *
 * `subprocess_def_id` 是**物理列**（`biz_flow_node.subprocess_def_id`，带 FK），
 * 不在 `config` 里 —— 见后端 `NodeSpec` 的注释（外键只能在列上生效；只换子流程版本不该改 graph_hash）。
 * 因此它挂在 `FlowNode.subprocess_def_id` 上，本文件只处理 `config.sub`。
 */

/** 版本策略 */
export type VersionPolicy = 'PIN_CURRENT' | 'FOLLOW_LATEST';

/** 子流程没走完时父流程的动作 */
export type FailAction = 'ABORT' | 'CONTINUE';

/** `config.sub` 的形状 */
export interface SubConfig {
  /** 版本策略（缺省 `PIN_CURRENT`） */
  version_policy?: VersionPolicy;
  /** 失败动作（缺省 `ABORT`） */
  fail_action?: FailAction;
  /** 父 `form_data` → 子 `form_data`：**键 = 子流程的键**，值 = 父表单路径（`a.b.c`） */
  param_mapping?: Record<string, string>;
  /** 子 `form_data` → 父 `form_data`：**键 = 父表单路径**，值 = 子表单路径 */
  result_mapping?: Record<string, string>;
  /** 是否在父侧生成一条「子流程在跑」的任务（父流程的人看得见） */
  spawn_task?: boolean;
}

/** 新建子流程节点时的默认配置（与后端缺省值一致） */
export const DEFAULT_SUB_CONFIG: SubConfig = {
  version_policy: 'PIN_CURRENT',
  fail_action: 'ABORT',
  spawn_task: false,
};

/** 版本策略选项（属性面板下拉） */
export const VERSION_POLICY_OPTIONS: Array<{ label: string; value: VersionPolicy; hint: string }> = [
  {
    label: '钉住当前版本',
    value: 'PIN_CURRENT',
    hint: '子流程改版不影响已经发起的父流程（推荐）',
  },
  {
    label: '跟随最新版本',
    value: 'FOLLOW_LATEST',
    hint: '每次进入本节点都用子流程当下的最新发布版本',
  },
];

/** 失败动作选项 */
export const FAIL_ACTION_OPTIONS: Array<{ label: string; value: FailAction; hint: string }> = [
  {
    label: '终止父流程',
    value: 'ABORT',
    hint: '子流程被驳回 ⇒ 父流程一起终止（内控默认）',
  },
  {
    label: '继续往下走',
    value: 'CONTINUE',
    hint: '子流程被驳回只算本节点不通过，父流程继续（如「预算调整失败也要留痕报销」）',
  },
];

/** 从节点 `config` 里取 `config.sub`（形状不对 → 空对象，面板不白屏） */
export function subConfigOf(node: { config?: unknown } | null | undefined): SubConfig {
  const sub = (node?.config as { sub?: unknown } | undefined)?.sub;
  if (!sub || typeof sub !== 'object' || Array.isArray(sub)) {
    return {};
  }
  return sub as SubConfig;
}

/** 组装一个「改节点」的 patch（只更新 `config.sub`，不碰别的 config 分区） */
export function subConfigPatch(
  node: { config?: unknown } | null | undefined,
  next: Partial<SubConfig>,
): { config: Record<string, unknown> } {
  const base = (node?.config && typeof node.config === 'object' && !Array.isArray(node.config))
    ? { ...(node.config as Record<string, unknown>) }
    : {};
  const sub = { ...subConfigOf(node), ...next };
  // 去掉 undefined：后端 `parse_config` 拒绝未知键，但 undefined 在 JSON 里会变成 null，
  // 而 `null` 对 `param_mapping` 是合法「空」、对 `version_policy` 是「用缺省」—— 两者都无害；
  // 这里仍然收敛掉，免得面板出现 `"fail_action": null` 这种脏配置。
  for (const key of Object.keys(sub) as Array<keyof SubConfig>) {
    if (sub[key] === undefined) {
      delete sub[key];
    }
  }
  base.sub = sub;
  return { config: base };
}

/** 映射表的行（面板编辑用：数组比对象好维护） */
export interface MappingRow {
  /** 键（`param_mapping` 是子流程键；`result_mapping` 是父表单路径） */
  key: string;
  /** 值（表单路径） */
  value: string;
}

/** 对象 → 行数组（保持插入顺序：Object.keys 对字符串键天然稳定） */
export function mappingRowsOf(mapping: Record<string, string> | undefined): MappingRow[] {
  if (!mapping || typeof mapping !== 'object') {
    return [];
  }
  return Object.entries(mapping)
    .filter(([key, value]) => typeof value === 'string' && key.trim() !== '')
    .map(([key, value]) => ({ key, value }));
}

/** 行数组 → 对象（**丢掉空行**：空键在后端是 65045） */
export function rowsToMapping(rows: MappingRow[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const row of rows) {
    const key = (row.key ?? '').trim();
    const value = (row.value ?? '').trim();
    if (key === '' || value === '') {
      continue;
    }
    out[key] = value;
  }
  return out;
}
