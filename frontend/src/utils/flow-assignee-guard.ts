/**
 * `65018`（无可用审批人）→「指派审批人」引导框的**全局接线**（FIX-1）。
 *
 * # 为什么做成全局（而不是每个提交按钮各接一遍）
 *
 * 发起审批的入口散布在十几个页面：销售单 / 采购单 / 报销 / 预算 / 盘点 / 处置 /
 * 应付作废 / 凭证红冲 …。新环境（只有 admin 一个账号）里它们的失败**完全同源**：
 * 审批节点绑的角色没人 → 兜底超管又正好是制单人 → 内控（职责分离）剔光 → 422 `65018`。
 *
 * 逐个页面接一遍必然漏一个（漏的那个就是下次的「新环境跑不通」），所以接线放在
 * `api/request.ts` 的响应拦截器里：**任何**请求拿到 `65018` 都走这里。
 *
 * # 三个出口（与卡面 ② 一一对应）
 *
 * | 情形 | 行为 |
 * |------|------|
 * | 有 `data` 载荷 + 当前用户有 `system:setup:manage` | 弹框 → 选人指派 → **自动重试原请求** |
 * | 有载荷但无权限 | 提示「请联系管理员为该角色指派审批人」（不弹框、不可自助） |
 * | 无载荷（如流程预演接口） / 预演类 URL | 什么都不做，交回调用方按原样报错 |
 *
 * # 为什么用「模块级单例 + Promise」而不是 Pinia
 *
 * 对话框要**把结果回传给拦截器**（决定要不要重试），Pinia state 存 Promise 解析器
 * 既不好测也不好读。这里用一个极小的 `pending` 单例：拦截器 `await` 一个 Promise，
 * 对话框关闭时 resolve 它。组件与拦截器之间没有循环依赖（本文件**不 import 任何
 * 运行时模块**，只 import 类型）。
 */

/** 审批流「无可用审批人」业务码（`shared/error_code.rs::FLOW_NO_ASSIGNEE`）。 */
export const FLOW_NO_ASSIGNEE_CODE = 65018;

/** 指派审批人所需权限码（`system/setup/handler.rs` 上的 `#[nik::permission]`）。 */
export const SETUP_MANAGE_PERMISSION = 'system:setup:manage';

/** 向导只读权限码（决定要不要在登录后自动弹向导）。 */
export const SETUP_READ_PERMISSION = 'system:setup:read';

/**
 * 后端 `65018` 载荷（`business::flow::no_assignee::NoAssigneePayload` 的镜像）。
 *
 * 字段名就是**后端契约**（`flow/no_assignee.rs` 有单测钉住字段集合），
 * 改这里必须同步改那边。
 */
export interface NoAssigneePayload {
  /** 流程实例 id（发起失败时是「已回滚」的那个 id，仅供排障） */
  flow_instance_id?: string;
  /** 实例节点行 id（`biz_flow_instance_node.id`） */
  node_id?: string;
  /** 节点 key（如 `n_sales`） */
  node_key: string;
  /** 节点名称（如「销售主管审批」） */
  node_name: string;
  /** 审批人来源（`ROLE` / `USER` / `DEPT_LEADER` …） */
  assignee_type: string;
  /** 角色 id（来源是 ROLE 且角色存在时） */
  role_id?: string;
  /** 角色编码（前端据此调指派接口，跨环境稳定） */
  role_code?: string;
  /** 角色名称 */
  role_name?: string;
  /** 角色在职成员数（0 = 角色没人） */
  member_count: number;
  /** 其中「启用且非机器人」的人数 */
  usable_member_count: number;
  /** 解析出的候选人数（去重前） */
  candidate_count: number;
  /** 机读原因码（`ROLE_NO_MEMBER` / `SELF_ONLY` / `DEPT_NO_LEADER` …） */
  reason: string;
  /** 原因的人读说明 */
  reason_text: string;
  /** 被内控 / 自审剔掉的人（「张三：发起人本人」） */
  excluded?: string[];
  /** 下一步怎么办 */
  fix_hint: string;
}

/** 原因码 → 一句人话（与后端 `no_assignee::Reason::text()` 对齐；后端漏了也有兜底）。 */
const REASON_TEXT: Record<string, string> = {
  NODE_NOT_CONFIGURED: '该节点没有配置审批人来源',
  ROLE_NOT_FOUND: '节点绑定的角色已被删除或改名',
  ROLE_NO_MEMBER: '该角色下没有任何在职成员',
  ROLE_MEMBER_UNAVAILABLE: '该角色有成员记录，但成员均已停用或已删除',
  ROBOT_ONLY: '该节点只解析到机器人账号，而节点不允许机器人审批',
  SELF_ONLY: '该节点解析到的审批人都是制单人 / 发起人本人（内控不允许自审）',
  USER_UNAVAILABLE: '节点指定的审批人已停用或已删除',
  DEPT_NO_LEADER: '该节点绑定的部门没有设置负责人',
  SUPERIOR_NOT_FOUND: '发起人没有部门，或其上级部门没有设置负责人',
  TASK_PARTY_MISSING: '关联任务的负责人 / 创建人 / 分派人缺失',
  FORM_VAR_MISSING: '表单里用于指定审批人的变量没有填写或填写有误',
  NO_CANDIDATE: '该节点没有任何可用审批人',
};

/** 载荷的一句话原因（优先用后端 `reason_text`，其次按码查表）。 */
export function reasonText(payload: NoAssigneePayload): string {
  return payload.reason_text || REASON_TEXT[payload.reason] || '该节点没有可用审批人';
}

/**
 * 这个原因能不能靠「给角色加人」解决。
 *
 * `ROLE_NOT_FOUND` / `NODE_NOT_CONFIGURED` 加人**没用**（要去改流程定义），
 * 前端不应该给出「选人」按钮，否则用户点完还是 422。
 */
export function canFixByAssigningRole(payload: NoAssigneePayload): boolean {
  if (!payload.role_code) {
    return false;
  }
  return !['ROLE_NOT_FOUND', 'NODE_NOT_CONFIGURED', 'DEPT_NO_LEADER', 'SUPERIOR_NOT_FOUND'].includes(
    payload.reason,
  );
}

/**
 * 预演类接口：`65018` 是它们的**正常返回值**（预演就是在报告「按现在的配置解析出谁」），
 * 弹框只会挡住流程设计器。
 */
const PREVIEW_URL_PATTERNS: RegExp[] = [
  /\/flow\/node\/task\/preview/,
  /\/flow\/def\/[^/]+\/preview/,
  /\/flow\/instance\/preview/,
];

/** 拦截器是否可以对这个 URL 触发引导。 */
export function isGuardableUrl(url?: string): boolean {
  if (!url) {
    return true;
  }
  return !PREVIEW_URL_PATTERNS.some((pattern) => pattern.test(url));
}

/** 一条待处理的指派请求（对话框关闭时 resolve）。 */
interface PendingFix {
  /** 后端载荷 */
  payload: NoAssigneePayload;
  /** 关闭对话框：`true` = 已指派、可以重试原请求 */
  resolve: (assigned: boolean) => void;
}

/**
 * 待处理请求的**单例**（同一时刻只可能有一个：用户一次只能点一次提交）。
 *
 * 用 `let` + 订阅者列表而不是 Vue `ref`：这个模块被 axios 拦截器在**任何**时刻调用，
 * 不依赖 Vue 的响应式上下文（组件侧通过 [`subscribePendingFix`] 拿到变化）。
 */
let pending: PendingFix | null = null;
const listeners = new Set<(value: PendingFix | null) => void>();

/** 订阅待处理请求的变化（组件 `onMounted` 订阅、`onBeforeUnmount` 取消）。 */
export function subscribePendingFix(listener: (value: PendingFix | null) => void): () => void {
  listeners.add(listener);
  listener(pending);
  return () => {
    listeners.delete(listener);
  };
}

/** 当前待处理请求（组件初始化用）。 */
export function currentPendingFix(): PendingFix | null {
  return pending;
}

function emit(): void {
  for (const listener of listeners) {
    listener(pending);
  }
}

/** 打开引导框并等用户决定（`true` = 已指派，拦截器据此重试）。 */
export function requestAssigneeFix(payload: NoAssigneePayload): Promise<boolean> {
  // 上一次没关的（理论上不会发生）先当作「放弃」，避免 Promise 泄漏
  if (pending) {
    pending.resolve(false);
  }
  return new Promise<boolean>((resolve) => {
    pending = { payload, resolve };
    emit();
  });
}

/** 对话框关闭：`assigned` 决定要不要重试原请求。 */
export function settleAssigneeFix(assigned: boolean): void {
  const current = pending;
  pending = null;
  emit();
  current?.resolve(assigned);
}

/**
 * 拦截器入口：`65018` 要不要弹框？弹了要不要重试？
 *
 * # 返回值
 *
 * - `true`：用户完成指派 → 调用方**重试原请求**；
 * - `false`：不处理（无载荷 / 预演接口 / 用户放弃 / 无权限）→ 调用方按原样报错。
 *
 * # 无权限时为什么不弹框
 *
 * 卡面 ④：普通用户看到「请联系管理员为该角色指派审批人」，**不能**自己指派。
 * 这里给一条明确提示（带上角色名与节点名），比让用户点开一个必然 403 的弹框好。
 */
export async function offerAssigneeFix(
  error: { code?: number; data?: unknown },
  url?: string,
): Promise<boolean> {
  if (error.code !== FLOW_NO_ASSIGNEE_CODE) {
    return false;
  }
  const payload = normalizePayload(error.data);
  if (!payload || !isGuardableUrl(url)) {
    return false;
  }
  if (!hasPermission(SETUP_MANAGE_PERMISSION)) {
    // 动态 import 只为避免把 tdesign 拽进 request.ts 的静态依赖图（登录页也要能跑）
    const { MessagePlugin } = await import('tdesign-vue-next');
    const target = payload.role_name ? `角色「${payload.role_name}」` : `节点「${payload.node_name}」`;
    void MessagePlugin.warning(
      `请联系管理员为${target}指派审批人（${reasonText(payload)}）`,
    );
    return false;
  }
  // 弹框（`ROLE_NOT_FOUND` / `NODE_NOT_CONFIGURED` 这类「加人也解决不了」的原因，
  // 对话框里不给选人按钮，只给「去改流程定义」的说明 —— 见 `FlowAssigneeDialog.vue`）
  return await requestAssigneeFix(payload);
}

/** 把 `unknown` 的 `data` 收窄成载荷（字段不全就当没有，绝不猜）。 */
export function normalizePayload(data: unknown): NoAssigneePayload | null {
  if (!data || typeof data !== 'object') {
    return null;
  }
  const raw = data as Record<string, unknown>;
  if (typeof raw.node_key !== 'string' || typeof raw.node_name !== 'string') {
    return null;
  }
  const asString = (value: unknown): string | undefined =>
    typeof value === 'string' && value.trim() !== '' ? value : undefined;
  const asNumber = (value: unknown): number =>
    typeof value === 'number' && Number.isFinite(value) ? value : 0;
  return {
    flow_instance_id: asString(raw.flow_instance_id),
    node_id: asString(raw.node_id),
    node_key: raw.node_key,
    node_name: raw.node_name,
    assignee_type: asString(raw.assignee_type) ?? '',
    role_id: asString(raw.role_id),
    role_code: asString(raw.role_code),
    role_name: asString(raw.role_name),
    member_count: asNumber(raw.member_count),
    usable_member_count: asNumber(raw.usable_member_count),
    candidate_count: asNumber(raw.candidate_count),
    reason: asString(raw.reason) ?? 'NO_CANDIDATE',
    reason_text: asString(raw.reason_text) ?? '',
    excluded: Array.isArray(raw.excluded)
      ? raw.excluded.filter((item): item is string => typeof item === 'string')
      : undefined,
    fix_hint: asString(raw.fix_hint) ?? '',
  };
}

/**
 * 当前登录人是否持有某权限码。
 *
 * 为什么不让本模块直接 `useUserStore()`：`api/request.ts` 静态引入本模块，而 store 依赖
 * `api/auth` → `api/request`，静态互相引用会形成运行时环（浏览器里表现为
 * 「useUserStore is not defined」这类难查的启动期错误）。
 *
 * 改用**注册制**：布局组件（`BasicLayout`）在挂载时把 `userStore.hasPermission` 注册进来。
 * 未注册（登录页 / 极早期）按「无权限」处理 —— fail-closed，不会误弹一个必然 403 的框。
 */
let permissionResolver: ((code: string) => boolean) | null = null;

/** 注册权限判定函数（`BasicLayout` 挂载时调用）。 */
export function registerPermissionResolver(resolver: (code: string) => boolean): void {
  permissionResolver = resolver;
}

/** 当前登录人是否持有某权限码（未注册 ⇒ `false`）。 */
export function hasPermission(code: string): boolean {
  return permissionResolver ? permissionResolver(code) : false;
}
