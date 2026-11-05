-- ============================================================================
-- SUPERCONV（P0-FIX-2B-C）· 阶段 1.5 回退：删掉本阶段补的目录授权
-- ----------------------------------------------------------------------------
-- 判据：datafix_superconv_snapshot(batch='SUPERCONV-20261105-DIRS', tbl='sys_role_menu')
-- 幂等：已删过 ⇒ 0 行变更（快照还在，可以反复对齐）。
-- ============================================================================
\set ON_ERROR_STOP on

DO $$
DECLARE
  v_batch text := 'SUPERCONV-20261105-DIRS';
  v_have  int;
  v_del   int;
BEGIN
  IF to_regclass('public.datafix_superconv_snapshot') IS NULL THEN
    RAISE EXCEPTION '找不到快照表 —— 先跑过 15_grant_menu_dirs.up.sql 才谈回滚';
  END IF;

  SELECT count(*) INTO v_have FROM datafix_superconv_snapshot
   WHERE batch = v_batch AND tbl = 'sys_role_menu';
  IF v_have = 0 THEN
    RAISE NOTICE '没有本阶段快照（未执行过），无需回退';
    RETURN;
  END IF;

  DELETE FROM sys_role_menu rm
   USING datafix_superconv_snapshot s
   WHERE s.batch = v_batch AND s.tbl = 'sys_role_menu' AND s.pk = rm.id::text;
  GET DIAGNOSTICS v_del = ROW_COUNT;
  RAISE NOTICE '阶段 1.5 已回退：删除 % 行目录授权（快照 % 行）', v_del, v_have;
END $$;
