/**
 * 附件必传闸门（**ATT-08**）：把 `GET /api/business/attachment/check` 的结果翻译成
 * 「按钮能不能点 / 旁边红字写什么 / tooltip 说什么」这三件事。
 *
 * # 为什么翻译逻辑在这一层（而不是各页面自己拼）
 *
 * 三个单据页都要画同一套东西。各自拼文案的话，同一个缺口会出现三种说法
 * （「缺少发票影像」「请上传发票」「还差 1 个附件」），用户会以为是三件不同的事。
 * 所以：**判定与文案都来自后端**（`missing[].hint` / `missing[].message`），
 * 这里只做「拼装 + 降级」，且是纯函数（可单测穷举，不必起组件）。
 *
 * # 降级口径（**故意不禁用按钮**）
 *
 * 查询失败（403 无权看 / 404 单据没了 / 500 规则表读不出来）时：
 *
 * - **不禁用**按钮 —— 一次数据库抖动或一次权限口径调整，都不该把所有人的
 *   「提交」按钮锁死；真正的闸门在后端（绕过前端直接提交仍然 400）。
 * - **不画红字** —— 不能把「不知道」显示成「缺附件」，那会让用户白找半天。
 *
 * 唯一的例外是 `can_proceed = false` 的正常响应：那是后端明确的判定，必须照着禁用。
 */

/** 一条缺口（结构上等价于后端 `AttachmentCheckMissingRes` / 前端 `AttachmentCheckMissing`）。 */
export interface GateMissingLike {
  /** 需求的人话名（`发票影像` / `银行回单` …） */
  role_label: string;
  /** 要求的扩展名（`pdf/ofd/xml`）；`null` = 不限格式 */
  need: string | null;
  /** 命中白名单的附件数 */
  have: number;
  /** 至少要几个 */
  min: number;
  /** 该规则范围内的附件总数（不分格式） */
  total: number;
  /** 还差几个 */
  shortfall: number;
  /** 按钮旁的一行红字（后端拼） */
  hint: string;
  /** 完整人话（与 400 报文一致） */
  message: string;
}

/** 校验结果（结构上等价于后端 `AttachmentCheckRes` / 前端 `AttachmentCheckResult`）。 */
export interface GateCheckLike {
  /** 能不能放行 */
  can_proceed: boolean;
  /** 动作中文（`提交` / `确认` / `生效`） */
  at_label: string;
  /** 缺口清单 */
  missing: GateMissingLike[];
  /** 合并后的红字；后端没给时由 `missing` 兜底 */
  hint?: string | null;
  /** 合并后的完整报文；后端没给时由 `missing` 兜底 */
  message?: string | null;
}

/** 闸门状态（页面直接用这三项渲染）。 */
export interface AttachmentGate {
  /** 按钮能不能点（`false` → disabled） */
  canProceed: boolean;
  /** 按钮旁红字；`null` = 不画 */
  hint: string | null;
  /** 点 disabled 按钮的 tooltip；`null` = 不挂 tooltip */
  tooltip: string | null;
  /**
   * 是否拿到了**有效判定**。
   *
   * - `false`：还在查 / 查询失败降级 → 页面既不画红字也不加 tooltip，
   *   但按钮**照常可点**（理由见文件头「降级口径」）；
   * - `true`：`canProceed` 是后端给的真判定。
   */
  resolved: boolean;
  /** 降级原因（仅排查用，界面不显示；`null` = 没有降级） */
  degradedReason: string | null;
  /** 动作中文（tooltip 前缀「无法提交」用；未知时为 null） */
  atLabel: string | null;
}

/** 「查询中 / 还没单据」的初始态：不禁用、不画红字。 */
export function pendingGate(): AttachmentGate {
  return {
    canProceed: true,
    hint: null,
    tooltip: null,
    resolved: false,
    degradedReason: null,
    atLabel: null,
  };
}

/**
 * 查询失败时的降级态：**不禁用按钮、不画红字**（见文件头）。
 *
 * @param reason 失败原因（只用于排查，不显示给用户）
 */
export function degradedGate(reason: string | null): AttachmentGate {
  return {
    canProceed: true,
    hint: null,
    tooltip: null,
    resolved: false,
    degradedReason: reason ?? null,
    atLabel: null,
  };
}

/** 后端没给 `hint` 时的兜底红字：逐条拼 `missing[].hint`（不自己编造格式说明）。 */
export function summarizeMissing(missing: GateMissingLike[]): string | null {
  const hints = missing
    .map((item) => (item.hint ?? '').trim())
    .filter((text) => text.length > 0);
  if (!hints.length) return null;
  return hints.join('；');
}

/** 后端没给 `message` 时的兜底报文（tooltip 用）：逐条 `missing[].message`。 */
export function summarizeDetail(missing: GateMissingLike[]): string | null {
  const lines = missing
    .map((item) => (item.message ?? '').trim())
    .filter((text) => text.length > 0);
  if (!lines.length) return null;
  return lines.join('\n');
}

/**
 * 校验结果 → 闸门状态。
 *
 * # tooltip 的形状
 *
 * ```text
 * 无法提交：缺少发票影像（需 pdf/ofd/xml 格式）
 * 发票必须上传发票影像（pdf/ofd/xml）：至少要 1 个，当前 0 个（该单据共 0 个附件），还差 1 个
 * ```
 *
 * 第一行是「为什么点不了 + 缺什么」，第二行是后端 400 的原文（还差几个、现有是什么格式）。
 * 两行都在：只给短句用户不知道为什么，只给长句 tooltip 会挤成一团看不清重点。
 */
export function gateFromCheck(result: GateCheckLike): AttachmentGate {
  const atLabel = (result.at_label ?? '').trim() || null;
  if (result.can_proceed) {
    return {
      canProceed: true,
      hint: null,
      tooltip: null,
      resolved: true,
      degradedReason: null,
      atLabel,
    };
  }
  const missing = result.missing ?? [];
  const hint = (result.hint ?? '').trim() || summarizeMissing(missing);
  const detail = (result.message ?? '').trim() || summarizeDetail(missing);
  const headline = hint
    ? `无法${atLabel ?? '提交'}：${hint}`
    : `无法${atLabel ?? '提交'}：缺少必传附件（后端未给出具体说明）`;
  const tooltip = detail && detail !== hint ? `${headline}\n${detail}` : headline;
  return {
    canProceed: false,
    // 后端既没给 hint 也没有 missing（理论上不会发生）：仍要画一句，否则按钮灰着没有解释
    hint: hint ?? '缺少必传附件',
    tooltip,
    resolved: true,
    degradedReason: null,
    atLabel,
  };
}

/**
 * 按钮的 tooltip 文案：**权限优先，其次附件缺口**。
 *
 * 两件事都缺时先说权限：没有权限的人连上传都做不了，告诉他「缺附件」只会让他去点上传再撞 403。
 * 两者都没有 → 返回 `''`（调用方据此 `:disabled="!tip"`，不挂空 tooltip）。
 */
export function gateButtonTip(gate: AttachmentGate, permissionTip?: string | null): string {
  const permission = (permissionTip ?? '').trim();
  if (permission) return permission;
  return (gate.tooltip ?? '').trim();
}
