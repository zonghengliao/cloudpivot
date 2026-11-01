/**
 * 审批详情 / 待办卡片的**表单字段呈现层**（纯函数，零 Vue 依赖，可单测）。
 *
 * # 为什么要有这一层（P1-FIX）
 *
 * 老板报的原话：「审批内容里都是显示的是字段名和 ID 号。这个没法看出具体内容」。
 * 原来 `instance/detail.vue` 的口径是「键名原样展示 + `String(v)`」，于是审批页上出现
 *
 * ```text
 * supplier_id: 01a18def-1a2b-7329-a9d3-c6afa9e04220
 * total_with_tax: 226.0000
 * price_mode: EXCL
 * ```
 *
 * 三条硬约束（卡面「六、硬红线」）决定了本文件的形状：
 *
 * | # | 约束 | 落地方式 |
 * |---|---|---|
 * | 2 | 不认识的字段**降级显示原键名**（不许丢信息） | {@link fieldLabel} 查不到就原样返回键名 |
 * | 3 | ID 查不到名称时**显示 ID 前 8 位**（不许空白） | {@link formatIdValue} 兜底 `(ID: 01a18def…)` |
 * | 4 | 明细**必须渲染成表格**（不许 JSON 字符串） | {@link buildFormItemTables} 把「对象数组」拆成列 + 行 |
 *
 * # 真实 `form_data` 长什么样（别照卡面样例猜）
 *
 * 2026-11-01 生产库 `biz_flow_instance.form_data` 实测（采购 PO_APPROVE）：
 *
 * ```json
 * { "amount": 226.0, "order_id": "01a18dee-…", "order_no": "PO-20261031-000003",
 *   "create_by": "01a18bc8-…", "total_qty": "100.0000", "price_mode": "EXCL",
 *   "supplier_id": "d1370005-…", "total_amount": "200.0000",
 *   "warehouse_id": "d1370001-…", "total_with_tax": "226.0000" }
 * ```
 *
 * 两个和卡面设计不一致的事实，都在这里按**真实数据**处理：
 *
 * 1. **金额是「数字字符串」**（`"226.0000"`），只有 `amount` 是 JSON 数字
 *    （`purchase_handler::purchase_form_data` 刻意这么写：条件边比较要 `as_f64()`）。
 *    所以格式化必须能吃字符串，不能只判 `typeof === 'number'`。
 * 2. **`form_data` 里没有 `items`**（也没有 `supplier_name` / `order_date`）。
 *    明细得另外走业务单接口（`flow-business-items.ts`），译名得走主数据
 *    （`use-flow-master-names.ts`）—— 这里只负责「拿到数据之后怎么显示」。
 */

/** 空值占位（`null` / `undefined` / 空串统一显示它，绝不显示空白） */
export const EMPTY_PLACEHOLDER = '—';

/**
 * 流程表单字段的中文标签。不认识的键降级显示原键名（不丢信息）。
 *
 * 键名口径 = 真实 `form_data` 的键（见文件头注释），不是卡面样例。
 */
export const FORM_FIELD_LABELS: Record<string, string> = {
  // ===== 采购（`purchase_handler::purchase_form_data`）=====
  order_no: '采购单号',
  order_id: '采购单',
  supplier_id: '供应商',
  warehouse_id: '收货仓库',
  order_date: '下单日期',
  expect_date: '期望到货',
  credit_days: '账期(天)',
  price_mode: '金额口径',
  total_amount: '价款',
  total_tax: '税额',
  total_with_tax: '价税合计',
  remark: '备注',

  // ===== 收货（`stockin_handler::stock_in_form_data`）=====
  in_no: '收货单号',
  stock_in_id: '收货单',
  in_type: '入库类型',
  in_date: '入库日期',
  invoice_received: '发票已到',
  source_order_no: '源单号',

  // ===== 调拨（`transfer_handler`）=====
  transfer_no: '调拨单号',
  transfer_id: '调拨单',
  from_warehouse_id: '调出仓库',
  to_warehouse_id: '调入仓库',

  // ===== 付款（`payable_payment`）=====
  payment_no: '付款单号',
  payment_id: '付款单',
  payable_id: '应付单',
  payable_no: '应付单号',
  amount_text: '付款金额(原值)',
  unpaid_amount: '未付金额',
  amount: '金额',
  pay_date: '付款日期',
  pay_method: '付款方式',

  // ===== 明细行（业务单接口下发的行字段）=====
  items: '明细',
  line_no: '行号',
  material_id: '物料',
  material_code: '物料编码',
  material_name: '物料名称',
  spec: '规格',
  unit_id: '单位',
  qty: '数量',
  price: '单价',
  price_excl: '不含税单价',
  price_incl: '含税单价',
  tax_rate: '税率(%)',
  tax_amount: '税额',
  with_tax_amount: '价税合计',
  received_qty: '已收货数量',

  // ===== 通用 =====
  status: '状态',
  create_by: '制单人',
  initiator: '发起人',
  total_qty: '数量合计',
};

/**
 * 枚举值的中文映射（`键 → 值 → 中文`）。
 *
 * 只收「本仓 `form_data` 真会出现」的枚举；真值来自后端 CHECK 约束 / 常量：
 * `price_mode` 见 `purchase_handler::PRICE_MODES`，`in_type` 见 `V0xxx` 的 `ck_biz_stock_in_type`，
 * `status` 见各模块状态机。查不到就原样显示大写值（不猜）。
 */
export const FORM_ENUM_LABELS: Record<string, Record<string, string>> = {
  price_mode: { EXCL: '不含税', INCL: '含税' },
  in_type: {
    PURCHASE: '采购收货',
    TRANSFER: '调拨入库',
    PRODUCE: '生产入库',
    RETURN: '退货入库',
    OTHER: '其他入库',
  },
  status: {
    DRAFT: '草稿',
    SUBMITTED: '已提交',
    PENDING: '审批中',
    RUNNING: '审批中',
    APPROVED: '已审批',
    PARTIAL_RECEIVED: '部分收货',
    RECEIVED: '已收货',
    CLOSED: '已关闭',
    CANCELLED: '已取消',
    POSTED: '已过账',
    REJECTED: '已驳回',
    WITHDRAWN: '已撤回',
    VOID: '已作废',
  },
  pay_method: {
    TRANSFER: '银行转账',
    CASH: '现金',
    CHECK: '支票',
    ACCEPTANCE: '承兑汇票',
    OTHER: '其他',
  },
};

/** 字段值的译名来源（决定去查哪张主数据表） */
export type FormNameSource = 'supplier' | 'warehouse' | 'material' | 'unit' | 'user';

/**
 * 哪些字段的值是**主数据 id**，需要译成名称。
 *
 * 键一律是「字段键」（明细行里的键同名，所以一张表两处都能用）。
 * 没列进来的 uuid 字段照样会走 `(ID: xxxxxxxx…)` 兜底，只是查不到名称。
 */
export const FIELD_NAME_SOURCE: Record<string, FormNameSource> = {
  supplier_id: 'supplier',
  from_warehouse_id: 'warehouse',
  to_warehouse_id: 'warehouse',
  warehouse_id: 'warehouse',
  material_id: 'material',
  unit_id: 'unit',
  create_by: 'user',
};

/** 译名函数：`(字段键, id) → 名称`；查不到返回 `undefined`（由调用方兜底） */
export type FormNameLookup = (key: string, id: string) => string | undefined;

/** 一个待显示的字段 */
export interface FormField {
  /** 原始键（嵌套字段是 `form.xxx` 这种点号路径） */
  key: string;
  /** 中文标签（查不到 = 原键名） */
  label: string;
  /** 已格式化的显示值 */
  display: string;
  /** 是否为空值（UI 可置灰，便于一眼看出「这里没填」） */
  empty: boolean;
}

/** 明细表格的一列 */
export interface FormItemColumn {
  /** 表格内部的列键（`c0` / `c1`…；TDesign 的 `colKey`） */
  colKey: string;
  /** 列标题（中文；查不到的原始键名） */
  title: string;
}

/** 明细表格 */
export interface FormItemTable {
  /** 原始键（`items` / `form.items`） */
  key: string;
  /** 表名（如「明细」） */
  label: string;
  /** 列定义（直接喂 `t-table` 的 `columns`） */
  columns: FormItemColumn[];
  /** 行（已格式化成纯字符串，键是 `colKey` + `__rowKey`） */
  rows: Array<Record<string, string>>;
}

/** UUID 形状（本仓主键是 uuid v7；大小写都认） */
const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** 十进制数字字符串（允许负号 / 小数；`"226.0000"` 命中，`"12a"` 不命中） */
const DECIMAL_RE = /^-?\d+(\.\d+)?$/;

/** 是否 uuid 形状 */
export function isUuidLike(value: unknown): boolean {
  return typeof value === 'string' && UUID_RE.test(value.trim());
}

/** 键的叶子段（`form.amount` → `amount`） */
export function leafKey(key: string): string {
  const index = key.lastIndexOf('.');
  return index >= 0 ? key.slice(index + 1) : key;
}

/**
 * 字段键 → 中文标签。
 *
 * 三级降级：**完整键** → **叶子键**（嵌套字段 `form.amount` 用 `amount` 的标签） → **原键名**。
 * 最后一级是硬红线 2：不认识就原样显示，不许吞掉字段。
 */
export function fieldLabel(key: string): string {
  const trimmed = String(key ?? '').trim();
  if (!trimmed) {
    return String(key ?? '');
  }
  const exact = FORM_FIELD_LABELS[trimmed];
  if (exact) {
    return exact;
  }
  const leaf = leafKey(trimmed);
  if (leaf !== trimmed && FORM_FIELD_LABELS[leaf]) {
    return FORM_FIELD_LABELS[leaf];
  }
  return trimmed;
}

/** 数量类字段（合计行也有 `.0000`，必须先于金额判断，否则会被格式化成 `¥100.00`） */
function isQtyField(key: string): boolean {
  return /qty|quantity/i.test(key);
}

/**
 * 比例 / 天数 / 计数类：长得像金额但**不是**金额（`tax_rate: 13` 不能变 `¥13.00`）
 */
function isRatioField(key: string): boolean {
  return /rate|ratio|percent|days|count/i.test(key);
}

/**
 * 「纯技术引用」键 → 它对应的**人可读单号**键。
 *
 * 例：采购 `form_data` 同时有 `order_id`（uuid）与 `order_no`（`PO-20261031-000003`）。
 * 两个都显示时，`order_id` 那行只会多出一串 `(ID: 01a18dee…)` 噪声 —— 老板报的正是这个。
 * 所以：**单号存在时隐藏 id**（信息没丢，单号才是人看的那个）；单号不在时 id 照常显示。
 */
const INTERNAL_REF_NO_KEY: Record<string, string> = {
  order_id: 'order_no',
  stock_in_id: 'in_no',
  transfer_id: 'transfer_no',
  payment_id: 'payment_no',
  payable_id: 'payable_no',
};

/**
 * 「同一件事说两遍」的字段 → 它**已有的等价字段**。
 *
 * 采购 `form_data` 里 `amount` 是给条件节点算边用的数字（= `total_with_tax`），
 * 页面上再显示一遍「金额 ¥226.00」纯属重复；付款里 `amount_text` 又是 `amount` 的字符串原值。
 * 规则：**等价字段在场时**隐藏那个技术性更强的（信息没丢，人看的那个还在）。
 */
const DUPLICATE_KEYS: Record<string, string> = {
  amount: 'total_with_tax',
  amount_text: 'amount',
};

/**
 * 字段的显示顺序（业务上从上往下读的顺序：单号 → 供应商/仓库 → 日期 → 金额 → 数量 → 备注 → 人）。
 *
 * 不在这里的键按**原始顺序**排在后面 —— 顺序只影响观感，不影响「有没有丢字段」。
 */
const FIELD_ORDER = [
  'order_no',
  'in_no',
  'transfer_no',
  'payment_no',
  'payable_no',
  'supplier_id',
  'from_warehouse_id',
  'to_warehouse_id',
  'warehouse_id',
  'in_type',
  'price_mode',
  'order_date',
  'in_date',
  'pay_date',
  'expect_date',
  'total_amount',
  'tax_amount',
  'total_tax',
  'total_with_tax',
  'with_tax_amount',
  'amount',
  'total_qty',
  'credit_days',
  'status',
  'remark',
  'create_by',
];

/** 金额类字段（`amount` / `price` / `tax` / `total` / `fee`），排除数量与比例 */
function isAmountField(key: string): boolean {
  if (isQtyField(key) || isRatioField(key)) {
    return false;
  }
  return /amount|price|tax|total|money|fee/i.test(key);
}

/** 值 → 数字（数字直接过；数字字符串按十进制解析；其余 `null`） */
export function toNumber(value: unknown): number | null {
  if (typeof value === 'number') {
    return Number.isFinite(value) ? value : null;
  }
  if (typeof value === 'string') {
    const text = value.trim();
    if (!DECIMAL_RE.test(text)) {
      return null;
    }
    const parsed = Number(text);
    return Number.isFinite(parsed) ? parsed : null;
  }
  return null;
}

/** 千分位（`"1234567.89"` → `"1,234,567.89"`；负号、小数段原样保留） */
export function groupThousands(text: string): string {
  const negative = text.startsWith('-');
  const body = negative ? text.slice(1) : text;
  const [intPart, fracPart] = body.split('.');
  const grouped = intPart.replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  const suffix = fracPart === undefined ? '' : `.${fracPart}`;
  return `${negative ? '-' : ''}${grouped}${suffix}`;
}

/**
 * 金额 → `¥226.00`（固定两位小数 + 千分位）。
 *
 * 非数字返回 `null`（由调用方决定降级成原值），这样 `price_mode: "EXCL"` 不会被误格式化。
 */
export function formatMoney(value: unknown): string | null {
  const number = toNumber(value);
  if (number === null) {
    return null;
  }
  const fixed = Math.abs(number).toFixed(2);
  const signed = number < 0 ? `-${fixed}` : fixed;
  return `¥${groupThousands(signed)}`;
}

/**
 * 数量 → `100` / `3.5`（去掉 `"100.0000"` 的尾巴 0，最多保留 4 位小数）。
 *
 * 不补 `¥`、不带单位：单位来自主数据（明细行有 `unit_id` 时由
 * {@link buildItemTable} 追加），顶层 `total_qty` 不知道单位就不编。
 */
export function formatQty(value: unknown): string | null {
  const number = toNumber(value);
  if (number === null) {
    return null;
  }
  const fixed = Math.abs(number).toFixed(4);
  const trimmed = fixed.includes('.') ? fixed.replace(/0+$/, '').replace(/\.$/, '') : fixed;
  const text = trimmed === '' || trimmed === '0' ? '0' : trimmed;
  return groupThousands(number < 0 && text !== '0' ? `-${text}` : text);
}

/**
 * 主数据 id → 名称。
 *
 * 查不到就 `(ID: 前 8 位…)`（硬红线 3：不许空白，也不许把整条 uuid 糊在页面上）。
 */
export function formatIdValue(id: string, lookup?: FormNameLookup, key = ''): string {
  const text = String(id ?? '').trim();
  if (!text) {
    return EMPTY_PLACEHOLDER;
  }
  const name = lookup?.(key, text)?.trim();
  if (name) {
    return name;
  }
  return `(ID: ${text.slice(0, 8)}…)`;
}

/** 对象数组 = 明细（元素都是普通对象；`[{...}]` 才算，`[1,2]` 算普通数组） */
export function isItemRows(value: unknown): value is Array<Record<string, unknown>> {
  return (
    Array.isArray(value) &&
    value.length > 0 &&
    value.every((item) => typeof item === 'object' && item !== null && !Array.isArray(item))
  );
}

/** 该字段的值是不是「主数据 id」（决定要不要译名；`from_warehouse_id` 这类前缀不规则的也在表里） */
export function isNameField(key: string): boolean {
  if (FIELD_NAME_SOURCE[key]) {
    return true;
  }
  const bare = key.endsWith('_id') ? key.slice(0, -3) : key;
  return (NAME_SOURCE_NAMES as string[]).includes(bare);
}

/** 合法的译名来源名（`_id` 结尾的键去掉后缀能直接对上） */
const NAME_SOURCE_NAMES: FormNameSource[] = ['supplier', 'warehouse', 'material', 'unit', 'user'];

/**
 * 按字段类型格式化**单个值**。
 *
 * 判定顺序（每一步都有单测钉住）：
 * `空` → `布尔` → `明细数组` → `普通数组` → `对象` → `枚举`（含键的叶子段）→ `uuid 译名` → `数量` → `金额` → 原值。
 *
 * 顺序里两个容易踩的点：
 * - **枚举要在金额前**：`status: "POSTED"` 不会进金额分支（非数字），但 `price_mode: "EXCL"`
 *   若先走金额判定会得到 `null` 再落到枚举，结果一样；显式排前面是为了「`status: "1"` 这种脏数据」
 *   也能按枚举映射。
 * - **数量要在金额前**：`total_qty: "100.0000"` 命中 `total`，不先判 `qty` 就会变成 `¥100.00`。
 */
export function formatFieldValue(
  key: string,
  value: unknown,
  lookup?: FormNameLookup,
): string {
  if (value === null || value === undefined) {
    return EMPTY_PLACEHOLDER;
  }
  if (typeof value === 'string' && value.trim() === '') {
    return EMPTY_PLACEHOLDER;
  }
  if (typeof value === 'boolean') {
    return value ? '是' : '否';
  }
  if (isItemRows(value)) {
    return `${value.length} 行`;
  }
  if (Array.isArray(value)) {
    if (value.length === 0) {
      return EMPTY_PLACEHOLDER;
    }
    return value
      .map((item) => (item === null || item === undefined ? '' : String(item)))
      .filter((text) => text !== '')
      .join('、');
  }
  if (typeof value === 'object') {
    // 嵌套对象在 `collectEntries` 里已被展开；走到这里说明是「深层 / 循环引用」之类的兜底，
    // 宁可给 JSON 也不丢信息（明细数组已在上面被拦掉，不会违反硬红线 4）。
    try {
      return JSON.stringify(value);
    } catch {
      return String(value);
    }
  }

  const text = typeof value === 'string' ? value.trim() : String(value);
  const enumLabel = FORM_ENUM_LABELS[key]?.[text] ?? FORM_ENUM_LABELS[leafKey(key)]?.[text];
  if (enumLabel) {
    return enumLabel;
  }
  if (isNameField(key)) {
    const name = lookup?.(key, text)?.trim();
    if (name) {
      return name;
    }
    // 查不到名称：uuid 给前 8 位（硬红线 3），非 uuid 的编码（`m-1`）就原样给
    return isUuidLike(text) ? formatIdValue(text) : text;
  }
  if (isUuidLike(text)) {
    return formatIdValue(text, lookup, key);
  }
  if (isQtyField(key)) {
    const qty = formatQty(text);
    if (qty !== null) {
      return qty;
    }
  }
  if (isRatioField(key)) {
    // `tax_rate: "13.0000"` → `13`（去掉尾随 0；比例类不加 ¥、不加千分位语义）
    const ratio = formatQty(text);
    if (ratio !== null) {
      return ratio;
    }
  }
  if (isAmountField(key)) {
    const money = formatMoney(text);
    if (money !== null) {
      return money;
    }
  }
  // 日期（`2026-10-31` / ISO 串）本来就人可读，原样返回（卡面单测 ⑨）
  return text;
}

/** 展开后的一条（嵌套字段用点号路径） */
interface FlatEntry {
  /** 点号路径键 */
  key: string;
  /** 原始值 */
  value: unknown;
}

/** 展开对象的层数上限（超过就整块交给 `formatFieldValue` 兜底，防脏数据打爆页面） */
const MAX_FLATTEN_DEPTH = 3;

/**
 * 把 `form_data` 展开成扁平条目。
 *
 * - 普通对象递归展开（`form: { amount: 1 }` → `form.amount`），最多 3 层；
 * - 数组 / 空对象 / `null` 不展开（数组要么是明细、要么是枚举多选，整条显示）；
 * - 空对象展开后没有条目 —— 那是「字段存在但没内容」，不该在页面上凭空多一行。
 */
function collectEntries(
  data: Record<string, unknown>,
  prefix = '',
  depth = 0,
  out: FlatEntry[] = [],
): FlatEntry[] {
  for (const [rawKey, value] of Object.entries(data)) {
    const key = prefix ? `${prefix}.${rawKey}` : rawKey;
    const isPlainObject =
      typeof value === 'object' &&
      value !== null &&
      !Array.isArray(value) &&
      Object.getPrototypeOf(value) === Object.prototype;
    if (isPlainObject && depth < MAX_FLATTEN_DEPTH) {
      collectEntries(value as Record<string, unknown>, key, depth + 1, out);
      continue;
    }
    out.push({ key, value });
  }
  return out;
}

/**
 * 表单字段列表（**不含明细数组**，明细走 {@link buildFormItemTables}）。
 *
 * 三件事：未知键显示原键名（硬红线 2）、空值显示 `—`（单测 ⑥）、
 * 「单号存在时隐藏对应的技术 id」（{@link INTERNAL_REF_NO_KEY}）、按 {@link FIELD_ORDER} 排序。
 */
export function buildFormFields(
  formData: Record<string, unknown> | null | undefined,
  lookup?: FormNameLookup,
): FormField[] {
  if (!formData || typeof formData !== 'object') {
    return [];
  }
  return collectEntries(formData)
    .filter((entry) => !isItemRows(entry.value))
    .filter((entry) => !isHiddenRef(entry.key, formData) && !isDuplicateKey(entry.key, formData))
    .map((entry, index) => ({ entry, index }))
    .sort((left, right) => orderRank(left) - orderRank(right) || left.index - right.index)
    .map(({ entry }) => {
      const display = formatFieldValue(entry.key, entry.value, lookup);
      return {
        key: entry.key,
        label: fieldLabel(entry.key),
        display,
        empty:
          display === EMPTY_PLACEHOLDER ||
          (Array.isArray(entry.value) && entry.value.length === 0),
      };
    });
}

/** 技术引用键是否该藏（前提：同一条 `form_data` 里有它的单号） */
function isHiddenRef(key: string, formData: Record<string, unknown>): boolean {
  const noKey = INTERNAL_REF_NO_KEY[key];
  if (!noKey) {
    return false;
  }
  const noValue = formData[noKey];
  return typeof noValue === 'string' && noValue.trim() !== '';
}

/** 重复字段是否该藏（前提：同一条 `form_data` 里有它的等价字段且非空） */
function isDuplicateKey(key: string, formData: Record<string, unknown>): boolean {
  const twin = DUPLICATE_KEYS[key];
  if (!twin) {
    return false;
  }
  const twinValue = formData[twin];
  return twinValue !== null && twinValue !== undefined && String(twinValue).trim() !== '';
}

/** 排序权重（不在 {@link FIELD_ORDER} 里的排最后，保持原始顺序） */
function orderRank(item: { entry: FlatEntry; index: number }): number {
  const rank = FIELD_ORDER.indexOf(leafKey(item.entry.key));
  return rank < 0 ? FIELD_ORDER.length : rank;
}

/** 明细列的顺序（先给「业务上最该看的」，其余键按出现顺序跟在后面） */
const ITEM_COLUMN_ORDER = [
  'line_no',
  'material_code',
  'material_name',
  'material_id',
  'spec',
  'qty',
  'unit_id',
  'price',
  'price_excl',
  'price_incl',
  'tax_rate',
  'amount',
  'tax_amount',
  'with_tax_amount',
  'received_qty',
  'remark',
];

/** 明细里**不显示**的技术列（主键 / 外键 / 审计时间；显示了只是噪声） */
const ITEM_HIDDEN_KEYS = new Set(['id', 'order_id', 'stock_in_id', 'transfer_id', 'payable_id', 'created_at', 'updated_at', 'deleted_at']);

/** 数量列：这些键格式化后要追加单位（如 `100 件`） */
function isUnitAttachableKey(key: string): boolean {
  return /qty|quantity/i.test(key);
}

/**
 * 明细行 → 表格（列标题中文、单元格已格式化、数量自动带单位）。
 *
 * @param key 原始字段键（用于 `:key`）
 * @param label 表名（一般是 `fieldLabel(key)`）
 * @param rows 明细行（业务单接口 / `form_data` 的数组）
 * @param lookup 译名函数（物料 / 单位）
 */
export function buildItemTable(
  key: string,
  label: string,
  rows: Array<Record<string, unknown>>,
  lookup?: FormNameLookup,
): FormItemTable {
  const present = new Set<string>();
  for (const row of rows) {
    for (const rowKey of Object.keys(row)) {
      if (!ITEM_HIDDEN_KEYS.has(rowKey)) {
        present.add(rowKey);
      }
    }
  }
  const hasQtyColumn = [...present].some((rowKey) => isUnitAttachableKey(rowKey));
  if (hasQtyColumn) {
    // 数量列已经带单位（`100 件`），再单开一列「单位」就是重复
    present.delete('unit_id');
  }
  const ordered = [
    ...ITEM_COLUMN_ORDER.filter((rowKey) => present.has(rowKey)),
    ...[...present].filter((rowKey) => !ITEM_COLUMN_ORDER.includes(rowKey)),
  ];
  const columns: FormItemColumn[] = ordered.map((rowKey, index) => ({
    colKey: `c${index}`,
    title: fieldLabel(rowKey),
  }));

  const tableRows = rows.map((row, rowIndex) => {
    // 行内单位（明细行有 `unit_id` 时，数量列显示成 `100 件`）
    const unitName = row.unit_id ? lookup?.('unit_id', String(row.unit_id))?.trim() : undefined;
    const cells: Record<string, string> = { __rowKey: `r${rowIndex}` };
    ordered.forEach((rowKey, index) => {
      let text = formatFieldValue(rowKey, row[rowKey], lookup);
      if (unitName && isUnitAttachableKey(rowKey) && text !== EMPTY_PLACEHOLDER) {
        text = `${text} ${unitName}`;
      }
      cells[`c${index}`] = text;
    });
    return cells;
  });

  return { key, label, columns, rows: tableRows };
}

/** `form_data` 里的所有明细表（对象数组；嵌套的也算，键是点号路径） */
export function buildFormItemTables(
  formData: Record<string, unknown> | null | undefined,
  lookup?: FormNameLookup,
): FormItemTable[] {
  if (!formData || typeof formData !== 'object') {
    return [];
  }
  return collectEntries(formData)
    .filter((entry) => isItemRows(entry.value))
    .map((entry) =>
      buildItemTable(
        entry.key,
        fieldLabel(entry.key),
        entry.value as Array<Record<string, unknown>>,
        lookup,
      ),
    );
}

/** 摘要字段的优先顺序（采购 / 收货 / 调拨 / 付款 四类单都能取到东西） */
const SUMMARY_PRIORITY = [
  'supplier_id',
  'from_warehouse_id',
  'to_warehouse_id',
  'warehouse_id',
  'total_with_tax',
  'amount',
  'unpaid_amount',
  'total_qty',
  'pay_date',
  'in_type',
  'price_mode',
];

/**
 * 待办卡片上那一行摘要（不是原始字段，给人看的）。
 *
 * 取「业务上最该先看到的」几个字段，按 `标签：值 · 标签：值` 拼：
 * `供应商：示例供应商（种子） · 价税合计：¥226.00 · 数量合计：100`
 *
 * - 只认白名单键（谁排前面按 `SUMMARY_PRIORITY`），避免把 `order_id` / `create_by` 这类噪声塞进摘要；
 * - 一个白名单键都没有（自定义流程 / 空表单）→ 返回空串，卡片不显示摘要行（不编造内容）。
 */
export function formSummary(
  formData: Record<string, unknown> | null | undefined,
  lookup?: FormNameLookup,
  maxParts = 3,
): string {
  if (!formData || typeof formData !== 'object' || maxParts <= 0) {
    return '';
  }
  const parts: string[] = [];
  for (const key of SUMMARY_PRIORITY) {
    if (parts.length >= maxParts) {
      break;
    }
    if (!(key in formData)) {
      continue;
    }
    const display = formatFieldValue(key, formData[key], lookup);
    if (display === EMPTY_PLACEHOLDER) {
      continue;
    }
    parts.push(`${fieldLabel(key)}：${display}`);
  }
  return parts.join(' · ');
}

