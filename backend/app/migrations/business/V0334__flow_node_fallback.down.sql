-- ======================================================
-- V0334 down：回滚节点兜底人（P3-03）
--
-- 顺序：先删索引，再删约束，最后删列。
-- 说明：三列都是**纯配置 / 留痕**——`fallback_*` 没有任何业务数据依赖
-- （只在审批人解析时读），`assigned_by_rule` 丢的只是「这条待办是按哪条规则派下来的」
-- 这个事实，待办本身（assignee_id / status）不受影响。审计需要这段历史时不要回滚。
-- ======================================================

DROP INDEX IF EXISTS idx_biz_flow_task_by_rule;
DROP INDEX IF EXISTS idx_biz_flow_node_fallback_role;
DROP INDEX IF EXISTS idx_biz_flow_node_fallback_user;

ALTER TABLE biz_flow_task DROP CONSTRAINT IF EXISTS ck_biz_flow_task_assigned_by_rule;
ALTER TABLE biz_flow_task DROP COLUMN IF EXISTS assigned_by_rule;

ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS ck_biz_flow_node_fallback_exclusive;
ALTER TABLE biz_flow_node DROP COLUMN IF EXISTS fallback_role_id;
ALTER TABLE biz_flow_node DROP COLUMN IF EXISTS fallback_user_id;

COMMENT ON COLUMN biz_flow_node.empty_assignee IS
    '空审批人策略（SKIP / ERROR / FALLBACK_ADMIN）';
