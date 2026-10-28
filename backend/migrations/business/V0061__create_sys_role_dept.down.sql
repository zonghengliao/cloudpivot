-- V0061 回滚：删除 `sys_role_dept` 整表。
--
-- 数据影响：所有角色的「自定义部门集合」配置丢失（data_scope=5 的角色回滚后会退化成
-- 「可见部门集合为空」——因为范围枚举仍在，只是集合没了；生产回滚前须把该表导出留档）。
--
-- 表上的两个索引（idx_sys_role_dept_role / idx_sys_role_dept_dept）随表一起消失，
-- 不需要单独 DROP INDEX；uniques 约束 uk（role_id, dept_id）同理。

DROP TABLE IF EXISTS sys_role_dept;
