-- V0067 回滚：删除 `sys_export_task.mask_applied`。
--
-- 数据影响：产物「是否已脱敏」的审计标记丢失（历史行不可恢复）。
-- 若 B6 的代码仍在，写入该列的语句会直接报 `column does not exist`（500）——
-- 因此回滚顺序同样是「先回滚 B6 代码、再回滚本迁移」。

ALTER TABLE sys_export_task
    DROP COLUMN IF EXISTS mask_applied;
