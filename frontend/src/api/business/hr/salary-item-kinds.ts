/**
 * 薪资项配置页的出参契约、词表与公式编辑器纯逻辑（ARCH-9 §4.4.2 / §5.2 #32~#35 /
 * §6.2.4 · HR-44）。
 *
 * 纯逻辑文件：**不 import 请求层、不 import Vue**，`tsx --test` 里可以直接 import 做单测
 * （`tests/unit/hr-salary-item-page.test.ts` 会读 Rust 源码把词表与上限对拍）。
 * 请求函数在 `./salary-item.ts`，那里 `export * from './salary-item-kinds'`，
 * 页面只认 `@/api/business/hr/salary-item` 一个入口。
 *
 * # 字段名逐条抄自后端，不缩写
 *
 * | 块 | 后端出处 | 形状 |
 * |---|---|---|
 * | 列表行 | `salary_item_dto.rs::SalaryItemRes`（HR-31） | {@link SalaryItemRow} |
 * | 写入口 | `salary_item_dto.rs::SalaryItemWriteReq` | {@link SalaryItemForm} |
 * | 检查回执 | `salary_item_dto.rs::SalaryItemValidateRes`（#35） | {@link SalaryItemValidateRes} |
 * | 变量候选项 | `salary_item_dto.rs::SalaryItemVarOption` | {@link SalaryItemVarOption} |
 *
 * # 六处与「想当然」不一样的地方（先读，都会让页面做错事）
 *
 * 1. **前端不做求值**（§4.4.2 最后一行）：`ROUND` 的口径、`Decimal` 精度、惰性 `IF`、
 *    除零报错全在后端。本文件的 `tokenizeFormula` 只做**语法高亮**，不产生任何金额；
 *    要算钱走 `POST /salary/calc`（HR-42 的卡）。
 * 2. **「检查」与「保存」是同一份校验代码**（后端 `salary_item_dto` 头注释）：
 *    `POST /salary-item/validate` 与 `POST/PUT` 走同一套结构层 + 语义层。本文件的
 *    {@link validateItemForm} 只挡**后端还没机会看**的边（必填 / 列宽 / 科目分档），
 *    不做公式语义判断 —— 那必须让后端说，否则出现「前端说能存、后端 400」。
 * 3. **检查失败也是 HTTP 200**：`ok=false` 是「查询结论」不是请求错误（后端 #35 头注释），
 *    所以页面不能靠 `catch` 判断公式对不对，必须看 `res.ok`。
 * 4. **编码不可改**（后端 PUT 守卫 `item_code` 不可改）：公式按 `{编码}` 引用、
 *    `biz_employee_salary.item_code` 是弱引用（不建 FK），改名会让引用它的公式与薪资标准
 *    一起**静默**失效。所以编辑态把编码框 disable，而不是等 400。
 * 5. **系统项（`is_system=true`，即 `GROSS`/`NET`）只能改公式与名称**：后端 PUT 守卫
 *    拒绝停用、拒绝改 `item_type` / `calc_type`。{@link isTypeLocked} 决定那两个下拉是否 disable。
 * 6. **公式错误要能把光标落到出错位置**：后端错误串是中文人话，部分带「第 N 个字符」，
 *    部分只带被点名的变量（```` `{BASEE}` ````）。{@link formulaIssues} 把两种都换算成
 *    字符区间（{@link FormulaIssue.start} / `end`），页面据此高亮 + 把 textarea 选区设过去。
 *    这一层是纯函数，单测直接喂后端原文案断言位置。
 *
 * # 变量面板的两种来源
 *
 * 后端 `available` 里 `kind` 只有两个值：`ITEM`（已有薪资项编码）与 `ATTENDANCE`（考勤口径）。
 * 薪资项的 `label` 后端下发的是**编码本身**（`salary_item_dto::available_vars`），所以
 * {@link varPanelGroups} 优先用列表里已加载的 `item_name` 补中文名 —— 面板上显示
 * 「基本工资 / BASE」比只显示 `BASE` 更好点，但**插入的永远是 `{BASE}`**（插入编码，不插名称）。
 *
 * # 单位与列宽（与迁移 V0383 / DTO 常量逐字一致，单测对拍）
 *
 * `item_code ≤32` / `item_name ≤50` / `formula ≤500` / `remark ≤500` / 科目码 `≤30` /
 * `sort_order ≤999999`。任何一处放宽都会让用户「填得进去、存不进去」。
 */

// ==================== 权限码（与 `V0387__hr_menu_perms` 逐字一致） ====================

/**
 * 本页 3 个权限码。
 *
 * 设计 §5.2 的薪资项 4 条接口只用 3 个码：`#35` 检查**复用 `update`**
 * （「能改公式的人才能检查公式」）。`salary-item:delete` 被 V0387 种下来了，但后端没有
 * DELETE 路由（配置表的生命周期是「停用」不是「删行」），本页不使用它。
 */
export const SALARY_ITEM_PERMS = {
  /** 配置列表（`GET /salary-item/page`） */
  list: 'business:hr:salary-item:list',
  /** 新建（`POST /salary-item`） */
  create: 'business:hr:salary-item:create',
  /** 修改 + 公式检查（`PUT /salary-item/{id}` / `POST /salary-item/validate`） */
  update: 'business:hr:salary-item:update',
} as const;

// ==================== 域取值（与 `ck_biz_sal_item_*` 逐字一致） ====================

/** 类型五档（顺序 = `ck_biz_sal_item_type` / 后端 `ITEM_TYPES`） */
export const ITEM_TYPE_VALUES = ['EARNING', 'PRE_DEDUCT', 'WITHHOLD', 'EMPLOYER', 'SUMMARY'] as const;

/** 薪资项类型 */
export type ItemType = (typeof ITEM_TYPE_VALUES)[number];

/** 类型 → 中文（`salary_item_dto::ITEM_TYPES` 逐字一致） */
export const ITEM_TYPE_LABELS: Record<ItemType, string> = {
  EARNING: '收入项',
  PRE_DEDUCT: '应发前扣减',
  WITHHOLD: '代扣项',
  EMPLOYER: '单位承担',
  SUMMARY: '汇总项',
};

/**
 * 类型 → 标签主题。设计 §6.2.4「类型（彩色标签）」：
 * 加项绿、扣减橙、代扣蓝、单位承担紫（TDesign 无 purple，取 `warning` 之外的 `primary`）、
 * 汇总灰。颜色只是视觉分组，判断逻辑一律用码值。
 */
export const ITEM_TYPE_THEMES: Record<ItemType, 'success' | 'warning' | 'primary' | 'default' | 'danger'> = {
  EARNING: 'success',
  PRE_DEDUCT: 'warning',
  WITHHOLD: 'primary',
  EMPLOYER: 'default',
  SUMMARY: 'danger',
};

/** 算法三档（顺序 = `ck_biz_sal_item_calc` / 后端 `CALC_TYPES`） */
export const CALC_TYPE_VALUES = ['FIXED', 'FORMULA', 'INPUT'] as const;

/** 薪资项算法 */
export type CalcType = (typeof CALC_TYPE_VALUES)[number];

/** 算法 → 中文（后端 `CALC_TYPES` 逐字一致） */
export const CALC_TYPE_LABELS: Record<CalcType, string> = {
  FIXED: '取薪资标准',
  FORMULA: '公式计算',
  INPUT: '手工录入',
};

/** 来源五档（顺序 = `ck_biz_sal_item_source` / 后端 `SOURCES`） */
export const SOURCE_VALUES = ['EMPLOYEE_STANDARD', 'ATTENDANCE', 'INPUT', 'SYSTEM', 'MES'] as const;

/** 取数来源 */
export type SalarySource = (typeof SOURCE_VALUES)[number];

/** 来源 → 中文（后端 `SOURCES` 逐字一致） */
export const SOURCE_LABELS: Record<SalarySource, string> = {
  EMPLOYEE_STANDARD: '员工薪资标准',
  ATTENDANCE: '考勤',
  INPUT: '手工录入',
  SYSTEM: '系统计算',
  MES: 'MES 结算汇总',
};

/** 新建时来源缺省值（后端 `DEFAULT_SOURCE`） */
export const DEFAULT_SOURCE: SalarySource = 'INPUT';

// ==================== 公式 DSL（§4.4.2 的 EBNF 逐条对照） ====================

/** 允许的函数（后端 `FUNCS`；大小写敏感 —— 求值器只认大写） */
export const FORMULA_FUNCS = ['ROUND', 'MIN', 'MAX', 'ABS', 'CEIL', 'FLOOR', 'IF'] as const;

/** 函数 → 简式签名（变量面板底部「可用函数」提示，不参与求值） */
export const FORMULA_FUNC_HINTS: Record<(typeof FORMULA_FUNCS)[number], string> = {
  ROUND: 'ROUND(x[, n]) 四舍五入到 n 位',
  MIN: 'MIN(a, b) 取小',
  MAX: 'MAX(a, b) 取大',
  ABS: 'ABS(x) 绝对值',
  CEIL: 'CEIL(x) 向上取整',
  FLOOR: 'FLOOR(x) 向下取整',
  IF: 'IF(cond, a, b) 条件（两支惰性求值）',
};

/**
 * 考勤变量 11 个（后端 `salary_item_formula::ATT_VARS` 逐字一致：变量名 + 中文名）。
 *
 * 顺序 = Rust 里的数组顺序（单测按顺序对拍）。第二列是 `biz_attendance_record` 的列名，
 * 前端用不到（页面上不显示列名），所以这里只留 `name` / `label` 两段。
 */
export const ATT_VARS: ReadonlyArray<{ name: string; label: string }> = [
  { name: 'ATT_SHOULD_DAYS', label: '应出勤天数' },
  { name: 'ATT_ACTUAL_DAYS', label: '实际出勤天数' },
  { name: 'ATT_LEAVE_PAID_DAYS', label: '带薪假天数' },
  { name: 'ATT_LEAVE_UNPAID_DAYS', label: '无薪假天数（扣款依据）' },
  { name: 'ATT_SICK_DAYS', label: '病假天数' },
  { name: 'ATT_ABSENT_DAYS', label: '旷工天数' },
  { name: 'ATT_OT_WD', label: '平时加班小时' },
  { name: 'ATT_OT_WE', label: '休息日加班小时' },
  { name: 'ATT_OT_HO', label: '节假日加班小时' },
  { name: 'ATT_LATE_COUNT', label: '迟到次数' },
  { name: 'ATT_EARLY_COUNT', label: '早退次数' },
];

/** 考勤变量名清单（判「未定义变量」与面板分组用） */
export const ATT_VAR_NAMES: readonly string[] = ATT_VARS.map((item) => item.name);

// ==================== 上限（与后端 DTO / 迁移列宽逐字一致） ====================

/** 公式最长字符数（`formula VARCHAR(500)` / 后端 `MAX_FORMULA_LEN`） */
export const MAX_FORMULA_LEN = 500;
/** 编码最长字符数（`item_code VARCHAR(32)`） */
export const MAX_ITEM_CODE_LEN = 32;
/** 名称最长字符数（`item_name VARCHAR(50)`） */
export const MAX_ITEM_NAME_LEN = 50;
/** 科目码最长字符数（`debit/credit_account_code VARCHAR(30)`） */
export const MAX_ACCOUNT_CODE_LEN = 30;
/** 备注最长字符数（`remark VARCHAR(500)`） */
export const MAX_REMARK_LEN = 500;
/** 计算顺序上限（后端 `MAX_SORT_ORDER`） */
export const MAX_SORT_ORDER = 999_999;
/** 列表默认每页条数（后端 `DEFAULT_PAGE_SIZE`） */
export const DEFAULT_PAGE_SIZE = 20;
/** 列表每页上限（后端 `MAX_PAGE_SIZE`） */
export const MAX_PAGE_SIZE = 200;

/** 编码合法性（后端 `formula::is_var_name`：大写字母开头 + 大写字母/数字/下划线） */
export const ITEM_CODE_PATTERN = /^[A-Z][A-Z0-9_]*$/;

// ==================== 出参契约 ====================

/** 薪资项列表行（`salary_item_dto::SalaryItemRes` 逐字段） */
export interface SalaryItemRow {
  id: string;
  item_code: string;
  item_name: string;
  item_type: string;
  item_type_label: string;
  calc_type: string;
  calc_type_label: string;
  source: string;
  source_label: string;
  formula: string | null;
  /** 公式引用的**薪资项**编码（后端已抽好；不含 `ATT_*`） */
  depends_on: string[];
  sort_order: number;
  debit_account_code: string | null;
  credit_account_code: string | null;
  is_taxable: boolean;
  is_active: boolean;
  /** 系统项：不可停用、不可改类型/算法（`GROSS`/`NET`） */
  is_system: boolean;
  remark: string | null;
  create_by: string | null;
  create_by_name: string | null;
  update_by: string | null;
  update_by_name: string | null;
  created_at: string;
  updated_at: string;
}

/** 变量面板一行（`salary_item_dto::SalaryItemVarOption`） */
export interface SalaryItemVarOption {
  /** 变量名（插入公式的就是它：`BASE` → `{BASE}`） */
  name: string;
  /** 中文名（薪资项后端下发的是编码本身，前端用列表里的 `item_name` 补） */
  label: string;
  /** `ITEM` 薪资项 / `ATTENDANCE` 考勤 */
  kind: string;
  /** 停用的薪资项 `false`：面板上灰掉，但仍可点（历史公式可能引用了它） */
  usable: boolean;
}

/** 检查回执（`salary_item_dto::SalaryItemValidateRes`；`ok=false` 也是 200） */
export interface SalaryItemValidateRes {
  ok: boolean;
  errors: string[];
  refs: string[];
  item_refs: string[];
  att_refs: string[];
  undefined: string[];
  /** 依赖顺序：依赖在前，末位本项（`BASE → OT_WD → GROSS`） */
  dependency_order: string[];
  cycle: string[] | null;
  available: SalaryItemVarOption[];
}

// ==================== 写入口（表单态） ====================

/**
 * 表单态：全部是字符串 / 布尔（`t-input` 直接绑），提交前过 {@link toWritePayload} 归一化。
 *
 * `sort_order` 用字符串存：`t-input-number` 清空时回 `null`，绑定成 `number | undefined`
 * 在 TS 上到处要 `?? 0`；这里统一 `''` 表示「没填」，提交时按后端口径省略（新建取「最大 + 10」）。
 */
export interface SalaryItemForm {
  item_code: string;
  item_name: string;
  item_type: string;
  calc_type: string;
  source: string;
  formula: string;
  sort_order: string;
  debit_account_code: string;
  credit_account_code: string;
  is_taxable: boolean;
  is_active: boolean;
  remark: string;
}

/** 新建表单的缺省值（类型 `EARNING` + 算法 `INPUT`：最常建的「输入项」） */
export function emptyItemForm(): SalaryItemForm {
  return {
    item_code: '',
    item_name: '',
    item_type: 'EARNING',
    calc_type: 'INPUT',
    source: DEFAULT_SOURCE,
    formula: '',
    sort_order: '',
    debit_account_code: '',
    credit_account_code: '',
    is_taxable: true,
    is_active: true,
    remark: '',
  };
}

/** 行 → 表单（编辑态；编码可读但输入框 disable） */
export function formFromRow(row: SalaryItemRow): SalaryItemForm {
  return {
    item_code: row.item_code,
    item_name: row.item_name,
    item_type: row.item_type,
    calc_type: row.calc_type,
    source: row.source,
    formula: row.formula ?? '',
    sort_order: String(row.sort_order ?? ''),
    debit_account_code: row.debit_account_code ?? '',
    credit_account_code: row.credit_account_code ?? '',
    is_taxable: row.is_taxable,
    is_active: row.is_active,
    remark: row.remark ?? '',
  };
}

/** 写入口（`SalaryItemWriteReq`；`undefined` = 不传，后端按「缺省即保持原值」处理） */
export interface SalaryItemWritePayload {
  item_code?: string;
  item_name: string;
  item_type: string;
  calc_type: string;
  source: string;
  formula?: string;
  sort_order?: number;
  debit_account_code?: string;
  credit_account_code?: string;
  is_taxable?: boolean;
  is_active?: boolean;
  remark?: string;
}

/**
 * 表单 → 写入报文。
 *
 * 三条归一化（每条都有后端依据）：
 * - 非 `FORMULA` 算法的 `formula` **不发**（后端 `parse_write` 明确「其它算法必须留空」，
 *   发了空串会被当成「给了公式」再报错）；
 * - 空串科目发 `undefined` 而不是 `''`（后端按 `None` 判「没填」，`''` 会变成「填了个空」）；
 * - `sort_order` 空串不发（后端新建取「当前最大 + 10」，修改保持原值）。
 */
export function toWritePayload(form: SalaryItemForm): SalaryItemWritePayload {
  const payload: SalaryItemWritePayload = {
    item_name: form.item_name.trim(),
    item_type: form.item_type,
    calc_type: form.calc_type,
    source: form.source,
    is_taxable: form.is_taxable,
    is_active: form.is_active,
  };
  const code = form.item_code.trim();
  if (code) {
    payload.item_code = code;
  }
  if (form.calc_type === 'FORMULA' && form.formula.trim()) {
    payload.formula = form.formula.trim();
  }
  const sort = form.sort_order.trim();
  if (sort) {
    payload.sort_order = Number(sort);
  }
  const debit = form.debit_account_code.trim();
  if (debit) {
    payload.debit_account_code = debit;
  }
  const credit = form.credit_account_code.trim();
  if (credit) {
    payload.credit_account_code = credit;
  }
  const remark = form.remark.trim();
  if (remark) {
    payload.remark = remark;
  }
  return payload;
}

/** 类型 → 科目必填档（`ck_biz_sal_item_account` 逐条：SUMMARY 无科目、WITHHOLD 只贷方） */
export function accountNeeds(itemType: string): { debit: boolean; credit: boolean } {
  if (itemType === 'SUMMARY') {
    return { debit: false, credit: false };
  }
  if (itemType === 'WITHHOLD') {
    return { debit: false, credit: true };
  }
  return { debit: true, credit: true };
}

/** 是否要用公式框（只有 `FORMULA` 算法；`FIXED` 取标准、`INPUT` 手工录） */
export function needsFormula(calcType: string): boolean {
  return calcType === 'FORMULA';
}

/** 系统项：类型 / 算法不可改（后端 PUT 守卫） */
export function isTypeLocked(row: Pick<SalaryItemRow, 'is_system'> | null): boolean {
  return Boolean(row?.is_system);
}

/** 系统项：不可停用（后端 PUT 守卫） */
export function isActiveLocked(row: Pick<SalaryItemRow, 'is_system'> | null): boolean {
  return Boolean(row?.is_system);
}

/** 类型 / 算法中文名（未知码值原样返回，便于把后端新值直接显示出来） */
export function itemTypeLabel(code: string): string {
  return ITEM_TYPE_LABELS[code as ItemType] ?? code;
}

/** 算法中文名 */
export function calcTypeLabel(code: string): string {
  return CALC_TYPE_LABELS[code as CalcType] ?? code;
}

/** 来源中文名 */
export function sourceLabel(code: string): string {
  return SOURCE_LABELS[code as SalarySource] ?? code;
}

/** 类型标签主题（未知码值用 `default`，不猜颜色） */
export function itemTypeTheme(code: string): 'success' | 'warning' | 'primary' | 'default' | 'danger' {
  return ITEM_TYPE_THEMES[code as ItemType] ?? 'default';
}

/** 下拉选项（类型 / 算法 / 来源） */
export function itemTypeOptions(): Array<{ label: string; value: string }> {
  return ITEM_TYPE_VALUES.map((value) => ({ label: ITEM_TYPE_LABELS[value], value }));
}

/** 算法下拉选项 */
export function calcTypeOptions(): Array<{ label: string; value: string }> {
  return CALC_TYPE_VALUES.map((value) => ({ label: CALC_TYPE_LABELS[value], value }));
}

/** 来源下拉选项 */
export function sourceOptions(): Array<{ label: string; value: string }> {
  return SOURCE_VALUES.map((value) => ({ label: SOURCE_LABELS[value], value }));
}

// ==================== 列表筛选 ====================

/** 列表筛选态（`salary_item_dto::SalaryItemPageQuery` 的前端形态） */
export interface SalaryItemFilter {
  keyword: string;
  item_type: string;
  calc_type: string;
  is_active: string;
}

/** 空筛选（`is_active=''` = 全部） */
export function emptyItemFilter(): SalaryItemFilter {
  return { keyword: '', item_type: '', calc_type: '', is_active: '' };
}

/**
 * 筛选态 → 查询串。
 *
 * 空串一律**不发**（后端 `filter_of` 对空串当「没筛」，但发了 `item_type=` 会走
 * `Some("")` 的上限校验路径 —— 少一个空参数少一次误判）。
 * `is_active` 是 `'1'` / `'0'` 字符串，转成数字发（后端是 `Option<i32>`）。
 */
export function buildPageQuery(
  filter: SalaryItemFilter,
  page: number,
  pageSize: number = DEFAULT_PAGE_SIZE,
): Record<string, unknown> {
  const query: Record<string, unknown> = {
    page: Math.max(1, Math.trunc(page) || 1),
    page_size: Math.min(MAX_PAGE_SIZE, Math.max(1, Math.trunc(pageSize) || DEFAULT_PAGE_SIZE)),
  };
  const keyword = filter.keyword.trim();
  if (keyword) {
    query.keyword = keyword;
  }
  if (filter.item_type) {
    query.item_type = filter.item_type;
  }
  if (filter.calc_type) {
    query.calc_type = filter.calc_type;
  }
  if (filter.is_active === '1' || filter.is_active === '0') {
    query.is_active = Number(filter.is_active);
  }
  return query;
}

/** 是否填了筛选（决定「重置」按钮是否可用 / 列表头是否显示「已筛选」） */
export function isItemFiltered(filter: SalaryItemFilter): boolean {
  return Boolean(filter.keyword.trim() || filter.item_type || filter.calc_type || filter.is_active);
}

// ==================== 表单校验（只挡后端看不见的边） ====================

/** 字段 → 首条错误（键与表单字段同名，页面据此在表单项下显示） */
export type FormErrors = Record<string, string>;

/**
 * 本地校验（提交前）。
 *
 * 只做三件事：**必填 / 列宽 / 按类型分档必填** —— 这三类后端也会拒，但等到请求回来才提示
 * 等于让用户白等一次；公式的语法与依赖**故意不判**（见文件头第 2 条）。
 *
 * `creating=false`（编辑）时跳过编码形状校验：编码不可改，改了也发不出去。
 */
export function validateItemForm(form: SalaryItemForm, options: { creating: boolean }): FormErrors {
  const errors: FormErrors = {};
  const code = form.item_code.trim();
  const name = form.item_name.trim();

  if (options.creating) {
    if (!code) {
      errors.item_code = '编码必填：公式里按 `{编码}` 引用它（例：BASE、OT_WD）';
    } else if (!ITEM_CODE_PATTERN.test(code)) {
      errors.item_code = '编码只允许大写字母开头 + 大写字母/数字/下划线（小写或带 `-` 的编码引用不到）';
    } else if (code.length > MAX_ITEM_CODE_LEN) {
      errors.item_code = `编码最长 ${MAX_ITEM_CODE_LEN} 个字符（当前 ${code.length} 个）`;
    } else if (ATT_VAR_NAMES.includes(code)) {
      errors.item_code = '该编码与考勤变量同名（`ATT_*` 已被考勤占用），请换一个（如 ATT_BONUS）';
    }
  }

  if (!name) {
    errors.item_name = '名称必填';
  } else if (name.length > MAX_ITEM_NAME_LEN) {
    errors.item_name = `名称最长 ${MAX_ITEM_NAME_LEN} 个字符（当前 ${name.length} 个）`;
  }

  if (!ITEM_TYPE_VALUES.includes(form.item_type as ItemType)) {
    errors.item_type = '请选择类型';
  }
  if (!CALC_TYPE_VALUES.includes(form.calc_type as CalcType)) {
    errors.calc_type = '请选择算法';
  }
  if (form.item_type === 'SUMMARY' && form.calc_type !== 'FORMULA') {
    errors.calc_type = '汇总项必须用「公式计算」：它的值只能由其它项汇总出来';
  }
  if (!SOURCE_VALUES.includes(form.source as SalarySource)) {
    errors.source = '请选择取数来源';
  }

  if (needsFormula(form.calc_type)) {
    const formula = form.formula.trim();
    if (!formula) {
      errors.formula = '算法是「公式计算」时必须给公式（点右侧变量面板插入 `{编码}`）';
    } else if (formula.length > MAX_FORMULA_LEN) {
      errors.formula = `公式最长 ${MAX_FORMULA_LEN} 个字符（当前 ${formula.length} 个）`;
    }
  }

  const needs = accountNeeds(form.item_type);
  const debit = form.debit_account_code.trim();
  const credit = form.credit_account_code.trim();
  if (needs.debit && !debit) {
    errors.debit_account_code = '该类型必须填借方科目（过账要用）';
  }
  if (needs.credit && !credit) {
    errors.credit_account_code = '该类型必须填贷方科目（过账要用）';
  }
  if (debit.length > MAX_ACCOUNT_CODE_LEN) {
    errors.debit_account_code = `科目码最长 ${MAX_ACCOUNT_CODE_LEN} 个字符`;
  }
  if (credit.length > MAX_ACCOUNT_CODE_LEN) {
    errors.credit_account_code = `科目码最长 ${MAX_ACCOUNT_CODE_LEN} 个字符`;
  }

  const sort = form.sort_order.trim();
  if (sort) {
    const value = Number(sort);
    if (!Number.isInteger(value) || value < 0 || value > MAX_SORT_ORDER) {
      errors.sort_order = `计算顺序只能是 0~${MAX_SORT_ORDER} 的整数（公司内唯一）`;
    }
  }

  if (form.remark.trim().length > MAX_REMARK_LEN) {
    errors.remark = `备注最长 ${MAX_REMARK_LEN} 个字符`;
  }

  return errors;
}

/** 表单错误 → 可直接弹的一条人话（保存时用；逐项错误留在表单下方） */
export function firstFormError(errors: FormErrors): string {
  const order = [
    'item_code',
    'item_name',
    'item_type',
    'calc_type',
    'source',
    'formula',
    'debit_account_code',
    'credit_account_code',
    'sort_order',
    'remark',
  ];
  for (const key of order) {
    if (errors[key]) {
      return errors[key];
    }
  }
  return Object.values(errors)[0] ?? '';
}

// ==================== 公式：分词（只做高亮，不求值） ====================

/** 分词种类：变量 / 函数 / 数字 / 运算符 / 标点 / 其它（空格等原样保留） */
export type FormulaTokenKind = 'var' | 'var-invalid' | 'func' | 'number' | 'op' | 'punct' | 'text';

/** 一个分词片段（`start` / `end` 是**字符**下标，闭开区间） */
export interface FormulaToken {
  kind: FormulaTokenKind;
  text: string;
  start: number;
  end: number;
}

const OPERATOR_CHARS = new Set(['+', '-', '*', '/', '<', '>', '=', '!', '&', '|', '%']);
const PUNCT_CHARS = new Set(['(', ')', ',', '.']);

/**
 * 把公式切成分词（供高亮预览 + 报错区间着色）。
 *
 * **必须覆盖全文**：`tokens.map(t => t.text).join('') === formula`（单测断言）——
 * 少一个字符，高亮就会错位。
 *
 * 变量：`{NAME}` 且 `NAME` 合法 → `var`，否则 `var-invalid`（后端会报「变量名不合法」，
 * 这里先着色，让人一眼看到哪儿写错了）。函数名只认 `FORMULA_FUNCS` 里的大写形式；
 * 小写 `round` 算 `text`（后端会提示「函数名请用大写」）。
 */
export function tokenizeFormula(formula: string): FormulaToken[] {
  const tokens: FormulaToken[] = [];
  const chars = Array.from(formula);
  let index = 0;
  const push = (kind: FormulaTokenKind, start: number, end: number): void => {
    tokens.push({ kind, text: chars.slice(start, end).join(''), start, end });
  };

  while (index < chars.length) {
    const ch = chars[index];

    if (ch === '{') {
      const close = chars.indexOf('}', index + 1);
      if (close === -1) {
        // 未闭合：一直吃到结尾（后端报「{ 与 } 数量不配对」）
        push('var-invalid', index, chars.length);
        break;
      }
      const name = chars.slice(index + 1, close).join('');
      const ok = ITEM_CODE_PATTERN.test(name);
      push(ok ? 'var' : 'var-invalid', index, close + 1);
      index = close + 1;
      continue;
    }

    if (ch === '_' || /[A-Za-z]/.test(ch)) {
      let end = index + 1;
      while (end < chars.length && (chars[end] === '_' || /[A-Za-z0-9]/.test(chars[end]))) {
        end += 1;
      }
      const word = chars.slice(index, end).join('');
      const isFunc = (FORMULA_FUNCS as readonly string[]).includes(word);
      // 函数名后面必须跟 `(`（允许中间有空格），否则就是个裸标识符（后端报错）
      let probe = end;
      while (probe < chars.length && chars[probe] === ' ') {
        probe += 1;
      }
      push(isFunc && chars[probe] === '(' ? 'func' : 'text', index, end);
      index = end;
      continue;
    }

    if (/[0-9]/.test(ch)) {
      let end = index + 1;
      while (end < chars.length && /[0-9.]/.test(chars[end])) {
        end += 1;
      }
      push('number', index, end);
      index = end;
      continue;
    }

    if (OPERATOR_CHARS.has(ch)) {
      let end = index + 1;
      while (end < chars.length && OPERATOR_CHARS.has(chars[end])) {
        end += 1;
      }
      push('op', index, end);
      index = end;
      continue;
    }

    if (PUNCT_CHARS.has(ch)) {
      push('punct', index, index + 1);
      index += 1;
      continue;
    }

    push('text', index, index + 1);
    index += 1;
  }

  return tokens;
}

// ==================== 公式：变量插入 ====================

/** 插入结果：新公式 + 插入后光标应落的位置（字符下标） */
export interface InsertResult {
  formula: string;
  caret: number;
}

/** 需要在前后补空格时才合并的两个字符（避免 `{POST}{MEAL}` 这种连写） */
function isOperandTail(ch: string | undefined): boolean {
  return ch !== undefined && /[A-Za-z0-9_})]/.test(ch);
}

/** 后缀合并判定：后面紧跟的是操作数开头（字母 / 数字 / `_` / `{`） */
function isOperandHead(ch: string | undefined): boolean {
  return ch !== undefined && /[A-Za-z0-9_{]/.test(ch);
}

/**
 * 在光标处插入变量（变量面板点一下就是这条）。
 *
 * 三条规则（都是为了不产生非法公式）：
 * 1. 有选区就**替换**选区（改公式时最常见的动作是「选中写错的变量、点对的变量」）；
 * 2. 前后都是操作数时**补空格**：`{POST}` 末尾再接 `{MEAL}` 会连成 `{POST}{MEAL}`，
 *    后端解析不了（没见过这种错误串的用户根本不知道自己错在哪）；
 * 3. 光标落在插入的变量**之后**，连续点两个变量不用再手动挪光标。
 *
 * `selectionStart` / `selectionEnd` 都是**字符**下标（textarea 给的是 UTF-16 下标，
 * 公式是纯 ASCII 白名单，两者只在中文/emoji 时不同 —— 口径统一走 {@link toCharIndex}）。
 */
export function insertVariable(
  formula: string,
  name: string,
  selectionStart: number,
  selectionEnd: number = selectionStart,
): InsertResult {
  // 兼容传入反悔的下标（负数 / 越界 / 反序）
  const chars = Array.from(formula);
  const start = Math.min(Math.max(0, Math.min(selectionStart, selectionEnd)), chars.length);
  const end = Math.min(Math.max(0, Math.max(selectionStart, selectionEnd)), chars.length);
  const token = `{${name}}`;

  const before = start > 0 ? chars[start - 1] : undefined;
  const after = end < chars.length ? chars[end] : undefined;
  const prefix = isOperandTail(before) ? ' ' : '';
  const suffix = isOperandHead(after) ? ' ' : '';

  const next = chars.slice(0, start).join('') + prefix + token + suffix + chars.slice(end).join('');
  const caret = start + prefix.length + token.length;
  return { formula: next, caret };
}

/**
 * UTF-16 下标（textarea 的 `selectionStart`）→ 字符下标（后端「第 N 个字符」的口径）。
 *
 * 公式白名单里没有非 ASCII 字符，所以正常情况两者相等；但**错误位置可能指向用户刚粘进来的
 * 中文**（`公式第 3 个字符 \`中\` 不在允许的字符里`）—— 那时 UTF-16 与「字符」就可能差一位。
 * 统一换算一次，避免「点了报错、光标偏一格」。
 */
export function toCharIndex(text: string, codeUnitIndex: number): number {
  return Array.from(text.slice(0, Math.max(0, codeUnitIndex))).length;
}

/** 字符下标 → UTF-16 下标（落 textarea 选区时用） */
export function toCodeUnitIndex(text: string, charIndex: number): number {
  return Array.from(text).slice(0, Math.max(0, charIndex)).join('').length;
}

// ==================== 公式：报错定位 ====================

/** 一条可定位的问题（`start`/`end` 是字符下标；`located=false` 表示只能原位显示） */
export interface FormulaIssue {
  message: string;
  start: number;
  end: number;
  /** 是否定位到了具体位置（false 时页面不高亮，只列在错误清单里） */
  located: boolean;
}

/** 「公式第 N 个字符」 */
const POSITION_PATTERN = /第\s*(\d+)\s*个字符/;
/** 反引号里的变量：`` `{BASEE}` `` */
const BACKTICK_VAR_PATTERN = /`\{([A-Za-z0-9_]*)\}`/g;
/** 反引号里的裸标识符：`` `SELECT` `` / `` `round` `` */
const BACKTICK_WORD_PATTERN = /`([A-Za-z_][A-Za-z0-9_]*)`/g;

/**
 * 后端错误串 → 可定位的问题清单（检查按钮的核心）。
 *
 * 后端（`salary_item_formula` / `salary_item_dto`）的错误串分三类：
 *
 * | 形态 | 例 | 定位方式 |
 * |---|---|---|
 * | 带字符号 | ``公式第 3 个字符 `;` 不在允许的字符里`` | 直接取第 3 个字符 |
 * | 点名变量 | ``公式引用了未定义的变量 `{BASEE}``` | 在公式里找 `{BASEE}` 的位置 |
 * | 只描述 | `公式里 `(` 没有配对的 `)`` | 不定位（`located=false`），仍列出来 |
 *
 * 变量优先于字符号：同一条消息里既有「未定义变量」又有字符号时，变量位置对用户更有用。
 * 找不到任何位置时把区间退成 `[0, 0)`（光标落到开头），页面仍然显示消息本身。
 */
export function formulaIssues(errors: readonly string[], formula: string): FormulaIssue[] {
  const chars = Array.from(formula);
  return errors.map((message) => {
    // ① 点名变量（可能有多个，取第一个能被找到的）
    BACKTICK_VAR_PATTERN.lastIndex = 0;
    let match: RegExpExecArray | null = BACKTICK_VAR_PATTERN.exec(message);
    while (match) {
      const token = `{${match[1]}}`;
      const at = findCharSequence(chars, token);
      if (at >= 0) {
        return { message, start: at, end: at + Array.from(token).length, located: true };
      }
      match = BACKTICK_VAR_PATTERN.exec(message);
    }

    // ② 「第 N 个字符」
    const position = POSITION_PATTERN.exec(message);
    if (position) {
      const at = Math.min(Math.max(0, Number(position[1]) - 1), Math.max(0, chars.length - 1));
      return { message, start: at, end: Math.min(chars.length, at + 1), located: chars.length > 0 };
    }

    // ③ 点名的函数 / 关键字（`SELECT` / `round`）
    BACKTICK_WORD_PATTERN.lastIndex = 0;
    let word = BACKTICK_WORD_PATTERN.exec(message);
    while (word) {
      const at = findCharSequence(chars, word[1]);
      if (at >= 0) {
        return { message, start: at, end: at + Array.from(word[1]).length, located: true };
      }
      word = BACKTICK_WORD_PATTERN.exec(message);
    }

    return { message, start: 0, end: 0, located: false };
  });
}

/** 在字符数组里找子串（返回字符下标，找不到回 -1） */
function findCharSequence(chars: readonly string[], needle: string): number {
  const target = Array.from(needle);
  if (target.length === 0 || target.length > chars.length) {
    return -1;
  }
  for (let start = 0; start + target.length <= chars.length; start += 1) {
    let hit = true;
    for (let offset = 0; offset < target.length; offset += 1) {
      if (chars[start + offset] !== target[offset]) {
        hit = false;
        break;
      }
    }
    if (hit) {
      return start;
    }
  }
  return -1;
}

/** 一条问题的「第 N 个字符」位置（1 起，供卡片上显示 `第 12 个字符`） */
export function issuePositionText(issue: FormulaIssue): string {
  return issue.located ? `第 ${issue.start + 1} 个字符` : '整条公式';
}

/** 问题区间 → `第 12 个字符 {ATT_OT_WD2}` 这样的定位标签（错误清单每条前面显示） */
export function issueLocationText(issue: FormulaIssue, formula: string): string {
  if (!issue.located) {
    return '整条公式';
  }
  const snippet = Array.from(formula).slice(issue.start, issue.end).join('');
  const head = `第 ${issue.start + 1} 个字符`;
  return snippet ? `${head} ${snippet}` : head;
}

// ==================== 检查结果 → 页面展示 ====================

/** 「检查」结果的展示态（页面只读这一份，不再自己拼字符串） */
export interface ValidateView {
  /** 是否通过 */
  ok: boolean;
  /** 可定位的问题（顺序 = 后端 `errors` 顺序） */
  issues: FormulaIssue[];
  /** 依赖链文本（`BASE → OT_WD → GROSS`）；为空时页面显示「无依赖」 */
  dependencyText: string;
  /** 环文本（`A → B → A`）；无环为空串 */
  cycleText: string;
  /** 引用变量数（`refs.length`） */
  refCount: number;
  /** 未定义变量（去重保序） */
  undefined: string[];
}

/** 检查回执 + 当前公式 → 展示态 */
export function validateView(res: SalaryItemValidateRes, formula: string): ValidateView {
  return {
    ok: res.ok,
    issues: formulaIssues(res.errors ?? [], formula),
    dependencyText: dependencyTextOf(res.dependency_order ?? []),
    cycleText: (res.cycle ?? []).join(' → '),
    refCount: (res.refs ?? []).length,
    undefined: res.undefined ?? [],
  };
}

/** 依赖顺序 → `BASE → OT_WD → GROSS`（空数组 → `''`，页面显示「无依赖」） */
export function dependencyTextOf(order: readonly string[]): string {
  return order.filter((code) => Boolean(code)).join(' → ');
}

/** 检查通过后的一句话（标题行；依赖为空时不硬编故事） */
export function validateSummary(view: ValidateView): string {
  if (!view.ok) {
    return `公式未通过：${view.issues.length} 处问题`;
  }
  if (view.dependencyText) {
    return `语法通过 · 依赖顺序 ${view.dependencyText}`;
  }
  return '语法通过 · 无薪资项依赖';
}

// ==================== 变量面板 ====================

/** 面板分组（薪资项在前、考勤在后：客户改公式九成在引用别的项） */
export interface VarPanelGroup {
  key: 'ITEM' | 'ATTENDANCE';
  title: string;
  items: SalaryItemVarOption[];
}

/** 考勤变量的中文名（后端 `available_vars` 已带，这里是兜底：老响应没有 label 时用） */
export function attVarLabel(name: string): string | undefined {
  return ATT_VARS.find((item) => item.name === name)?.label;
}

/**
 * `available` → 面板分组。
 *
 * 三处加工：
 * - 薪资项的 `label` 后端下发的是编码本身，这里用 {@link nameOfItem} 给的映射补中文名；
 * - 自己不能引用自己（`{GROSS}` 里写 `{GROSS}` 是自环，后端检查才报，面板上先灰掉）；
 * - 停用项（`usable=false`）保留但标记，历史公式里可能还引用着它。
 *
 * 分组内保持后端顺序（= `sort_order`），不重排。
 */
export function varPanelGroups(
  available: readonly SalaryItemVarOption[],
  options: { selfCode?: string; itemNames?: Record<string, string> } = {},
): VarPanelGroup[] {
  const selfCode = options.selfCode ?? '';
  const itemNames = options.itemNames ?? {};
  const toOption = (option: SalaryItemVarOption): SalaryItemVarOption => {
    const label = option.kind === 'ITEM' ? itemNames[option.name] || option.label : option.label;
    const usable = option.usable && option.name !== selfCode;
    return { ...option, label, usable };
  };
  const items = available.filter((option) => option.kind === 'ITEM').map(toOption);
  const attendance = available.filter((option) => option.kind !== 'ITEM').map(toOption);
  const groups: VarPanelGroup[] = [];
  if (items.length) {
    groups.push({ key: 'ITEM', title: '薪资项', items });
  }
  if (attendance.length) {
    groups.push({ key: 'ATTENDANCE', title: '考勤变量', items: attendance });
  }
  return groups;
}

/** 变量 → 面板一行文本（`基本工资  BASE`；名称缺失时只显示编码） */
export function varOptionText(option: SalaryItemVarOption): string {
  return option.label && option.label !== option.name ? `${option.label}  ${option.name}` : option.name;
}

/**
 * 变量面板的兜底清单：**检查接口还没调过**时也要能点插入。
 *
 * 后端 `available` 只在 `POST /salary-item/validate` 的回执里下发（列表接口没有）。
 * 抽屉一打开就为了变量面板打一次 validate 不划算（且新建时公式是空的，validate 会报
 * 「公式不能为空」）。所以打开时用**已加载的列表** + 本地 {@link ATT_VARS} 拼一份，
 * 等真正点过「检查」再用后端那份（以后端为准，口径永远只有一份真相）。
 */
export function fallbackVarOptions(rows: readonly SalaryItemRow[], selfCode = ''): SalaryItemVarOption[] {
  const items: SalaryItemVarOption[] = rows
    .filter((row) => row.item_code !== selfCode)
    .map((row) => ({
      name: row.item_code,
      label: row.item_name,
      kind: 'ITEM',
      usable: Boolean(row.is_active),
    }))
    .sort((left, right) => left.name.localeCompare(right.name, 'en'));
  const attendance: SalaryItemVarOption[] = ATT_VARS.map((item) => ({
    name: item.name,
    label: item.label,
    kind: 'ATTENDANCE',
    usable: true,
  }));
  return [...items, ...attendance];
}

/** 列表行编码 → 名称（补变量面板的中文名） */
export function itemNameMap(rows: readonly SalaryItemRow[]): Record<string, string> {
  const map: Record<string, string> = {};
  for (const row of rows) {
    map[row.item_code] = row.item_name;
  }
  return map;
}

// ==================== 列定义 ====================

/**
 * 列表列（设计 §6.2.4：顺序 / 编码 / 名称 / 类型 / 算法 / 公式 / 科目 / 启用）。
 *
 * `colKey` 是 `t-table` 的列 key（与 {@link SalaryItemRow} 字段同名或为派生列）。
 * `width` 用字符串（TDesign 接受 `120` 或 `'120px'`，字符串更明确）。
 */
export const SALARY_ITEM_COLUMNS: Array<{
  colKey: string;
  title: string;
  width?: string;
  align?: 'left' | 'center' | 'right';
  ellipsis?: boolean;
}> = [
  { colKey: 'sort_order', title: '顺序', width: '80px', align: 'right' },
  { colKey: 'item_code', title: '编码', width: '150px' },
  { colKey: 'item_name', title: '名称', width: '160px', ellipsis: true },
  { colKey: 'item_type', title: '类型', width: '110px' },
  { colKey: 'calc_type', title: '算法', width: '110px' },
  { colKey: 'formula', title: '公式', width: '300px', ellipsis: true },
  { colKey: 'account', title: '科目（借 / 贷）', width: '190px' },
  { colKey: 'is_active', title: '启用', width: '80px', align: 'center' },
  { colKey: 'op', title: '操作', width: '100px', align: 'center' },
];

/** 科目列文本（`5602.10 / 2211.01`；`SUMMARY` 无科目 → `—`） */
export function accountText(row: Pick<SalaryItemRow, 'debit_account_code' | 'credit_account_code'>): string {
  const debit = row.debit_account_code?.trim() || '—';
  const credit = row.credit_account_code?.trim() || '—';
  return `${debit} / ${credit}`;
}

/** 公式列文本（无公式 → `—`；超长由表格 ellipsis 兜，tooltip 显示全文） */
export function formulaText(row: Pick<SalaryItemRow, 'formula'>): string {
  return row.formula?.trim() || '—';
}

/** 依赖列 / 抽屉里的「依赖」文本（`BASE / POST`；无依赖 → 空串） */
export function dependsText(row: Pick<SalaryItemRow, 'depends_on'>): string {
  const list = row.depends_on ?? [];
  return list.join(' / ');
}

/** 新建时的建议计算顺序（列表最大 + 10，与后端 `next_sort_order` 同口径） */
export function nextSortOrder(rows: readonly SalaryItemRow[]): number {
  const max = rows.reduce((acc, row) => Math.max(acc, Number(row.sort_order) || 0), 0);
  return max + 10;
}
