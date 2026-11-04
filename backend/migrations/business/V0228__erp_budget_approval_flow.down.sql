-- V0228 down：撤掉 budget_approve 流程种子。
--
-- ⚠️ 有审批单跑在这个版本上时拒绝回滚（否则审批详情/时间线会指向空气）。
DO $$
DECLARE
    running int;
BEGIN
    IF to_regclass('public.biz_flow_instance') IS NOT NULL THEN
        SELECT count(*) INTO running FROM biz_flow_instance
         WHERE version_id = '20000000-0000-4000-8000-0000000c0229' AND status NOT IN ('APPROVED','REJECTED','WITHDRAWN','CANCELLED');
        IF running > 0 THEN
            RAISE EXCEPTION 'V0228 拒绝回滚：还有 % 张预算审批单跑在该版本上', running;
        END IF;
    END IF;
END $$;

DELETE FROM biz_flow_node_assignee
 WHERE node_id IN (SELECT id FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000c0229');
DELETE FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000c0229';
DELETE FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000c0229';
DELETE FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c0229';
DELETE FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000c0228';
