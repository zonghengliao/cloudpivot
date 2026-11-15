-- ============================================================================
-- V0367 down：把本迁移发的版本删掉，上一版复位为当前（有审批单跑在新版上则拒绝）
--
-- 回滚语义与 V0251 / V0355 / V0359 同口径：
--   * 本迁移发的版本（change_note 以 'V0367：' 开头）的节点 / 边 / 绑定 / 版本行全删；
--   * 按版本号倒序处理，删掉一版后把该流程**剩余版本里版本号最高且已归档**的那一版
--     复位为 PUBLISHED + is_current，定义指回去（多版本流程安全：
--     po_approve 删 v5 后复位到被本迁移归档的 v2，不会错跳到 DRAFT v3/v4）；
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
        SELECT v.id AS version_id, v.def_id, v.version_no, d.flow_code
          FROM biz_flow_def_version v
          JOIN biz_flow_def d ON d.id = v.def_id
         WHERE v.change_note LIKE 'V0367：%'
         ORDER BY v.def_id, v.version_no DESC
    LOOP
        SELECT count(*) INTO n_live FROM biz_flow_instance WHERE version_id = t.version_id;
        IF n_live > 0 THEN
            RAISE EXCEPTION 'V0367 down 拒绝回滚：流程 % 的 v% 上还有 % 张审批单（删版本会留下指向空气的审批单）。先让它们跑完 / 作废再回滚',
                t.flow_code, t.version_no, n_live;
        END IF;

        DELETE FROM biz_flow_node_assignee
         WHERE node_id IN (SELECT id FROM biz_flow_node WHERE version_id = t.version_id);
        DELETE FROM biz_flow_edge WHERE version_id = t.version_id;
        DELETE FROM biz_flow_node WHERE version_id = t.version_id;
        DELETE FROM biz_flow_def_version WHERE id = t.version_id;
        n_del := n_del + 1;

        -- 复位到「版本号最高且已归档」的那一版（= 被本迁移归档的旧当前版本）
        SELECT id INTO c_prev
          FROM biz_flow_def_version
         WHERE def_id = t.def_id
           AND version_status = 'ARCHIVED'
           AND version_no < t.version_no
         ORDER BY version_no DESC
         LIMIT 1;

        IF c_prev IS NULL THEN
            RAISE NOTICE 'V0367 down：流程 % 已无更早的归档版本，跳过复位（请人工检查）', t.flow_code;
            CONTINUE;
        END IF;

        UPDATE biz_flow_def_version
           SET is_current = TRUE, version_status = 'PUBLISHED',
               published_at = coalesce(published_at, now()), updated_at = now()
         WHERE id = c_prev;

        SELECT id INTO c_def FROM biz_flow_def WHERE id = t.def_id;
        UPDATE biz_flow_def
           SET current_version_id = c_prev,
               current_version_no = (SELECT version_no FROM biz_flow_def_version WHERE id = c_prev),
               updated_at = now()
         WHERE id = c_def;

        RAISE NOTICE 'V0367 down：流程 % 已回滚到 v%（旧版的 APPROVAL 节点恢复为全员会签）',
            t.flow_code, (SELECT version_no FROM biz_flow_def_version WHERE id = c_prev);
    END LOOP;

    IF n_del = 0 THEN
        RAISE NOTICE 'V0367 down：没有本迁移发的版本，跳过';
    ELSE
        RAISE NOTICE 'V0367 down 完成：删除 % 个本迁移发布的版本', n_del;
    END IF;
END $$;
