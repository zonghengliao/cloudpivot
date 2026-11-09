/**
 * 序列号列表页的纯逻辑（W4b-2）。
 *
 * 为什么单独一个 `.ts` 而不是全塞 `.vue`：本仓单测是 `tsx --test`（没有 DOM、没有
 * `@vue/test-utils`），`.vue` 里的判断一条都测不到。所以「状态怎么显示 / 这一行还能不能
 * 出库 / 扫码命中的行怎么在列表里标出来 / 查询参数怎么组装 / 批量生成请求体带什么」
 * 全放这里，`.vue` 只负责渲染和拍接口。
 *
 * # 口径来源（**不是卡面草图**）
 *
 * | 语义 | 真口径 | 出处 |
 * |---|---|---|
 * | 状态三值 | `IN_STOCK` / `OUTBOUND` / `SCRAPPED` | `backend/.../serial_model.rs::ALL_STATUSES` |
 * | 标签配色 | 在库=success(绿) / 已出库=primary(蓝) / 已报废=default(灰) | 卡面 §四·② 要求，theme 用 TDesign 语义色，**不写死 hex** |
 * | 状态机 | `IN_STOCK→OUTBOUND`、`IN_STOCK→SCRAPPED`、`OUTBOUND→SCRAPPED` | `serial_model.rs::can_transition` |
 * | 可写目标 | 只有 `OUTBOUND` / `SCRAPPED` | `serial_dto.rs::WRITABLE_TARGETS` |
 * | 来源类型 | `PURCHASE` / `TRANSFER` / `PRODUCE` / `RETURN` | `serial_model.rs::SOURCE_TYPES` |
 * | 单次上限 | 1000 | `serial_model.rs::MAX_BATCH_COUNT` |
 */
import type { SerialBatchRes, SerialRow, SerialSourceType, SerialStatus } from '@/api/business/erp';
import {
  SERIAL_SOURCE_TYPES,
  SERIAL_SOURCE_TYPE_LABELS,
} from '@/api/business/erp/doc-kinds';

/** 默认每页条数（服务端分页；序列号是一物一码，条数比主数据多一个量级） */
export const SERIAL_PAGE_SIZE = 20;

/** 单次批量生成上限（与 `serial_model::MAX_BATCH_COUNT` 一致，前端提前拦） */
export const MAX_BATCH_COUNT = 1000;

/**
 * 成本单价最多 6 位小数（与 `biz_inventory_txn.unit_cost numeric(18,6)` 同精度）。
 *
 * 前端只做「位数」校验，**不做舍入**：6 位四舍五入是后端 `stockin_model::unit_price`
 * （`MidpointAwayFromZero`，`:125`）的事，前端抹一次等于把两个数悄悄改成不同的值。
 */
export const MAX_UNIT_COST_DECIMALS = 6;

/**
 * 参数错误码（`shared::error_code::PARAM_INVALID`）。
 *
 * `batch-generate` 的 `unit_cost` 缺失 / `≤0` / 非数字 / 超范围都是 **HTTP 400 + 10001**，
 * 报文里已经写清「为什么必须填」，所以前端**原样透出**（见 `batchGenerateErrorText`）。
 */
export const PARAM_INVALID_CODE = 10001;

/** 状态的三态顺序（下拉 / 勾选条都用它，别处不要重排） */
export const SERIAL_STATUSES: readonly SerialStatus[] = ['IN_STOCK', 'OUTBOUND', 'SCRAPPED'];

/** 一个状态的显示口径 */
export interface SerialStatusMeta {
  /** 中文标签 */
  label: string;
  /** TDesign tag 主题：绿(success) / 蓝(primary) / 灰(default) */
  theme: 'success' | 'primary' | 'default';
}

/**
 * 状态 → 标签 / 配色。
 *
 * 颜色走 TDesign 的 `theme` 语义槽，**不写 `#2ba471` 这种裸 hex**：本仓有多套色板
 * （22 套 × 明暗双模式），写死 hex 会在暗色板下瞎眼，`pnpm check:palettes` 的语义也保不住。
 */
export const SERIAL_STATUS_META: Readonly<Record<SerialStatus, SerialStatusMeta>> = {
  IN_STOCK: { label: '在库', theme: 'success' },
  OUTBOUND: { label: '已出库', theme: 'primary' },
  SCRAPPED: { label: '已报废', theme: 'default' },
};

/** 状态下拉项（`''` = 全部 → 请求里整键缺席） */
export const SERIAL_STATUS_FILTER_OPTIONS: ReadonlyArray<{ label: string; value: SerialStatus }> =
  SERIAL_STATUSES.map((status) => ({ label: SERIAL_STATUS_META[status].label, value: status }));

/** 来源类型下拉项（批量生成弹窗用；`''` = 不填，后端按「无来源」处理）。
 *
 * P2-5 词表收敛：取值与中文都来自 `doc-kinds.ts` 的 `SERIAL_SOURCE_TYPES` /
 * `SERIAL_SOURCE_TYPE_LABELS`（= 后端 `SerialSourceType`），不再手抄一遍。 */
export const SOURCE_TYPE_OPTIONS: ReadonlyArray<{ label: string; value: SerialSourceType }> =
  SERIAL_SOURCE_TYPES.map((value) => ({
    label: SERIAL_SOURCE_TYPE_LABELS[value],
    value,
  }));

/** 状态 → 中文（未知值原样回显，不吞数据：后端加了新状态时页面至少不空白） */
export function serialStatusLabel(status: string): string {
  return SERIAL_STATUS_META[status as SerialStatus]?.label ?? status;
}

/** 状态 → tag 主题（未知值给 default，别让它继承上一个行的颜色） */
export function serialStatusTheme(status: string): SerialStatusMeta['theme'] {
  return SERIAL_STATUS_META[status as SerialStatus]?.theme ?? 'default';
}

/**
 * 状态机：`from → to` 是否允许。
 *
 * 与后端 `serial_model::can_transition` 逐条对齐：
 * `IN_STOCK→OUTBOUND` ✅、`IN_STOCK→SCRAPPED` ✅、`OUTBOUND→SCRAPPED` ✅，其余一律 ❌
 * （含原地流转 `IN_STOCK→IN_STOCK`：幂等重放没有业务意义，后端也是拒的）。
 */
export function canTransition(from: string, to: string): boolean {
  return (
    (from === 'IN_STOCK' && to === 'OUTBOUND') ||
    (from === 'IN_STOCK' && to === 'SCRAPPED') ||
    (from === 'OUTBOUND' && to === 'SCRAPPED')
  );
}

/** 某一行的按钮可用性 */
export interface SerialActions {
  /** 能否出库（只有 `IN_STOCK`） */
  outbound: boolean;
  /** 能否报废（`IN_STOCK` / `OUTBOUND`） */
  scrap: boolean;
}

/**
 * 行 → 可用的状态流转动作。
 *
 * 页面用它决定 `<t-link disabled>`，`disabled` 的同时还得给出理由（`serialActionHint`），
 * 否则用户只会看到两个灰字点不动。
 */
export function availableActions(status: string): SerialActions {
  return {
    outbound: canTransition(status, 'OUTBOUND'),
    scrap: canTransition(status, 'SCRAPPED'),
  };
}

/** 不可点时的原因（tooltip / 灰字说明） */
export function serialActionHint(status: string): string {
  switch (status) {
    case 'IN_STOCK':
      return '在库：可出库，也可直接报废';
    case 'OUTBOUND':
      return '已出库：只能报废，不能再出库（出库不可逆）';
    case 'SCRAPPED':
      return '已报废：终态，没有可执行的流转';
    default:
      return '未知状态：只读';
  }
}

// ==================== 筛选 / 分页参数 ====================

/** 筛选行状态 */
export interface SerialFilterState {
  /** 物料 id（`''` = 全部） */
  material_id: string;
  /** 仓库 id（`''` = 全部） */
  warehouse_id: string;
  /** 状态（`''` = 全部） */
  status: SerialStatus | '';
  /** 序列号（模糊；条码枪扫半截号也能搜） */
  serial_no: string;
}

/** 空筛选（重置用；每次返回新对象，避免共享引用被就地改） */
export function emptySerialFilter(): SerialFilterState {
  return { material_id: '', warehouse_id: '', status: '', serial_no: '' };
}

/**
 * 筛选行 → `GET /erp/serial/page` 的参数。
 *
 * 口径：`''` 一律**整键缺席**（不是发空串）。后端把 `?serial_no=` 当「等于空串」过滤，
 * 发了就查出 0 行 —— 这是本仓 API 层 `asParams` 的既有约定，这里保持一致。
 */
export function buildSerialQuery(
  filter: SerialFilterState,
  page: number,
  pageSize: number = SERIAL_PAGE_SIZE,
): Record<string, unknown> {
  const query: Record<string, unknown> = { page, page_size: pageSize };
  if (filter.material_id) {
    query.material_id = filter.material_id;
  }
  if (filter.warehouse_id) {
    query.warehouse_id = filter.warehouse_id;
  }
  if (filter.status) {
    query.status = filter.status;
  }
  const serialNo = filter.serial_no.trim();
  if (serialNo) {
    query.serial_no = serialNo;
  }
  return query;
}

/** 是否处于筛选态（空态文案要分「没数据」和「没匹配」） */
export function isSerialFiltered(filter: SerialFilterState): boolean {
  return (
    Boolean(filter.material_id) ||
    Boolean(filter.warehouse_id) ||
    Boolean(filter.status) ||
    Boolean(filter.serial_no.trim())
  );
}

// ==================== 扫码命中态 ====================

/** 列表行 + 「本次扫码命中」标记（`SerialRow` 的字段不动，纯附加） */
export type SerialRowWithScan = SerialRow & {
  /** 是否本次扫码命中（页面据此高亮 + 滚动到该行） */
  scanned: boolean;
};

/**
 * 把扫码结果合进当前页列表：命中的行打 `scanned: true`。
 *
 * 两个必须覆盖的情形：
 * 1. 命中行**在当前页** → 该行高亮，其余行 `false`；
 * 2. 命中行**不在当前页**（扫到一个第 3 页的号）→ 全部 `false`，由 `hitOnPage === false`
 *    告诉页面「别高亮，得先把筛选改成这个号再查」—— 否则页面会静默什么都不发生。
 *
 * 不修改入参（`rows.map` 出新对象），这样 `rows.value` 的原始数据不会被历史扫码污染。
 */
export function applyScanHighlight(
  rows: SerialRow[],
  scannedId: string | null,
): { list: SerialRowWithScan[]; hitOnPage: boolean } {
  let hitOnPage = false;
  const list = rows.map((row) => {
    const scanned = Boolean(scannedId) && row.id === scannedId;
    if (scanned) {
      hitOnPage = true;
    }
    return { ...row, scanned };
  });
  return { list, hitOnPage };
}

// ==================== 批量生成表单 ====================

/** 批量生成表单 */
export interface BatchGenerateFormState {
  /** 物料 id（下拉只列 `serial_required = true` 的） */
  material_id: string;
  /** 仓库 id（后端不变式①要求必填） */
  warehouse_id: string;
  /**
   * 入库库位（库位字典 `biz_location.id`，`''` = 不指定 → 后端按仓库默认库位兜底）。
   *
   * E2E-FIX-6：原来表单没有这个字段，提交也不发 `location_id` ⇒ 批量生成的库存
   * 永远落「未分配」桶，而收货 / 期初落「默认库位」桶 —— 同物料同仓两桶并存
   * （库位库存页显示「(未分配)」、扫码看不到库位、巡检报假差异）。
   *
   * 注意这里存的是**库位字典** id（`biz_location.id`），不是「库位库存行」id
   * （`biz_inventory_location.id`，批量生成前通常还不存在）；请求体字段名是
   * `location_dict_id`，与收货入库 `location_dict_id` 同口径。
   */
  location_dict_id: string;
  /** 数量 */
  count: number;
  /**
   * 成本单价（**必填**，不含税，> 0，最多 6 位小数）。
   *
   * 用**字符串**承载而不是 `number`：`t-input-number` 会把 `12.34` 变成 `12.339999999999999`，
   * 校验「最多 6 位小数」和金额预览都会被浮点误差带偏；`t-input` + 字符串与后端
   * `unit_cost`（收字符串、按 6 位解析）一一对应。
   */
  unit_cost: string;
  /** 来源类型（`''` = 不填） */
  source_type: SerialSourceType | '';
  /** 批次号（可空，≤64） */
  batch_no: string;
}

/** 表单字段错误（字段名 → 文案） */
export type BatchFormErrors = Partial<Record<keyof BatchGenerateFormState, string>>;

/** 空表单（新建时用） */
export function emptyBatchForm(): BatchGenerateFormState {
  return {
    material_id: '',
    warehouse_id: '',
    location_dict_id: '',
    count: 10,
    unit_cost: '',
    source_type: '',
    batch_no: '',
  };
}

/**
 * 从某仓库的库位列表里挑出「默认库位」，供批量生成表单**自动带出**。
 *
 * 口径与后端 `stockin_location::default_location_of` 一致：只看 `is_default = true`
 * 且**已启用**（`status === 1`）且未删除的行；每仓最多一个（库侧部分唯一索引兜底），
 * 万一有多行就取排序最靠前的那一行（与后端 `ORDER BY` 兜底行为一致）。
 *
 * 找不到就返回 `''` —— 表单留空，提交时后端按「仓库默认库位」再兜一次；
 * 仓库连默认库位都没有时两边都落「未分配」桶（收货也是这个行为，不在这里编造库位）。
 */
export function defaultLocationIdOf(
  locations: ReadonlyArray<{ id: string; is_default?: boolean; status?: number }>,
): string {
  const hit = locations.find((item) => item.is_default === true && (item.status ?? 1) === 1);
  return hit?.id ?? '';
}

/**
 * 仓库切换后，表单里的库位应该变成什么。
 *
 * - 没选仓库 → 清空（库位选择依赖仓库）；
 * - 选了仓库 → 默认带出该仓默认库位；
 * - 上一个库位仍在新仓库的列表里（切回来 / 多仓库同名）→ 保留用户的选择。
 *
 * 顺序很重要：先看「旧值是否还在列表里」，否则用户手选的库位会被默认值悄悄覆盖。
 */
export function locationAfterWarehouseSwitch(
  locations: ReadonlyArray<{ id: string; is_default?: boolean; status?: number }>,
  current: string,
): string {
  if (current && locations.some((item) => item.id === current)) {
    return current;
  }
  return defaultLocationIdOf(locations);
}

/**
 * 校验批量生成表单（前后端同口径，前端只是提前拦一次，后端仍会再判）。
 *
 * `warehouse_id` 必填不是「卡面要求」而是**不变式①**：一物一码物料的账面数量按
 * 「物料×仓库」由序列号派生，没仓库就没法记账（后端 `400 + 66005`）。
 */
export function validateBatchForm(form: BatchGenerateFormState): BatchFormErrors {
  const errors: BatchFormErrors = {};
  if (!form.material_id) {
    errors.material_id = '请选择物料（只有一物一码物料能生成序列号）';
  }
  if (!form.warehouse_id) {
    errors.warehouse_id = '请选择仓库：库存数量按「物料×仓库」由序列号派生';
  }
  const count = Number(form.count);
  if (!Number.isInteger(count) || count < 1) {
    errors.count = '数量必须是不小于 1 的整数';
  } else if (count > MAX_BATCH_COUNT) {
    errors.count = `单次最多生成 ${MAX_BATCH_COUNT} 个序列号`;
  }
  // 成本单价：批量生成 = 无来源入库，没有采购单可挂，成本只能由操作人录（ADR ①′）。
  // 顺序照后端 `BatchGenerateReq::parse_unit_cost`：空 → 非数字 → ≤0 → 位数（后端还有一条 < 1e11，交后端拒）。
  const unitCostText = form.unit_cost.trim();
  const unitCost = Number(unitCostText);
  if (!unitCostText) {
    errors.unit_cost = '请填写成本单价：批量生成按「期初/其他入库」记账，成本由操作人负责，必须录单价';
  } else if (!Number.isFinite(unitCost)) {
    errors.unit_cost = '成本单价必须是数字';
  } else if (unitCost <= 0) {
    errors.unit_cost = '成本单价必须大于 0';
  } else if (!hasAtMostDecimals(unitCostText, MAX_UNIT_COST_DECIMALS)) {
    errors.unit_cost = `成本单价最多 ${MAX_UNIT_COST_DECIMALS} 位小数`;
  }
  if (form.batch_no.trim().length > 64) {
    errors.batch_no = '批次号最长 64 个字符';
  }
  return errors;
}

/**
 * 小数位是否不超过 `max` 位。
 *
 * 只数**文本**的小数点后面几位（`"12.3400000"` → 7 位），不经过 `Number()`：浮点化之后
 * `12.3400000` 与 `12.34` 是同一个数，位数校验会直接失效。
 * 没有小数点（`"12"`）或指数写法（`"1e3"`）都算通过 —— 后端 `Decimal::from_str` 自己认。
 */
function hasAtMostDecimals(text: string, max: number): boolean {
  const dot = text.indexOf('.');
  return dot < 0 || text.length - dot - 1 <= max;
}

/**
 * 本次入库金额预览 = `数量 × 单价`，按 **4 位小数**（与 `biz_inventory.amount numeric(18,4)` 一致）。
 *
 * 非法输入（数量不是正整数、单价空/非正数）返回 `'—'`：预览区宁可留空也不要显示 `NaN`——
 * 会计看到 `NaN 元` 会以为系统坏了。
 */
export function formatBatchAmount(form: Pick<BatchGenerateFormState, 'count' | 'unit_cost'>): string {
  const count = Number(form.count);
  const unitCostText = (form.unit_cost ?? '').trim();
  const unitCost = Number(unitCostText);
  if (!Number.isInteger(count) || count < 1) {
    return '—';
  }
  if (!unitCostText || !Number.isFinite(unitCost) || unitCost <= 0) {
    return '—';
  }
  return (count * unitCost).toFixed(4);
}

/** 表单里有没有错（提交前一眼判空） */
export function hasBatchFormError(errors: BatchFormErrors): boolean {
  return Object.keys(errors).length > 0;
}

/** 表单 → `POST /erp/serial/batch-generate` 请求体（照 `BatchGenerateReq`，空值不发） */
export function buildBatchPayload(form: BatchGenerateFormState): {
  material_id: string;
  warehouse_id: string;
  location_dict_id?: string;
  count: number;
  unit_cost: number;
  source_type?: SerialSourceType;
  batch_no?: string;
} {
  const payload: {
    material_id: string;
    warehouse_id: string;
    location_dict_id?: string;
    count: number;
    unit_cost: number;
    source_type?: SerialSourceType;
    batch_no?: string;
  } = {
    material_id: form.material_id,
    warehouse_id: form.warehouse_id,
    count: Number(form.count),
    // 必填字段，**不做「空就 0」兜底**：0 会被后端 400（零成本入库拒收），把非法值悄悄变合法才是坑。
    // 字符串 → number 只是请求体的 JSON 形态，后端 `OptionalDecimal::Num/ Raw` 两种都收。
    unit_cost: Number(form.unit_cost),
  };
  // 入库库位（库位字典 id）：选了就发，没选就整键缺席 —— 后端按「仓库默认库位」兜底，
  // 与收货入库同一口径（E2E-FIX-6）。**不要**发空串：后端把空串当「没传」没问题，
  // 但 `location_dict_id: ''` 在日志/抓包里看起来像「传了一个空库位」，容易误判。
  const locationDictId = (form.location_dict_id ?? '').trim();
  if (locationDictId) {
    payload.location_dict_id = locationDictId;
  }
  if (form.source_type) {
    payload.source_type = form.source_type;
  }
  const batchNo = form.batch_no.trim();
  if (batchNo) {
    payload.batch_no = batchNo;
  }
  return payload;
}

// ==================== 展示辅助 ====================

/**
 * 批量生成失败 → 用户向文案。
 *
 * 400（`code = 10001`）的 `msg` 是后端**逐字**给的原因
 * （「unit_cost 必填：批量生成按「期初/其他入库」记账，必须录成本单价（不含税）」），
 * 里面已经写了「为什么必须填」，前端**不改写、不加前缀**（改写了单测就跟规格 §2.3 的报文对不上，
 * 也会把一个会计能看懂的句子变成一句抽象错误）；
 * 只有拿不到 msg 的场合（网关 502、空响应体、非业务错误）才回落到兜底文案。
 *
 * 用鸭子类型判（`message`），不 `import { ApiError }`：单测直接传 `{ code: 10001, message: '…' }` 就够。
 */
export function batchGenerateErrorText(error: unknown, fallback = '批量生成失败，请稍后重试'): string {
  if (typeof error === 'string') {
    return error.trim() || fallback;
  }
  if (typeof error === 'object' && error !== null) {
    const message = (error as { message?: unknown }).message;
    const text = typeof message === 'string' ? message.trim() : '';
    if (text) {
      return text;
    }
  }
  return fallback;
}

/**
 * 批量生成成功 → 一行摘要（弹窗提示 + 列表上方的成功条共用）。
 *
 * 金额 / 数量直接用后端回显的**字符串**拼接，不 `Number()` 再 `toFixed(4)`：
 * 账上是 4 位小数、均价 6 位，前端重算一遍就可能显示成跟账上不一样的数（B2 的容差只有 0.0001）。
 * 老后端（响应里没有 `amount` / `unit_cost`）时退化成「刷新后可见」，不显示 `undefined 元`。
 */
export function formatBatchResultText(
  result: Pick<SerialBatchRes, 'count' | 'period_key' | 'serial_nos'> & Partial<SerialBatchRes>,
): string {
  const head = result.serial_nos.slice(0, 3).join(' / ');
  const tail = result.serial_nos.length > 3 ? ' …' : '';
  const base = `生成了 ${result.count} 个序列号（${result.period_key}）：${head}${tail}`;
  const amount = (result.amount ?? '').trim();
  const unitCost = (result.unit_cost ?? '').trim();
  if (!amount || !unitCost) {
    return `${base}；本次入库已在服务端入账，刷新列表可见`;
  }
  const inventory = result.inventory;
  const stock = inventory
    ? `，账面结存 ${inventory.qty} / ${inventory.amount} 元（均价 ${inventory.avg_cost}）`
    : '';
  return `${base}；本次入库 ${result.count} 件 × ${unitCost} = ${amount} 元${stock}`;
}

/**
 * RFC3339 → `YYYY-MM-DD HH:mm`（北京时间）。
 *
 * 只做「截断 + 换 T」不做时区换算：后端出参是 `DateTime<Utc>`（`…Z`），直接 `new Date()`
 * 会按浏览器本地时区渲染，测试机与老大都在 UTC+8，直接本地化反而是对的；
 * 这里刻意不引 dayjs，避免为一行时间多一个运行时依赖。
 */
export function formatTime(value: string | null | undefined): string {
  if (!value) {
    return '—';
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

/** 空值占位（表格里 null 不要留白，留白看着像脏数据） */
export function dash(value: string | null | undefined): string {
  const text = (value ?? '').trim();
  return text === '' ? '—' : text;
}

/** 来源类型 → 中文（批量生成成功后回显用） */
export function sourceTypeLabel(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return SOURCE_TYPE_OPTIONS.find((item) => item.value === value)?.label ?? value;
}

/**
 * 「资源不存在」业务码（后端 `shared::error_code` 的通用 404 码）。
 *
 * 扫码未命中是 **HTTP 404 + `code = 10002`**（`serial_handler::not_found`），不是 200 带空体。
 * 页面必须把它译成「未找到该序列号」这种**红字提示**，而不是弹一个「请求失败」的红条 ——
 * 扫错号/扫到别人的标签，是仓库作业的日常，不是系统故障。
 */
export const SERIAL_NOT_FOUND_CODE = 10002;

/**
 * 判断一个错误是不是「序列号不存在」。
 *
 * 用鸭子类型判（`code` / `message`），不 `import { ApiError }`：连单测都不需要构造 axios，
 * 传 `{ code: 10002 }` 或 `{ message: '序列号 X 不存在或已删除' }` 就能测。
 * 两道判据并存是因为历史接口里 `10002` 偶尔被包成 `-1`（令牌刷新路径），文案更稳。
 */
export function isSerialNotFound(error: unknown): boolean {
  if (typeof error !== 'object' || error === null) {
    return false;
  }
  const code = (error as { code?: unknown }).code;
  if (code === SERIAL_NOT_FOUND_CODE) {
    return true;
  }
  const message = (error as { message?: unknown }).message;
  return typeof message === 'string' && message.includes('不存在');
}
