-- ======================================================
-- V0297 down：撤掉收款退款审批的种子流程（只删本迁移种的那一套固定 uuid）
--
-- 顺序：审批人 → 边 → 节点 → 版本 → 定义（子表在前，`biz_flow_node_assignee` 有
-- ON DELETE CASCADE，这里仍显式删，回滚轨迹更清楚）。
--
-- ⚠ 对**已产生实例**的库主动拒绝：`biz_flow_instance.def_id` 是 RESTRICT 外键，
-- 直接删定义会被库拦下（报错信息难懂）。这里先给出可读的拒绝原因，让人知道
-- 「先处理在跑的退款审批单，再回滚迁移」。
-- ======================================================
DO $$
DECLARE
    used int;
BEGIN
    SELECT count(*) INTO used FROM biz_flow_instance
     WHERE def_id = '20000000-0000-4000-8000-0000000d0701'::uuid AND deleted_at IS NULL;
    IF used > 0 THEN
        RAISE EXCEPTION 'V0297 down 拒绝执行：还有 % 张审批实例引用 receipt_refund 流程（先处理它们再回滚）', used;
    END IF;
END $$;

DELETE FROM biz_flow_node_assignee
 WHERE node_id IN ('20000000-0000-4000-8000-0000000d0711'::uuid, '20000000-0000-4000-8000-0000000d0712'::uuid, '20000000-0000-4000-8000-0000000d0713'::uuid);
DELETE FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000d0702'::uuid;
DELETE FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000d0702'::uuid;
DELETE FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0702'::uuid;
DELETE FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d0701'::uuid;
