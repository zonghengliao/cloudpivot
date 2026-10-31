import { definePageChecks } from './_pagerunner';
import { pages } from './_catalog';

/** ERP 单据 5 页：采购订单 / 收货入库 / 应付单 / 采购明细 / 单号序列（后两页是 2026-10-31 补齐路由的 404 页）。 */
definePageChecks(pages(['采购订单', '收货入库', '应付单', '采购明细', '单号序列']));
