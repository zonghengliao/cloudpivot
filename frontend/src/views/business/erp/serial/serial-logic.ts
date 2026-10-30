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
import type { SerialRow, SerialSourceType, SerialStatus } from '@/api/business/erp';

/** 默认每页条数（服务端分页；序列号是一物一码，条数比主数据多一个量级） */
export const SERIAL_PAGE_SIZE = 20;

/** 单次批量生成上限（与 `serial_model::MAX_BATCH_COUNT` 一致，前端提前拦） */
export const MAX_BATCH_COUNT = 1000;

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

/** 来源类型下拉项（批量生成弹窗用；`''` = 不填，后端按「无来源」处理） */
export const SOURCE_TYPE_OPTIONS: ReadonlyArray<{ label: string; value: SerialSourceType }> = [
  { label: '采购入库', value: 'PURCHASE' },
  { label: '调拨入库', value: 'TRANSFER' },
  { label: '生产入库', value: 'PRODUCE' },
  { label: '退货入库', value: 'RETURN' },
];

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
  /** 数量 */
  count: number;
  /** 来源类型（`''` = 不填） */
  source_type: SerialSourceType | '';
  /** 批次号（可空，≤64） */
  batch_no: string;
}

/** 表单字段错误（字段名 → 文案） */
export type BatchFormErrors = Partial<Record<keyof BatchGenerateFormState, string>>;

/** 空表单（新建时用） */
export function emptyBatchForm(): BatchGenerateFormState {
  return { material_id: '', warehouse_id: '', count: 10, source_type: '', batch_no: '' };
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
  if (form.batch_no.trim().length > 64) {
    errors.batch_no = '批次号最长 64 个字符';
  }
  return errors;
}

/** 表单里有没有错（提交前一眼判空） */
export function hasBatchFormError(errors: BatchFormErrors): boolean {
  return Object.keys(errors).length > 0;
}

/** 表单 → `POST /erp/serial/batch-generate` 请求体（照 `BatchGenerateReq`，空值不发） */
export function buildBatchPayload(form: BatchGenerateFormState): {
  material_id: string;
  warehouse_id: string;
  count: number;
  source_type?: SerialSourceType;
  batch_no?: string;
} {
  const payload: {
    material_id: string;
    warehouse_id: string;
    count: number;
    source_type?: SerialSourceType;
    batch_no?: string;
  } = {
    material_id: form.material_id,
    warehouse_id: form.warehouse_id,
    count: Number(form.count),
  };
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
