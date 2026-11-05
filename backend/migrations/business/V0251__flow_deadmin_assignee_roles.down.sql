-- V0251 down：删掉三条流程的 v2，v1 复位为当前版本（有实例跑在 v2 上则拒绝）

DO $$
DECLARE
    c_def uuid;
    c_v1  uuid;
    c_v2  CONSTANT uuid := '20000000-0000-4000-8000-0000002b0001';
    n_live int;
BEGIN
    SELECT id INTO c_def FROM biz_flow_def WHERE flow_code = 'payable_pay' AND deleted_at IS NULL;
    IF c_def IS NULL OR NOT EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = c_v2) THEN
        RAISE NOTICE 'V0251 down payable_pay：没有 v2，跳过';
        RETURN;
    END IF;
    -- v1 必须按 version_no=1 取：此刻 def.current_version_id 指向 v2
    SELECT id INTO c_v1 FROM biz_flow_def_version WHERE def_id = c_def AND version_no = 1;
    SELECT count(*) INTO n_live FROM biz_flow_instance WHERE version_id = c_v2;
    IF n_live > 0 THEN
        RAISE EXCEPTION 'V0251 down payable_pay 拒绝回滚：% 张审批单跑在 v2 上（删版本会留下指向空气的审批单）', n_live;
    END IF;
    DELETE FROM biz_flow_node_assignee WHERE node_id IN (SELECT id FROM biz_flow_node WHERE version_id = c_v2);
    DELETE FROM biz_flow_edge WHERE version_id = c_v2;
    DELETE FROM biz_flow_node WHERE version_id = c_v2;
    DELETE FROM biz_flow_def_version WHERE id = c_v2;
    UPDATE biz_flow_def_version SET is_current = TRUE, version_status = 'PUBLISHED', updated_at = now() WHERE id = c_v1;
    UPDATE biz_flow_def SET current_version_id = c_v1, current_version_no = 1, updated_at = now() WHERE id = c_def;
    RAISE NOTICE 'V0251 down payable_pay：已回滚到 v1';
END $$;

DO $$
DECLARE
    c_def uuid;
    c_v1  uuid;
    c_v2  CONSTANT uuid := '20000000-0000-4000-8000-0000002b0002';
    n_live int;
BEGIN
    SELECT id INTO c_def FROM biz_flow_def WHERE flow_code = 'stock_in_cancel' AND deleted_at IS NULL;
    IF c_def IS NULL OR NOT EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = c_v2) THEN
        RAISE NOTICE 'V0251 down stock_in_cancel：没有 v2，跳过';
        RETURN;
    END IF;
    -- v1 必须按 version_no=1 取：此刻 def.current_version_id 指向 v2
    SELECT id INTO c_v1 FROM biz_flow_def_version WHERE def_id = c_def AND version_no = 1;
    SELECT count(*) INTO n_live FROM biz_flow_instance WHERE version_id = c_v2;
    IF n_live > 0 THEN
        RAISE EXCEPTION 'V0251 down stock_in_cancel 拒绝回滚：% 张审批单跑在 v2 上（删版本会留下指向空气的审批单）', n_live;
    END IF;
    DELETE FROM biz_flow_node_assignee WHERE node_id IN (SELECT id FROM biz_flow_node WHERE version_id = c_v2);
    DELETE FROM biz_flow_edge WHERE version_id = c_v2;
    DELETE FROM biz_flow_node WHERE version_id = c_v2;
    DELETE FROM biz_flow_def_version WHERE id = c_v2;
    UPDATE biz_flow_def_version SET is_current = TRUE, version_status = 'PUBLISHED', updated_at = now() WHERE id = c_v1;
    UPDATE biz_flow_def SET current_version_id = c_v1, current_version_no = 1, updated_at = now() WHERE id = c_def;
    RAISE NOTICE 'V0251 down stock_in_cancel：已回滚到 v1';
END $$;

DO $$
DECLARE
    c_def uuid;
    c_v1  uuid;
    c_v2  CONSTANT uuid := '20000000-0000-4000-8000-0000002b0003';
    n_live int;
BEGIN
    SELECT id INTO c_def FROM biz_flow_def WHERE flow_code = 'stock_transfer' AND deleted_at IS NULL;
    IF c_def IS NULL OR NOT EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = c_v2) THEN
        RAISE NOTICE 'V0251 down stock_transfer：没有 v2，跳过';
        RETURN;
    END IF;
    -- v1 必须按 version_no=1 取：此刻 def.current_version_id 指向 v2
    SELECT id INTO c_v1 FROM biz_flow_def_version WHERE def_id = c_def AND version_no = 1;
    SELECT count(*) INTO n_live FROM biz_flow_instance WHERE version_id = c_v2;
    IF n_live > 0 THEN
        RAISE EXCEPTION 'V0251 down stock_transfer 拒绝回滚：% 张审批单跑在 v2 上（删版本会留下指向空气的审批单）', n_live;
    END IF;
    DELETE FROM biz_flow_node_assignee WHERE node_id IN (SELECT id FROM biz_flow_node WHERE version_id = c_v2);
    DELETE FROM biz_flow_edge WHERE version_id = c_v2;
    DELETE FROM biz_flow_node WHERE version_id = c_v2;
    DELETE FROM biz_flow_def_version WHERE id = c_v2;
    UPDATE biz_flow_def_version SET is_current = TRUE, version_status = 'PUBLISHED', updated_at = now() WHERE id = c_v1;
    UPDATE biz_flow_def SET current_version_id = c_v1, current_version_no = 1, updated_at = now() WHERE id = c_def;
    RAISE NOTICE 'V0251 down stock_transfer：已回滚到 v1';
END $$;
