-- ======================================================
-- V0123：审批流 · 运行侧索引补齐（批次 D · D2）
--
-- 依据：`docs/批次D-工作流审批-技术架构.md` §6.3（POC 实测补出来的 3 条索引）+ 卡面
--       「索引：待办列表（assignee_id + status）、实例（def_id + status）、节点（instance_id + round）」。
--
-- 为什么单独一条迁移：卡面要求「V0123（索引）」与 V0122（建表）分离，
-- 好处是索引可以在线单独补 / 单独回（DBA 视角：建表变更与性能变更不是同一类审批）。
--
-- 三条 POC 补的索引（数字是 §7.4 的实测）：
--   ① idx_biz_flow_task_node           会签计票 41.4 ms → 0.099 ms（418×）
--      —— `uk_biz_flow_task_pending` 是 `WHERE status='PENDING'` 的部分索引，
--         服务不了「数 APPROVED 有几条」，所以计票必须另有一条跨状态索引。
--   ② idx_biz_flow_instance_node_active 「审批中看板」39.1 ms → 0.116 ms
--   ③ idx_biz_flow_cc_unread            「抄送未读」红点
--
-- 全部 `IF NOT EXISTS`：与 V0122 内联建的同名索引不冲突（重复执行安全）。
-- 回滚：`V0123__flow_instance_indexes.down.sql`（只 DROP 本文件建的 3 条 + 断言）
-- ======================================================

CREATE INDEX IF NOT EXISTS idx_biz_flow_task_node
    ON biz_flow_task (instance_node_id, status);

CREATE INDEX IF NOT EXISTS idx_biz_flow_instance_node_active
    ON biz_flow_instance_node (activated_at DESC)
    WHERE status = 'ACTIVE';

CREATE INDEX IF NOT EXISTS idx_biz_flow_cc_unread
    ON biz_flow_cc (receiver_id)
    WHERE NOT is_read;
