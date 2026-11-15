-- ============================================================================
-- V0355 down：把 budget_approve 从 v2 回滚到 v1（有实例跑在 v2 上则拒绝）
--
-- 回滚语义与 V0251 同口径：
--   * v2 的节点 / 边 / 审批人绑定 / 版本行全删；
--   * v1 复位为 PUBLISHED + is_current，定义指回 v1；
--   * 只要还有审批单挂在 v2 上就**拒绝**回滚 —— 删版本会让那些单指向空气，
--     那不是「回滚」而是丢数据。先让它们跑完 / 作废，再回滚。
-- ============================================================================
DO $$
DECLARE
    c_v2 CONSTANT uuid := '20000000-0000-4000-8000-0000000c022b';
    c_def   uuid;
    c_v1    uuid;
    n_live  int;
BEGIN
    SELECT d.id INTO c_def FROM biz_flow_def d WHERE d.flow_code = 'budget_approve' AND d.deleted_at IS NULL;
    IF c_def IS NULL OR NOT EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = c_v2) THEN
        RAISE NOTICE 'V0355 down budget_approve：没有本迁移发的 v2，跳过';
        RETURN;
    END IF;

    -- v1 必须按 version_no = 1 取：此刻 def.current_version_id 指向 v2
    SELECT id INTO c_v1 FROM biz_flow_def_version WHERE def_id = c_def AND version_no = 1;
    IF c_v1 IS NULL THEN
        RAISE EXCEPTION 'V0355 down 拒绝回滚：budget_approve 找不到 version_no = 1 的版本';
    END IF;

    SELECT count(*) INTO n_live FROM biz_flow_instance WHERE version_id = c_v2;
    IF n_live > 0 THEN
        RAISE EXCEPTION 'V0355 down 拒绝回滚：% 张审批单跑在 v2 上（删版本会留下指向空气的审批单）', n_live;
    END IF;

    DELETE FROM biz_flow_node_assignee WHERE node_id IN (SELECT id FROM biz_flow_node WHERE version_id = c_v2);
    DELETE FROM biz_flow_edge WHERE version_id = c_v2;
    DELETE FROM biz_flow_node WHERE version_id = c_v2;
    DELETE FROM biz_flow_def_version WHERE id = c_v2;

    UPDATE biz_flow_def_version
       SET is_current = TRUE, version_status = 'PUBLISHED', updated_at = now()
     WHERE id = c_v1;
    UPDATE biz_flow_def
       SET current_version_id = c_v1, current_version_no = 1, updated_at = now()
     WHERE id = c_def;

    RAISE NOTICE 'V0355 down budget_approve：已回滚到 v1（部门主管节点恢复为角色会签）';
END $$;
