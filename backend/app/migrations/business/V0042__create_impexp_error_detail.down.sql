-- V0042 回滚：删除导入错误明细表
--
-- 只删明细表，不动 sys_import_task（任务行的 error_summary 摘要仍在）。
-- 若同时要回滚任务表，请先执行本文件、再执行 V0041 的 down。

DROP INDEX IF EXISTS idx_sys_import_task_error_task;
DROP TABLE IF EXISTS sys_import_task_error;
