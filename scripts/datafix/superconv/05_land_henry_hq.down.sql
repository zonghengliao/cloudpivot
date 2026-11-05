-- ============================================================================
-- SUPERCONV（P0-FIX-2B-C）· 阶段 0 回退：把 henry.liao 的 dept_id 还原成快照值
-- ----------------------------------------------------------------------------
-- 判据：datafix_superconv_snapshot(batch='SUPERCONV-20261105-LAND', tbl='sys_user')
-- 幂等：已等于快照值 ⇒ 0 行变更。
-- ============================================================================
\set ON_ERROR_STOP on

DO $$
DECLARE
  v_batch text := 'SUPERCONV-20261105-LAND';
  v_uid   uuid;
  v_old   uuid;
  v_upd   int;
  v_have  int;
BEGIN
  IF to_regclass('public.datafix_superconv_snapshot') IS NULL THEN
    RAISE EXCEPTION '找不到快照表 —— 先跑过 05_land_henry_hq.up.sql 才谈回滚';
  END IF;

  SELECT count(*) INTO v_have FROM datafix_superconv_snapshot
   WHERE batch = v_batch AND tbl = 'sys_user';
  IF v_have <> 1 THEN
    RAISE EXCEPTION '快照期望 1 行，实际 % 行，停手', v_have;
  END IF;

  SELECT (s.row_data->>'id')::uuid, (s.row_data->>'dept_id')::uuid
    INTO v_uid, v_old
    FROM datafix_superconv_snapshot s
   WHERE s.batch = v_batch AND s.tbl = 'sys_user';

  UPDATE sys_user SET dept_id = v_old, updated_at = now()
   WHERE id = v_uid AND dept_id IS DISTINCT FROM v_old;
  GET DIAGNOSTICS v_upd = ROW_COUNT;

  IF (SELECT dept_id FROM sys_user WHERE id = v_uid) IS DISTINCT FROM v_old THEN
    RAISE EXCEPTION '回滚后 dept_id 仍不等于快照值，停手';
  END IF;

  RAISE NOTICE '阶段 0 已回退：henry.liao.dept_id 还原为 %（变更 % 行）', coalesce(v_old::text, 'NULL'), v_upd;
END $$;
