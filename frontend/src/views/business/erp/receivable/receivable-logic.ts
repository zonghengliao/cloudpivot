/**
 * 应收台账页纯逻辑（SO-15）—— 所有能测的判断都在这里，`index.vue` 只负责渲染。
 *
 * # 字段口径（逐字抄后端，不猜）
 *
 * - 列表 / 详情：`backend/src/business/erp/receivable_query.rs` 的 `ReceivableRes`
 *   → 前端类型 `@/api/business/erp` 的 `Receivable`（`ar_no` / `customer_name` /
 *   `source_no` / `amount` / `received_amount` / `unsettled_amount` / `due_date` / `status`）；
 * - 账龄：`receivable_aging.rs` 的 `ReceivableAgingRes`。**分档口径与应付同源**
 *   （同一份 `BUCKETS` / `bucket_of`），所以前端绝不写死 30/60/90 ——
 *   表头与分桶标签一律按响应里的 `buckets[]` 渲染，后端加档位前端自动跟上。
 * - 核销：坏账核销是 `PATCH /receivable/{id}/write-off`，原因 ≥ 4 字
 *   （`receivable::REASON_MIN_CHARS`）。收款单核销（`biz_receipt_allocation`）属
 *   SO-16/SO-17，**尚未落地** —— 详情里的「核销记录」面板据实显示空态，
 *   不伪造收款单号。
 *
 * # 状态域（唯一真相 `receivable::STATUSES` / `ck_biz_ar_status` / V0290）
 *
 * `OPEN` 未收 → `PARTIAL` 部分已收 → `SETTLED` 已收清；
 * `OPEN`/`PARTIAL` → `WRITTEN_OFF` 坏账核销（终态）；
 * `OPEN`/`PARTIAL` → `CANCELLED` 来源出库作废冲销（终态，V0290）。
 *
 * ⚠️ **不是**应付的那 4 个值（`PARTIAL_PAID` / `PENDING_CANCEL` 传给应收接口会 400）。
 */
import type { PrimaryTableCol } from 'tdesign-vue-next';
import type {
  Receivable,
  ReceivableQuery,
  ReceivableSourceType,
  ReceivableStatus,
} from '@/api/business/erp';

/** 每页条数（与销售订单 / 发货单 / 应付列表同口径；后端上限 200） */
export const RECEIVABLE_PAGE_SIZE = 20;

/** 坏账核销原因长度域（`receivable::REASON_MIN_CHARS` / `REASON_MAX_CHARS`） */
export const WRITE_OFF_REASON_MIN = 4;
export const WRITE_OFF_REASON_MAX = 500;

/** 应收的 5 个权限码（`V0288` 种的 `sys_menu.auth_code`，逐字一致） */
export const RECEIVABLE_PERMS = {
  list: 'business:erp:receivable:list',
  detail: 'business:erp:receivable:detail',
  aging: 'business:erp:receivable:aging',
  create: 'business:erp:receivable:create',
  writeOff: 'business:erp:receivable:write-off',
} as const;

/** 状态中文（兜底；后端 `receivable::status_label` 也下发，正常情况用后端的） */
export const RECEIVABLE_STATUS_LABELS: Record<string, string> = {
  OPEN: '未收',
  PARTIAL: '部分已收',
  SETTLED: '已收清',
  WRITTEN_OFF: '坏账核销',
  CANCELLED: '已作废',
};

/** 状态配色：未收蓝、部分已收橙、已收清绿、坏账核销红、已作废灰 */
export const RECEIVABLE_STATUS_THEMES: Record<
  string,
  'default' | 'primary' | 'success' | 'warning' | 'danger'
> = {
  OPEN: 'primary',
  PARTIAL: 'warning',
  SETTLED: 'success',
  WRITTEN_OFF: 'danger',
  CANCELLED: 'default',
};

/** 状态下拉（值 = 后端域值，标签 = 后端中文，**不是**应付那套） */
export const RECEIVABLE_STATUS_OPTIONS: { label: string; value: ReceivableStatus }[] = [
  { label: '未收', value: 'OPEN' },
  { label: '部分已收', value: 'PARTIAL' },
  { label: '已收清', value: 'SETTLED' },
  { label: '坏账核销', value: 'WRITTEN_OFF' },
  { label: '已作废', value: 'CANCELLED' },
];

/** 来源下拉（`SALES_OUT` = 出库过账自动挂账；`MANUAL` = 手工挂账） */
export const RECEIVABLE_SOURCE_OPTIONS: { label: string; value: ReceivableSourceType }[] = [
  { label: '销售出库', value: 'SALES_OUT' },
  { label: '手工挂账', value: 'MANUAL' },
];

/** 页面筛选状态（v-model 形状；提交前由 {@link buildReceivableQuery} 收敛） */
export interface ReceivableFilterState {
  status: ReceivableStatus | '';
  customerId: string;
  arNo: string;
  sourceType: ReceivableSourceType | '';
  /** 到期日下界 `yyyy-MM-dd`（含） */
  dueFrom: string;
  /** 到期日上界 `yyyy-MM-dd`（含） */
  dueTo: string;
}

/** 空筛选（重置 / 首次进入用） */
export function emptyReceivableFilter(): ReceivableFilterState {
  return { status: '', customerId: '', arNo: '', sourceType: '', dueFrom: '', dueTo: '' };
}

/** 是否带了任何筛选条件（决定列表头上的「已筛选」徽标） */
export function isReceivableFiltered(filter: ReceivableFilterState): boolean {
  return Boolean(
    filter.status ||
      filter.customerId ||
      filter.arNo.trim() ||
      filter.sourceType ||
      filter.dueFrom ||
      filter.dueTo,
  );
}

/**
 * 筛选 → 请求参数。
 *
 * **空串一律不发**：后端把 `?status=` 当「等于空串」过滤（查 0 行），
 * 而 `?customer_id=` 会被当成非法 UUID 直接 400。`ar_no` 两端空格在这里去掉。
 */
export function buildReceivableQuery(
  filter: ReceivableFilterState,
  page: number,
  pageSize: number = RECEIVABLE_PAGE_SIZE,
): ReceivableQuery {
  const query: ReceivableQuery = { page: Math.max(1, Math.trunc(page) || 1), page_size: pageSize };
  if (filter.status) query.status = filter.status;
  if (filter.customerId) query.customer_id = filter.customerId;
  const arNo = filter.arNo.trim();
  if (arNo) query.ar_no = arNo;
  if (filter.sourceType) query.source_type = filter.sourceType;
  if (filter.dueFrom) query.due_date_from = filter.dueFrom;
  if (filter.dueTo) query.due_date_to = filter.dueTo;
  return query;
}

/** 状态中文：优先用后端给的 `status_label`，没给才回落本地表，最后回落原始编码 */
export function receivableStatusLabel(row: Pick<Receivable, 'status' | 'status_label'>): string {
  return row.status_label || RECEIVABLE_STATUS_LABELS[row.status] || row.status || '—';
}

/** 状态配色（未知状态给 `default`，不抛错） */
export function receivableStatusTheme(
  status: string,
): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  return RECEIVABLE_STATUS_THEMES[status] ?? 'default';
}

/** 来源中文（`SALES_OUT` 出库过账 / `MANUAL` 手工挂账；未知原样返回） */
export function sourceTypeLabel(sourceType: string): string {
  if (sourceType === 'SALES_OUT') return '销售出库';
  if (sourceType === 'MANUAL') return '手工挂账';
  return sourceType || '—';
}

/** 只有 `OPEN` / `PARTIAL` 有余额，也**只有这两种**能坏账核销（后端断言同一口径） */
export function canWriteOff(row: Pick<Receivable, 'status'>): boolean {
  return row.status === 'OPEN' || row.status === 'PARTIAL';
}

/**
 * 不能核销的原因（按钮 hover 提示 / 置灰说明）。
 *
 * 三种终态各有各的理由，文案要指名道姓 —— 「按钮灰了不知道为什么」是最烦的体验。
 */
export function writeOffBlockedReason(row: Pick<Receivable, 'status' | 'status_label'>): string {
  if (canWriteOff(row)) return '';
  const label = receivableStatusLabel(row);
  if (row.status === 'SETTLED') return `${label}：钱已经收齐了，没有余额可核销`;
  if (row.status === 'WRITTEN_OFF') return `${label}：已经核销过（终态，不可重复核销）`;
  if (row.status === 'CANCELLED') return `${label}：来源出库单已作废，这笔销售从未发生`;
  return `${label}：只有「未收 / 部分已收」的应收单能核销`;
}

/**
 * 核销原因校验（返回空串 = 通过）。
 *
 * 与后端 `receivable::validate_reason` 同口径：`trim` 后按**字符数**（不是字节数，
 * 中文一个字算一个）判 4..=500，短了 / 长了都直接 400。
 */
export function writeOffReasonError(reason: string | null | undefined): string {
  const text = (reason ?? '').trim();
  const chars = [...text].length;
  if (chars === 0) return '坏账核销必须给出原因（会计估计变更，没原因无法审计）';
  if (chars < WRITE_OFF_REASON_MIN) {
    return `原因至少 ${WRITE_OFF_REASON_MIN} 个字，例如「客户已注销，经理批准核销」`;
  }
  if (chars > WRITE_OFF_REASON_MAX) {
    return `原因不能超过 ${WRITE_OFF_REASON_MAX} 个字（当前 ${chars} 个）`;
  }
  return '';
}

/** 提交前的原因归一：去首尾空格 + 把连续空白折成一个空格（与后端收到的字符串一致） */
export function normalizeWriteOffReason(reason: string): string {
  return reason.trim().replace(/\s+/g, ' ');
}

/**
 * 逾期天数 = 今天 − 到期日（正数 = 逾期，0 = 到期当天，负数 = 未到期）。
 *
 * 与账龄同口径：**到期当天不算逾期**。日期解析一律从 `yyyy-MM-dd` 拆数字自己算，
 * 不用 `Date.parse`（带时区的字符串在东八区会漂一天）。
 */
export function overdueDaysOf(dueDate: string | null | undefined, today: string): number {
  const due = parseDateParts(dueDate);
  const now = parseDateParts(today);
  if (!due || !now) return 0;
  return Math.round((utcMs(now) - utcMs(due)) / 86_400_000);
}

/** 逾期文案（列表 / 详情共用；未逾期返回空串，调用方不渲染徽标） */
export function overdueText(row: Pick<Receivable, 'due_date' | 'status'>, today: string): string {
  if (!hasBalance(row.status)) return '';
  const days = overdueDaysOf(row.due_date, today);
  if (days > 0) return `逾期 ${days} 天`;
  if (days === 0) return '今天到期';
  return '';
}

/** 有余额的状态（`OPEN` / `PARTIAL`）—— 账龄 / 逾期只看这两种 */
export function hasBalance(status: string): boolean {
  return status === 'OPEN' || status === 'PARTIAL';
}

/** 已收比例（0-100，一位小数；金额走整数分，避免浮点误差） */
export function receivedPercent(amount: string | number, received: string | number): number {
  const total = Number(amount) || 0;
  const done = Number(received) || 0;
  if (total <= 0) return 0;
  return Math.min(100, Math.round((done / total) * 1000) / 10);
}

/**
 * 账龄一格占该行 / 合计的比例（0-100，一位小数）。
 *
 * 后端金额是 `NUMERIC` 出的字符串，直接 `Number()` 只是用来算**像素宽度**，
 * 不参与任何会计断言 —— 真金额一律 `formatMoney`（整数分）显示。
 */
export function bucketShare(amount: string | number | null | undefined, total: string | number | null | undefined): number {
  const part = Number(amount ?? 0) || 0;
  const whole = Number(total ?? 0) || 0;
  if (whole <= 0 || part <= 0) return 0;
  return Math.min(100, Math.round((part / whole) * 1000) / 10);
}

/** 账龄天数文案：正数逾期 / 0 到期当天 / 负数未到期（与应付账龄页同一套说法） */
export function agingDaysText(days: number): string {
  if (days > 0) return `逾期 ${days}`;
  if (days === 0) return '到期当天';
  return `未到期 ${-days}`;
}

/**
 * 列表列定义（`colKey` 与 `Receivable` 字段同名，插槽按 colKey 命中）。
 *
 * 卡面要求的 8 列：单号 / 客户 / 来源单号 / 金额 / 已收 / 未收 / 到期日 / 状态。
 *
 * 宽度合计压到 ~1080px：`ar_no` 固定左、`op` 固定右，两端固定列会把中间的列
 * **压在滚动区外**（实测 1500px 视口下「状态」整列看不见，只能横向拖）——
 * 所以这里宁可窄一点让 9 列一屏放得下，也不让最关键的状态列被挤走。
 */
export const RECEIVABLE_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'ar_no', title: '应收单号', width: 150, fixed: 'left' },
  { colKey: 'customer_name', title: '客户', width: 112, ellipsis: true },
  { colKey: 'source_no', title: '来源单号', width: 142, ellipsis: true },
  { colKey: 'amount', title: '应收金额', width: 95, align: 'right' },
  { colKey: 'received_amount', title: '已收', width: 85, align: 'right' },
  { colKey: 'unsettled_amount', title: '未收', width: 95, align: 'right' },
  { colKey: 'due_date', title: '到期日', width: 160 },
  { colKey: 'status', title: '状态', width: 92 },
  { colKey: 'op', title: '操作', width: 150, fixed: 'right' },
];

// ==================== 核销 / 冲销留痕（详情面板） ====================

/**
 * 备注里的核销留痕前缀 —— **逐字**抄后端：
 * - `receivable::write_off` 追加 `[坏账核销] {reason}`；
 * - `receivable::cancel_for_stock_out` 追加 `CANCEL_REASON_PREFIX` = `[出库作废冲销] {reason}`。
 *
 * 后端把原因**同时**写 `biz_status_log.reason` 与单据 `remark`（一行一条，`\n` 分隔），
 * 前端读不到 `biz_status_log`（没有对外接口），所以详情里的「核销记录」就按这两个前缀
 * 从 `remark` 拆 —— 前缀是硬契约，改了后端这里必须同步。
 */
export const WRITE_OFF_REMARK_PREFIX = '[坏账核销]';
export const CANCEL_REMARK_PREFIX = '[出库作废冲销]';

/** 一条核销 / 冲销留痕（详情「核销记录」面板的一行） */
export interface SettlementRecord {
  /** `WRITE_OFF` 坏账核销 / `CANCEL` 出库作废冲销 */
  kind: 'WRITE_OFF' | 'CANCEL';
  /** 中文标题（「坏账核销」/「出库作废冲销」） */
  title: string;
  /** 原因（已去掉前缀与首尾空白） */
  reason: string;
  /** 发生时间：留痕行没有自己的时间，统一用单据的 `settled_at`（后端核销/冲销时落的就是它） */
  time: string | null;
}

/**
 * 从详情拆「核销记录」。
 *
 * 只有 `WRITTEN_OFF` / `CANCELLED` 两种终态才会在后端 append 留痕；
 * `OPEN` / `PARTIAL` / `SETTLED` 一律返回空数组（不要拿「已收金额 > 0」去编收款行）。
 *
 * 时间用 `settled_at`：`write_off` 与 `cancel_for_stock_out` 都在改状态那次 UPDATE 里
 * 写 `settled_at = now()`，所以它就是留痕的发生时刻。
 */
export function settlementRecordsOf(
  detail: Pick<Receivable, 'status' | 'settled_at' | 'remark'>,
): SettlementRecord[] {
  const remark = detail.remark ?? '';
  if (!remark.trim()) return [];
  const records: SettlementRecord[] = [];
  for (const line of remark.split('\n')) {
    const text = line.trim();
    if (text.startsWith(WRITE_OFF_REMARK_PREFIX)) {
      records.push({
        kind: 'WRITE_OFF',
        title: '坏账核销',
        reason: text.slice(WRITE_OFF_REMARK_PREFIX.length).trim(),
        time: detail.settled_at,
      });
    } else if (text.startsWith(CANCEL_REMARK_PREFIX)) {
      records.push({
        kind: 'CANCEL',
        title: '出库作废冲销',
        reason: text.slice(CANCEL_REMARK_PREFIX.length).trim(),
        time: detail.settled_at,
      });
    }
  }
  return records;
}

/**
 * 收款核销（`biz_receipt_allocation`）**后端尚未落地**（SO-16 / SO-17）。
 *
 * 这个开关是给「详情 → 核销记录」面板用的：`false` 时面板据实显示空态 +
 * 一句「收款单模块未上线」，**不伪造收款单号**。SO-17 落地后把它翻成 `true`，
 * 再把收款明细接到 `receiptAllocationsOf()`。
 */
export const RECEIPT_ALLOCATION_READY = false;

/** 收款核销空态文案（已收金额 > 0 时说明「收了钱但明细查不到」的原因） */
export function receiptAllocationEmptyText(received: string | number): string {
  const done = Number(received) || 0;
  if (RECEIPT_ALLOCATION_READY) return '还没有收款核销记录（收款单审核通过后才会核销到本单）';
  if (done > 0) {
    return '本单已收金额不为 0，但收款单核销明细接口（SO-17）尚未上线，暂无法列出是哪几张收款单';
  }
  return '还没有收款核销记录（收款单 / 核销明细模块尚未上线）';
}

// ==================== 账龄看板（同 `receivable_aging`） ====================

/** 账龄基准日的来源中文（后端 `as_of_source`） */
export function agingAsOfSourceText(source: string): string {
  if (source === 'QUERY') return '手动指定';
  if (source === 'PERIOD') return '期间末日';
  if (source === 'TODAY') return '今天';
  return source || '—';
}

/** 账龄表里的期间状态中文（后端 `ReportPeriodStatus`） */
export function agingPeriodStatusText(status: string | null | undefined): string {
  if (status === 'CLOSED') return '已结账';
  if (status === 'DERIVED') return '未开账·按自然月推算';
  if (status === 'OPEN' || status === 'NOT_CLOSED') return '未结账';
  return status || '';
}

/**
 * 账龄档位配色：未到期绿、1-30 蓝、31-60 橙、61-90 橙、90+ 红。
 *
 * 键是后端 `AgingBucketDef.key`（`payable_aging::BUCKETS` 同源），认不出就 `default` ——
 * 后端加档位时最多是灰色，不会白屏。
 */
export function bucketThemeOf(key: string): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  switch (key) {
    case 'NOT_DUE':
      return 'success';
    case 'D0_30':
      return 'primary';
    case 'D31_60':
      return 'warning';
    case 'D61_90':
      return 'warning';
    case 'OVER_90':
      return 'danger';
    default:
      return 'default';
  }
}

/** 账龄明细行状态中文（应收域值，兜底原始编码） */
export function agingStatusText(status: string): string {
  return RECEIVABLE_STATUS_LABELS[status] || status || '—';
}

// ==================== 内部：纯日期算术 ====================

function parseDateParts(value: string | null | undefined): [number, number, number] | null {
  if (!value) return null;
  const match = /^(\d{4})-(\d{2})-(\d{2})/.exec(value.trim());
  if (!match) return null;
  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  if (!Number.isFinite(year) || !Number.isFinite(month) || !Number.isFinite(day)) return null;
  return [year, month, day];
}

function utcMs(parts: [number, number, number]): number {
  return Date.UTC(parts[0], parts[1] - 1, parts[2]);
}
