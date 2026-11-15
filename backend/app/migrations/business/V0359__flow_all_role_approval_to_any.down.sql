-- ============================================================================
-- V0359 down：把本迁移发的版本删掉，旧版复位为当前（有审批单跑在新版上则拒绝）
--
-- 回滚语义与 V0251 / V0355 同口径：
--   * 本迁移发的版本（change_note 以 'E2E07-FIX-1：' 开头）的节点 / 边 / 绑定 / 版本行全删；
--   * 该流程剩下的最大 version_no 复位为 PUBLISHED + is_current，定义指回去；
--   * 只要还有审批单挂在被删版本上就**拒绝**回滚 —— 删版本会让那些单指向空气。
-- ============================================================================
DO $$
DECLARE
    t       record;
    c_prev  uuid;
    c_def   uuid;
    n_live  int;
    n_del   int := 0;
BEGIN
    FOR t IN
        SELECT v.id AS version_id, v.def_id, v.version_no
          FROM biz_flow_def_version v
         WHERE v.change_note LIKE 'E2E07-FIX-1：%'
         ORDER BY v.def_id, v.version_no DESC
    LOOP
        SELECT count(*) INTO n_live FROM biz_flow_instance WHERE version_id = t.version_id;
        IF n_live > 0 THEN
            RAISE EXCEPTION 'V0359 down 拒绝回滚：流程 def=% 的 v% 上还有 % 张审批单（删版本会留下指向空气的审批单）。先让它们跑完 / 作废再回滚',
                t.def_id, t.version_no, n_live;
        END IF;

        DELETE FROM biz_flow_node_assignee
         WHERE node_id IN (SELECT id FROM biz_flow_node WHERE version_id = t.version_id);
        DELETE FROM biz_flow_edge WHERE version_id = t.version_id;
        DELETE FROM biz_flow_node WHERE version_id = t.version_id;
        DELETE FROM biz_flow_def_version WHERE id = t.version_id;
        n_del := n_del + 1;

        -- 复位到剩下的最高版本
        SELECT id INTO c_prev
          FROM biz_flow_def_version
         WHERE def_id = t.def_id
         ORDER BY version_no DESC
         LIMIT 1;
        SELECT id INTO c_def FROM biz_flow_def WHERE id = t.def_id;

        UPDATE biz_flow_def_version
           SET is_current = TRUE, version_status = 'PUBLISHED', updated_at = now()
         WHERE id = c_prev;
        UPDATE biz_flow_def
           SET current_version_id = c_prev,
               current_version_no = (SELECT version_no FROM biz_flow_def_version WHERE id = c_prev),
               updated_at = now()
         WHERE id = c_def;

        RAISE NOTICE 'V0359 down：def=% 已回滚到 v%（旧版的 APPROVAL 节点恢复为全员会签）',
            t.def_id, (SELECT version_no FROM biz_flow_def_version WHERE id = c_prev);
    END LOOP;

    IF n_del = 0 THEN
        RAISE NOTICE 'V0359 down：没有本迁移发的版本，跳过';
    ELSE
        RAISE NOTICE 'V0359 down 完成：删除 % 个本迁移发布的版本', n_del;
    END IF;
END $$;
