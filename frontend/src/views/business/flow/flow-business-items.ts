/**
 * 审批详情里的**业务明细**——现在读的是后端冻结在 `form_data.items` 里的**审批快照**。
 *
 * # 口径变更（P1-FIX 收口，后端 `dbc99c3` 起）
 *
 * 上一版（`t_5326e4b4`）的做法是「`form_data` 没明细 → 去业务单接口拿」：
 * `GET /api/business/erp/purchase-order-item/page?order_id=…`。两个毛病：
 *
 * 1. **审批人 403**：真正的审批人是岗位角色（`DEPT_LEADER` 等），生产实测四个审批角色
 *    `business:erp:*` 授权数全是 0，这个接口对他们 **HTTP 403** → 明细区永远是空的；
 * 2. **不是审批快照**：读的是单据**当前**明细，改单 / 作废后「审批时看到的」≠「当时的」。
 *
 * 后端已把明细冻进 `biz_flow_instance.form_data.items`（8 键：`line_no` / `material_code` /
 * `material_name` / `unit_name` / `qty` / `price` / `amount` / `tax_rate`，按 `line_no` 升序、
 * 数值是十进制字符串），所以这里**只读 `form_data`，不再发任何 ERP 请求**。
 *
 * # 老实例（本卡之前提交的）怎么办
 *
 * 它们的 `form_data` 里**没有** `items`。绝不渲染空表格，也绝不空白 —— 由
 * {@link readItemSnapshot} 给出 `legacy: true`，页面显示一行
 * 「明细：—（该审批单未冻结明细）」，其余字段照旧（名字走 `(ID: 前8位…)` 兜底）。
 * 判定用「有明细的业务类型」白名单（{@link ITEM_SNAPSHOT_BIZ_TYPES}），自定义流程不会被误报。
 */

/** 明细快照在 `form_data` 里的键（行内 8 键见后端 `form_snapshot::ITEM_KEYS`） */
export const ITEM_SNAPSHOT_KEY = 'items';

/** 采购审批的业务类型（`purchase_bridge::BIZ_TYPE`） */
export const PO_APPROVE_BIZ_TYPE = 'PO_APPROVE';

/**
 * 后端会给「明细」冻结快照的业务类型（真值来自后端，不是猜的）：
 *
 * | biz_type | 出处 |
 * |---|---|
 * | `PO_APPROVE` | `purchase_bridge::BIZ_TYPE`（采购订单审批） |
 * | `STOCK_IN_POST` | `stockin_model::POST_BIZ_TYPE`（收货过账审批） |
 * | `STOCK_TRANSFER` | `transfer_model::published_transfer_flow`（库存调拨审批，`V0157`） |
 *
 * 只用来判定「老实例缺明细」这一句提示；明细本身永远只看 `form_data.items` 有没有。
 */
export const ITEM_SNAPSHOT_BIZ_TYPES: readonly string[] = [
  PO_APPROVE_BIZ_TYPE,
  'STOCK_IN_POST',
  'STOCK_TRANSFER',
];

/** 明细行（键 = 后端快照的 8 键，值原样交给格式化层） */
export type BusinessItemRow = Record<string, unknown>;

/** 明细快照的读取结果 */
export interface ItemSnapshotState {
  /** 明细行（快照里没有 / 不是对象数组 → `[]`） */
  rows: BusinessItemRow[];
  /**
   * 该单「本该有明细，但快照里没有」= 老实例。
   *
   * 页面据此显示 `—`（不是空表格）；`items: []` 算「明确没有明细」，**不算**老实例。
   */
  legacy: boolean;
}

/** 普通对象行（排除 `null` / 数组） */
function isPlainObjectRow(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * 读 `form_data.items`（纯函数，零请求、零 Vue 依赖，可单测）。
 *
 * @param bizType 实例的 `biz_type`（只用于「老实例」判定）
 * @param formData 实例的 `form_data`（可能是 `{}` / `undefined`）
 *
 * 边界：
 * - 没有 `items` 键 → `{ rows: [], legacy: 有明细的业务类型 ? true : false }`；
 * - `items` 是空数组 → `{ rows: [], legacy: false }`（按「无明细」处理，不谎报老实例）；
 * - `items` 是对象数组 → 原样给行（顺序就是后端的 `line_no` 升序，前端**不重排**）；
 * - `items` 是脏数据（非空数组但元素全不是对象）→ `rows: []` + `legacy: true`（一样显示 `—`）。
 */
export function readItemSnapshot(
  bizType: string,
  formData: Record<string, unknown> | null | undefined,
): ItemSnapshotState {
  const capable = ITEM_SNAPSHOT_BIZ_TYPES.includes(String(bizType ?? '').trim());
  const value = formData ? formData[ITEM_SNAPSHOT_KEY] : undefined;
  if (Array.isArray(value)) {
    const rows = value.filter(isPlainObjectRow) as BusinessItemRow[];
    return { rows, legacy: value.length > 0 && rows.length === 0 };
  }
  return { rows: [], legacy: capable };
}
