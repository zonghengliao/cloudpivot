-- ======================================================
-- V0063：角色互斥（SoD）· 新建 `sys_role_mutex`（批次 B · B1）
--
-- 依据：docs/批次B-技术架构.md 附录 A.4（DDL 原文，已在 PG 18.6 实跑）；语义见 §4.1
-- 回滚：V0063__create_sys_role_mutex.down.sql（DROP TABLE）
--
-- 角色互斥：A 与 B 不可同时授予同一用户。互斥是**对称**关系，A×B 与 B×A 同一条。
-- 对称性由「表达式唯一索引 LEAST/GREATEST」在库层保证（POC-6 实测：反向插入被
-- uk_sys_role_mutex_pair 拒绝，自反插入被 ck_sys_role_mutex_distinct 拒绝）。
-- ======================================================

CREATE TABLE sys_role_mutex (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    role_a_id UUID NOT NULL REFERENCES sys_role(id),
    role_b_id UUID NOT NULL REFERENCES sys_role(id),
    reason VARCHAR(255) NOT NULL,                    -- 互斥理由（合规留痕，必填）
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_role_mutex_distinct CHECK (role_a_id <> role_b_id)
);

COMMENT ON TABLE sys_role_mutex IS '角色互斥（SoD）：两角色不可同时授予同一用户（对称关系）';
COMMENT ON COLUMN sys_role_mutex.role_a_id IS '互斥角色 A（与 role_b_id 无方向语义）';
COMMENT ON COLUMN sys_role_mutex.role_b_id IS '互斥角色 B（与 role_a_id 无方向语义）';
COMMENT ON COLUMN sys_role_mutex.reason IS '互斥理由（合规留痕，必填）';

-- 对称唯一：规范化后 (A,B) 与 (B,A) 落到同一个键 → 同一条规则只能存一行
CREATE UNIQUE INDEX uk_sys_role_mutex_pair
    ON sys_role_mutex (LEAST(role_a_id, role_b_id), GREATEST(role_a_id, role_b_id))
    WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_role_mutex_b ON sys_role_mutex (role_b_id) WHERE deleted_at IS NULL;
