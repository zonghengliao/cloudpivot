-- ============================================================================
-- SUPERCONV（P0-FIX-2B-C）· 阶段 1 回退：删掉给 laoliu 补的 5 个业务角色绑定
-- ----------------------------------------------------------------------------
-- 判据：datafix_superconv_snapshot(batch='SUPERCONV-20261105-ROLES', tbl='sys_user_role')
-- 幂等：已删过 ⇒ 0 行变更。
-- 注意：回退顺序 = 20.down（把超管加回）→ 10.down（摘掉业务角色）→ 15.down → 05.down，
--       否则中间会出现「既没有超管也没有业务角色」的窗口。
-- ============================================================================
\set ON_ERROR_STOP on

DO $$
DECLARE
  v_batch text := 'SUPERCONV-20261105-ROLES';
  v_have  int;
  v_del   int;
BEGIN
  IF to_regclass('public.datafix_superconv_snapshot') IS NULL THEN
    RAISE EXCEPTION '找不到快照表 —— 先跑过 10_grant_laoliu_roles.up.sql 才谈回滚';
  END IF;

  SELECT count(*) INTO v_have FROM datafix_superconv_snapshot
   WHERE batch = v_batch AND tbl = 'sys_user_role';
  IF v_have = 0 THEN
    RAISE NOTICE '没有本阶段快照（未执行过），无需回退';
    RETURN;
  END IF;

  DELETE FROM sys_user_role ur
   USING datafix_superconv_snapshot s
   WHERE s.batch = v_batch AND s.tbl = 'sys_user_role' AND s.pk = ur.id::text;
  GET DIAGNOSTICS v_del = ROW_COUNT;

  -- 硬断言：laoliu 已不持这 5 个角色
  IF EXISTS (
    SELECT 1 FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
      JOIN sys_role r ON r.id = ur.role_id
     WHERE u.username = 'laoliu'
       AND r.role_code IN ('DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER')
  ) THEN
    RAISE EXCEPTION '回退后 laoliu 仍持有目标业务角色，停手';
  END IF;

  RAISE NOTICE '阶段 1 已回退：删除 % 行业务角色绑定（快照 % 行）', v_del, v_have;
END $$;
