/**
 * 审批详情里的**业务明细**（采购单明细行）。
 *
 * # 为什么不在 `form_data` 里
 *
 * 卡面以为 `form_data` 有 `items: [...]`，生产库实测**没有**
 * （`purchase_handler::purchase_form_data` 只冻结单头：单据号 / 金额 / 供应商 / 仓库 / 金额口径）。
 * 审批人要看「这单买了什么、几件、多少钱」，只能去业务单拿明细：
 * `GET /api/business/erp/purchase-order-item/page?order_id=…`（`fetchPurchaseOrderItems`）。
 *
 * # 三条边界（都是硬约束）
 *
 * 1. **只在采购审批上用**：`biz_type` 必须是 `PO_APPROVE`，其它流程（自定义流程 / 收货 / 付款）
 *    没有「明细页」，这里一律返回空数组，由 `form_data` 自己的数组字段兜底。
 * 2. **接口 403 / 404 / 超时都不许冒泡**：审批岗位没有 ERP 明细权限时，明细区就**不显示**，
 *    审批页其余内容照常（详情页不许因为一块明细挂掉）。
 * 3. **不是审批快照**：读的是业务单**当前**明细（`form_data` 没冻结明细，这是唯一可得来源）。
 *    单据在审批期间被改（只有 `DRAFT` 能改，`SUBMITTED` 之后不可改）——所以实际就是提交时那一刻的值。
 *
 * 顺带把明细行的键**对齐 `form_data` 的口径**：接口给的是 `price`（不含税单价），
 * 这里保留原键不动，让 `form-field-labels.ts` 的 `fieldLabel` / `isAmountField` 统一处理。
 */
import { fetchPurchaseOrderItems } from '@/api/business/erp';

/** 采购审批的业务类型（`purchase_bridge::BIZ_TYPE`） */
export const PO_APPROVE_BIZ_TYPE = 'PO_APPROVE';

/**
 * 单据明细行能变成「明细表」的业务类型白名单。
 *
 * 只有采购：收货 / 付款的明细字段名与采购不同（`stockin` 走 `stock-in-item/page`），
 * 未在本卡验证过，宁可先不显示，也不拿错字段拼一张错表。
 */
const ITEM_BIZ_TYPES = [PO_APPROVE_BIZ_TYPE];

/** 明细行（键 = 后端 `PurchaseOrderItemLine` 的键，值原样给格式化层） */
export type BusinessItemRow = Record<string, unknown>;

/**
 * 取业务单明细行。
 *
 * @param bizType 实例的 `biz_type`（`PO_APPROVE` 等）
 * @param formData 实例的 `form_data`（采购单 id 优先取 `order_id`）
 * @param bizId 实例的 `biz_id`（`order_id` 缺失时的兜底）
 * @returns 明细行；不适用 / 拉不到 → `[]`
 */
export async function loadBusinessItemRows(
  bizType: string,
  formData: Record<string, unknown> | null | undefined,
  bizId: string,
): Promise<BusinessItemRow[]> {
  if (!ITEM_BIZ_TYPES.includes(bizType)) {
    return [];
  }
  const rawId = formData?.order_id ?? bizId;
  const orderId = typeof rawId === 'string' ? rawId.trim() : '';
  if (!orderId) {
    return [];
  }
  try {
    const rows = await fetchPurchaseOrderItems(orderId);
    return (rows ?? []).map((row) => ({ ...row }) as BusinessItemRow);
  } catch {
    // 403（无 ERP 明细权限）/ 404 / 网络错误：明细区不显示，审批正文照常
    return [];
  }
}
