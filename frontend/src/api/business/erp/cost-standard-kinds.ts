/**
 * 标准成本卡维护页类型与词表（M5-09 · 标准成本「版本表 + 批量导入」）。
 *
 * # 与后端一一对应（字段逐字抄 `cost_standard_dto.rs`，不自己取名）
 *
 * | 后端 | 前端 |
 * |---|---|
 * | `StandardRes` | {@link StandardRow} |
 * | `StandardItemRes` | {@link StandardItemRes} |
 * | `StandardDetailRes` | {@link StandardDetailRes}（`head` + `items`） |
 * | `StandardEffectiveRes` | {@link StandardEffectiveRes} |
 * | `PeriodSheetRes` | {@link PeriodSheetRes} |
 * | `StandardStatusRes` | {@link StandardStatusRes} |
 * | `StandardImportRes`（M5-09） | {@link StandardImportResult} |
 *
 * # 金额一律是字符串
 *
 * 后端 `rust_decimal::Decimal` 不开 `serde-float`，出 JSON 是 `"17.166666"`；
 * **入参也必须是字符串**（发数字会被 422 拒）。页面上一律按字符串收发，
 * 只有「预览」才转成数字算一遍（`cost-standard-logic.ts::previewAmounts`），
 * 且明确标注「服务端为准」。
 *
 * # 三要素口径（库层 CHECK 锁死，前端只做展示）
 *
 * ```
 * 标准材料成本 = Σ round(用量 × 单价, 6)
 * 标准人工成本 = round(标准工时 × 标准工资率, 6)
 * 标准总成本   = 料 + 工 + 费
 * ```
 */

// ==================== 权限码（复用 cost-method 四个，零新增） ====================

/**
 * 本页四个权限码。
 *
 * 全部**复用** `business:erp:cost-method:*`（V0460 已种、已授权）：
 * 标准成本卡与「按什么方法核算」是同一类东西（全公司财务口径配置、同一批维护角色），
 * 而且后端 M5-08/M5-09 的 10 条路由就是挂这四个码（`cost_standard_handler.rs`）。
 * 新开码必然要种菜单行，种了没有真路由对应就是死码（`permission_audit_tests::n14`）。
 */
export const COST_STANDARD_PERMISSIONS = {
  /** 列表 / 详情 / 生效版本 / 期间矩阵 / 模板下载 */
  list: 'business:erp:cost-method:list',
  /** 新建 + 批量导入 */
  create: 'business:erp:cost-method:create',
  /** 修改 / 生效 / 失效 */
  update: 'business:erp:cost-method:update',
  /** 软删 */
  delete: 'business:erp:cost-method:delete',
} as const;

// ==================== 业务码（版本护栏，M5-08） ====================

/** `66108`：同一物料启用版本的生效期重叠（建 / 改 / 生效 / 导入都会撞）。 */
export const COST_STANDARD_OVERLAP_CODE = 66108;
/** `66109`：已生效过的版本不许改口径（只能改停止日 / 状态 / 备注）。 */
export const COST_STANDARD_LOCKED_CODE = 66109;
/** `66110`：已生效过的版本不许删（要停用请走「失效」）。 */
export const COST_STANDARD_DELETE_CODE = 66110;
/**
 * `66111`：发布状态不接受该动作（M5-10）。
 *
 * 三种情形共用这个码：非草稿提交发布 / 审批中改口径·删·置生效 / 未发布手工置生效。
 * 报文本就是人话且带出路，前端**原样展示**后端报文，不自己编替代文案。
 */
export const COST_STANDARD_PUBLISH_GUARD_CODE = 66111;

/**
 * `10002`：通用「资源不存在」（`error_code::RESOURCE_NOT_FOUND`）。
 *
 * 「查生效版本」用它判「这一天确实没有可参与取数的版本」：后端 `effective` 的取数闸
 * 只认 `publish_state = 'PUBLISHED'`，所以草稿 / 审批中的卡也走这个 404 —— 页面要能
 * 补一句「是不是还没发布」，否则用户会以为「我明明建过」。
 */
export const COST_STANDARD_NOT_FOUND_CODE = 10002;

// ==================== 状态 ====================

/** 版本状态：1 = 生效（参与期间取数）/ 0 = 失效。 */
export const STANDARD_STATUS_ACTIVE = 1;
/** 版本状态：失效（不占生效窗口，历史版本靠它留痕）。 */
export const STANDARD_STATUS_INACTIVE = 0;

/** 状态下拉（**值里不要放 `''`** 当「全部」：TDesign 把空串当未选）。 */
export const STANDARD_STATUS_OPTIONS: ReadonlyArray<{ label: string; value: number }> = [
  { label: '生效', value: STANDARD_STATUS_ACTIVE },
  { label: '失效', value: STANDARD_STATUS_INACTIVE },
];

/** 状态 → 中文（后端 `status_label` 的同一口径）。 */
export function statusLabel(status: number | null | undefined): string {
  return status === STANDARD_STATUS_ACTIVE ? '生效' : '失效';
}

/** 状态 → TDesign 主题（生效绿 / 失效灰）。 */
export function statusTheme(status: number | null | undefined): 'success' | 'default' {
  return status === STANDARD_STATUS_ACTIVE ? 'success' : 'default';
}

// ==================== 发布轴（M5-10「审核后生效」） ====================

/**
 * 发布状态三值（与后端 `publish_state` / `ck_biz_cost_std_publish` 逐字一致）。
 *
 * 与 `status` 是**两条正交的轴**：
 * - `status`（1/0）= 参与期间取数；
 * - `publish_state` = 发布审批走到哪一步。
 * 取数要求两者同时满足（`status = 1 && publish_state = 'PUBLISHED'`），
 * 所以「新建的卡」在差异分析里**还看不到**，必须先走发布审批。
 */
export const PUBLISH_STATE_DRAFT = 'DRAFT';
/** 审批中（已提交，等审批人处理；此期间不许改口径 / 删 / 手工置生效）。 */
export const PUBLISH_STATE_PENDING = 'PENDING';
/** 已发布（审批通过；只有它 + status=1 才参与取数）。 */
export const PUBLISH_STATE_PUBLISHED = 'PUBLISHED';
/** 发布状态下拉（**值里不要放 `''`**：TDesign 把空串当未选）。 */
export const PUBLISH_STATE_OPTIONS: ReadonlyArray<{ label: string; value: string }> = [
  { label: '草稿', value: PUBLISH_STATE_DRAFT },
  { label: '审批中', value: PUBLISH_STATE_PENDING },
  { label: '已发布', value: PUBLISH_STATE_PUBLISHED },
];

/** 发布状态 → 中文（与后端 `publish_state_label` 逐字一致）。 */
export function publishStateLabel(state: string | null | undefined): string {
  switch ((state ?? '').trim().toUpperCase()) {
    case PUBLISH_STATE_DRAFT:
      return '草稿';
    case PUBLISH_STATE_PENDING:
      return '审批中';
    case PUBLISH_STATE_PUBLISHED:
      return '已发布';
    default:
      return '未知';
  }
}

/** 发布状态 → TDesign 主题（已发布绿 / 审批中蓝 / 草稿灰）。 */
export function publishStateTheme(state: string | null | undefined): 'success' | 'primary' | 'default' {
  switch ((state ?? '').trim().toUpperCase()) {
    case PUBLISH_STATE_PUBLISHED:
      return 'success';
    case PUBLISH_STATE_PENDING:
      return 'primary';
    default:
      return 'default';
  }
}

// ==================== 出参 ====================

/** 一版标准成本（列表行 / 详情卡头共用）。 */
export interface StandardRow {
  id: string;
  material_id: string;
  /** 物料编码（实时 join；物料被删后为 null） */
  material_code: string | null;
  material_name: string | null;
  /** `RAW` / `SEMI` / `FINISHED` / `SERVICE` */
  material_type: string | null;
  material_spec: string | null;
  /** 同物料内递增的版本号 */
  version_no: number;
  /** 生效起始日 `yyyy-MM-dd` */
  effective_from: string;
  /** 停止日（null = 至今有效） */
  effective_to: string | null;
  status: number;
  status_label: string;
  /** 发布状态 `DRAFT` / `PENDING` / `PUBLISHED`（M5-10） */
  publish_state: string;
  /** 发布状态文本（草稿 / 审批中 / 已发布） */
  publish_state_label: string;
  /** 在跑 / 最近一次的发布审批实例 id（审批中必有；已发布保留作审计） */
  publish_instance_id: string | null;
  /** 最近一次发布时刻（ISO） */
  published_at: string | null;
  /** 单位标准材料成本（料 = Σ明细金额） */
  std_material_cost: string;
  /** 单位标准人工成本 */
  std_labor_cost: string;
  /** 单位标准制造费用 */
  std_overhead_cost: string;
  /** 单位标准总成本 */
  std_total_cost: string;
  /** 单位标准工时 */
  std_labor_hours: string;
  /** 标准工资率 */
  std_labor_rate: string;
  /** 标准 BOM 行数 */
  items_count: number;
  remark: string | null;
  created_at: string;
  updated_at: string;
}

/** 标准 BOM 一行。 */
export interface StandardItemRes {
  id: string;
  line_no: number;
  component_material_id: string;
  component_material_code: string | null;
  component_material_name: string | null;
  std_qty: string;
  std_price: string;
  /** 金额 = round(用量 × 单价, 6)，服务端算 */
  std_amount: string;
}

/** 版本详情（卡头 + 标准 BOM）。 */
export interface StandardDetailRes {
  head: StandardRow;
  items: StandardItemRes[];
}

/** 「某物料某期间（某天）生效的是哪一版」。 */
export interface StandardEffectiveRes {
  period_code: string | null;
  /** 实际取数日期（期间 = 期末） */
  on_date: string;
  version: StandardDetailRes;
  /** 期间内换过版（另一版也在期间内生效过） */
  switch_in_period: boolean;
  other_versions: StandardRow[];
  /** 口径说明（差异分析按哪一天的标准取数） */
  note: string;
}

/** 期间 × 物料矩阵一行。 */
export interface PeriodSheetRes {
  head: StandardRow;
  switch_in_period: boolean;
}

/** 生效 / 失效结果。 */
export interface StandardStatusRes {
  version: StandardDetailRes;
  message: string;
  /** 需要操作人注意的后果（失效后该期间已无标准成本…） */
  warnings: string[];
}

/** 导入结果（`StandardImportRes`）。 */
export interface StandardImportResult {
  /** 成功入库的数据行数（全有或全无：0 或等于文件数据行数） */
  success: number;
  /** 有错的行数（按行去重） */
  failed: number;
  /** 新建的版本数（= 文件里不同「物料 + 期间」组的个数） */
  versions_created: number;
  rows: ImportIssue[];
}

/** 一行导入错误（后端 `cost_labor_import::ImportIssue`，`column` = 中文列名）。 */
export interface ImportIssue {
  /** Excel 真实行号（表头是第 1 行） */
  line: number;
  /** 中文列名（整行级错误时为 `-`） */
  column: string;
  message: string;
}

// ==================== 入参 ====================

/** 标准 BOM 一行（请求）。 */
export interface StandardItemPayload {
  component_material_id: string;
  /** 字符串入参（后端 `Decimal`） */
  std_qty: string;
  std_price: string;
}

/** 新建一版（`POST /cost-standard`）。 */
export interface StandardCreatePayload {
  material_id: string;
  /** 整期便捷写法：`yyyy-MM` ⇒ 生效区间落整期 */
  period_code?: string;
  effective_from?: string;
  effective_to?: string;
  std_labor_hours?: string;
  std_labor_rate?: string;
  std_overhead_cost?: string;
  items?: StandardItemPayload[];
  status?: number;
  remark?: string;
}

/** 改一版（`PUT /cost-standard/{id}`）：**缺省的键 = 不改这一项**。 */
export interface StandardUpdatePayload {
  material_id?: string;
  period_code?: string;
  effective_from?: string;
  effective_to?: string;
  /** `true` = 把停止日清成「至今有效」（传空串表达不了这个语义） */
  clear_effective_to?: boolean;
  std_labor_hours?: string;
  std_labor_rate?: string;
  std_overhead_cost?: string;
  /** `[]` = 清空标准 BOM */
  items?: StandardItemPayload[];
  status?: number;
  remark?: string;
}

/** 生效 / 失效（`POST /cost-standard/{id}/status`）。 */
export interface StandardStatusPayload {
  status: number;
  /** 变更原因（给了会以 `[生效|失效 yyyy-MM-dd]` 前缀追加到备注留痕） */
  reason?: string;
}

/** 版本列表查询。 */
export interface StandardPageQuery {
  material_id?: string;
  status?: number | null;
  /** 发布状态 `DRAFT` / `PENDING` / `PUBLISHED`（M5-10；不传 = 全部） */
  publish_state?: string;
  /** `yyyy-MM`：版本生效区间与该期间有交集即命中 */
  period_code?: string;
  /** `yyyy-MM-dd`：版本覆盖该日即命中 */
  on_date?: string;
  keyword?: string;
  page?: number;
  page_size?: number;
}

/** 「某物料某期间生效版本」查询（与 `period_code` 二选一）。 */
export interface StandardEffectiveQuery {
  material_id: string;
  period_code?: string;
  on_date?: string;
}

/** 期间 × 物料矩阵查询。 */
export interface PeriodSheetQuery {
  /** `yyyy-MM`（必填，缺了 400） */
  period_code: string;
  keyword?: string;
  page?: number;
  page_size?: number;
}

/** 列表每页条数（与本仓其它成本页一致）。 */
export const PAGE_SIZE = 20;

/** 下拉一次取的条数（物料这种几百条的字典够用，超了在下拉里搜）。 */
export const OPTION_LIMIT = 200;
