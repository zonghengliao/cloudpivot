-- ======================================================
-- V0249 回滚：QA 夹具角色的数据范围 4（仅本人）→ 1（全部数据）
--
-- 迁移前基线（生产实测 2026-11-05 13:44）：这 4 个角色的 data_scope 全为 1。
-- 与 V0247 及其它种子类迁移同口径：
--   · 还原的是「基线」而不是「上一次的值」；
--   · 只碰当前值确实是 4 的行 —— 管理界面里别人后来配成 2/3/5 的不动；
--   · 不恢复/不删除角色行，不碰 sys_user_role / sys_role_menu / sys_role_mutex。
--
-- 回滚后这 4 个夹具角色重新回到「能看全部数据」的状态：只在确认「它们确实没有活跃
-- 持有者」时才安全（有持有者时回滚等于把该账号的可见范围放大，需人复核）。
-- ======================================================

DO $$
DECLARE
    v_rows integer;
BEGIN
    UPDATE sys_role
       SET data_scope = 1,
           updated_at = CURRENT_TIMESTAMP
     WHERE role_code IN ('QA_LIMITED', 'QA_VIEWER', 'MUTEX_A', 'MUTEX_B')
       AND deleted_at IS NULL
       AND data_scope = 4;
    GET DIAGNOSTICS v_rows = ROW_COUNT;
    RAISE NOTICE 'V0249 down：% 个 QA 夹具角色 data_scope 4 → 1', v_rows;

    IF EXISTS (SELECT 1 FROM sys_role
                WHERE role_code IN ('QA_LIMITED', 'QA_VIEWER', 'MUTEX_A', 'MUTEX_B')
                  AND deleted_at IS NULL
                  AND data_scope = 4) THEN
        RAISE EXCEPTION 'V0249 down 自检失败：仍有夹具角色停留在 data_scope=4';
    END IF;

    RAISE NOTICE 'V0249 down 完成：夹具角色已回到基线 1（角色行与权限码未动）';
END $$;
