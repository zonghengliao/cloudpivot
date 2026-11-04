/**
 * 盘点单页纯逻辑（DC-14 后端 / 前端页面）—— 状态按钮 / 表单校验 / 请求体 / 展示口径。
 *
 * # 为什么单独一个 `.ts`
 *
 * 与 `stock-out-logic.ts` / `stockin-logic.ts` 同理：用例跑在没有 DOM 的 node 里，
 * `.vue` 的 `setup()` 起不来，而这里有五件**错了会出事**的事：
 *
 * 1. **动作按钮只能由后端 `next_statuses` 驱动** —— 盘点**没有** `DRAFT → POSTED`
 *    这条边（必须先审批 + 独立过账）。前端若照着出库单抄一份状态机，就会给用户
 *    一个后端必然 409 的按钮；
 * 2. **全盘不许带 items、抽盘必须带 1–2000 行** —— 后端两种错法都 400；
 * 3. **账面数 / 差异 / 单价 / 金额一律只读** —— 账面数是建单快照、差异是 PG 生成列，
 *    客户端能写它等于「自己填账面数」；实盘数是唯一可写的量；
 * 4. **已过账作废必须写原因**（红字凭证摘要 + 审计追溯都靠它），空理由提交就是白跑；
 * 5. **没配审批流程时是 409 `66033`（fail-closed）**，只有「持 `business:erp:skip-approval`
 *    + 理由 ≥ 10 字」才能显式跳过 —— 页面要把这条出路做出来，但不能自作主张跳过。
 */

import type {
  CreateStockTakeReq,
  StockTakeDetail,
  StockTakeItemLine,
  StockTakeRow,
  StockTakeScopeItemReq,
  StockTakeStatus,
  StockTakeStatusReq,
  StockTakeType,
  UpdateStockTakeReq,
} from '@/api/business/erp/stock-take';

/** 每页条数（与出库单 / 收货单列表同口径） */
export const STOCK_TAKE_PAGE_SIZE = 20;

/** 备注上限（后端 `MAX_REMARK_LEN`） */
export const STOCK_TAKE_MAX_REMARK = 500;

/** 原因上限（后端 `MAX_REASON_LEN`，列 `VARCHAR(255)`） */
export const STOCK_TAKE_MAX_REASON = 255;

/** 明细行上限（后端 `MAX_LINES = 2000`） */
export const STOCK_TAKE_MAX_LINES = 2000;

/** 「跳过审批」理由最小字数（后端 `approval_gate::MIN_SKIP_REASON_CHARS`） */
export const MIN_SKIP_REASON_CHARS = 10;

/** 「跳过审批」的权限码（后端 `approval_gate::PERM_SKIP_APPROVAL`） */
export const PERM_SKIP_APPROVAL = 'business:erp:skip-approval';

/** 「没有已发布审批流程」的业务码（后端 `error_code::APPROVAL_FLOW_NOT_CONFIGURED`） */
export const CODE_FLOW_NOT_CONFIGURED = 66033;

/** 盘点类型下拉（中文名与后端 `take_type_label` 逐字一致） */
export const TAKE_TYPE_OPTIONS: ReadonlyArray<{ label: string; value: StockTakeType }> = [
  { label: '全盘', value: 'FULL' },
  { label: '抽盘', value: 'PARTIAL' },
];

/** 状态筛选下拉 */
export const STOCK_TAKE_STATUS_FILTER_OPTIONS: ReadonlyArray<{ label: string; value: StockTakeStatus }> = [
  { label: '草稿', value: 'DRAFT' },
  { label: '待过账', value: 'PENDING_POST' },
  { label: '已过账', value: 'POSTED' },
  { label: '已作废', value: 'CANCELLED' },
];

/** 盘点类型 → 中文名（后端没给 label 时的兜底译名） */
export function takeTypeLabel(value: StockTakeType | string | null | undefined): string {
  const hit = TAKE_TYPE_OPTIONS.find((item) => item.value === value);
  return hit ? hit.label : String(value ?? '—');
}

/** 状态 → 中文名（后端没给 label 时的兜底译名） */
export function stockTakeStatusLabel(status: StockTakeStatus | string | null | undefined): string {
  switch (status) {
    case 'DRAFT':
      return '草稿';
    case 'PENDING_POST':
      return '待过账';
    case 'POSTED':
      return '已过账';
    case 'CANCELLED':
      return '已作废';
    default:
      return String(status ?? '—');
  }
}

/**
 * 状态 → TDesign 标签主题。
 *
 * `PENDING_POST` 用 `warning`：「等过账」是盘点的关键交接位（审批过了但账没动），
 * 卡在这里就等于年底账实不符没人调平 —— 必须显眼。
 */
export function stockTakeStatusTheme(
  status: StockTakeStatus | string | null | undefined,
): 'default' | 'primary' | 'warning' | 'success' | 'danger' {
  switch (status) {
    case 'DRAFT':
      return 'default';
    case 'PENDING_POST':
      return 'warning';
    case 'POSTED':
      return 'success';
    case 'CANCELLED':
      return 'danger';
    default:
      return 'default';
  }
}

/** 列表筛选状态 */
export interface StockTakeFilterState {
  /** 单号关键字（模糊） */
  keyword: string;
  /** 状态（空 = 全部） */
  status: string;
  /** 盘点类型（空 = 全部） */
  take_type: string;
  /** 仓库（空 = 全部） */
  warehouse_id: string;
}

/** 空筛选条件 */
export function emptyStockTakeFilter(): StockTakeFilterState {
  return { keyword: '', status: '', take_type: '', warehouse_id: '' };
}

/** 筛选是否生效（空态文案区分「还没有单」和「没有匹配」） */
export function isStockTakeFiltered(filter: StockTakeFilterState): boolean {
  return Boolean(filter.keyword.trim() || filter.status || filter.take_type || filter.warehouse_id);
}

/** 筛选条件 → 查询参数（空串一律不发） */
export function buildStockTakeQuery(
  filter: StockTakeFilterState,
  page: number,
  pageSize: number = STOCK_TAKE_PAGE_SIZE,
): Record<string, string | number> {
  const query: Record<string, string | number> = { page, page_size: pageSize };
  if (filter.keyword.trim()) {
    query.keyword = filter.keyword.trim();
  }
  if (filter.status) {
    query.status = filter.status;
  }
  if (filter.take_type) {
    query.take_type = filter.take_type;
  }
  if (filter.warehouse_id) {
    query.warehouse_id = filter.warehouse_id;
  }
  return query;
}

// ==================== 建单表单 ====================

/** 抽盘范围里的一行（key 只用于 v-for，不提交） */
export interface StockTakeScopeItemForm {
  /** 行内稳定 key */
  key: string;
  /** 物料 id */
  material_id: string;
  /** 库位账行 id（`undefined` = 未指定库位桶） */
  location_id?: string;
}

/** 建单表单 */
export interface StockTakeFormState {
  /** 盘点类型 */
  take_type: StockTakeType;
  /** 盘点仓库 */
  warehouse_id: string;
  /** 盘点日期 `YYYY-MM-DD` */
  take_date: string;
  /** 备注（盘点范围 / 组织说明） */
  remark: string;
  /** 抽盘范围（`FULL` 时页面隐藏，提交时必须是空数组） */
  items: StockTakeScopeItemForm[];
}

/** 校验错误（key = 字段名 / `items.<index>.<field>`） */
export type StockTakeFormErrors = Record<string, string>;

/** 今天（CST，`YYYY-MM-DD`）—— 盘点日期默认值 */
export function todayString(now: Date = new Date()): string {
  const shifted = new Date(now.getTime() + 8 * 3600 * 1000);
  return shifted.toISOString().slice(0, 10);
}

/** 一行空范围明细 */
export function emptyScopeItem(key: string): StockTakeScopeItemForm {
  return { key, material_id: '', location_id: undefined };
}

/** 空表单（默认全盘 + 今天；全盘不需要范围行） */
export function emptyStockTakeForm(now: Date = new Date()): StockTakeFormState {
  return {
    take_type: 'FULL',
    warehouse_id: '',
    take_date: todayString(now),
    remark: '',
    items: [],
  };
}

/**
 * 表单校验（纯逻辑，不查库）。
 *
 * 顺序与后端一致：仓库 → 日期 → 类型特有的范围交叉校验。**全盘带 items 也算错**：
 * 页面切类型时要顺手清空，但用户先选抽盘填了几行再切回全盘时，这里必须拦住。
 */
export function validateStockTakeForm(form: StockTakeFormState): StockTakeFormErrors {
  const errors: StockTakeFormErrors = {};

  if (!TAKE_TYPE_OPTIONS.some((item) => item.value === form.take_type)) {
    errors.take_type = '请选择盘点类型（全盘 / 抽盘）';
  }
  if (!form.warehouse_id) {
    errors.warehouse_id = '请选择盘点仓库';
  }
  if (!form.take_date) {
    errors.take_date = '请选择盘点日期（它决定会计期间）';
  }
  if (form.remark.length > STOCK_TAKE_MAX_REMARK) {
    errors.remark = `备注最长 ${STOCK_TAKE_MAX_REMARK} 字（当前 ${form.remark.length} 字）`;
  }

  if (form.take_type === 'PARTIAL') {
    if (form.items.length === 0) {
      errors.items = '抽盘必须指定范围（至少 1 行物料）：空抽盘等于什么都没盘';
      return errors;
    }
    if (form.items.length > STOCK_TAKE_MAX_LINES) {
      errors.items = `抽盘明细最多 ${STOCK_TAKE_MAX_LINES} 行（当前 ${form.items.length} 行）`;
      return errors;
    }
    const seen = new Set<string>();
    form.items.forEach((item, index) => {
      const prefix = `items.${index}`;
      if (!item.material_id) {
        errors[`${prefix}.material_id`] = '请选择物料';
        return;
      }
      const bucket = `${item.material_id}|${item.location_id ?? ''}`;
      if (seen.has(bucket)) {
        errors[`${prefix}.material_id`] = '同一物料同一库位重复：同一库存桶只能盘一次（否则过账会双倍调平）';
      }
      seen.add(bucket);
    });
  } else if (form.items.length > 0) {
    // 理论上页面切类型会清空，但保留这道闸：后端对「FULL + items」直接 400
    errors.items = '全盘不接受范围明细：全盘按该仓现有库存桶自动快照，只盘几个物料请用抽盘';
  }

  return errors;
}

/** 表单是否可提交 */
export function isStockTakeFormValid(form: StockTakeFormState): boolean {
  return Object.keys(validateStockTakeForm(form)).length === 0;
}

/** 数字 → 定长字符串（去掉浮点尾巴；后端收 `Decimal`，字符串最稳） */
export function qtyText(value: number | string | null | undefined): string {
  const num = Number(value ?? 0);
  if (!Number.isFinite(num)) {
    return '0';
  }
  return String(Number(num.toFixed(4)));
}

/**
 * 表单 → 建单请求体。
 *
 * **只挑后端认的字段**：范围行里没有账面数 / 单价 / 实盘数（那些由服务端快照）。
 * 全盘时 `items` 显式给 `[]`（不是省略）：后端把「全盘 + 非空 items」当错误，
 * 显式空数组是最没有歧义的写法。
 */
export function buildStockTakePayload(form: StockTakeFormState): CreateStockTakeReq {
  const items: StockTakeScopeItemReq[] =
    form.take_type === 'PARTIAL'
      ? form.items.map((item) => ({
          material_id: item.material_id,
          location_id: item.location_id || null,
        }))
      : [];
  return {
    warehouse_id: form.warehouse_id,
    take_date: form.take_date,
    take_type: form.take_type,
    remark: form.remark.trim() ? form.remark.trim() : null,
    items,
  };
}

// ==================== 填实盘数（改单） ====================

/** 实盘数编辑态：明细 id → 输入框里的原始文本（`''` = 还没填） */
export type StockTakeActualState = Record<string, string>;

/** 详情 → 实盘数编辑态（只有草稿能改） */
export function detailToActuals(detail: Pick<StockTakeDetail, 'items'>): StockTakeActualState {
  const state: StockTakeActualState = {};
  for (const item of detail.items) {
    // 草稿里 actual_qty 的默认值就是账面数（建单时后端把 actual 初始化成 book），
    // 直接回显它，用户只改有差异的那几行。
    state[item.id] = formatQty(item.actual_qty);
  }
  return state;
}

/**
 * 实盘数校验：**每行必须填且 ≥ 0**。
 *
 * 后端 `actual_qty >= 0`（`ck_biz_st_item_qty`），空值会被 serde 判 400；
 * 0 是合法的（整箱盘空就是 0），所以这里只拦「没填 / 负数 / 非数字」。
 */
export function validateActuals(
  items: ReadonlyArray<Pick<StockTakeItemLine, 'id' | 'line_no'>>,
  actuals: StockTakeActualState,
): StockTakeFormErrors {
  const errors: StockTakeFormErrors = {};
  for (const item of items) {
    const raw = (actuals[item.id] ?? '').trim();
    if (!raw) {
      errors[`actuals.${item.id}`] = `第 ${item.line_no} 行没填实盘数（0 也要显式填 0）`;
      continue;
    }
    const num = Number(raw);
    if (!Number.isFinite(num)) {
      errors[`actuals.${item.id}`] = `第 ${item.line_no} 行的实盘数不是数字`;
      continue;
    }
    if (num < 0) {
      errors[`actuals.${item.id}`] = `第 ${item.line_no} 行的实盘数不能为负（盘亏记正数，方向看差异列）`;
    }
  }
  return errors;
}

/** 详情 + 实盘数编辑态 → 改单请求体（只发后端认的字段） */
export function buildActualPayload(
  detail: Pick<StockTakeDetail, 'take_date' | 'remark' | 'items'>,
  actuals: StockTakeActualState,
): UpdateStockTakeReq {
  return {
    take_date: detail.take_date,
    remark: detail.remark ?? null,
    lines: detail.items.map((item) => ({
      item_id: item.id,
      actual_qty: qtyText(actuals[item.id] ?? item.actual_qty),
    })),
  };
}

// ==================== 状态动作 ====================

/** 详情抽屉里由 `next_statuses` 驱动的一个按钮 */
export interface StockTakeActionSpec {
  /** 目标状态 */
  status: StockTakeStatus;
  /** 按钮文案 */
  label: string;
  /** 按钮主题 */
  theme: 'primary' | 'danger' | 'default';
  /** 点下去要不要先弹「填原因」的弹窗 */
  needReason: boolean;
  /** 点下去要不要先弹确认弹窗（过账会真动账） */
  needConfirm: boolean;
  /** 稳定的 data-testid（真机验收按它点） */
  testid: string;
}

/**
 * 动作按钮的文案与语义（以 `(当前状态, 目标状态)` 为准）。
 *
 * 盘点特有的一条：`DRAFT → PENDING_POST` 是**提交审批**（不是出库单那句「提交待过账」）
 * ——种子流程 `stock_take_post` 会按差异金额分档起审批。
 */
function actionOf(current: string, target: StockTakeStatus): StockTakeActionSpec | null {
  switch (target) {
    case 'PENDING_POST':
      return {
        status: target,
        label: '提交审批',
        theme: 'primary',
        needReason: false,
        needConfirm: true,
        testid: 'btn-stocktake-submit',
      };
    case 'POSTED':
      return {
        status: target,
        label: '过账',
        theme: 'primary',
        needReason: false,
        needConfirm: true,
        testid: 'btn-stocktake-post',
      };
    case 'DRAFT':
      return {
        status: target,
        label: current === 'PENDING_POST' ? '撤回（回草稿）' : '退回草稿',
        theme: 'default',
        needReason: false,
        needConfirm: false,
        testid: 'btn-stocktake-withdraw',
      };
    case 'CANCELLED':
      return { status: target, label: '作废', theme: 'danger', needReason: true, needConfirm: false, testid: 'btn-stocktake-cancel' };
    default:
      return null;
  }
}

/** 后端下发顺序里认识的 4 个状态，未知值一律丢弃 */
const KNOWN_TARGETS: ReadonlyArray<StockTakeStatus> = ['POSTED', 'PENDING_POST', 'DRAFT', 'CANCELLED'];

/**
 * **详情抽屉的按钮列表 = 后端 `next_statuses` 的投影**。
 *
 * 前端不写第二份状态机：`next_statuses` 里没有的目标，这里一个按钮都不会出。
 * 特别注意草稿态只有 `PENDING_POST` 与 `CANCELLED` 两个目标 —— **没有「直接过账」**，
 * 这正是盘点的内控（谁盘的 / 谁批的 / 谁过的账三个人可分）。
 */
export function statusActions(current: string, nextStatuses: ReadonlyArray<string>): StockTakeActionSpec[] {
  const actions: StockTakeActionSpec[] = [];
  for (const raw of nextStatuses ?? []) {
    if (!KNOWN_TARGETS.includes(raw as StockTakeStatus)) {
      continue;
    }
    const target = raw as StockTakeStatus;
    if (actions.some((item) => item.status === target)) {
      continue;
    }
    const spec = actionOf(current, target);
    if (spec) {
      actions.push(spec);
    }
  }
  return actions;
}

/** 只有草稿能改（后端 `ensure_draft` 同口径：其它状态 409） */
export function canEdit(row: Pick<StockTakeRow, 'status'>): boolean {
  return row.status === 'DRAFT';
}

/** 只有草稿能删（后端软删同样只认 DRAFT） */
export function canDelete(row: Pick<StockTakeRow, 'status'>): boolean {
  return row.status === 'DRAFT';
}

/**
 * 作废原因校验。
 *
 * 后端只在 `POSTED → CANCELLED` 上强制要求原因（`STOCK_TAKE_REASON_EDGES`），
 * 但页面对所有作废都要求写 —— 草稿作废的原因虽然不进凭证，却是「这张单为什么没用了」
 * 的唯一记录；留空则后端会用「盘点单作废」这种没有信息量的默认值。
 */
export function validateCancelReason(reason: string, currentStatus: string): string {
  const text = reason.trim();
  if (!text) {
    return currentStatus === 'POSTED'
      ? '作废已过账的盘点单必须写原因：它会进状态日志与红字凭证摘要，审计要能回答「为什么这笔盘盈盘亏被撤销」'
      : '请写一句作废原因（例如：仓库选错，重新建单）';
  }
  if (text.length > STOCK_TAKE_MAX_REASON) {
    return `作废原因最长 ${STOCK_TAKE_MAX_REASON} 字（当前 ${text.length} 字）`;
  }
  return '';
}

/** 作废请求体 */
export function buildCancelPayload(reason: string): StockTakeStatusReq {
  return { status: 'CANCELLED', reason: reason.trim() };
}

// ==================== 跳过审批（fail-closed 的唯一出口） ====================

/** 接口错误的最小形状（不 import axios：单测里能直接喂对象） */
export interface ErrorLike {
  /** 业务码（`66033` 没配审批流程 / `10003` 无权限 / `10001` 参数 …） */
  code?: number;
  /** 报文（**原样展示**：后端报文里点名了流程码与两条出路） */
  message?: string;
}

/** 报错是否属于「没有已发布审批流程」（此时才谈得上显式跳过） */
export function isFlowNotConfigured(error: unknown): boolean {
  return Boolean(error && typeof error === 'object' && (error as ErrorLike).code === CODE_FLOW_NOT_CONFIGURED);
}

/** 跳过审批理由校验（≥ 10 字；理由要进 `biz_status_log` 供审计追溯） */
export function validateSkipReason(reason: string): string {
  const text = reason.trim();
  const chars = [...text].length;
  if (!chars) {
    return `跳过审批必须写清理由（≥ ${MIN_SKIP_REASON_CHARS} 字）：它会写进单据状态日志，审计据此追溯「为什么这一次没有复核」`;
  }
  if (chars < MIN_SKIP_REASON_CHARS) {
    return `跳过审批的理由至少 ${MIN_SKIP_REASON_CHARS} 字（当前 ${chars} 字）`;
  }
  return '';
}

/** 提交审批（显式跳过）请求体 */
export function buildSkipSubmitPayload(reason: string): StockTakeStatusReq {
  return { status: 'PENDING_POST', skip_approval: true, skip_reason: reason.trim() };
}

// ==================== 状态历史 ====================

/** 状态历史的一条 */
export interface StockTakeHistoryEntry {
  /** 稳定 key */
  key: string;
  /** 里程碑名 */
  label: string;
  /** 时间（RFC3339；`null` = 后端没下发该时间） */
  at: string | null;
  /** 补充说明 */
  note: string;
}

/**
 * 状态历史（由详情里能拿到的时间戳拼出来）。
 *
 * 后端没有单独的「状态流转日志」接口，但详情下发了 `created_at` / `posted_at` /
 * `cancelled_at` / `cancel_reason`，足够画出四个里程碑。**只为真实发生过的事打点**。
 */
export function statusHistory(
  detail: Pick<StockTakeDetail, 'created_at' | 'status' | 'posted_at' | 'cancelled_at' | 'cancel_reason'>,
): StockTakeHistoryEntry[] {
  const entries: StockTakeHistoryEntry[] = [
    { key: 'created', label: '创建（草稿，账面数已快照）', at: detail.created_at, note: '' },
  ];
  if (detail.status === 'PENDING_POST') {
    entries.push({
      key: 'pending',
      label: '提交审批（待过账）',
      at: null,
      note: '审批已通过或显式跳过；账一分没动，等持过账权限的人过账',
    });
  }
  if (detail.posted_at) {
    entries.push({
      key: 'posted',
      label: '过账（三本账已调平 + 差异凭证已生成）',
      at: detail.posted_at,
      note: '',
    });
  }
  if (detail.cancelled_at) {
    entries.push({
      key: 'cancelled',
      label: '作废（已过账的按快照回滚 + 红冲凭证）',
      at: detail.cancelled_at,
      note: detail.cancel_reason ?? '',
    });
  }
  return entries;
}

// ==================== 展示口径 ====================

/**
 * 按明细差异汇总出的盘盈 / 盘亏（**前端展示用**）。
 *
 * # 为什么草稿阶段不能直接用后端的 `total_gain_qty` / `total_loss_qty`
 *
 * 后端 `refresh_totals` 只重算 `total_book_qty` / `total_actual_qty`；盘盈 / 盘亏数量与金额
 * 只在**过账**那一刻由 `set_diff_totals` 写（金额要按账上移动加权单价才算得出）。
 * 所以草稿 / 待过账时后端下发的是 `0.00` —— 直接渲染就等于告诉用户「盘了 5 件却没有盘盈」。
 *
 * 这里按明细的 `diff_qty`（PG 生成列，逐行真值）汇总**数量**：正数进盘盈、负数绝对值进盘亏。
 * 金额不汇总（前端没有单价口径，过账后以后端为准）。
 */
export interface DiffAggregate {
  /** 盘盈数量合计（正差异） */
  gainQty: number;
  /** 盘亏数量合计（负差异的绝对值） */
  lossQty: number;
  /** 有差异的行数 */
  diffLines: number;
}

/** 按明细汇总盘盈 / 盘亏数量 */
export function aggregateDiff(items: ReadonlyArray<Pick<StockTakeItemLine, 'diff_qty'>>): DiffAggregate {
  let gainQty = 0;
  let lossQty = 0;
  let diffLines = 0;
  for (const item of items) {
    const num = Number(item.diff_qty ?? 0);
    if (!Number.isFinite(num) || num === 0) {
      continue;
    }
    diffLines += 1;
    if (num > 0) {
      gainQty += num;
    } else {
      lossQty += -num;
    }
  }
  return { gainQty: Number(gainQty.toFixed(4)), lossQty: Number(lossQty.toFixed(4)), diffLines };
}

/** 盘盈 / 盘亏的展示口径（过账后以后端合计为准，否则按明细汇总并标注来源） */
export interface GainLossDisplay {
  /** 盘盈展示文本 */
  gain: string;
  /** 盘亏展示文本 */
  loss: string;
  /** `true` = 后端合计（已过账）；`false` = 前端按明细汇总（草稿 / 待过账） */
  fromBackend: boolean;
}

export function gainLossDisplay(
  detail: Pick<StockTakeDetail, 'status' | 'total_gain_qty' | 'total_loss_qty' | 'items'>,
): GainLossDisplay {
  if (detail.status === 'POSTED' || detail.status === 'CANCELLED') {
    return {
      gain: formatQty(detail.total_gain_qty),
      loss: formatQty(detail.total_loss_qty),
      fromBackend: true,
    };
  }
  const agg = aggregateDiff(detail.items);
  return { gain: formatQty(agg.gainQty), loss: formatQty(agg.lossQty), fromBackend: false };
}

/** 差异方向：正数盘盈 / 负数盘亏 / 0 无差异 */
export function diffDirection(value: number | string | null | undefined): 'gain' | 'loss' | 'zero' {
  const num = Number(value ?? 0);
  if (!Number.isFinite(num) || num === 0) {
    return 'zero';
  }
  return num > 0 ? 'gain' : 'loss';
}

/** 差异展示：带正负号（`+3` / `-2` / `0`），让方向一眼可见 */
export function formatDiff(value: number | string | null | undefined): string {
  const num = Number(value ?? 0);
  if (!Number.isFinite(num) || num === 0) {
    return '0';
  }
  const text = formatQty(num);
  return num > 0 ? `+${text}` : text;
}

/** 金额展示（两位小数 + 千分位） */
export function formatMoney(value: number | string | null | undefined): string {
  const num = Number(value ?? 0);
  if (!Number.isFinite(num)) {
    return '0.00';
  }
  return num.toLocaleString('zh-CN', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}

/** 数量展示（最多 4 位小数，去掉无意义的 0） */
export function formatQty(value: number | string | null | undefined): string {
  const num = Number(value ?? 0);
  if (!Number.isFinite(num)) {
    return '0';
  }
  return String(Number(num.toFixed(4)));
}

/** 日期只留 `MM-DD`（列表列宽有限；详情里给完整日期） */
export function shortDate(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return value.length >= 10 ? value.slice(5, 10) : value;
}

/** 空值占位 */
export function dash(value: string | null | undefined): string {
  return value && String(value).trim() ? String(value) : '—';
}

/** RFC3339 → `YYYY-MM-DD HH:mm`（浏览器本地时区，也就是用户看到的北京时间） */
export function formatDateTime(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return String(value);
  }
  const pad = (num: number) => String(num).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** 短 id（仓库 / 库位这种还没译名的兜底展示） */
export function shortId(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return value.length > 8 ? `${value.slice(0, 8)}…` : value;
}

/**
 * 错误 → 页面文案。
 *
 * 关键口径：**原样带上后端 `msg`**。盘点的报文里有「仓库 WH-01 当前没有任何库存行」
 * 「抽盘明细里有重复行」「没有配置已发布的审批流程 `stock_take_post`」这类
 * 用户唯一的自救信息，只写「操作失败」等于把它们丢掉。
 */
export function errorText(error: unknown): string {
  if (error && typeof error === 'object') {
    const like = error as ErrorLike;
    const message = (like.message ?? '').trim();
    if (message) {
      return typeof like.code === 'number' && like.code > 0 ? `${message}（错误码 ${like.code}）` : message;
    }
  }
  if (typeof error === 'string' && error.trim()) {
    return error;
  }
  return '操作失败：接口没有返回说明，请刷新后重试';
}
