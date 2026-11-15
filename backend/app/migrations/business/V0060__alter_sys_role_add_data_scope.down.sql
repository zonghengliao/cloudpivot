-- V0060 回滚：删除 `sys_role.data_scope` 与其 CHECK 约束。
--
-- 顺序必须是「先删约束、后删列」：约束依赖该列，直接 DROP COLUMN 虽然由 PG 自动
-- 连带删除，但显式先删约束能让回滚脚本的意图可审计（也避免未来把列改成被其它对象引用时
-- 报错信息变成 "cannot drop column because other objects depend on it"）。
--
-- 数据影响：所有角色的数据范围配置一并丢失（回滚即回到「无数据权限概念」的 V0059 状态）。
-- 这一步只删本迁移新增的对象，`sys_role` 的其它列与数据不动。

ALTER TABLE sys_role
    DROP CONSTRAINT IF EXISTS ck_sys_role_data_scope;

ALTER TABLE sys_role
    DROP COLUMN IF EXISTS data_scope;
