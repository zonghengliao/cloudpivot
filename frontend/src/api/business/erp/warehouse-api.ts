/**
 * 兼容壳（W4a-1）：契约已迁到 `@/api/business/erp`（`./index.ts`）。
 *
 * 保留本文件只为「旧 import 路径不炸」，**新代码一律 `from '@/api/business/erp'`**。
 * 这里刻意不写任何实现：两份实现等于两套契约，改一处漏一处就是线上 `undefined`
 * （字段名 `warehouse_code / warehouse_name / warehouse_path` 尤其容易漏）。
 */
export * from './index';
