-- ======================================================
-- V0333 down：回滚角色升级链（P3-02）
--
-- 顺序：先清预置数据（把列还原成 NULL），再删约束 / 索引 / 列。
-- 说明：`escalation_role_id` 是本迁移新增的**纯配置列**，没有业务数据依赖它
-- （升级链只在审批人解析时读），所以回滚不需要安全阀。
-- `biz_flow_task` 上两列是**留痕**：回滚会丢掉「哪些待办是升级派下来的」这个事实，
-- 但待办本身（assignee_id / status）不受影响 —— 审计需要这段历史时不要回滚。
-- ======================================================

UPDATE sys_role SET escalation_role_id = NULL;

DROP INDEX IF EXISTS idx_sys_role_escalation;

ALTER TABLE sys_role DROP CONSTRAINT IF EXISTS ck_sys_role_escalation_self;

ALTER TABLE sys_role DROP COLUMN IF EXISTS escalation_role_id;

ALTER TABLE biz_flow_task DROP COLUMN IF EXISTS escalate_reason;
ALTER TABLE biz_flow_task DROP COLUMN IF EXISTS escalated_from_role;
