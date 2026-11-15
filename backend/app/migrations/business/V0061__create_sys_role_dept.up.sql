-- ======================================================
-- V0061：数据权限 · 新建 `sys_role_dept`（角色-自定义部门集合，批次 B · B1）
--
-- 依据：docs/批次B-技术架构.md 附录 A.2（DDL 原文，已在 PG 18.6 实跑）；语义见 §2.1 / §2.3
-- 回滚：V0061__create_sys_role_dept.down.sql（DROP TABLE）
--
-- 双外键与 sys_user_role（V0003）同风格：这是「授权数据」，指向不存在的部门就是脏授权。
-- sys_dept 为软删表，FK 不阻塞正常删部门（软删只是 UPDATE deleted_at）。
-- 切换 data_scope 时**不清空**本表，便于来回切换不丢配置；读取时只在 data_scope=5 才取。
-- ======================================================

CREATE TABLE sys_role_dept (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),  -- 主键
    role_id UUID NOT NULL REFERENCES sys_role(id),  -- 角色 ID
    dept_id UUID NOT NULL REFERENCES sys_dept(id),  -- 部门 ID
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 创建时间
    create_by UUID,                                  -- 创建人
    UNIQUE (role_id, dept_id)                        -- 同一角色对同部门只一行
);

COMMENT ON TABLE sys_role_dept IS '角色-自定义部门集合（data_scope=5 时生效）';
COMMENT ON COLUMN sys_role_dept.role_id IS '角色 ID（sys_role.id）';
COMMENT ON COLUMN sys_role_dept.dept_id IS '部门 ID（sys_dept.id）';
COMMENT ON COLUMN sys_role_dept.created_at IS '创建时间';
COMMENT ON COLUMN sys_role_dept.create_by IS '创建人 ID';

CREATE INDEX idx_sys_role_dept_role ON sys_role_dept (role_id);
CREATE INDEX idx_sys_role_dept_dept ON sys_role_dept (dept_id);
