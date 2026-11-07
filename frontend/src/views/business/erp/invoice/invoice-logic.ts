/**
 * 销售发票页纯逻辑（SO-21）—— 所有能测的判定都在这里，`index.vue` 只负责渲染。
 *
 * # 口径逐字抄后端，不猜
 *
 * | 语义 | 出处（`backend/src/business/erp/invoice.rs`） |
 * |------|------|
 * | 状态域 4 值 | `invoice::STATUSES` / `ck_biz_invoice_status`（V0298） |
 * | 状态中文 | `invoice::status_label`（前端只做兜底） |
 * | 票种 2 值 | `invoice::TYPES` / `ck_biz_invoice_type` |
 * | 状态机边 | `state_machine::INVOICE_EDGES`（4 条，经 `invoice::LEGAL_EDGES` 导出） |
 * | 作废必给原因 | `INVOICE_REASON_EDGES` = `DRAFT→VOID` / `ISSUED→VOID` |
 * | 终态 | `INVOICE_LOCKED` = `POSTED` / `VOID` |
 * | 原因长度 | `invoice::REASON_MIN_CHARS` = 4 / `REASON_MAX_CHARS` = 500（按**字符**数） |
 * | 额度口径 | `invoiced_total`：`SUM(total_with_tax)` WHERE `receivable_id=?` AND `status <> 'VOID'` |
 * | 金额三列 | `resolve_amounts` / `split_tax`：先定不含税 `round(total/(1+rate),4)`，税额 = 合计 − 不含税 |
 *
 * # 为什么状态域必须在这里再钉一次
 *
 * 发票的 `DRAFT/ISSUED/POSTED/VOID` 与收款单的 `…CANCELLED`、应收的 `OPEN/PARTIAL/…`
 * 是**三套值**。后端 `InvoicePageQuery::validate` 是白名单、错一个就 400，但 TS 里
 * `status` 只是 `string`，抄错了 `vue-tsc` 抓不到 —— 所以 `INVOICE_STATUSES` /
 * `INVOICE_EDGES` 由 `tests/unit/erp-invoice.test.ts` 与后端源码逐条比对钉住。
 *
 * # 为什么金额一律走整数分
 *
 * 后端 `Decimal` 出 JSON 是字符串（`"1130.0000"`），前端 `+` 会变成字符串拼接。
 * 「这张应收还能开多少」是**会算错钱**的地方（多开一分后端 409），
 * 累加 / 比较全部过 `toCents`，只在显示前换回来（`views/business/erp/shared/money.ts`）。
 */
import type { PrimaryTableCol } from 'tdesign-vue-next';
import type { Invoice, InvoiceQuery, InvoiceStatus, InvoiceType } from '@/api/business/erp';
import { formatCents, toCents } from '../shared/money';

/** 每页条数（与销售订单 / 发货单 / 应收 / 收款同口径；后端上限 200） */
export const INVOICE_PAGE_SIZE = 20;

/** 开票弹窗里「未收应收」一次拉多少张（种子规模够用；后端上限 200） */
export const RECEIVABLE_FETCH_LIMIT = 200;

/** 作废原因长度域（`invoice::REASON_MIN_CHARS` / `REASON_MAX_CHARS`，按**字符**数） */
export const INVOICE_REASON_MIN = 4;
export const INVOICE_REASON_MAX = 500;

/** 发票默认税率 13%（**百分数**：`invoice::default_tax_rate` = 13，FIX-6 统一标度） */
export const INVOICE_DEFAULT_TAX_RATE = 13;

/** 发票的 4 个权限码（`V0299` 种的 `sys_menu.auth_code`，逐字一致） */
export const INVOICE_PERMS = {
  list: 'business:erp:invoice:list',
  detail: 'business:erp:invoice:detail',
  create: 'business:erp:invoice:create',
  status: 'business:erp:invoice:status',
} as const;

/** 状态域（`invoice::STATUSES`，顺序与后端一致） */
export const INVOICE_STATUSES: InvoiceStatus[] = ['DRAFT', 'ISSUED', 'POSTED', 'VOID'];

/**
 * 状态机边（`state_machine::INVOICE_EDGES`，ARCH-7 §4.5）。
 *
 * 四条边一条不多一条不少，**故意**没有：
 * - `POSTED→VOID`：已归档（已交付 / 已报税留档）的发票不能再作废；
 * - 任何 `→DRAFT`：发票一旦取号就不能原地重开。
 */
export const INVOICE_EDGES: [InvoiceStatus, InvoiceStatus][] = [
  ['DRAFT', 'ISSUED'],
  ['ISSUED', 'POSTED'],
  ['DRAFT', 'VOID'],
  ['ISSUED', 'VOID'],
];

/** 需要原因才能走的边（`INVOICE_REASON_EDGES`）：两条作废边都要原因 */
export const INVOICE_REASON_EDGES: [InvoiceStatus, InvoiceStatus][] = [
  ['DRAFT', 'VOID'],
  ['ISSUED', 'VOID'],
];

/**
 * 入账态（`state_machine::INVOICE_LOCKED` = `ISSUED` / `POSTED` / `VOID`）：
 * 一旦开票（凭证已生成、应收已回写）金额就不可再改，更正只能作废重开。
 */
export const INVOICE_LOCKED: InvoiceStatus[] = ['ISSUED', 'POSTED', 'VOID'];

/** 状态中文兜底（正常用后端 `status_label`；缺失时才回落这里） */
export const INVOICE_STATUS_LABELS: Record<string, string> = {
  DRAFT: '草稿',
  ISSUED: '已开票',
  POSTED: '已归档',
  VOID: '已作废',
};

/** 状态配色：草稿灰、已开票蓝（凭证已生成）、已归档绿、已作废灰 */
export const INVOICE_STATUS_THEMES: Record<
  string,
  'default' | 'primary' | 'success' | 'warning' | 'danger'
> = {
  DRAFT: 'default',
  ISSUED: 'primary',
  POSTED: 'success',
  VOID: 'default',
};

/** 状态下拉（值 = 后端域值，标签 = 后端中文） */
export const INVOICE_STATUS_OPTIONS: { label: string; value: InvoiceStatus }[] = INVOICE_STATUSES.map(
  (status) => ({ label: INVOICE_STATUS_LABELS[status] ?? status, value: status }),
);

/** 票种域（`invoice::TYPES`） */
export const INVOICE_TYPES: InvoiceType[] = ['OUT', 'OUT_RED'];

/** 票种中文（`invoice::type_label`） */
export const INVOICE_TYPE_LABELS: Record<string, string> = {
  OUT: '销项正票',
  OUT_RED: '销项红字票',
};

/** 票种下拉（`OUT_RED` 也列出来：列表里真会有退货流程生成的红字票，筛选要能选） */
export const INVOICE_TYPE_OPTIONS: { label: string; value: InvoiceType }[] = INVOICE_TYPES.map(
  (type) => ({ label: INVOICE_TYPE_LABELS[type] ?? type, value: type }),
);

/** 来源类型中文（`invoice::SOURCE_TYPES`；后端不下发中文，前端只做展示映射） */
export const INVOICE_SOURCE_LABELS: Record<string, string> = {
  SALES_ORDER: '销售订单',
  SHIPMENT: '发货单',
  SALES_OUT: '销售出库',
  MANUAL: '手工挂账',
};

/**
 * 来源展示文案：「来源单号（来源类型中文）」。
 *
 * 手工挂账这类 `source_no` 本身就是占位码 `MANUAL`，拼起来会成「MANUAL（MANUAL）」
 * 或「MANUAL（手工挂账）」这种噪声 —— 所以**单号为空或等于类型码时只显示类型中文**。
 */
export function invoiceSourceText(row: {
  source_type?: string | null;
  source_no?: string | null;
}): string {
  const no = (row.source_no ?? '').trim();
  const type = (row.source_type ?? '').trim();
  if (!no && !type) return '—';
  if (!type) return no || '—';
  const shown = INVOICE_SOURCE_LABELS[type] ?? type;
  if (!no || no === type) return shown;
  return `${no}（${shown}）`;
}

/**
 * 列表列（order 决定列序；金额列右对齐、窄列定宽）。
 *
 * 宽度合计 **1121px**：1600 视口、侧栏展开时 `t-table__content` 实测约 1288px，放得下；
 * 1280 视口（968px）会横向滚动 —— `invoice_no` 固定左、`op` 固定右，
 * 被盖住的只会是中间的「税额 / 价税合计」，所以单号列不再加宽（20 位 mono 不能折行）。
 */
export const INVOICE_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'invoice_no', title: '发票号', width: 160, fixed: 'left' },
  // 关联应收单号：卡面要求「可点击跳转」，列宽按 `AR-yyyyMMdd-000001`（19 位 mono）+ 链接留白
  { colKey: 'receivable_no', title: '关联应收', width: 150 },
  { colKey: 'customer_name', title: '客户', width: 120, ellipsis: true },
  { colKey: 'invoice_date', title: '开票日期', width: 106 },
  { colKey: 'amount', title: '金额(不含税)', width: 112, align: 'right' },
  { colKey: 'tax_amount', title: '税额', width: 96, align: 'right' },
  { colKey: 'total_with_tax', title: '价税合计', width: 108, align: 'right' },
  { colKey: 'status', title: '状态', width: 88 },
  // 操作列最宽的一档是 DRAFT / ISSUED：详情 + 开票/归档 + 作废 三个 text 按钮 ≈ 170px，
  // 再窄「作废」会折行（本仓 TDesign 固定列实测），所以 190。
  { colKey: 'op', title: '操作', width: 190, fixed: 'right' },
];

/** 页面筛选状态（v-model 形状；提交前由 {@link buildInvoiceQuery} 收敛） */
export interface InvoiceFilterState {
  status: InvoiceStatus | '';
  invoiceType: InvoiceType | '';
  /** 发票号 / 客户名（模糊，后端两列 `LIKE`） */
  invoiceNo: string;
  /** 开票日期下界 `yyyy-MM-dd`（含） */
  dateFrom: string;
  /** 开票日期上界 `yyyy-MM-dd`（含） */
  dateTo: string;
}

/** 空筛选（首次进入 / 重置） */
export function emptyInvoiceFilter(): InvoiceFilterState {
  return { status: '', invoiceType: '', invoiceNo: '', dateFrom: '', dateTo: '' };
}

/** 是否带了任何筛选（决定列表头上的「已筛选」徽标） */
export function isInvoiceFiltered(filter: InvoiceFilterState): boolean {
  return Boolean(
    filter.status ||
      filter.invoiceType ||
      filter.invoiceNo.trim() ||
      filter.dateFrom ||
      filter.dateTo,
  );
}

/**
 * 筛选 → 请求参数。
 *
 * **空串一律不发**：后端把 `?status=` 当白名单校验里的空值放行但仍参与过滤（查 0 行），
 * 而 `?invoice_no=` 是模糊键，发空串等于没筛 —— 所以统一在 `buildInvoiceQuery` 里收敛，
 * 别指望 URL 拼装层。
 */
export function buildInvoiceQuery(
  filter: InvoiceFilterState,
  page: number,
  pageSize: number = INVOICE_PAGE_SIZE,
): InvoiceQuery {
  const query: InvoiceQuery = { page: Math.max(1, Math.trunc(page) || 1), page_size: pageSize };
  if (filter.status) query.status = filter.status;
  if (filter.invoiceType) query.invoice_type = filter.invoiceType;
  const invoiceNo = filter.invoiceNo.trim();
  if (invoiceNo) query.invoice_no = invoiceNo;
  if (filter.dateFrom) query.date_from = filter.dateFrom;
  if (filter.dateTo) query.date_to = filter.dateTo;
  return query;
}

/** 状态中文：后端 `status_label` 优先，没给才回落本地表，最后回落编码 */
export function invoiceStatusLabel(row: Pick<Invoice, 'status' | 'status_label'>): string {
  return row.status_label || INVOICE_STATUS_LABELS[row.status] || row.status || '—';
}

/** 状态配色（未知状态回落灰） */
export function invoiceStatusTheme(status: string): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  return INVOICE_STATUS_THEMES[status] ?? 'default';
}

/** 票种中文：后端 `invoice_type_label` 优先，没给才回落本地表 */
export function invoiceTypeLabel(row: Pick<Invoice, 'invoice_type' | 'invoice_type_label'>): string {
  return row.invoice_type_label || INVOICE_TYPE_LABELS[row.invoice_type] || row.invoice_type || '—';
}

/** 税率 → 百分比文案（**百分数口径**：`13` → `13%`；0 税率显示 `0%`；缺失 / 非数字显示 `—`） */
export function taxRateText(value: number | string | null | undefined): string {
  if (value === null || value === undefined || value === '') return '—';
  const rate = typeof value === 'number' ? value : Number(value);
  if (!Number.isFinite(rate)) return '—';
  // FIX-6：库里的 tax_rate 已是百分数（13 = 13%），不再 ×100
  const percent = Math.round(rate * 100) / 100;
  return `${percent}%`;
}

/**
 * 价税合计 → (不含税, 税额)，**整数分**，两者相加恒等于合计。
 *
 * 镜像后端 `invoice::split_tax`：先定不含税（`total / (1 + rate)`），
 * 再让税额 = 合计 − 不含税 —— 反过来先定税额会有 1 分尾差撞
 * `ck_biz_invoice_amount`。后端定标 4 位、这里定标到分（面板只做预览，
 * 权威金额以后端回读的 `amount` / `tax_amount` 为准；提交时也只发合计）。
 */
export function splitTaxCents(
  totalCents: number,
  taxRate: number = INVOICE_DEFAULT_TAX_RATE,
): { amountCents: number; taxCents: number } {
  if (!Number.isFinite(totalCents) || totalCents === 0) {
    return { amountCents: 0, taxCents: 0 };
  }
  const rate = Number.isFinite(taxRate) ? taxRate : 0;
  // FIX-6：税率是百分数（13 = 13%）⇒ 除数 = 1 + 13/100
  const divisor = 1 + rate / 100;
  if (divisor <= 0) return { amountCents: totalCents, taxCents: 0 };
  const amountCents = Math.round(totalCents / divisor);
  return { amountCents, taxCents: totalCents - amountCents };
}

/**
 * 某张应收的**已开票价税合计**（整数分）。
 *
 * 口径与后端 `invoice::invoiced_total` 逐字一致：**作废（VOID）的票不占额度**，
 * 其余状态（含 DRAFT —— 草稿已经占了号，后端建单时就判额度）都算。
 * `excludeId` 用于「详情里算不算自己」，列表侧一般不用。
 */
export function invoicedTotalCents(
  invoices: Pick<Invoice, 'status' | 'total_with_tax' | 'id'>[],
  excludeId?: string,
): number {
  return invoices.reduce((sum, item) => {
    if (item.status === 'VOID') return sum;
    if (excludeId && item.id === excludeId) return sum;
    return sum + toCents(item.total_with_tax);
  }, 0);
}

/**
 * 剩余可开票额度（整数分）= 应收金额 − 已开票合计（排除 VOID）。
 *
 * 镜像后端 `invoice::headroom_of`。**可能为负**（历史数据 / 手工改库导致已超额），
 * 调用方据此判断「还能不能开」，别在这层夹到 0 —— 夹了就看不出「已经开超了」。
 */
export function invoiceHeadroomCents(
  receivableAmount: number | string | null | undefined,
  invoices: Pick<Invoice, 'status' | 'total_with_tax' | 'id'>[],
): number {
  return toCents(receivableAmount) - invoicedTotalCents(invoices);
}

/** 额度文案（负数标红由调用方决定样式，这里只给文字） */
export function headroomText(cents: number): string {
  return formatCents(cents);
}

// ==================== 状态动作 ====================

/** 从当前状态能去哪（按 `INVOICE_EDGES` 推，顺序与后端 `describe_next` 一致） */
export function allowedNextStatuses(from: string): InvoiceStatus[] {
  return INVOICE_EDGES.filter(([source]) => source === from).map(([, target]) => target);
}

/** 这条边是否合法（前端闸；后端仍会再判一次，别拿它当唯一防线） */
export function canTransition(from: string, to: string): boolean {
  return INVOICE_EDGES.some(([source, target]) => source === from && target === to);
}

/** 这条边是否必须给原因 */
export function edgeNeedsReason(from: string, to: string): boolean {
  return INVOICE_REASON_EDGES.some(([source, target]) => source === from && target === to);
}

/** 开票（DRAFT → ISSUED） */
export function canIssue(row: Pick<Invoice, 'status'>): boolean {
  return canTransition(row.status, 'ISSUED');
}

/** 归档（ISSUED → POSTED） */
export function canArchive(row: Pick<Invoice, 'status'>): boolean {
  return canTransition(row.status, 'POSTED');
}

/** 作废（DRAFT / ISSUED → VOID） */
export function canVoid(row: Pick<Invoice, 'status'>): boolean {
  return canTransition(row.status, 'VOID');
}

/**
 * 作废按钮的置灰原因（空串 = 可以点）。
 *
 * `POSTED` 的文案要**说出为什么**：用户会以为「归档了还不能作废？」——
 * 后端这条边根本不存在（ARCH-7 §4.5），要冲这张票只能走红字票。
 */
export function voidBlockedReason(row: Pick<Invoice, 'status'>): string {
  if (canVoid(row)) return '';
  if (row.status === 'POSTED') return '已归档是终态：不能再作废，需开红字票冲销';
  if (row.status === 'VOID') return '已作废（终态）';
  return `当前状态 ${row.status} 不能作废`;
}

/** 作废影响提示（ISSUED 来的会红冲收入凭证 + 撤回应收的发票号，务必让用户看见） */
export function voidImpactText(row: Pick<Invoice, 'status'>): string {
  return row.status === 'ISSUED'
    ? '⚠️ 这张票已开票（收入凭证已生成）：作废会生成红字凭证冲销收入，并把应收单上的发票号撤掉。'
    : '这张票还是草稿（账上没动）：作废只改状态，不产生凭证。';
}

/** 作废原因校验（空串 = 通过；与后端 `validate_reason` 同口径：去空白、4~500 字） */
export function invoiceReasonError(raw: string): string {
  const text = (raw ?? '').trim();
  const length = [...text].length;
  if (length < INVOICE_REASON_MIN) {
    return `作废原因至少 ${INVOICE_REASON_MIN} 个字（说明为什么作废这张发票）`;
  }
  if (length > INVOICE_REASON_MAX) {
    return `作废原因最多 ${INVOICE_REASON_MAX} 个字，当前 ${length} 个字`;
  }
  return '';
}

// ==================== 开票弹窗 ====================

/** 开票弹窗的表单形状（`v-model` 用；提交前由 {@link invoiceFormError} 校验） */
export interface InvoiceFormState {
  /** 关联应收（必填：本页只做「从应收开票」，手工开票走应收页手工挂账后的同一入口） */
  receivableId: string;
  /** 开票日期 `yyyy-MM-dd` */
  invoiceDate: string;
  /** 价税合计（元；自动带出 = 应收金额，可改小做分次开票） */
  total: number;
  /** 税率（**百分数** 0~100，默认 13 = 13%；FIX-6 与订单 / 收货同口径） */
  taxRate: number;
  /** 手填税控号（空 = 后端取 `IV-yyyyMMdd-000001`） */
  invoiceNo: string;
  /** 发票代码（可空） */
  invoiceCode: string;
  remark: string;
}

/**
 * 开票表单校验（空串 = 通过）。
 *
 * `receivableAmount` / `invoicedCents` 是选中应收的金额与**已开票合计**，
 * 用来做「不超过应收」的前端闸 —— 后端同一把锁会再判一次（409 点名三个数字），
 * 前端这道只是省一次往返 + 让用户马上看到还能开多少。
 */
export function invoiceFormError(
  form: InvoiceFormState,
  context: {
    receivableAmount?: number | string | null;
    invoicedCents?: number;
    hasReceivable: boolean;
  },
): string {
  if (!form.receivableId || !context.hasReceivable) {
    return '请先选择要开票的应收单';
  }
  // 「额度已用尽」优先于「金额 ≤ 0」：选到一张开满的应收时自动带出的金额是 0，
  // 先报「必须大于 0」会让用户以为是自己填错，真正的根因是这张应收没额度了。
  const headroom = toCents(context.receivableAmount) - (context.invoicedCents ?? 0);
  if (headroom <= 0) {
    return '这张应收已经没有可开票额度了（已开票价税合计已达应收金额）';
  }
  const totalCents = toCents(form.total);
  if (totalCents <= 0) {
    return '价税合计必须大于 0';
  }
  const rate = Number(form.taxRate);
  if (!Number.isFinite(rate) || rate < 0 || rate > 100) {
    return '税率必须在 0 ~ 100 之间（百分数，13 = 13%）';
  }
  if (totalCents > headroom) {
    return `价税合计 ${formatCents(totalCents)} 超过这张应收的剩余可开票额度 ${formatCents(headroom)}`;
  }
  if (form.invoiceNo.trim().length > 64) {
    return '发票号最多 64 个字符';
  }
  if (form.invoiceCode.trim().length > 32) {
    return '发票代码最多 32 个字符';
  }
  return '';
}

/** 应收下拉项文案：单号 + 客户 + 金额 + 到期日 +（已开票时）状态提示 */
export function receivableOptionLabel(ar: {
  ar_no: string;
  customer_name: string;
  amount: number | string;
  due_date: string;
  status_label?: string;
  invoice_no?: string | null;
}): string {
  const parts = [ar.ar_no, ar.customer_name || '（客户已删除）', formatCents(toCents(ar.amount))];
  if (ar.due_date) parts.push(`到期 ${ar.due_date}`);
  if (ar.invoice_no) parts.push(`已开票 ${ar.invoice_no}`);
  return parts.join(' · ');
}

/** 这张票「下一步做什么」的一句话提示（详情抽屉的引导文案） */
export function nextActionHint(row: Pick<Invoice, 'status'>): string {
  switch (row.status) {
    case 'DRAFT':
      return '草稿：确认金额无误后点「开票」→ 生成收入凭证（借 1122 / 贷 5001 + 2221.02）并回写应收的发票号。';
    case 'ISSUED':
      return '已开票：凭证已生成。交付 / 报税留档后点「归档」；要冲销就点「作废」（生成红字凭证）。';
    case 'POSTED':
      return '已归档（终态）：账务上已定稿，不能再作废；需要冲销请开红字票。';
    case 'VOID':
      return '已作废（终态）：作废时已红冲凭证、撤回应收的发票号，额度已释放。';
    default:
      return `未知状态「${row.status}」：只认 DRAFT / ISSUED / POSTED / VOID。`;
  }
}
