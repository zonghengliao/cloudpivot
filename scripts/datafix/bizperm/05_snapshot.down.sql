-- ============================================================================
-- BIZPERM · 阶段 0 回退：清掉本批快照（快照本身是只读留痕，删它不影响业务表）
-- ============================================================================
\set ON_ERROR_STOP on
DO $$
DECLARE v_n int;
BEGIN
  IF to_regclass('public.datafix_bizperm_snapshot') IS NULL THEN
    RAISE NOTICE '快照表不存在，无需回退';
    RETURN;
  END IF;
  DELETE FROM datafix_bizperm_snapshot WHERE batch LIKE 'BIZPERM-20261105%';
  GET DIAGNOSTICS v_n = ROW_COUNT;
  RAISE NOTICE '已清掉快照 % 行', v_n;
END $$;
