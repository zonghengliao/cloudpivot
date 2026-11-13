-- ============================================================================
-- V0403 down：撤掉 HR_MANAGER × {审批流目录 …0860, 审批中心页面 …0862}
--
-- 只删本迁移加的那两对（角色按 role_code 现查），不动 sys_menu / 其它授权。
-- 删完 HR_MANAGER 的 `business:flow:approve`（…0803）会重新变成孤儿按钮 ——
-- 那是 V0403 之前的库状态（闸⑦ 红），回滚就该回到那里。
--
-- 注意：本迁移**不**回滚 V0400 的 approve 授权。若只想撤「看不到审批中心」这件事，
-- 正确做法是连 V0400 一起回滚（否则留下孤儿按钮）。
-- ============================================================================

DO $$
DECLARE
    v_role    uuid;
    v_deleted int := 0;
BEGIN
    SELECT id INTO v_role
      FROM sys_role
     WHERE role_code = 'HR_MANAGER' AND deleted_at IS NULL;
    IF v_role IS NULL THEN
        RAISE NOTICE 'V0403 down：HR_MANAGER 不存在或已软删 → 无需回滚';
        RETURN;
    END IF;

    DELETE FROM sys_role_menu
     WHERE role_id = v_role
       AND menu_id IN (
               '20000000-0000-0000-0000-000000000860'::uuid,
               '20000000-0000-0000-0000-000000000862'::uuid
           );
    GET DIAGNOSTICS v_deleted = ROW_COUNT;

    RAISE NOTICE 'V0403 down：已删除 HR_MANAGER 的审批流目录 / 审批中心页面授权 % 行（approve 按钮会重新变孤儿）',
        v_deleted;
END $$;
