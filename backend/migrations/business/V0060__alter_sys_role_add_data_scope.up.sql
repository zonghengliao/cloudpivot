-- ======================================================
-- V0060：数据权限 · `sys_role` 增加数据范围列（批次 B · B1）
--
-- 依据：docs/批次B-技术架构.md 附录 A.1（DDL 原文，已在 PG 18.6 实跑）
--       + docs/批次B-DDL.md §1
-- 回滚：V0060__alter_sys_role_add_data_scope.down.sql（DROP CONSTRAINT + DROP COLUMN）
--
-- 默认值 1（全部）是**刻意的**：迁移前系统不存在数据权限概念，所有角色的实际可见范围
-- 都是「全部」。默认 1 = 迁移后可见范围与迁移前逐行一致，不产生无预告的数据消失。
-- 收紧由管理员按角色配置（模板见架构文档 §5.2 的 V0068，默认不执行）。
--
-- 不建索引：data_scope 只在「解析当前用户 scope」时按 role_id 读取（走主键），
-- 不参与任何过滤条件。
-- ======================================================

ALTER TABLE sys_role
    ADD COLUMN IF NOT EXISTS data_scope SMALLINT NOT NULL DEFAULT 1;

ALTER TABLE sys_role
    ADD CONSTRAINT ck_sys_role_data_scope CHECK (data_scope IN (1, 2, 3, 4, 5));

COMMENT ON COLUMN sys_role.data_scope IS
    '数据范围：1 全部 / 2 本部门 / 3 本部门及以下 / 4 仅本人 / 5 自定义部门集合';
