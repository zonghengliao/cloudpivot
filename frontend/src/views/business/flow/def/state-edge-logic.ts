/**
 * 状态机边配置面板的**纯逻辑**（卡 t_a392a5da）。
 *
 * 为什么单拆一个 `.ts`：口径（哪条边禁不掉、哪个开关锁死、`confirm` 怎么传、理由多长）
 * 全在这里，「哪个状态出哪个按钮 / 少传一个字段会怎样」被 `vue-tsc` 抓不到，
 * 只有写成函数才能被 `pnpm test:unit` 钉住（同 `erp/period/period-logic.ts` 的做法）。
 *
 * # 三条与后端逐字对齐的硬口径（`backend/src/business/shared/state_machine.rs`
 *   的 `upsert_edge_config` + `backend/src/business/flow/edge_admin.rs` 的 `EdgeUpsertReq`）
 *
 * 1. **`confirm: true` 必须显式传**：缺了直接 400 / `10001`
 *    （真机实测报文：「改状态机配置需要二次确认（confirm = true）」）。
 * 2. **`mandatory` 边禁不掉**：内核拒绝 `enabled = false`；前端把开关做成只读并给原因，
 *    别让用户点完才吃 400。
 * 3. **`requires_reason` 只能加不能减**：BASE 边 `requires_reason = true` 时，
 *    配置层必须也为 `true`（前端锁死开关并标注「BASE 要求」）。
 */
import type {
  EntityStateEdges,
  StateEdge,
  StateEdgeChangeReq,
  StateEdgeConfigRow,
  StateEdgePendingChange,
} from '@/api/business/flow/state-edge';

/** 两个权限码（与 `edge_admin.rs::PERM_LIST / PERM_MANAGE`、`V0206` 逐字一致） */
export const STATE_EDGE_PERMISSIONS = {
  /** 读：BASE 边 / 当前配置 / 有效边 */
  list: 'business:flow:state-edge:list',
  /** 改 / 删配置（V0206 只授 SUPER_ADMIN） */
  manage: 'business:flow:state-edge:manage',
} as const;

/** 变更理由的最小长度（前端提示用；后端不强制，但审批单标题要能看懂） */
export const MIN_CHANGE_REASON_CHARS = 4;

/** 变更表单的本地形状（全是可控字符串 / 布尔） */
export interface EdgeChangeForm {
  /** 是否启用 */
  enabled: boolean;
  /** 是否要求原因 */
  requires_reason: boolean;
  /** 显示文案（空串 = 用 BASE 默认） */
  label: string;
  /** 追加的权限码（空串 = 不追加） */
  auth_code: string;
  /** 变更理由 */
  reason: string;
}

/** 一条边的表格行（视图只负责画，不再做判断） */
export interface EdgeRowView {
  /** 行 key（entity:from>to，跨实体唯一） */
  key: string;
  /** 变更前状态 */
  from: string;
  /** 变更后状态 */
  to: string;
  /** 边文案（后端下发） */
  label: string;
  /** 硬约束 / 可选 */
  mandatoryText: string;
  /** 硬约束边的 tag 主题 */
  mandatoryTheme: 'primary' | 'default';
  /** 必须填原因 / 不要求原因 */
  reasonText: string;
  /** 权限码（`—` 表示不追加） */
  authCodeText: string;
  /** 当前配置（已配置 / 默认） */
  configuredText: string;
  /** 是否启用（启用 / 已禁用） */
  enabledText: string;
  /** 生效状态 tag 主题 */
  enabledTheme: 'success' | 'danger';
  /** 是否允许「禁用」（mandatory 边不允许） */
  canDisable: boolean;
  /** 「必须填原因」开关是否锁死（BASE 要求 → 锁死） */
  reasonLocked: boolean;
  /** 原始数据（点击变更时用它回填表单） */
  edge: StateEdge;
}

/** 行 key：`entity:from>to` */
export function edgeRowKey(entity: string, from: string, to: string): string {
  return `${entity}:${from}>${to}`;
}

/**
 * 把后端下发的「BASE 边 + 配置行 + 有效边」三份数据**叠成一张表**（只做投影，不做状态机推理）。
 *
 * 为什么必须叠：`base_edges` 是代码里的真相（`EdgeRes::of_base` 里 `configured` 恒 false、
 * `enabled` 恒 true），**被配置禁用的边只会在 `effective_edges` 里消失**、配置值只在 `configs` 里。
 * 页面上要显示「这条边现在到底启不启用 / 有没有被改过文案」，就得按 (from, to) 把三份对起来。
 *
 * 叠加规则（全部来自后端字段，前端不新增任何判断）：
 * - 有配置行 → `configured = true`，`enabled / requires_reason / label / auth_code` 用配置行的值；
 * - 无配置行 → `configured = false`，`enabled / requires_reason / label` 用 BASE 边的值；
 * - `requires_reason` 取 `BASE || 配置`（内核口径：只能加不能减，所以两者取或就是真值）；
 * - 启用以配置行为准；配置行缺失时才看 `effective_edges` 里有没有这条边。
 */
export function mergeBaseEdges(view: EntityStateEdges): StateEdge[] {
  const configOf = (from: string, to: string): StateEdgeConfigRow | undefined =>
    view.configs.find((row) => row.from === from && row.to === to);
  const effectiveOf = (from: string, to: string): StateEdge | undefined =>
    view.effective_edges.find((edge) => edge.from === from && edge.to === to);

  return view.base_edges.map((base) => {
    const config = configOf(base.from, base.to);
    const configLabel = config?.label && config.label.trim() ? config.label : null;
    return {
      from: base.from,
      to: base.to,
      label: configLabel ?? base.label,
      mandatory: base.mandatory,
      requires_reason: base.requires_reason || config?.requires_reason === true,
      auth_code: config?.auth_code ?? null,
      configured: Boolean(config),
      enabled: config ? config.enabled : Boolean(effectiveOf(base.from, base.to)),
    };
  });
}

/** 把一条边投影成表格行（所有中文文案在这里定，视图里不许再写判断） */
export function edgeRowView(entity: string, edge: StateEdge): EdgeRowView {
  return {
    key: edgeRowKey(entity, edge.from, edge.to),
    from: edge.from,
    to: edge.to,
    label: edge.label,
    mandatoryText: edge.mandatory ? '硬约束' : '可选',
    mandatoryTheme: edge.mandatory ? 'primary' : 'default',
    reasonText: edge.requires_reason ? '必须填原因' : '不要求原因',
    authCodeText: edge.auth_code ?? '—',
    configuredText: edge.configured ? '已配置' : '默认',
    enabledText: edge.enabled ? '启用' : '已禁用',
    enabledTheme: edge.enabled ? 'success' : 'danger',
    canDisable: !edge.mandatory,
    reasonLocked: edge.requires_reason,
    edge,
  };
}

/** 回填变更表单：配置值优先，没有配置行就用 BASE 边（后端 `effective_edges` 已经是合并结果） */
export function changeFormOf(edge: StateEdge): EdgeChangeForm {
  return {
    enabled: edge.enabled,
    requires_reason: edge.requires_reason,
    // BASE 默认文案不算「配置覆盖」：回填成空串，用户不改就不写死一份文案到库里
    label: edge.configured ? edge.label : '',
    auth_code: edge.auth_code ?? '',
    reason: '',
  };
}

/** 变更理由校验（返回 `null` = 通过） */
export function changeReasonError(reason: string): string | null {
  const text = reason.trim();
  if (!text) {
    return '请填写变更理由（会写进审批单标题，复核人靠它判断该不该批）';
  }
  if (text.length < MIN_CHANGE_REASON_CHARS) {
    return `变更理由至少 ${MIN_CHANGE_REASON_CHARS} 个字（当前 ${text.length} 个）`;
  }
  return null;
}

/**
 * 拼 `PUT /business/flow/state-edge` 的请求体。
 *
 * - `confirm` 恒为 `true`（用户点「提交审批」这一下就是二次确认；后端缺了直接 400）；
 * - `label` / `auth_code` 空串归一成 `null`（后端 `Option<String>`，空串会被当成「把文案改成空」）；
 * - `requires_reason` 对 BASE 要求原因的边**强制 true**（只能加不能减，前端先挡一层）。
 */
export function buildChangeReq(entity: string, edge: StateEdge, form: EdgeChangeForm): StateEdgeChangeReq {
  const label = form.label.trim();
  const authCode = form.auth_code.trim();
  const reason = form.reason.trim();
  return {
    entity,
    from_status: edge.from,
    to_status: edge.to,
    enabled: form.enabled,
    requires_reason: edge.requires_reason ? true : form.requires_reason,
    label: label ? label : null,
    auth_code: authCode ? authCode : null,
    confirm: true,
    reason: reason ? reason : null,
  };
}

/** 变更内容的「人话」摘要（确认弹窗 / 审批前让人再看一眼） */
export function changeSummary(edge: StateEdge, form: EdgeChangeForm): string {
  const parts: string[] = [];
  parts.push(`${form.enabled ? '启用' : '禁用'}`);
  if (edge.requires_reason || form.requires_reason) {
    parts.push('必须填原因');
  }
  if (form.label.trim()) {
    parts.push(`文案改为「${form.label.trim()}」`);
  }
  if (form.auth_code.trim()) {
    parts.push(`追加权限码 ${form.auth_code.trim()}`);
  }
  return `${parts.join(' · ')}`.trim();
}

/** 在跑审批单的一行文案（entity 维度展示用） */
export function pendingChangeText(item: StateEdgePendingChange): string {
  const who = item.initiator_name ? `${item.initiator_name} 发起` : '发起人未知';
  return `${item.instance_no} · ${item.from} → ${item.to} · ${item.summary}（${who}）`;
}

/**
 * 「已送审」结果的提示文案。
 *
 * `applied = false` 是**成功路径**（等审批）；只有 `applied = true` 才是「已直接生效」
 * （流程被下线 + 持 skip 码 + 理由齐全的显式跳过）。两者文案必须分得清，
 * 否则用户会把「已送审」当成「已生效」。
 */
export function changeResultText(res: { applied: boolean; instance_no: string | null; message: string }): string {
  if (!res.applied) {
    return `已提交审批${res.instance_no ? ` ${res.instance_no}` : ''}：审批通过后才会生效（驳回或撤回则配置不变）`;
  }
  return res.message || '已直接生效';
}
