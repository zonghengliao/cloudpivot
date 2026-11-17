/**
 * 客户信用面板纯逻辑（M3-14）—— 判定文案 / 占用率 / 明细行全在这里，`.vue` 只渲染。
 *
 * # 数据来源只有一个
 *
 * 面板**不自己算钱**：额度、已占、在审、可用、判定全部读后端
 * `GET /api/business/erp/credit/{customer_id}`（M3-10 的 `CreditSnapshotRes`，
 * 见 `@/api/business/erp/credit`）。前端拿 `used + pending` 再减一次看起来也能出数，
 * 但那是第二份公式 —— 它和后端 `credit::decide`（提交守卫用的同一函数）一旦分叉，
 * 屏幕上写着「额度内」、点提交却 409，用户只会认为是 bug。所以这里只做**展示口径**：
 * 格式化、百分比、配色、文案、行顺序。
 *
 * # 三个必须先记住的字段语义（抄错就显示错）
 *
 * 1. **`credit_limit = 0` 是「不控额度」，不是「零额度」**：此时后端 `available` /
 *    `available_after` 都是 `null`（不是 0），所以显示「不控额度」而不是「可用 0.00」；
 * 2. **`available` 已被后端夹在 0 以上**：所以「已超额」只能看 `is_over` / `over`，
 *    不能写 `available < 0`（那个条件永远不成立，超额永远不报警）；
 * 3. **`open_receivable` 是对照口径**：它和 `used` 的口径不同源（`used` 来自有效订单
 *    未核销部分），**不进占用合计**，页面上必须标「对照，不重复计入」，否则财务会拿它
 *    和占用相加。
 *
 * # 金额一律走整数分
 *
 * 后端 `NUMERIC(18,4)` 出 JSON 是字符串（`"1000.0000"`），直接 `+` 会拼字符串。
 * 统一过 `toCents` / `formatMoney`（`../shared/money`，同一份被单测钉住的实现）。
 */
import type { CreditPolicy, CreditSnapshot, CreditVerdictLabel, DecimalText } from '@/api/business/erp/credit';
import { formatMoney, toCents, toNumber } from '../shared/money';

/** 额度 0 的展示语（`available` 为 `null` 时也用它） */
export const CREDIT_UNLIMITED_TEXT = '不控额度';

/** 「接近超额」阈值：占用率 ≥ 80% 转警告色（TDesign 的 warning） */
export const CREDIT_NEAR_LIMIT_PCT = 80;

/** 判定兜底（后端新增变体、前端没跟上时用它 —— 显示原值而不是白屏） */
const UNKNOWN_VERDICT_THEME = 'default' as const;

/** 判定标签的展示元信息（`key` 与后端 `verdict_label` 逐字一致，单测对拍源码） */
export interface CreditVerdictMeta {
  /** 报文枚举名 */
  key: string;
  /** 人话标签（页面上那枚 tag） */
  label: string;
  /** TDesign tag / alert 主题 */
  theme: 'success' | 'warning' | 'danger' | 'default' | 'primary';
  /** 一句话解释（tooltip） */
  hint: string;
  /** 是否「不许提交」 */
  blocked: boolean;
}

/**
 * 五种判定 → 展示（顺序 = 从松到紧；后端 `credit::CreditVerdict` 的 5 个变体）。
 *
 * `WARN` / `PASS` 都是「已超额但按客户策略放行」，区别只在后端有没有写警示台账；
 * 页面上都给 warning（别给 success —— 让销售以为还有余量）。
 */
export const CREDIT_VERDICT_META: Record<CreditVerdictLabel, CreditVerdictMeta> = {
  UNCONTROLLED: {
    key: 'UNCONTROLLED',
    label: '不控额度',
    theme: 'default',
    hint: '信用额度为 0 = 不做额度控制，下单不校验信用',
    blocked: false,
  },
  WITHIN: {
    key: 'WITHIN',
    label: '额度内',
    theme: 'success',
    hint: '占用（含在审单）没有超过信用额度',
    blocked: false,
  },
  WARN: {
    key: 'WARN',
    label: '超限·警告放行',
    theme: 'warning',
    hint: '已超额度，但客户策略是 WARN：提交放行并留一条警示台账',
    blocked: false,
  },
  PASS: {
    key: 'PASS',
    label: '超限·直接放行',
    theme: 'warning',
    hint: '已超额度，但客户策略是 PASS：提交放行且不留痕',
    blocked: false,
  },
  BLOCK: {
    key: 'BLOCK',
    label: '超限·拦截',
    theme: 'danger',
    hint: '已超额度且客户策略是 BLOCK：提交会被拒绝（需调额或收款释放后重试）',
    blocked: true,
  },
};

/** 未知判定兜底：原样显示枚举名（不猜成「额度内」） */
export function creditVerdictMeta(verdict: string | null | undefined): CreditVerdictMeta {
  const key = (verdict ?? '').trim();
  if (key in CREDIT_VERDICT_META) {
    return CREDIT_VERDICT_META[key as CreditVerdictLabel];
  }
  return {
    key: key || 'UNKNOWN',
    label: key || '未知判定',
    theme: UNKNOWN_VERDICT_THEME,
    hint: '后端返回了前端还不认识的判定值，按原值显示（不猜）',
    blocked: false,
  };
}

/** 超限策略的人话（与 `ck_biz_customer_credit_policy` 逐字一致） */
export const CREDIT_POLICY_LABELS: Record<CreditPolicy, string> = {
  BLOCK: '拦截',
  WARN: '警告放行',
  PASS: '直接放行',
};

/**
 * 策略展示：未知值原样返回（后端 `normalize_policy` 对未知值 fail-closed 按 BLOCK 处理，
 * 页面别显示成「放行」）。
 */
export function creditPolicyText(policy: string | null | undefined): string {
  const key = (policy ?? '').trim();
  if (key in CREDIT_POLICY_LABELS) {
    return CREDIT_POLICY_LABELS[key as CreditPolicy];
  }
  return key || '—';
}

/** 额度展示：`0` 明确写成「0.00（不控额度）」，避免被读成「额度用完」 */
export function creditLimitText(snapshot: Pick<CreditSnapshot, 'credit_limit'>): string {
  const cents = toCents(snapshot.credit_limit);
  if (cents === 0) {
    return `${CREDIT_UNLIMITED_TEXT}（0.00）`;
  }
  return formatMoney(cents / 100);
}

/** 可用额度展示：`null`（= 不控额度）与 `0.00`（= 用光了）必须能区分 */
export function creditAvailableText(
  value: DecimalText | null | undefined,
  snapshot?: Pick<CreditSnapshot, 'credit_limit'>,
): string {
  if (value === null || value === undefined) {
    // `null` 只有两种来源：不控额度（额度 ≤ 0，与后端 `decide` 同口径），或后端没给
    const limit = snapshot ? toCents(snapshot.credit_limit) : 0;
    return limit <= 0 ? CREDIT_UNLIMITED_TEXT : '—';
  }
  return formatMoney(value);
}

/** 是否不控额度（`credit_limit <= 0`，与后端 `decide` 的首个分支同口径） */
export function isUncontrolled(snapshot: Pick<CreditSnapshot, 'credit_limit'>): boolean {
  return toCents(snapshot.credit_limit) <= 0;
}

/**
 * 占用率（%）：`占用 ÷ 额度 × 100`，**四舍五入到整数、不夹顶**（超额时给 133 这样的数，
 * 比夹成 100 更能说明「超了多少」）。不控额度 → `null`（没有分母）。
 */
export function creditUsagePercent(
  snapshot: Pick<CreditSnapshot, 'credit_limit' | 'occupied'>,
): number | null {
  const limitCents = toCents(snapshot.credit_limit);
  if (limitCents <= 0) {
    return null;
  }
  const occupiedCents = toCents(snapshot.occupied);
  return Math.round((occupiedCents * 100) / limitCents);
}

/** 进度条百分比：TDesign 只接受 0–100，超额一律画满 */
export function creditProgressPercent(
  snapshot: Pick<CreditSnapshot, 'credit_limit' | 'occupied'>,
): number {
  const percent = creditUsagePercent(snapshot);
  if (percent === null) {
    return 0;
  }
  return Math.min(100, Math.max(0, percent));
}

/**
 * 进度条状态色：超额度 → error；≥80% → warning；其余交给组件默认色。
 *
 * 判「超额」优先用后端 `is_over`（`available` 已被夹到 0 以上，不能用它判断）。
 */
export function creditProgressStatus(
  snapshot: Pick<CreditSnapshot, 'credit_limit' | 'occupied' | 'is_over'>,
): 'success' | 'warning' | 'error' | undefined {
  if (snapshot.is_over) {
    return 'error';
  }
  const percent = creditUsagePercent(snapshot);
  if (percent === null) {
    return undefined;
  }
  if (percent >= 100) {
    return 'error';
  }
  if (percent >= CREDIT_NEAR_LIMIT_PCT) {
    return 'warning';
  }
  return undefined;
}

/** 顶部一句话：不控额度 / 已超 X / 可用 X */
export function creditHeadlineText(
  snapshot: Pick<CreditSnapshot, 'credit_limit' | 'available' | 'over' | 'is_over'>,
): string {
  if (isUncontrolled(snapshot)) {
    return `${CREDIT_UNLIMITED_TEXT}，下单不校验信用`;
  }
  if (snapshot.is_over || toCents(snapshot.over) > 0) {
    return `已超额度 ${formatMoney(snapshot.over)}`;
  }
  return `可用 ${creditAvailableText(snapshot.available, snapshot)}`;
}

/** 明细行（顺序 = 页面展示顺序；`null` 表示该行不该出现） */
export interface CreditPanelRow {
  /** 稳定 key（`data-testid` / `v-for` 用） */
  key: string;
  /** 左侧标签 */
  label: string;
  /** 右侧值（已格式化） */
  value: string;
  /** 右侧补充说明（小字，可空） */
  hint?: string;
  /** 强调级别：`strong` = 主数字，`muted` = 对照口径，`danger` = 异常 */
  tone?: 'strong' | 'muted' | 'danger';
}

/**
 * 明细行：额度 / 已占 / 在审 / 占用 / 可用（+ 有本单时的试算三行，+ 漂移与超额的异常行）。
 *
 * 顺序固定，方便财务按「先看额度、再看占用、最后看可用」的顺序读；
 * `amount` 只在传了 `amount` 试算时出现（否则后端 `order_amount` 为 `null`）。
 */
export function creditPanelRows(snapshot: CreditSnapshot): CreditPanelRow[] {
  const rows: CreditPanelRow[] = [
    {
      key: 'limit',
      label: '信用额度',
      value: creditLimitText(snapshot),
      tone: isUncontrolled(snapshot) ? 'muted' : 'strong',
    },
    {
      key: 'used',
      label: '已占额度',
      value: formatMoney(snapshot.used),
      hint: '有效订单价税合计 − 已核销应收 − 已过账退货',
    },
    {
      key: 'pending',
      label: '在审订单',
      value: formatMoney(snapshot.pending),
      hint: '已提交待审批的订单（不含本单）',
    },
    {
      key: 'occupied',
      label: '占用合计',
      value: formatMoney(snapshot.occupied),
      hint: '已占 + 在审',
      tone: 'strong',
    },
    {
      key: 'available',
      label: '可用额度',
      value: creditAvailableText(snapshot.available, snapshot),
      hint: isUncontrolled(snapshot) ? '额度 0 = 不做额度控制' : '额度 − 占用（下限 0）',
      tone: 'strong',
    },
    {
      key: 'open_receivable',
      label: '未清应收',
      value: formatMoney(snapshot.open_receivable),
      hint: '对照口径，不重复计入占用',
      tone: 'muted',
    },
  ];

  // 试算本单（下单页传 amount，客户详情页不传）—— 不试算时整段不出现，避免「本单 0.00」误导
  if (snapshot.order_amount !== null && snapshot.order_amount !== undefined) {
    rows.push(
      {
        key: 'order_amount',
        label: '本单金额',
        value: formatMoney(snapshot.order_amount),
        hint: '本次订单价税合计（试算）',
      },
      {
        key: 'total',
        label: '试算合计',
        value: formatMoney(snapshot.total),
        hint: '占用 + 本单',
      },
      {
        key: 'available_after',
        label: '本单后可用',
        value: creditAvailableText(snapshot.available_after, snapshot),
        hint: '额度 − 试算合计（下限 0）',
      },
    );
  }

  // 超额：只在真的超了才出现（`over > 0`），否则一行 0.00 只是噪音
  if (toCents(snapshot.over) > 0) {
    rows.push({
      key: 'over',
      label: '超出额度',
      value: formatMoney(snapshot.over),
      hint: creditVerdictMeta(snapshot.verdict).hint,
      tone: 'danger',
    });
  }

  return rows;
}

/**
 * 缓存列对拍文案：`biz_customer.credit_used`（缓存）与现算 `used` 不一致时提示。
 *
 * 空串 = 不漂移（组件据此决定要不要渲染那条 alert）。漂移是运维信号，不是接口错误：
 * 占用永远以现算值为准，缓存列只用来发现「每日重算没跑」。
 */
export function creditDriftText(snapshot: CreditSnapshot): string {
  if (!snapshot.cache_drift) {
    return '';
  }
  return `缓存列 credit_used（${formatMoney(snapshot.cached_used)}）与现算已占（${formatMoney(
    snapshot.used,
  )}）不一致，页面按现算值显示；每日重算任务可能需要补跑`;
}

/** 冻结提示文案（空串 = 正常客户）。冻结比额度更硬：额度充足也不许开单 */
export function creditFrozenText(snapshot: Pick<CreditSnapshot, 'customer_status'>): string {
  return snapshot.customer_status === 'FROZEN'
    ? '客户已冻结：与额度无关，任何新单都不能提交（解冻后可继续）'
    : '';
}

/** 口径说明：优先用后端下发的 `basis`（唯一真相），缺失时用本地兜底 */
export const CREDIT_BASIS_FALLBACK =
  '已占 = 有效订单价税合计 − 已核销应收 − 已过账退货（下限 0）；在审 = 已提交待审批订单；' +
  '占用 = 已占 + 在审；可用 = 额度 − 占用（下限 0，额度 0 = 不控额度）；未清应收为对照口径，不重复计入';

/** 取口径说明（后端 `basis` 为空时退回本地常量） */
export function creditBasisText(snapshot?: Pick<CreditSnapshot, 'basis'> | null): string {
  const text = (snapshot?.basis ?? '').trim();
  return text || CREDIT_BASIS_FALLBACK;
}

/** 占用率文案：`40%（占用 40,000.00 / 额度 100,000.00）`；不控额度 → `不控额度` */
export function creditUsageText(snapshot: CreditSnapshot): string {
  const percent = creditUsagePercent(snapshot);
  if (percent === null) {
    return CREDIT_UNLIMITED_TEXT;
  }
  return `${percent}%（占用 ${formatMoney(snapshot.occupied)} / 额度 ${formatMoney(
    snapshot.credit_limit,
  )}）`;
}

/**
 * 信用额度输入框的即时提示（用的还是 `credit_limit` 的**字符串**原值，避免把
 * `"100000.5000"` 先转 number 再格式化时丢精度）。
 */
export function creditFormHint(limit: DecimalText | null | undefined, policy: string): string {
  const cents = toCents(limit);
  if (cents <= 0) {
    return '0 = 不控额度：下单不校验信用，超限策略也不生效';
  }
  return `额度 ${formatMoney(cents / 100)}，超限策略：${creditPolicyText(policy)}`;
}

/** 金额 → 数字（面板里算比例用；`null` / `''` / 非法值都按 0） */
export function creditNumber(value: DecimalText | null | undefined): number {
  return toNumber(value);
}
