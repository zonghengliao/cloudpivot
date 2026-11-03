/**
 * 调拨单页面的纯逻辑（W4c）。
 *
 * 为什么单独一个 `.ts` 而不是全塞 `.vue`：本仓单测是 `tsx --test`（没有 DOM、没有
 * `@vue/test-utils`），`.vue` 里的判断一条都测不到。所以「状态怎么显示 / 这一刻能点哪些
 * 按钮 / 建单校验 / 请求体怎么组装 / 审批进度怎么排」全放这里，`.vue` 只负责渲染和拍接口。
 *
 * # 口径来源（**不是卡面草图**）
 *
 * | 语义 | 真口径 | 出处 |
 * |---|---|---|
 * | 状态六值 | `DRAFT` / `PENDING` / `APPROVED` / `IN_TRANSIT` / `DONE` / `CANCELLED` | `V0140` `ck_transfer_status` |
 * | 状态机 | `DRAFT→PENDING`、`PENDING→CANCELLED`（撤回）/`PENDING→APPROVED`、`DRAFT→CANCELLED`、`APPROVED→IN_TRANSIT`、`IN_TRANSIT→DONE` | 后端 `transfer_model.rs::LEGAL_EDGES`（P8 实测：撤回发 `DRAFT` 会 409） |
 * | 同仓禁止 | `from_warehouse_id <> to_warehouse_id` | `V0140` `ck_transfer_diff_wh` |
 * | 明细数量 | `qty > 0`（恒正，方向由 from/to 表达） | `V0140` `ck_transfer_item_qty` |
 * | 删除范围 | 仅 `DRAFT` / `CANCELLED` | 卡面契约 `DELETE /erp/transfer/{id}` |
 * | 审批弱引用 | `approval_instance_id` → `biz_flow_instance.id`（`biz_type='STOCK_TRANSFER'`） | `V0140` §1 列注释 |
 *
 * # 标签配色为什么没有「紫」
 *
 * 卡面写「在途=紫」，但 TDesign 的 `t-tag` 只有 5 个语义槽
 * （`default` / `primary` / `success` / `warning` / `danger`），没有紫色槽。本仓 22 套色板 ×
 * 明暗双模式（`pnpm check:palettes`）全靠语义变量驱动，为了一个紫写裸 hex 会在别的色板
 * 里瞎眼。所以「在途」用 **`primary` + `dark` 实心**（比「已批准」的浅蓝块更重，视觉上
 * 一眼能分开），这是本卡唯一的配色偏差，已写进证据文档。
 */
import type {
  CreateTransferReq,
  TransferDetail,
  TransferItemReq,
  TransferOverdueSummary,
  TransferRow,
  TransferStatus,
  TransferItemSerial,
} from '@/api/business/erp';

/** 默认每页条数（服务端分页；调拨单是低频单据，20 足够一屏） */
export const TRANSFER_PAGE_SIZE = 20;

/**
 * 序列号选择器一次拉多少个候选（后端 `page_size` 上限 200）。
 *
 * 一物一码物料的单张调拨通常是个位到几十件；拉满 200 够选，超了页面会提示
 * 「只列出前 N 个」，避免「选不到想要的号」变成静默失败。
 */
export const SERIAL_OPTION_LIMIT = 200;

/** 状态六值（顺序 = 生命周期顺序，下拉与说明都按它排，别处不要重排） */
export const TRANSFER_STATUSES: readonly TransferStatus[] = [
  'DRAFT',
  'PENDING',
  'APPROVED',
  'IN_TRANSIT',
  'DONE',
  'CANCELLED',
];

/** TDesign `t-tag` 的主题槽（只有这 5 个，没有紫色） */
export type TransferTagTheme = 'default' | 'primary' | 'success' | 'warning' | 'danger';

/** TDesign `t-tag` 的描边/填充样式 */
export type TransferTagVariant = 'dark' | 'light' | 'outline' | 'light-outline';

/** 一个状态的显示口径 */
export interface TransferStatusMeta {
  /** 中文标签 */
  label: string;
  /** tag 主题（颜色由 TDesign 主题变量驱动，**不写裸 hex**） */
  theme: TransferTagTheme;
  /** tag 样式（「在途」靠 `dark` 与「已批准」的 `light` 区分） */
  variant: TransferTagVariant;
  /** 该状态的一句话解释（tooltip / 空态提示用） */
  hint: string;
}

/**
 * 状态 → 标签 / 配色。
 *
 * 卡面要求：草稿=灰 / 审批中=橙 / 已批准=蓝 / 在途=紫 / 已完成=绿 / 已取消=浅灰。
 * 紫色没有语义槽（见文件头），用 `primary + dark` 顶替，其余逐条照卡面。
 */
export const TRANSFER_STATUS_META: Readonly<Record<TransferStatus, TransferStatusMeta>> = {
  DRAFT: { label: '草稿', theme: 'default', variant: 'light', hint: '还没提交，可编辑 / 可删' },
  PENDING: { label: '审批中', theme: 'warning', variant: 'light', hint: '审批流程在跑，可撤回（撤回=取消整单，要填原因）' },
  APPROVED: { label: '已批准', theme: 'primary', variant: 'light', hint: '审批通过，等发货' },
  IN_TRANSIT: { label: '在途', theme: 'primary', variant: 'dark', hint: '已发货，货在在途仓' },
  DONE: { label: '已完成', theme: 'success', variant: 'light', hint: '已签收，终态只读' },
  CANCELLED: { label: '已取消', theme: 'default', variant: 'outline', hint: '已取消，终态只读' },
};

/** 状态下拉项（`''` = 全部 → 请求里整键缺席） */
export const TRANSFER_STATUS_FILTER_OPTIONS: ReadonlyArray<{ label: string; value: TransferStatus }> =
  TRANSFER_STATUSES.map((status) => ({ label: TRANSFER_STATUS_META[status].label, value: status }));

/** 状态 → 中文（未知值原样回显，不吞数据：后端加了新状态时页面至少不空白） */
export function transferStatusLabel(status: string): string {
  return TRANSFER_STATUS_META[status as TransferStatus]?.label ?? status;
}

/** 状态 → tag 主题（未知值给 default，别让它继承上一行的颜色） */
export function transferStatusTheme(status: string): TransferTagTheme {
  return TRANSFER_STATUS_META[status as TransferStatus]?.theme ?? 'default';
}

/** 状态 → tag 样式（未知值给 light） */
export function transferStatusVariant(status: string): TransferTagVariant {
  return TRANSFER_STATUS_META[status as TransferStatus]?.variant ?? 'light';
}

// ==================== 状态机 / 按钮显隐 ====================

/** 可执行的动作（含不是状态流转的「编辑」「删除」） */
export type TransferAction = 'edit' | 'submit' | 'withdraw' | 'ship' | 'receive' | 'remove';

/** 某一行的按钮可用性 */
export type TransferActions = Record<TransferAction, boolean>;

/** 状态流转：`from → to` 是否允许（与后端 `transfer_model::LEGAL_EDGES` 逐条对齐） */
export function canTransition(from: string, to: string): boolean {
  return (
    (from === 'DRAFT' && to === 'PENDING')
    // 「撤回」= 取消整单（PENDING → CANCELLED），**不是**退回草稿：
    // 后端 `transfer_model.rs` 的合法边只有 PENDING→APPROVED / PENDING→CANCELLED，
    // 发 `DRAFT` 会被 409「非法状态流转」拒掉（P8 实测：撤回按钮点了只报错）。
    || (from === 'PENDING' && to === 'CANCELLED')
    || (from === 'DRAFT' && to === 'CANCELLED')
    || (from === 'APPROVED' && to === 'IN_TRANSIT')
    || (from === 'IN_TRANSIT' && to === 'DONE')
  );
}

/**
 * 行 → 可点哪些按钮（卡面 §四·③ 的显隐表，逐条落地）。
 *
 * | 状态 | 编辑 | 提交审批 | 撤回 | 发货 | 收货 | 删除 |
 * |---|---|---|---|---|---|---|
 * | `DRAFT` | ✅ | ✅ | — | — | — | ✅ |
 * | `PENDING` | — | — | ✅ | — | — | — |
 * | `APPROVED` | — | — | — | ✅ | — | — |
 * | `IN_TRANSIT` | — | — | — | — | ✅ | — |
 * | `DONE` | 只读 | | | | | |
 * | `CANCELLED` | — | — | — | — | — | ✅ |
 *
 * 「编辑」比 `DRAFT` 更严一层：只有草稿能改（`PUT` 仅 DRAFT，见卡面契约）。
 * 「删除」允许 `CANCELLED`（卡面契约：仅 DRAFT / CANCELLED）。
 */
export function availableActions(status: string): TransferActions {
  return {
    edit: status === 'DRAFT',
    submit: status === 'DRAFT',
    withdraw: status === 'PENDING',
    ship: status === 'APPROVED',
    receive: status === 'IN_TRANSIT',
    remove: status === 'DRAFT' || status === 'CANCELLED',
  };
}

/** 动作 → 中文（按钮文案 / 确认框标题共用，避免两边各写一份） */
export const TRANSFER_ACTION_LABEL: Readonly<Record<TransferAction, string>> = {
  edit: '编辑',
  submit: '提交审批',
  withdraw: '撤回',
  ship: '发货',
  receive: '收货',
  remove: '删除',
};

/**
 * 动作 → 目标状态。
 *
 * `null` 表示这个动作**不是** `PATCH /status` 流转：
 * 「编辑」走 `PUT /transfer/{id}`，「删除」走 `DELETE /transfer/{id}`。
 * 「撤回」的目标是 `CANCELLED`（= 取消整单，后端那侧同时终止在跑的审批单）；
 * `APPROVED` 由审批引擎写，前端没有入口。
 */
export function actionTargetStatus(action: TransferAction): TransferStatus | null {
  switch (action) {
    case 'submit':
      return 'PENDING';
    case 'withdraw':
      return 'CANCELLED';
    case 'ship':
      return 'IN_TRANSIT';
    case 'receive':
      return 'DONE';
    default:
      return null;
  }
}

/**
 * 动作是否**不可逆**（要二次确认）。
 *
 * 发货：货一旦进在途仓就必须收货，撤回没有业务路径；
 * 收货：签收即终态（`DONE` 之后 `ck_biz_transfer_state` 不允许再动）；
 * 撤回：`PENDING → CANCELLED` 是**终态**（CANCELLED 只能删，不能再提交），且会终止在跑的审批单，
 * 所以也要确认 + 必填取消原因（后端 `ck_biz_transfer_state` 要求 `cancel_reason` 非空白）。
 * 只有「提交审批」是可逆的中间态，不弹确认。
 */
export function needsConfirm(action: TransferAction): boolean {
  return action === 'ship' || action === 'receive' || action === 'withdraw';
}

/** 取消原因的最大长度（与后端 `transfer_dto::MAX_REASON_LEN = 255` 同口径） */
export const WITHDRAW_REASON_MAX = 255;

/**
 * 撤回（取消）原因校验：`''` = 通过，否则是给用户看的错误文案。
 *
 * 逐字对齐后端 `TransferStatusReq::cancel_reason()`：空白视为没给 → 400。
 */
export function withdrawReasonError(reason: string): string {
  const text = reason.trim();
  if (!text) {
    return '撤回必须填写原因（取消后不可恢复）';
  }
  if (text.length > WITHDRAW_REASON_MAX) {
    return `原因最长 ${WITHDRAW_REASON_MAX} 个字符`;
  }
  return '';
}

/** 不可点时的原因（tooltip / 灰字说明） */
export function transferActionHint(status: string): string {
  const meta = TRANSFER_STATUS_META[status as TransferStatus];
  if (!meta) {
    return '未知状态：只读';
  }
  return `${meta.label}：${meta.hint}`;
}

// ==================== 筛选 / 分页参数 ====================

/** 筛选行状态 */
export interface TransferFilterState {
  /** 状态（`''` = 全部） */
  status: TransferStatus | '';
  /** 调出仓 id（`''` = 全部） */
  from_warehouse_id: string;
  /** 调入仓 id（`''` = 全部） */
  to_warehouse_id: string;
  /** 单号（模糊） */
  transfer_no: string;
  /**
   * 只看「超期在途」（LNK-F12 开关）：`true` = 请求带 `in_transit_overdue=true`。
   *
   * 关掉时**整键缺席**（= 后端「不筛」），不是发 `false`：
   * `in_transit_overdue=false` 是「排除超期」，与「关掉开关看全部」不是一回事，
   * 而且它跟 `status=IN_TRANSIT` 是后端 400 的矛盾组合。
   */
  in_transit_overdue: boolean;
}

/** 空筛选（重置用；每次返回新对象，避免共享引用被就地改） */
export function emptyTransferFilter(): TransferFilterState {
  return {
    status: '',
    from_warehouse_id: '',
    to_warehouse_id: '',
    transfer_no: '',
    in_transit_overdue: false,
  };
}

/**
 * 筛选行 → `GET /erp/transfer/page` 的参数。
 *
 * 口径：`''` 一律**整键缺席**（不是发空串）。后端把 `?transfer_no=` 当「等于空串」过滤，
 * 发了就查出 0 行 —— 与本仓 API 层 `asParams` 的既有约定一致。
 *
 * # 超期开关与状态筛选互斥（后端 400 的不是空列表）
 *
 * 后端 `TransferPageQuery::validate()` 对两种组合直接 400：
 * - `in_transit_overdue=true` + `status≠IN_TRANSIT`；
 * - `in_transit_overdue=false` + `status=IN_TRANSIT`。
 *
 * 所以这里**只有一条分支**：超期开关打开 → 发 `in_transit_overdue=true` 且**丢掉 `status`**
 * （页面同时把状态下拉置灰并清空，双保险：即便有代码绕过 UI 直接改 filter，也拼不出矛盾组合）。
 */
export function buildTransferQuery(
  filter: TransferFilterState,
  page: number,
  pageSize: number = TRANSFER_PAGE_SIZE,
): Record<string, unknown> {
  const query: Record<string, unknown> = { page, page_size: pageSize };
  if (filter.in_transit_overdue) {
    query.in_transit_overdue = true;
  } else if (filter.status) {
    query.status = filter.status;
  }
  if (filter.from_warehouse_id) {
    query.from_warehouse_id = filter.from_warehouse_id;
  }
  if (filter.to_warehouse_id) {
    query.to_warehouse_id = filter.to_warehouse_id;
  }
  const transferNo = filter.transfer_no.trim();
  if (transferNo) {
    query.transfer_no = transferNo;
  }
  return query;
}

/** 是否处于筛选态（空态文案要分「没数据」和「没匹配」） */
export function isTransferFiltered(filter: TransferFilterState): boolean {
  return (
    Boolean(filter.status)
    || Boolean(filter.from_warehouse_id)
    || Boolean(filter.to_warehouse_id)
    || Boolean(filter.transfer_no.trim())
    || filter.in_transit_overdue
  );
}

/** 超期开关打开时状态下拉必须置灰（否则用户能选出后端 400 的组合） */
export function isStatusFilterLocked(filter: TransferFilterState): boolean {
  return filter.in_transit_overdue;
}

// ==================== 超期在途（LNK-F12） ====================

/** 页头角标文案里的缺省阈值（后端 `sys_config` 读不到时也回退 7，两边同口径） */
export const OVERDUE_DEFAULT_THRESHOLD_DAYS = 7;

/**
 * 这一行是否要显示「超期 N 天」红标。
 *
 * 两个条件都要：`status === 'IN_TRANSIT'` **且** 后端判定 `in_transit_overdue === true`。
 * 只看 `overdue_days` 不够 —— 非超期行后端不下发该字段，但历史上可能有脏数据把它带过来。
 */
export function isOverdueRow(row: Pick<TransferRow, 'status' | 'in_transit_overdue'>): boolean {
  return row.status === 'IN_TRANSIT' && row.in_transit_overdue === true;
}

/**
 * 行内红标文案：`超期 N 天`；不该显示时返回 `''`（模板 `v-if` 直接吃掉）。
 *
 * `overdue_days` 缺失时回退成 `超期`（不带天数）—— 后端理论上只在超期时下发天数，
 * 但「红标出现但文案是 `超期 undefined 天`」比少一个数字难看得多。
 */
export function overdueBadgeText(row: Pick<TransferRow, 'status' | 'in_transit_overdue' | 'overdue_days'>): string {
  if (!isOverdueRow(row)) {
    return '';
  }
  const days = row.overdue_days;
  return typeof days === 'number' && Number.isFinite(days) ? `超期 ${days} 天` : '超期';
}

/**
 * 红标 hover 文案：告警台账（`overdue_alerted_at` / `overdue_alert_count`）。
 *
 * - 提醒过：`已于 2026-10-31 14:11 提醒过 1 次`（时间走本页 `formatTime` 口径，本地时区）；
 * - 没提醒过：说明阈值与「扫描任务还没跑到它」，别显示一颗时间都没有的空 tooltip。
 */
export function overdueBadgeTooltip(
  row: Pick<TransferRow, 'status' | 'in_transit_overdue' | 'overdue_days' | 'overdue_alerted_at' | 'overdue_alert_count'>,
  thresholdDays: number = OVERDUE_DEFAULT_THRESHOLD_DAYS,
): string {
  if (!isOverdueRow(row)) {
    return '';
  }
  const days = typeof row.overdue_days === 'number' ? row.overdue_days : null;
  const where = days === null
    ? `发货已超 ${thresholdDays} 天未收货`
    : `发货已 ${days} 天未收货（阈值 ${thresholdDays} 天）`;
  const at = row.overdue_alerted_at;
  if (!at) {
    return `${where}；扫描任务还没提醒过这张单`;
  }
  const count = typeof row.overdue_alert_count === 'number' && row.overdue_alert_count > 0
    ? row.overdue_alert_count
    : 1;
  return `${where}；已于 ${formatTime(at)} 提醒过 ${count} 次`;
}

/**
 * 页头角标文案：`发货超 7 天未收货 · 3 张`。
 *
 * `total === 0` 时返回 `''`（不显示角标 —— 「0 张超期」挂个红标只会让人每天多看一眼）。
 * `threshold_days` 缺失（旧版后端 / 接口还没上）回退 7，文案不出现 `undefined`。
 */
export function overdueSummaryText(
  summary: Pick<TransferOverdueSummary, 'threshold_days' | 'total'> | null | undefined,
): string {
  if (!summary || !summary.total || summary.total <= 0) {
    return '';
  }
  const threshold = typeof summary.threshold_days === 'number' && summary.threshold_days > 0
    ? summary.threshold_days
    : OVERDUE_DEFAULT_THRESHOLD_DAYS;
  return `发货超 ${threshold} 天未收货 · ${summary.total} 张`;
}

// ==================== 建单 / 改单表单 ====================

/** 表单里的一条明细（前端态；`key` 是 v-for 的稳定 key，提交时不发） */
export interface TransferItemForm {
  /** 前端行 key（增删行不能靠 index —— 删中间一行会让后面的行错位复用组件） */
  key: string;
  /** 物料 id（`''` = 未选） */
  material_id: string;
  /** 数量（恒 > 0；`t-input-number` 的出入参是 `number | string`，统一 `Number()` 归一） */
  qty: number | string;
  /** 已选序列号 id（一物一码物料必须 `length === qty`） */
  serial_ids: string[];
  /** 物料是否一物一码（来自物料主数据，用于「必须选序列号」的校验与列显隐） */
  serial_required: boolean;
}

/** 表单整体 */
export interface TransferFormState {
  /** 调出仓 id */
  from_warehouse_id: string;
  /** 调入仓 id */
  to_warehouse_id: string;
  /** 调拨日期 `YYYY-MM-DD` */
  transfer_date: string;
  /** 原因（≤255） */
  reason: string;
  /** 备注 */
  remark: string;
  /** 明细（至少 1 条） */
  items: TransferItemForm[];
}

/** 明细级错误 */
export interface TransferItemFormError {
  /** 物料错 */
  material_id?: string;
  /** 数量错 */
  qty?: string;
  /** 序列号错 */
  serial_ids?: string;
}

/** 表单错误（`field` 级 + 明细行级） */
export interface TransferFormErrors {
  /** 单头字段错 */
  from_warehouse_id?: string;
  to_warehouse_id?: string;
  transfer_date?: string;
  reason?: string;
  /** 明细整体错（「至少一条明细」「跨明细序列号重复」） */
  items?: string;
  /** 逐行错（key = `TransferItemForm.key`） */
  rows?: Record<string, TransferItemFormError>;
}

/** 新建一行的默认值（`key` 由调用方给，便于单测注入固定值） */
export function emptyTransferItem(key = 'row-1'): TransferItemForm {
  return { key, material_id: '', qty: 1, serial_ids: [], serial_required: false };
}

/** 加一行（页面的「+ 添加明细」用；`key` 由调用方保证唯一） */
export function addTransferItem(key: string): TransferItemForm {
  return emptyTransferItem(key);
}

/** 今天的 `YYYY-MM-DD`（本地时区；`t-date-picker` 的默认值，也是 `transfer_date` 的默认值） */
export function todayString(now: Date = new Date()): string {
  const pad = (n: number): string => String(n).padStart(2, '0');
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
}

/** 空表单（新建时用；每次返回新对象，避免弹窗之间互相污染） */
export function emptyTransferForm(now: Date = new Date()): TransferFormState {
  return {
    from_warehouse_id: '',
    to_warehouse_id: '',
    transfer_date: todayString(now),
    reason: '',
    remark: '',
    items: [emptyTransferItem('row-1')],
  };
}

/**
 * 跨明细重复的序列号 id（同一条货挂两条明细 —— `(item_id, serial_id)` 唯一键拦不住）。
 *
 * `V0140` 文件头 ③ 明确要求「W3b 校验时必须自己拦（跨明细去重）」，前端先拦一次
 * 是为了不让用户白等一次 400。返回的是**出现 ≥2 次**的 id（去重后的列表，顺序稳定）。
 */
export function collectDuplicateSerialIds(items: TransferItemForm[]): string[] {
  const seen = new Set<string>();
  const dup = new Set<string>();
  for (const item of items) {
    for (const serialId of item.serial_ids) {
      if (seen.has(serialId)) {
        dup.add(serialId);
      } else {
        seen.add(serialId);
      }
    }
  }
  return [...dup];
}

/** 数量是否合法：有限数、> 0、最多 4 位小数（`NUMERIC(18,4)`） */
export function isValidQty(qty: number | string | null | undefined): boolean {
  if (qty === null || qty === undefined || qty === '') {
    return false;
  }
  const num = typeof qty === 'number' ? qty : Number(qty);
  if (!Number.isFinite(num) || num <= 0) {
    return false;
  }
  // 乘 10000 后判整：0.0001 的粒度，避免浮点误差把 0.1 判成 0.10000000000000001
  return Math.abs(num * 10000 - Math.round(num * 10000)) < 1e-6;
}

/**
 * 校验建单 / 改单表单（前端先拦一次，后端仍会再判 —— 双保险不是二选一）。
 *
 * 三条卡面点名要拦的：
 * 1. 调出仓 == 调入仓 → 报错（`V0140` 有 CHECK，但让用户等一个 500 不合格）；
 * 2. 数量 <= 0 → 报错（`ck_transfer_item_qty`）；
 * 3. 一物一码物料：序列号个数必须 == 数量，且数量必须是整数（半件货没有序列号）。
 */
export function validateTransferForm(form: TransferFormState): TransferFormErrors {
  const errors: TransferFormErrors = {};

  if (!form.from_warehouse_id) {
    errors.from_warehouse_id = '请选择调出仓';
  }
  if (!form.to_warehouse_id) {
    errors.to_warehouse_id = '请选择调入仓';
  }
  if (form.from_warehouse_id && form.to_warehouse_id && form.from_warehouse_id === form.to_warehouse_id) {
    errors.to_warehouse_id = '调出仓与调入仓不能相同（同仓调拨没有业务含义）';
  }
  if (!form.transfer_date) {
    errors.transfer_date = '请选择调拨日期';
  }
  if (form.reason.trim().length > 255) {
    errors.reason = '原因最长 255 个字符';
  }

  const rows: Record<string, TransferItemFormError> = {};
  if (form.items.length === 0) {
    errors.items = '至少添加一条明细';
  }

  for (const item of form.items) {
    const rowError: TransferItemFormError = {};
    if (!item.material_id) {
      rowError.material_id = '请选择物料';
    }
    if (!isValidQty(item.qty)) {
      rowError.qty = '数量必须大于 0（最多 4 位小数）';
    } else if (item.serial_required) {
      const qty = Number(item.qty);
      if (!Number.isInteger(qty)) {
        rowError.qty = '一物一码物料的数量必须是整数（半件货没有序列号）';
      } else if (item.serial_ids.length !== qty) {
        rowError.serial_ids = `一物一码物料必须选 ${qty} 个序列号，当前 ${item.serial_ids.length} 个`;
      }
    }
    if (Object.keys(rowError).length > 0) {
      rows[item.key] = rowError;
    }
  }

  if (Object.keys(rows).length > 0) {
    errors.rows = rows;
  }

  const duplicates = collectDuplicateSerialIds(form.items);
  if (duplicates.length > 0) {
    const message = `序列号不能挂在两条明细下（重复 ${duplicates.length} 个）`;
    if (errors.items) {
      errors.items = `${errors.items}；${message}`;
    } else {
      errors.items = message;
    }
    // 重复的号落到**后出现的行**上，用户才知道是哪一行要改
    const claimed = new Set<string>();
    for (const item of form.items) {
      const hit = item.serial_ids.some((id) => {
        if (!duplicates.includes(id)) {
          return false;
        }
        if (claimed.has(id)) {
          return true;
        }
        claimed.add(id);
        return false;
      });
      if (hit) {
        const rowError = rows[item.key] ?? {};
        rowError.serial_ids = '该行的序列号已在别的明细里选过';
        rows[item.key] = rowError;
        errors.rows = rows;
      }
    }
  }

  return errors;
}

/** 表单有没有错（提交前一眼判空） */
export function hasTransferFormError(errors: TransferFormErrors): boolean {
  return (
    Boolean(errors.from_warehouse_id)
    || Boolean(errors.to_warehouse_id)
    || Boolean(errors.transfer_date)
    || Boolean(errors.reason)
    || Boolean(errors.items)
    || Object.keys(errors.rows ?? {}).length > 0
  );
}

/**
 * 表单 → `POST/PUT` 请求体（照 `CreateTransferReq`，空值不发）。
 *
 * 三条约定：
 * 1. 原因 / 备注 trim 后为空 → 整键缺席（不是发 `""`，后端存空串会脏）；
 * 2. `serial_ids` 只在非空时才发 —— 非一物一码物料发 `[]` 会让 W3b 的分支判断变复杂；
 * 3. `qty` 强制转 number（`t-input-number` 出参就是 number，但 `v-model` 可能是字符串）。
 */
export function buildTransferPayload(form: TransferFormState): CreateTransferReq {
  const items: TransferItemReq[] = form.items.map((item) => {
    const row: TransferItemReq = {
      material_id: item.material_id,
      qty: Number(item.qty),
    };
    if (item.serial_ids.length > 0) {
      row.serial_ids = [...item.serial_ids];
    }
    return row;
  });

  const payload: CreateTransferReq = {
    from_warehouse_id: form.from_warehouse_id,
    to_warehouse_id: form.to_warehouse_id,
    transfer_date: form.transfer_date,
    items,
  };

  const reason = form.reason.trim();
  if (reason) {
    payload.reason = reason;
  }
  const remark = form.remark.trim();
  if (remark) {
    payload.remark = remark;
  }
  return payload;
}

/**
 * D9：复制为新单后的提示语（空串 = 不需要提示）。
 *
 * 后端复制时**不复制序列号**（号码属于实物，原单那批已经跟着原单走），
 * 原单带过号码的话，新单的序列号是空的 —— 不解释一句，用户会以为「复制丢了号码」。
 */
export function transferCopyNotice(origin: {
  transfer_no: string;
  items?: ReadonlyArray<{ serials?: ReadonlyArray<unknown> }>;
}): string {
  const hasSerials = (origin.items ?? []).some((item) => (item.serials ?? []).length > 0);
  if (!hasSerials) {
    return '';
  }
  return `本单复制自 ${origin.transfer_no}：序列号属于实物、不会复制，请按本次实际货品重新选择序列号后再提交。`;
}

/**
 * 详情 → 编辑表单（仅 `DRAFT` 可编辑；非草稿页面不给入口，这里不做状态判断）。
 *
 * `key` 用明细 id（稳定、可复现），物料的一物一码标记由 `materialMap` 补
 * —— 明细行里没有 `serial_required`，得靠物料主数据。
 */
export function detailToForm(
  detail: TransferDetail,
  materialSerialRequired: Readonly<Record<string, boolean>> = {},
): TransferFormState {
  return {
    from_warehouse_id: detail.from_warehouse_id,
    to_warehouse_id: detail.to_warehouse_id,
    transfer_date: detail.transfer_date.slice(0, 10),
    reason: detail.reason ?? '',
    remark: detail.remark ?? '',
    items: (detail.items ?? []).map((item, index) => ({
      key: item.id || `row-${index + 1}`,
      material_id: item.material_id,
      qty: Number(item.qty),
      serial_ids: (item.serials ?? []).map((serial) => serial.serial_id),
      serial_required: materialSerialRequired[item.material_id] ?? false,
    })),
  };
}

// ==================== 展示辅助 ====================

/**
 * 数量 / 金额归一显示。
 *
 * `NUMERIC(18,4)` 在 JSON 里可能是 number 也可能是字符串（rust_decimal 走字符串更常见），
 * 表里混着 `"5"` / `5` / `5.0000` 三种长相很难看 → 统一 `Number()` 后去掉无意义的尾零。
 */
export function formatQty(value: number | string | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '—';
  }
  const num = Number(value);
  if (!Number.isFinite(num)) {
    return String(value);
  }
  return String(Number(num.toFixed(4)));
}

/** 明细数（后端没下发 `item_count` 时显示 `—`，不假装 0） */
export function itemCountText(row: Pick<TransferRow, 'item_count'>): string {
  return typeof row.item_count === 'number' ? String(row.item_count) : '—';
}

/** 空值占位（表格里 null 不要留白，留白看着像脏数据） */
export function dash(value: string | null | undefined): string {
  const text = (value ?? '').trim();
  return text === '' ? '—' : text;
}

/** RFC3339 / `YYYY-MM-DD` → `YYYY-MM-DD HH:mm`（无时间部分则原样回 `YYYY-MM-DD`） */
export function formatTime(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  if (/^\d{4}-\d{2}-\d{2}$/.test(value)) {
    return value;
  }
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return '—';
  }
  const pad = (n: number): string => String(n).padStart(2, '0');
  return (
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`
    + ` ${pad(date.getHours())}:${pad(date.getMinutes())}`
  );
}

/** uuid 前 8 位（表里放全串撑爆列宽） */
export function shortId(id: string | null | undefined): string {
  if (!id) {
    return '—';
  }
  return id.length > 8 ? id.slice(0, 8) : id;
}

/** 单条明细的序列号摘要（表格里最多显示 3 个 + 「+N」） */
export function serialSummary(serials: TransferItemSerial[] | undefined, limit = 3): string {
  const list = serials ?? [];
  if (list.length === 0) {
    return '—';
  }
  const names = list.slice(0, limit).map((serial) => serial.serial_no ?? shortId(serial.serial_id));
  const rest = list.length - names.length;
  return rest > 0 ? `${names.join(' / ')} +${rest}` : names.join(' / ');
}

// ==================== 审批进度（弱引用 biz_flow_instance） ====================

/** 审批时间线一步（页面只渲染这个结构） */
export interface ApprovalStep {
  /** 动作类型（决定圆点颜色） */
  tone: 'submit' | 'approve' | 'reject' | 'end' | 'pending';
  /** 谁做的（提交人 / 审批人 / 「系统」） */
  who: string;
  /** 做了什么（「提交」「同意」「驳回」…） */
  what: string;
  /** 时间（RFC3339，空串 = 还没发生） */
  at: string;
  /** 审批意见 */
  comment: string;
  /** 是否系统代点 */
  isAuto: boolean;
  /** 是否自审 */
  isSelf: boolean;
}

/** 页面需要的审批详情子集（只要时间线，避免整个 `FlowInstanceDetail` 依赖） */
export interface ApprovalDetailLike {
  /** 实例摘要（`started_at` / `ended_at` / `initiator_name` / `status` / `instance_no`） */
  instance: {
    initiator_name: string;
    instance_no: string;
    status: string;
    started_at: string;
    ended_at: string | null;
  };
  /** 审批时间线（`TaskRes[]`） */
  timeline: Array<{
    assignee_name: string;
    status: string;
    comment: string;
    created_at: string;
    completed_at: string | null;
    is_auto: boolean;
    is_self_approval: boolean;
    current_node_name: string;
  }>;
}

/** 待办状态 → 动作中文（`FlowTaskItem.status` 的 7 个值） */
export function approvalTaskLabel(status: string): string {
  switch (status) {
    case 'APPROVED':
      return '同意';
    case 'REJECTED':
      return '驳回';
    case 'SKIPPED':
      return '跳过（无人可派）';
    case 'TERMINATED':
      return '终止';
    case 'TRANSFERRED':
      return '转办';
    case 'DELEGATED':
      return '委派';
    case 'PENDING':
      return '待审批';
    default:
      return status;
  }
}

/** 待办状态 → 时间线圆点颜色 */
export function approvalToneOf(status: string): ApprovalStep['tone'] {
  switch (status) {
    case 'APPROVED':
      return 'approve';
    case 'REJECTED':
      return 'reject';
    case 'PENDING':
      return 'pending';
    default:
      return 'end';
  }
}

/**
 * `FlowInstanceDetail` → 审批进度时间线。
 *
 * 三段拼起来：
 * 1. 头部「提交」—— 发起人 + `instance.started_at`（**不是** timeline 里的第一条，
 *    timeline 只有待办，不含发起动作）；
 * 2. 中间每个待办一条（同意 / 驳回 / 跳过 / 转办…），时间取 `completed_at ?? created_at`；
 * 3. 尾部「完成」—— `instance.status` 的中文 + `ended_at`（实例还在跑则不出这一步，
 *    改成一条 `pending` 的「审批中」）。
 *
 * 实例状态中文：`RUNNING/APPROVED/REJECTED/WITHDRAWN/VOID/CANCELLED`。
 */
export function buildApprovalSteps(detail: ApprovalDetailLike): ApprovalStep[] {
  const instance = detail.instance;
  const steps: ApprovalStep[] = [
    {
      tone: 'submit',
      who: instance.initiator_name || '—',
      what: '提交',
      at: instance.started_at,
      comment: '',
      isAuto: false,
      isSelf: false,
    },
  ];

  for (const task of detail.timeline ?? []) {
    steps.push({
      tone: approvalToneOf(task.status),
      who: task.assignee_name || '—',
      what: `${task.current_node_name ? `${task.current_node_name}：` : ''}${approvalTaskLabel(task.status)}`,
      at: task.completed_at ?? task.created_at,
      comment: task.comment ?? '',
      isAuto: Boolean(task.is_auto),
      isSelf: Boolean(task.is_self_approval),
    });
  }

  if (instance.ended_at) {
    steps.push({
      tone: instance.status === 'APPROVED' ? 'approve' : instance.status === 'RUNNING' ? 'pending' : 'reject',
      who: '系统',
      what: instanceStatusLabel(instance.status),
      at: instance.ended_at,
      comment: '',
      isAuto: false,
      isSelf: false,
    });
  } else {
    steps.push({
      tone: 'pending',
      who: '系统',
      what: '审批中',
      at: '',
      comment: '',
      isAuto: false,
      isSelf: false,
    });
  }

  return steps;
}

/** 审批实例状态 → 中文（`flow/dto.rs` 的 6 个值） */
export function instanceStatusLabel(status: string): string {
  switch (status) {
    case 'RUNNING':
      return '审批中';
    case 'APPROVED':
      return '审批通过';
    case 'REJECTED':
      return '审批驳回';
    case 'WITHDRAWN':
      return '已撤回';
    case 'VOID':
      return '已作废';
    case 'CANCELLED':
      return '已取消';
    default:
      return status;
  }
}

/**
 * 深链参数里的调拨单号（`/business/erp/transfer?transfer_no=TR-…`）。
 *
 * 结账 / 年结的「挡路单据明细」给调拨单算的路由就是这种形态
 * （见后端 `period_items::route_of`），但调拨页原来**不消费**这个参数 ——
 * 点「去处理」只会落到一张没筛选的列表上（voucher / purchase / stock-in 三页都有深链处理，
 * 唯独这里缺）。
 *
 * 只接受字符串（或数组首元素）：用户手敲的 query 可能是 `?transfer_no=a&transfer_no=b`，
 * 取第一个；其它类型（对象 / 数字）一律当没带，不让页面崩在 `trim()` 上。
 */
export function transferNoFromQuery(query: Record<string, unknown> | null | undefined): string {
  const raw = query?.transfer_no;
  const first = Array.isArray(raw) ? raw[0] : raw;
  return typeof first === 'string' ? first.trim() : '';
}
