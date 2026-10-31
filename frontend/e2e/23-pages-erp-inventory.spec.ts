import { definePageChecks } from './_pagerunner';
import { pages } from './_catalog';

/** ERP 库存 4 页：即时库存 / 库位库存 / 库存流水 / 库存调拨。 */
definePageChecks(pages(['即时库存', '库位库存', '库存流水', '库存调拨']));
