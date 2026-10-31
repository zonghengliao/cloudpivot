import { definePageChecks } from './_pagerunner';
import { pages } from './_catalog';

/** ERP 单据 3 页：采购订单 / 收货入库 / 应付单。 */
definePageChecks(pages(['采购订单', '收货入库', '应付单']));
