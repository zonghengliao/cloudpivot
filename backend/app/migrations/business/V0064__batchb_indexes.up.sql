-- ======================================================
-- V0064：批次 B 索引补强（批次 B · B1）
--
-- 依据：docs/批次B-技术架构.md 附录 A.5（DDL 原文，已在 PG 18.6 实跑）；对应查询路径见 §5.5
-- 回滚：V0064__batchb_indexes.down.sql（DROP INDEX ×3，见下注）
--
-- **并发性判断（本批实测，见 docs/批次B-DDL.md §5）**：
--   目标环境行数（2026-10-28 实测）：prod sys_user=2 / sys_import_task=0 / sys_export_task=0；
--   test sys_user=34 / sys_import_task=4 / sys_export_task=1。全部远小于 100 万行阈值，
--   因此本文件保持普通 `CREATE INDEX`（事务内、可回滚、失败不留 INVALID 索引）。
--   若将来任一目标表 > 100 万行：在文件首行加 `-- +no-transaction` 指令，并把对应
--   语句改成 `CREATE INDEX CONCURRENTLY IF NOT EXISTS`（CONCURRENTLY 不可在事务内执行）。
--
-- **与 V0063 的重叠**：`idx_sys_role_mutex_b` 已由 V0063 建（同名、同定义），此处的
-- `IF NOT EXISTS` 是一次**幂等空操作**。保留它是为了与附录 A 原文逐字一致（便于对照评审）；
-- 相应地，本文件的 down 脚本**不删**该索引（它属于 V0063），否则会破坏「只回滚 V0064、
-- V0063 仍在」这一状态下的库内自洽。
-- ======================================================

-- 1) sys_user (dept_id, created_at DESC)：数据权限注入后最常见的组合（按部门集过滤 + 按创建时间排序）
CREATE INDEX IF NOT EXISTS idx_sys_user_dept_created
    ON sys_user (dept_id, created_at DESC) WHERE deleted_at IS NULL;

-- 2) 导入/导出任务按 create_by 注入 SELF 范围（V0041 建表时无此索引）
CREATE INDEX IF NOT EXISTS idx_sys_import_task_create_by
    ON sys_import_task (create_by) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_sys_export_task_create_by
    ON sys_export_task (create_by) WHERE deleted_at IS NULL;

-- 3) 互斥查询的第二个方向（第一个方向由 uk 覆盖）；V0063 已建同名索引，此处幂等空操作
CREATE INDEX IF NOT EXISTS idx_sys_role_mutex_b
    ON sys_role_mutex (role_b_id) WHERE deleted_at IS NULL;
