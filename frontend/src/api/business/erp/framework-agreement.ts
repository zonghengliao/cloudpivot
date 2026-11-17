/**
 * 框架协议 API 层（后端 `backend/app/src/business/erp/purchase/framework_agreement*`，M4-07/08/09）。
 *
 * # 本卡（M4-11）能用的**全部**接口只有 2 条只读 GET
 *
 * | 接口 | 用途 | 权限码 |
 * |------|------|--------|
 * | `GET /api/business/erp/framework-agreement/execution/page` | 协议台账 / 执行量总览（服务端分页） | `business:erp:framework-agreement:execution` |
 * | `GET /api/business/erp/framework-agreement/{id}/execution` | 单协议详情（单头汇总 + 逐行 + 下钻来源） | 同上 |
 *
 * ⚠ **没有** `POST/PUT/DELETE /framework-agreement`：协议的新建 / 编辑 / 提交 / 审批
 * 后端至今没有任何实现（M4-07 只建表、M4-08 只做下单引用校验、M4-09 只做统计视图），
 * 库里也**没有** `…:create` / `…:update` / `…:delete` 这三个权限码
 * （V0452 只种了页面行 `…:page` + 按钮行 `…:execution`）。
 * 所以页面**只画读写得到的入口**：不摆点了必然 404 的「新建 / 编辑」按钮 —— 那是
 * AGENTS「契约行有、接口没落地」的已知坑（前端照契约画按钮，用户点了必然报错）。
 * 要补维护链路得另开卡（`docs/planning/20261113-新卡清单.md` 里 M4 段目前**没有**这张卡）。
 *
 * # 四条渲染口径（都来自后端 DTO 注释，别在页面里重新猜）
 *
 * 1. **`null` 不等于 0**：`remaining_qty` / `cap_amount` / `exec_qty_rate` / `cap_qty`
 *    为 `null` 是「**不限量**」，显示成 0 会被读成「一点都不剩」—— 恰好相反。
 * 2. **负数要显形**：`remaining_qty < 0` = 超上限执行（`exec_state = OVER`），
 *    报表**不许**截断到 0；进度条封顶 100，真实倍数写在文案里。
 * 3. **`booked_drift != 0` 是数据分叉**（回写快照 M4-08 与单据链现算不一致），
 *    低调提示即可，不是错误（视图现算永远是真相，drift 只是解释「为什么两个数不一样」）。
 * 4. **空物料范围的协议是 200 + `items: []`**，不是 404；只有协议不存在 / 已软删才 404。
 */
import { get, type PageData } from '@/api/request';

/** 接口前缀（后端 `#[nik::get]` 注解不含 `/api`，这里必须写全，漏了会命中 SPA fallback 返回 HTML） */
const BASE = '/api/business/erp';

/** 后端 `NUMERIC` 在 JSON 里是**字符串**（`Decimal`），少数聚合列可能是 number */
export type DecimalText = string | number;

/** 协议状态（与 V0450 的 `ck_biz_fwa_status` 逐字一致：**只有三个**，没有 EXPIRED） */
export type AgreementStatus = 'DRAFT' | 'SUBMITTED' | 'APPROVED';

/**
 * 有效期状态（与 V0452 视图 `validity_state` 的 CASE 逐字一致）。
 *
 * 是**时间的函数**，不是状态列：`APPROVED` + 日期区间才对得上才叫 `ACTIVE`。
 */
export type ValidityState = 'DRAFT' | 'SUBMITTED' | 'NOT_STARTED' | 'ACTIVE' | 'EXPIRED';

/** 行 / 单头执行状态（与 V0452 的 `exec_state` 逐字一致，五态） */
export type ExecState = 'UNLIMITED' | 'NOT_STARTED' | 'PARTIAL' | 'DONE' | 'OVER';

/** 定价方式（`ck_biz_fwa_price_mode`：固定价 / 随行就市） */
export type PriceMode = 'FIXED' | 'FLOATING';

/**
 * 协议执行量的一行（= 后端 `AgreementExecRowRes`，28+ 列全收）。
 *
 * 同一个类型既是「总览列表的一行」也是「详情里的单头」—— 后端两个接口用的是**同一个**
 * 出参结构，页面别为列表另建一个窄类型（会漏字段、渲染时再猜一次）。
 */
export interface AgreementExecRow {
  /** 协议 id（详情接口的主键；列表行主键用它，不是 `id`） */
  agreement_id: string;
  /** 协议号 FWA-yyyyMMdd-000001 */
  agreement_no: string;
  /** 协议名称 */
  agreement_name: string;
  /** 供应商 id */
  supplier_id: string;
  /** 供应商名**快照**（签约主体历史事实） */
  supplier_name: string | null;
  /** 供应商**现名**（实时 join；快照为空时回退到它）—— 展示一律用它 */
  supplier_display_name: string;
  /** 经办部门 id */
  dept_id: string | null;
  /** 经办部门名快照 */
  dept_name: string | null;
  /** 经办人 id */
  owner_id: string | null;
  /** 经办人姓名快照 */
  owner_name: string | null;
  /** 有效期起（含） */
  start_date: string;
  /** 有效期止（含） */
  end_date: string;
  /** 签订日期 */
  sign_date: string | null;
  /** 协议状态 */
  status: AgreementStatus;
  /** 协议状态中文 */
  status_label: string;
  /** 有效期状态 */
  validity_state: ValidityState;
  /** 有效期状态中文 */
  validity_state_label: string;
  /** 今天是否生效（与下单准入同一判据） */
  is_effective: boolean;
  /** 距截止日还有几天（已过期 = 负数） */
  days_left: number;
  /** 定价方式 */
  price_mode: PriceMode;
  /** 定价方式中文 */
  price_mode_label: string;
  /** 协议价是否含税 */
  price_includes_tax: boolean;
  /** 协议价币种 */
  price_currency: string;
  /** 允许高于协议价的比例上限（0 = 必须 ≤ 协议价） */
  price_tolerance: DecimalText;
  /** 单头数量上限（null = 不限） */
  total_max_qty: DecimalText | null;
  /** 单头金额上限（null = 不限） */
  total_max_amount: DecimalText | null;
  /** 有效数量封顶（单头没写时回退明细合计；null = 不限） */
  cap_qty: DecimalText | null;
  /** 有效金额封顶（= 单头金额上限；null = 不限） */
  cap_amount: DecimalText | null;
  /** 明细上限合计（仅在明细全部设了上限时有值） */
  item_max_qty_sum: DecimalText | null;
  /** 物料行数 */
  item_count: number;
  /** 不限量的行数 */
  unlimited_items: number;
  /** 超上限的行数 */
  over_items: number;
  /** 已用满的行数 */
  done_items: number;
  /** 部分执行的行数 */
  partial_items: number;
  /** 未开工的行数 */
  not_started_items: number;
  /** 已用量合计（单据现算） */
  executed_qty: DecimalText;
  /** 已用金额合计（不含税） */
  executed_amount: DecimalText;
  /** 已用金额合计（含税） */
  executed_amount_with_tax: DecimalText;
  /** 已用金额（协议价口径） */
  executed_amount_capped: DecimalText;
  /** 回写快照：已用量合计（M4-08 的 `total_executed_qty`） */
  booked_total_qty: DecimalText;
  /** 回写快照：已用金额合计 */
  booked_total_amount: DecimalText;
  /** 剩余可下单量（**可为负**；null = 不限） */
  remaining_qty: DecimalText | null;
  /** 剩余可用金额（**可为负**；null = 不限） */
  remaining_amount: DecimalText | null;
  /** 数量执行率（0–1，6 位；null = 不限） */
  exec_qty_rate: DecimalText | null;
  /** 数量执行率百分比（0–100，2 位；null = 不限） */
  exec_qty_rate_pct: DecimalText | null;
  /** 金额执行率（0–1，6 位；null = 无金额上限） */
  exec_amount_rate: DecimalText | null;
  /** 金额执行率百分比 */
  exec_amount_rate_pct: DecimalText | null;
  /** 单头状态（五态） */
  exec_state: ExecState;
  /** 单头状态中文 */
  exec_state_label: string;
  /** 计入额度的采购单单数 */
  order_doc_count: number;
  /** 已释放（作废 / 软删）的采购单单数 */
  released_doc_count: number;
  /** 最近一次下单日 */
  last_order_on: string | null;
  /** 创建时间（RFC3339） */
  created_at: string;
}

/** 物料行执行量（= 后端 `AgreementExecItemRes`） */
export interface AgreementExecItem {
  /** 协议行 id */
  agreement_item_id: string;
  /** 行号 */
  line_no: number;
  /** 物料 id */
  material_id: string;
  /** 物料编码 */
  material_code: string;
  /** 物料名称 */
  material_name: string;
  /** 单位 id */
  unit_id: string;
  /** 单位名 */
  unit_name: string;
  /** 协议单价 */
  agreed_price: DecimalText;
  /** 进项税率（0–1 分数，0.13 = 13%） */
  tax_rate: DecimalText;
  /** 该物料数量上限（null = 不限） */
  max_qty: DecimalText | null;
  /** 已用量（单据现算） */
  executed_qty: DecimalText;
  /** 回写快照（M4-08 的 `executed_qty`） */
  booked_qty: DecimalText;
  /** 回写与单据链的差（非 0 = 分叉） */
  booked_drift: DecimalText;
  /** 已用金额（不含税） */
  executed_amount: DecimalText;
  /** 已用金额（含税） */
  executed_amount_with_tax: DecimalText;
  /** 已用金额（协议价口径） */
  executed_amount_capped: DecimalText;
  /** 行金额上限（数量上限 × 协议价；null = 不限） */
  cap_amount: DecimalText | null;
  /** 剩余数量（null = 不限；可为负） */
  remaining_qty: DecimalText | null;
  /** 剩余金额（null = 不限；可为负） */
  remaining_amount: DecimalText | null;
  /** 数量执行率（0–1，6 位；null = 不限） */
  exec_qty_rate: DecimalText | null;
  /** 数量执行率百分比 */
  exec_qty_rate_pct: DecimalText | null;
  /** 金额执行率（0–1，6 位） */
  exec_amount_rate: DecimalText | null;
  /** 行状态 */
  exec_state: ExecState;
  /** 行状态中文 */
  exec_state_label: string;
  /** 计入的采购单单数 */
  order_doc_count: number;
  /** 计入的采购明细行数 */
  order_line_count: number;
  /** 最近一次下单日 */
  last_order_on: string | null;
}

/**
 * 下钻：这一行的量被哪张采购单的哪一行用掉。
 *
 * **作废 / 软删的采购单也在里面**（`counted = false` + `verdict` 给原因）——
 * 不列出来用户会以为「量凭空少了」。
 */
export interface ExecSource {
  /** 协议行 id（与 `items[].agreement_item_id` 对应） */
  agreement_item_id: string;
  /** 采购单 id */
  order_id: string;
  /** 采购单号 */
  order_no: string;
  /** 采购单状态 */
  order_status: string;
  /** 采购单状态中文 */
  order_status_label: string;
  /** 下单日 */
  order_date: string;
  /** 采购单软删时间（有值 = 已删） */
  order_deleted_at: string | null;
  /** 采购明细行号 */
  line_no: number;
  /** 这一行用的数量 */
  qty: DecimalText;
  /** 这一行的不含税金额 */
  amount: DecimalText;
  /** 这一行的含税金额 */
  with_tax_amount: DecimalText;
  /** 这一行的下单价（不含税） */
  price: DecimalText;
  /** 是否计入已用量 */
  counted: boolean;
  /** 判定原因（「已用」/「采购单已作废」/「采购单已删除」） */
  verdict: string;
}

/** 单协议详情：单头汇总 + 逐行执行量 + 下钻来源（= 后端 `AgreementExecutionRes`） */
export interface AgreementExecution {
  /** 单头汇总 */
  agreement: AgreementExecRow;
  /** 逐行执行量（后端已按行号升序） */
  items: AgreementExecItem[];
  /** 下钻来源（后端已按行号 / 下单日 / 采购单号升序；空物料范围的协议这里是 `[]`） */
  sources: ExecSource[];
}

/**
 * 分页入参（后端 `ExecutionPageQuery`）。
 *
 * ⚠ 用 `type` 而不是 `interface`：`request()` 的 `params` 是 `Record<string, unknown>`，
 * 类型别名才有隐式索引签名（interface 赋不过去，会逼着页面到处写 as）。
 */
export type ExecutionQuery = {
  /** 页码（1 起） */
  page?: number;
  /** 每页条数（后端 clamp 到 1–200） */
  page_size?: number;
  /** 关键字：协议号 / 协议名称 / 供应商名 / 经办人（后端上限 64 字符） */
  keyword?: string;
  /** 供应商 id（uuid 字符串；非法值后端 400） */
  supplier_id?: string;
  /** 协议状态（闭集外的值后端 400，不静默忽略） */
  status?: AgreementStatus | '';
  /** 有效期状态（同上） */
  validity_state?: ValidityState | '';
  /** 只看「有行超上限」的协议 */
  over_only?: boolean;
};

/**
 * 执行量总览 / 协议台账分页。
 *
 * 排序由后端给（**生效中优先 → 快到期的优先**），前端不重排。
 */
export function fetchExecutionPage(query: ExecutionQuery): Promise<PageData<AgreementExecRow>> {
  return get<PageData<AgreementExecRow>>(`${BASE}/framework-agreement/execution/page`, query);
}

/**
 * 单协议执行量详情（单头 + 逐行 + 下钻）。
 *
 * 协议不存在 / 已软删 → 404；**空物料范围 → 200 + `items: []`**（不是 404）。
 */
export function fetchAgreementExecution(id: string): Promise<AgreementExecution> {
  return get<AgreementExecution>(`${BASE}/framework-agreement/${id}/execution`);
}
