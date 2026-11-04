-- ======================================================
-- V0216（回滚）：撤掉 po_approve v2，把 v1 复位为当前版本
--
-- 安全阀：**有审批单跑在 v2 上就拒绝回滚**（`biz_flow_instance.version_id` 是 RESTRICT 外键，
-- 但库侧报的是「违反外键」这种看不懂的话，这里先给人话 + 计数）。
-- v2 的节点/边/审批人随版本行级联删除，不需要单独删。
-- ======================================================
DO $$
DECLARE
    c_def uuid := '20000000-0000-4000-8000-0000000a0182';
    c_v1  uuid := '20000000-0000-4000-8000-0000000a0183';
    c_v2  uuid := '20000000-0000-4000-8000-0000000a0184';
    n_inst bigint;
BEGIN
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = c_v2) THEN
        RAISE NOTICE 'V0216 down：没有 v2，无需回滚';
        RETURN;
    END IF;

    SELECT count(*) INTO n_inst FROM biz_flow_instance WHERE version_id = c_v2;
    IF n_inst > 0 THEN
        RAISE EXCEPTION 'V0216 down 回滚中止：仍有 % 张审批单跑在 v2 上（先让它们跑完或撤回）', n_inst;
    END IF;

    DELETE FROM biz_flow_def_version WHERE id = c_v2;

    UPDATE biz_flow_def_version
       SET is_current = TRUE, version_status = 'PUBLISHED', updated_at = now()
     WHERE id = c_v1;

    UPDATE biz_flow_def
       SET current_version_id = c_v1, current_version_no = 1, updated_at = now()
     WHERE id = c_def;

    RAISE NOTICE 'V0216 down：po_approve 已回滚到 v1（金额三档、无超限复核）';
END $$;
