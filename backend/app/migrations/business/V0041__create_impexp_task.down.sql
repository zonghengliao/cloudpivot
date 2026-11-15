-- V0041 回滚：删除导入 / 导出任务表
--
-- 顺序说明：先删两张表的索引（显式删除，不依赖 DROP TABLE 的隐式行为），再删表。
-- 注意：本回滚不保留任何任务历史数据；sys_import_task_error（V0042）有外键指向
--       sys_import_task，回滚本迁移前必须先回滚 V0042，否则 DROP TABLE 会被外键挡住。

DROP INDEX IF EXISTS idx_sys_export_task_expire;
DROP INDEX IF EXISTS idx_sys_export_task_creator;
DROP INDEX IF EXISTS idx_sys_export_task_claim;
DROP INDEX IF EXISTS uk_sys_export_task_no;
DROP TABLE IF EXISTS sys_export_task;

DROP INDEX IF EXISTS idx_sys_import_task_biz;
DROP INDEX IF EXISTS idx_sys_import_task_creator;
DROP INDEX IF EXISTS idx_sys_import_task_claim;
DROP INDEX IF EXISTS uk_sys_import_task_no;
DROP TABLE IF EXISTS sys_import_task;
