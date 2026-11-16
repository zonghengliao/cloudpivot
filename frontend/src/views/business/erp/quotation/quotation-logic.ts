/**
 * 报价单页面的纯逻辑（M3-06）。
 *
 * # 为什么逻辑单独一个 `.ts`
 *
 * 「哪些边要原因」「能不能转单」「合计怎么来的」「请求发了什么」一旦漂移，
 * `vue-tsc` 抓不到（两边都是 `string` / `number`），线上表现是**算错钱**或**按钮点出 409**。
 * 这里全部做成纯函数，`tests/unit/quotation-page.test.ts` 用真后端常量对拍。
 *
 * # 断言口径照哪抄（不照卡面）
 *
 * - `backend/app/src/business/erp/sales/quotation_model.rs`：`status_label` / `allowed_targets`
 * - `backend/crates/cloudpivot-kernel/src/business/shared/state_machine.rs`：`QUOTATION_EDGES`、
 *   `QUOTATION_REASON_EDGES`
 * - `backend/app/src/business/erp/sales/quotation_handler.rs`：`MAX_LINES`、`line_amounts`
 *   （金额 = `round4(单价 × 数量)`，税 = `round4(金额 × 税率 / 100)`）
 * - `backend/app/migrations/business/V0433__quotation_permissions.up.sql`：5 个权限码
 *
 * # 三条口径（前端最容易写错的地方）
 *
 * 1. **发出 ≠ 改状态**：`DRAFT → SENT` 是审批闸后的动作，走 `submit-approval`；
 *    直接 `PATCH status=SENT` 后端一律 409 + `66088`；
 * 2. **审批不是状态**：`DRAFT` 的单子也可能在审批中，判「待批」只看 `approval` 段
 *    （列表行没有该段 → 不显示待批，别拿 `status` 猜）；
 * 3. **转单只认 `ACCEPTED`**，且已转过的单不重复建单（后端幂等，前端按钮也置灰并说明）。
 */
import type {
  ConvertQuotationReq,
  ConvertQuotationRes,
  CreateQuotationReq,
  DecimalText,
  ExpiredFilter,
  Quotation,
  QuotationLineReq,
  QuotationPageQuery,
  QuotationStatus,
  UpdateQuotationReq,
} from '@/api/business/erp/quotation';

/** 每页条数（与后端 `normalize_page` 默认 20 对齐） */
export const PAGE_SIZE = 20;

/** 单张报价单最多明细行（后端 `quotation_handler::MAX_LINES`） */
export const MAX_LINES = 200;

/** 行备注上限（库层 `biz_quotation_line.remark VARCHAR(500)`） */
export const MAX_LINE_REMARK = 500;

/** 单头备注上限（库层 TEXT；页面按 500 收，够写付款条件 / 报价说明） */
export const MAX_REMARK = 500;

/** 跳过审批的理由下限（后端 `approval_gate::authorize_missing_flow` 要求 ≥10 字） */
export const MIN_SKIP_REASON = 10;

/** 转单备注前缀（后端自己会拼来源，这里只在提示里说清） */
export const CONVERT_SOURCE_HINT = '订单备注会自动带「由报价单 QT-… 转换」，来源不会被覆盖';

/** 合法边（与 `state_machine::QUOTATION_EDGES` 逐条一致，顺序也一致） */
export const QUOTATION_EDGES: Array<[QuotationStatus, QuotationStatus]> = [
  ['DRAFT', 'SENT'],
  ['SENT', 'ACCEPTED'],
  ['SENT', 'REJECTED'],
  ['DRAFT', 'CANCELLED'],
  ['SENT', 'CANCELLED'],
  ['ACCEPTED', 'CANCELLED'],
];

/** 需要原因的边（与 `QUOTATION_REASON_EDGES` 逐条一致） */
export const QUOTATION_REASON_EDGES: Array<[QuotationStatus, QuotationStatus]> = [
  ['SENT', 'REJECTED'],
  ['SENT', 'CANCELLED'],
  ['ACCEPTED', 'CANCELLED'],
];

/** 边的动作名（按钮文案；后端 `QUOTATION_BASE` 的 label 同源） */
export const EDGE_LABELS: Record<string, string> = {
  'DRAFT->SENT': '发出报价',
  'SENT->ACCEPTED': '客户接受',
  'SENT->REJECTED': '客户拒绝',
  'DRAFT->CANCELLED': '作废草稿',
  'SENT->CANCELLED': '撤回已发出的报价',
  'ACCEPTED->CANCELLED': '作废已接受的报价',
};

/** 一条可执行的状态动作 */
export interface StatusAction {
  /** 目标状态 */
  target: QuotationStatus;
  /** 按钮文案 */
  label: string;
  /** 按钮主题 */
  theme: 'primary' | 'success' | 'danger' | 'warning' | 'default';
  /** 这条边要不要填原因 */
  needsReason: boolean;
}

/** 边的主题（拒绝 / 作废走红黄，正向走主色） */
function edgeTheme(from: QuotationStatus, target: QuotationStatus): StatusAction['theme'] {
  if (target === 'ACCEPTED') {
    return 'success';
  }
  if (target === 'REJECTED') {
    return 'danger';
  }
  if (target === 'CANCELLED') {
    return 'warning';
  }
  if (from === 'DRAFT' && target === 'SENT') {
    return 'primary';
  }
  return 'default';
}

/**
 * 当前状态可执行的动作列表（按 `allowed_targets` 过滤，顺序按边表）。
 *
 * `DRAFT → SENT` 不在里面：发出走审批（`submit-approval`），不由「改状态」触发。
 * 这样按钮列表里不会出现一个「点了必然 409」的动作。
 *
 * 必须带 `status`：「作废」在三条边上都出现（DRAFT / SENT / ACCEPTED 各一条），
 * 只按 `to` 过滤会把三条边全列出来。
 */
export function statusActionsOf(
  status: string,
  allowed: QuotationStatus[] | undefined,
): StatusAction[] {
  const list = allowed ?? [];
  return QUOTATION_EDGES.filter(([from, to]) => from === status && to !== 'SENT' && list.includes(to)).map(
    ([from, to]) => ({
      target: to,
      label: EDGE_LABELS[`${from}->${to}`] ?? `${to}`,
      theme: edgeTheme(from, to),
      needsReason: needsReason(from, to),
    }),
  );
}

/** 这条边要不要原因（后端 `needs_reason` 同源） */
export function needsReason(from: string, to: string): boolean {
  return QUOTATION_REASON_EDGES.some(([f, t]) => f === from && t === to);
}

// ==================== 列表筛选 ====================

/** 列表筛选状态 */
export interface FilterState {
  /** 单号模糊 */
  quote_no: string;
  /** 状态（`''` = 全部） */
  status: QuotationStatus | '';
  /** 客户（`''` = 全部） */
  customer_id: string;
  /** 有效期三态 */
  expired: ExpiredFilter;
}

/** 空筛选 */
export function emptyFilter(): FilterState {
  return { quote_no: '', status: '', customer_id: '', expired: 'ALL' };
}

/**
 * 筛选 → 分页入参。
 *
 * - 空串**整键缺席**（后端把 `?status=` 当「等于空串」过滤，会查出 0 行）；
 * - `expired=ALL` → 不传 `expired`（三态口径：不传 = 全部）；
 * - `page_size` 恒带（后端 0 = 默认 20，上限 200）。
 */
export function buildQuery(state: FilterState, page: number, pageSize = PAGE_SIZE): QuotationPageQuery {
  return {
    quote_no: state.quote_no.trim() || undefined,
    status: state.status || undefined,
    customer_id: state.customer_id || undefined,
    expired: state.expired === 'EXPIRED' ? true : state.expired === 'ACTIVE' ? false : undefined,
    page,
    page_size: pageSize,
  };
}

/** 有筛选吗（空态文案要分「没数据」和「筛没了」两种） */
export function isFiltered(state: FilterState): boolean {
  return (
    state.quote_no.trim() !== '' ||
    state.status !== '' ||
    state.customer_id !== '' ||
    state.expired !== 'ALL'
  );
}

/** 空态文案 */
export function emptyListText(filtered: boolean): string {
  return filtered
    ? '没有符合筛选条件的报价单：清空条件或换一段时间再查'
    : '还没有报价单：点右上角「新建报价单」开第一张（有效期是必填条款）';
}

// ==================== 金额 ====================

/** 保留 4 位小数（与后端 `round_dp(4, MidpointAwayFromZero)` 同口径；只用于前端预览） */
export function round4(value: number): number {
  if (!Number.isFinite(value)) {
    return 0;
  }
  return Math.round(value * 10000) / 10000;
}

/** 数值化（`''` / null / 非法 → 0） */
export function toNumber(value: DecimalText | null | undefined): number {
  if (value === null || value === undefined || value === '') {
    return 0;
  }
  const parsed = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

/** 一行金额三项（与后端 `line_amounts` 同序：金额 → 税额 → 含税） */
export function lineAmountsOf(line: {
  qty: DecimalText | null | undefined;
  price: DecimalText | null | undefined;
  tax_rate: DecimalText | null | undefined;
}): { amount: number; taxAmount: number; withTax: number } {
  const amount = round4(toNumber(line.price) * toNumber(line.qty));
  const taxAmount = round4((amount * toNumber(line.tax_rate)) / 100);
  return { amount, taxAmount, withTax: round4(amount + taxAmount) };
}

/** 合计三项（明细逐行先各自 round4，再求和 —— 与后端 `Totals::of` 同口径） */
export function totalsOf(
  lines: Array<{ qty: DecimalText | null | undefined; price: DecimalText | null | undefined; tax_rate: DecimalText | null | undefined }>,
): { amount: number; tax: number; withTax: number } {
  let amount = 0;
  let tax = 0;
  for (const line of lines) {
    const one = lineAmountsOf(line);
    amount = round4(amount + one.amount);
    tax = round4(tax + one.taxAmount);
  }
  return { amount, tax, withTax: round4(amount + tax) };
}

// ==================== 建单 / 改单表单 ====================

/** 一行草稿（`key` 只给 `v-for` 用，不进请求体） */
export interface LineDraft {
  /** 前端行 key（稳定，不提交） */
  key: string;
  /** 物料 id（`''` = 未选） */
  material_id: string;
  /** 数量（`''` = 未填；`t-input-number` 清空时给空串） */
  qty: number | string;
  /** 不含税单价（同上） */
  price: number | string;
  /** 税率百分数（同上） */
  tax_rate: number | string;
  /** 行备注 */
  remark: string;
}

/** 表单草稿 */
export interface FormDraft {
  /** 客户 id */
  customer_id: string;
  /** 报价日期 */
  quote_date: string;
  /** 有效期至 */
  valid_until: string;
  /** 单头备注 */
  remark: string;
  /** 明细 */
  items: LineDraft[];
}

/** 行 key 自增（同一秒内多次 add 也不撞） */
let lineSeed = 0;

/** 新建一行草稿 */
export function newLine(): LineDraft {
  lineSeed += 1;
  return { key: `qt-line-${lineSeed}`, material_id: '', qty: 1, price: '', tax_rate: '', remark: '' };
}

/** 北京时间今天（与后端 `today_cst()` 同口径） */
export function todayCst(): string {
  return new Date(Date.now() + 8 * 60 * 60 * 1000).toISOString().slice(0, 10);
}

/** 日期 + N 天（`yyyy-MM-dd`，跨月跨年交给 Date 算） */
export function addDays(date: string, days: number): string {
  const base = new Date(`${date}T00:00:00Z`);
  base.setUTCDate(base.getUTCDate() + days);
  return base.toISOString().slice(0, 10);
}

/**
 * 空表单。
 *
 * 有效期默认「今天 + 14 天」只是**页面预填**，用户可以改：
 * 后端不默认（给多少天是商业决定），但一个空白日期控件会让 90% 的人先吃一次 400。
 */
export function emptyForm(today = todayCst()): FormDraft {
  return {
    customer_id: '',
    quote_date: today,
    valid_until: addDays(today, 14),
    remark: '',
    items: [newLine()],
  };
}

/** 详情 → 编辑表单（只有 DRAFT 可编辑；明细整单替换，所以一行不落地带回来） */
export function formFrom(body: Quotation): FormDraft {
  const items: LineDraft[] =
    body.items.length > 0
      ? body.items.map((line) => {
          lineSeed += 1;
          return {
            key: `qt-line-${lineSeed}`,
            material_id: line.material_id,
            qty: toNumber(line.qty),
            price: toNumber(line.price),
            tax_rate: toNumber(line.tax_rate),
            remark: line.remark ?? '',
          };
        })
      : [newLine()];
  return {
    customer_id: body.customer_id,
    quote_date: body.quote_date,
    valid_until: body.valid_until,
    remark: body.remark ?? '',
    items,
  };
}

/** 一行草稿 → 请求体（金额不传：后端按单价 × 数量现算） */
export function linePayloadOf(line: LineDraft): QuotationLineReq {
  const remark = line.remark.trim();
  return {
    material_id: line.material_id,
    qty: toNumber(line.qty),
    price: toNumber(line.price),
    tax_rate: toNumber(line.tax_rate),
    remark: remark || null,
  };
}

/** 表单 → 建单请求体 */
export function buildCreatePayload(form: FormDraft): CreateQuotationReq {
  const remark = form.remark.trim();
  return {
    customer_id: form.customer_id,
    quote_date: form.quote_date,
    valid_until: form.valid_until,
    remark: remark || null,
    items: form.items.map(linePayloadOf),
  };
}

/** 表单 → 改单请求体（明细整单替换：传空数组会被后端拒，所以这里总带全量） */
export function buildUpdatePayload(form: FormDraft): UpdateQuotationReq {
  return buildCreatePayload(form);
}

/**
 * 建单 / 改单前的前端校验（后端同款规则，早一步给人话）。
 *
 * 顺序按「用户会先看到哪个错」排：客户 → 日期 → 有效期 → 明细。
 * 返回 `''` 表示通过。
 */
export function validateForm(form: FormDraft): string {
  if (!form.customer_id) {
    return '请选择客户：报价单必须挂在客户上（冻结客户不能开新报价）';
  }
  if (!form.quote_date) {
    return '请选择报价日期（留空时后端按今天 CST，但这里已经清空了就不能提交）';
  }
  if (!form.valid_until) {
    return '请填有效期至：有效期是报价的关键条款，系统不替你默认';
  }
  if (form.valid_until < form.quote_date) {
    return `有效期至（${form.valid_until}）不能早于报价日期（${form.quote_date}）`;
  }
  if (form.items.length === 0) {
    return '报价单至少要 1 行明细';
  }
  if (form.items.length > MAX_LINES) {
    return `一张报价单最多 ${MAX_LINES} 行明细（当前 ${form.items.length} 行）`;
  }
  for (const [index, line] of form.items.entries()) {
    const no = index + 1;
    if (!line.material_id) {
      return `第 ${no} 行还没选物料`;
    }
    const qty = toNumber(line.qty);
    if (!(qty > 0)) {
      return `第 ${no} 行数量必须大于 0（当前 ${line.qty === '' ? '空' : line.qty}）`;
    }
    const price = toNumber(line.price);
    if (price < 0) {
      return `第 ${no} 行单价不能为负（当前 ${line.price}）`;
    }
    const rate = toNumber(line.tax_rate);
    if (rate < 0 || rate > 100) {
      return `第 ${no} 行税率必须在 0–100 之间（当前 ${line.tax_rate}）`;
    }
    if (line.remark.length > MAX_LINE_REMARK) {
      return `第 ${no} 行备注超过 ${MAX_LINE_REMARK} 字`;
    }
  }
  if (form.remark.length > MAX_REMARK) {
    return `备注超过 ${MAX_REMARK} 字`;
  }
  // 同物料重复行不是错误（分批交期的报价很常见），但金额全 0 的静默单要提示一次
  const totals = totalsOf(form.items);
  if (totals.withTax <= 0) {
    return '全部明细金额都是 0：这张报价单发给客户没有任何意义，请填单价或数量';
  }
  return '';
}

// ==================== 转销售订单 ====================

/** 转单按钮的状态（`enabled=false` 时 `hint` 说明为什么点不了） */
export interface ConvertState {
  /** 现在能不能转 */
  enabled: boolean;
  /** 置灰原因 / 补充说明 */
  hint: string;
}

/**
 * 转单按钮状态。
 *
 * 三条（与后端 `convert_quotation` 的守卫一一对应）：
 * 1. 只有 `ACCEPTED` 能转 —— `SENT` 要先标「客户接受」（那条边自带未过期守卫）；
 * 2. 已转过（`converted_so_id` 非空）不再转 —— 后端幂等会回原单，前端也不该给第二次机会；
 * 3. 终态（已拒绝 / 已作废）没有转单语义。
 */
export function convertStateOf(body: Pick<Quotation, 'status' | 'converted_so_id' | 'expired'> | null | undefined): ConvertState {
  if (!body) {
    return { enabled: false, hint: '请先打开一张报价单' };
  }
  if (body.converted_so_id) {
    return { enabled: false, hint: '这张报价单已经转过销售订单：再点也只会返回原订单（不会建第二张）' };
  }
  if (body.status === 'ACCEPTED') {
    return {
      enabled: true,
      hint: body.expired
        ? '已接受但今天已过期：仍可转单（过期约束的是「接受时」），明细会整列搬进订单'
        : '明细会整列搬进新订单（金额不重算），订单状态是草稿',
    };
  }
  if (body.status === 'SENT') {
    return { enabled: false, hint: '先标「客户接受」（PATCH status=ACCEPTED）才能转单：没成交的报价不该生成订单' };
  }
  if (body.status === 'DRAFT') {
    return { enabled: false, hint: '草稿还没发给客户：先提交审批并发出去，客户接受后才能转单' };
  }
  return { enabled: false, hint: '已拒绝 / 已作废的报价单不能转单：重报请开新单' };
}

/** 转单表单草稿 */
export interface ConvertFormDraft {
  /** 下单日期（留空 = 今天；**不是**报价日期） */
  order_date: string;
  /** 要求交期 */
  delivery_date: string;
  /** 销售员 id（`''` = 不指定） */
  salesperson_id: string;
  /** 备注（后端会把来源拼在前面） */
  remark: string;
}

/** 空转单表单（下单日期默认今天） */
export function emptyConvertForm(today = todayCst()): ConvertFormDraft {
  return { order_date: today, delivery_date: '', salesperson_id: '', remark: '' };
}

/** 转单表单 → 请求体（空值整键缺席：后端 `null` 与「不传」等价，但少发一个键少一个歧义） */
export function buildConvertPayload(form: ConvertFormDraft): ConvertQuotationReq {
  const remark = form.remark.trim();
  return {
    order_date: form.order_date || undefined,
    delivery_date: form.delivery_date || undefined,
    salesperson_id: form.salesperson_id || undefined,
    remark: remark || undefined,
  };
}

/** 转单表单校验（交期不能早于下单日 —— 后端同款 400） */
export function validateConvertForm(form: ConvertFormDraft): string {
  if (form.order_date && form.delivery_date && form.delivery_date < form.order_date) {
    return `要求交期（${form.delivery_date}）不能早于下单日期（${form.order_date}）`;
  }
  if (form.remark.length > MAX_REMARK) {
    return `备注超过 ${MAX_REMARK} 字`;
  }
  return '';
}

/** 转单结果 → 成功提示文案（幂等命中要显式说出来，否则用户以为又建了一张） */
export function convertResultText(result: ConvertQuotationRes): string {
  const no = result.header.so_no;
  const count = result.meta.line_count ?? result.items.length;
  if (result.meta.already_converted) {
    return `这张报价单之前已转过：返回原订单 ${no}（未新建第二张）`;
  }
  return `已生成销售订单 ${no}（${count} 行，状态 ${result.header.status_label}）`;
}

/** 转单参考文案（详情页单头「销售订单」一行） */
export function convertedRefText(body: Pick<Quotation, 'converted_so_id' | 'converted_at'>, soNo?: string | null): string {
  if (!body.converted_so_id) {
    return '—';
  }
  const no = soNo || `订单 ${body.converted_so_id.slice(0, 8)}…`;
  const at = body.converted_at ? ` · ${body.converted_at.slice(0, 19).replace('T', ' ')}` : '';
  return `${no}${at}`;
}

// ==================== 审批 / 状态展示 ====================

/** 审批徽标（详情页；列表行没有 `approval` 段，返回 `null` 表示不显示） */
export interface ApprovalBadge {
  /** 徽标文案 */
  text: string;
  /** 徽标主题 */
  theme: 'default' | 'primary' | 'success' | 'danger' | 'warning';
}

/** 审批摘要 → 徽标（判「待批」**只看** `approval.status`，不看单据状态） */
export function approvalBadgeOf(
  body: Pick<Quotation, 'approval'> | null | undefined,
): ApprovalBadge | null {
  const approval = body?.approval;
  if (!approval) {
    return null;
  }
  const theme: ApprovalBadge['theme'] =
    approval.status === 'PENDING'
      ? 'warning'
      : approval.status === 'APPROVED'
        ? 'success'
        : approval.status === 'REJECTED'
          ? 'danger'
          : 'default';
  const pending = approval.status === 'PENDING' && approval.pending_tasks > 0;
  return {
    text: pending
      ? `${approval.status_label}（还等 ${approval.pending_tasks} 人）· ${approval.instance_no}`
      : `${approval.status_label} · ${approval.instance_no}`,
    theme,
  };
}

/** 这张单现在能不能点「提交审批」（与后端 submit_approval 的前置一致） */
export function canSubmitApproval(
  body: Pick<Quotation, 'status' | 'approval' | 'items'> | null | undefined,
): { enabled: boolean; hint: string } {
  if (!body) {
    return { enabled: false, hint: '请先打开一张报价单' };
  }
  if (body.status !== 'DRAFT') {
    return { enabled: false, hint: '只有草稿可以提交审批：已发出的报价要改价请开新单' };
  }
  const approval = body.approval ?? null;
  if (approval?.status === 'PENDING') {
    return {
      enabled: false,
      hint:
        approval.pending_tasks > 0
          ? `已在审批中（${approval.instance_no}，还等 ${approval.pending_tasks} 人）：等结果或从审批中心撤回`
          : `审批已退回发起人（${approval.instance_no}）：改完后在审批中心点「重新提交」，不要再走提交审批`,
    };
  }
  if (approval?.status === 'APPROVED') {
    return { enabled: false, hint: `已通过审批（${approval.instance_no}）：直接「发出」即可，不必再审一轮` };
  }
  return { enabled: true, hint: '提交后单据仍是草稿；审批通过由流程引擎自动发出' };
}

/**
 * 发出按钮的提示。
 *
 * 审批已通过 → 直接 `PATCH status=SENT`（后端放行）；
 * 否则一律走 `submit-approval`（直接 PATCH 会 409 + 66088）。
 */
export function issueHintOf(body: Pick<Quotation, 'approval'> | null | undefined): string {
  const approval = body?.approval ?? null;
  if (approval?.status === 'APPROVED') {
    return `审批已通过（${approval.instance_no}）：点「发出」把价格发给客户`;
  }
  return '发出报价需要先过审批：点「提交审批」送审，通过后自动发出（没配流程时需持跳过权限 + 理由 ≥10 字）';
}

/** 有效期列文案（过期是现算事实，不是状态） */
export function validUntilText(body: Pick<Quotation, 'valid_until' | 'expired'> | null | undefined): string {
  if (!body) {
    return '—';
  }
  return body.expired ? `${body.valid_until}（已过期）` : body.valid_until;
}
