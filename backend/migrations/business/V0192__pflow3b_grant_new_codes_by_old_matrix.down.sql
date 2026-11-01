-- ======================================================
-- V0192 down：回滚「4 个新权限码按老码矩阵补齐」
--
-- 回到 V0191 交付时的形态：这 4 个码只授给 SUPER_ADMIN。
--
-- ⚠️ 边界（写在这里免得回滚后才发现）：
-- 1. SUPER_ADMIN 的授权来自 V0191，本 down **不删**（否则回滚会把超管自己也削掉）；
-- 2. 本迁移期间由「数据驱动」补出去的自定义角色授权同样会被删 —— 它们也无法与
--    V0191 之后运维手工加的授权区分（sys_role_menu 没有来源列）。若线上确实手工授过，
--    回滚后按 `scripts/verify/pflow3b_gap_roles.sh` 的清单手工补回（脚本会打印当前的
--    授权矩阵，便于对比）。
-- 3. 不删 sys_menu / sys_role（V0191 的 down 负责），本迁移没有创建它们。
-- ======================================================

DELETE FROM sys_role_menu rm
 USING sys_role r
 WHERE rm.role_id = r.id
   AND r.role_code <> 'SUPER_ADMIN'
   AND rm.menu_id IN ('20000000-0000-0000-0000-000000001028'::uuid,
                      '20000000-0000-0000-0000-000000001029'::uuid,
                      '20000000-0000-0000-0000-000000001030'::uuid,
                      '20000000-0000-0000-0000-000000001031'::uuid);

-- 自检：回到「只有 SUPER_ADMIN 持有」的形态（空库 / 没有超管时允许 0 行）
DO $$
DECLARE
    v_other int;
BEGIN
    SELECT count(*) INTO v_other
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id
     WHERE r.role_code <> 'SUPER_ADMIN'
       AND rm.menu_id IN ('20000000-0000-0000-0000-000000001028'::uuid,
                          '20000000-0000-0000-0000-000000001029'::uuid,
                          '20000000-0000-0000-0000-000000001030'::uuid,
                          '20000000-0000-0000-0000-000000001031'::uuid);
    IF v_other > 0 THEN
        RAISE EXCEPTION 'V0192 down 自检失败：仍有 % 行非超管授权', v_other;
    END IF;
    RAISE NOTICE 'V0192 down 完成：4 个新权限码只剩 SUPER_ADMIN 持有';
END $$;
