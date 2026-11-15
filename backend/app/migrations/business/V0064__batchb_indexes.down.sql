-- V0064 回滚：删除本迁移**独占**的三个索引。
--
-- 为什么只删 3 个（附录 A 的 V0064 共 4 条 CREATE INDEX）：
--   `idx_sys_role_mutex_b` 由 V0063 创建，V0064 里那条是 `IF NOT EXISTS` 的幂等空操作。
--   若在这里 DROP 它，「回滚 V0064、保留 V0063」的库会丢掉 V0063 声明的索引 —— 状态不自洽。
--   因此 down 只删本迁移真正新增的索引；`idx_sys_role_mutex_b` 由 V0063 的 down（DROP TABLE）负责。
--
-- 索引是纯派生对象，删除不丢数据；重复执行安全（IF EXISTS）。

DROP INDEX IF EXISTS idx_sys_user_dept_created;
DROP INDEX IF EXISTS idx_sys_import_task_create_by;
DROP INDEX IF EXISTS idx_sys_export_task_create_by;

-- idx_sys_role_mutex_b 属于 V0063，本回滚不删（见上）。
