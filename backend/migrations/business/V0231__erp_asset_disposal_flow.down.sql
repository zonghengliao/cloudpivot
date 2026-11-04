-- ======================================================
-- V0231 down：删 asset_disposal 流程种子
-- **有审批单跑在这个版本上就拒绝回滚**（biz_flow_instance.version_id 是 RESTRICT，
-- 让它撞外键的报错信息读不懂；自己先数、先 RAISE 才能指路）。
-- ======================================================
DO $$
DECLARE
    n int;
BEGIN
    SELECT count(*) INTO n FROM biz_flow_instance WHERE version_id = '20000000-0000-4000-8000-0000000c0231';
    IF n > 0 THEN
        RAISE EXCEPTION 'V0231 回滚被拒绝：还有 % 条审批实例跑在 asset_disposal v1 上', n;
    END IF;
END $$;

DELETE FROM biz_flow_node_assignee
 WHERE node_id IN (SELECT id FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000c0231');
DELETE FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000c0231';
DELETE FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000c0231';
DELETE FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c0231';
DELETE FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000c0230';
