-- ============================================================================
-- SUPERCONV（P0-FIX-2B-C）· 阶段 2 回退：按快照把 3 行绑定加回去 + 还原 token_version
-- ----------------------------------------------------------------------------
-- 判据：datafix_superconv_snapshot(batch='SUPERCONV-20261105', tbl='sys_user_role')
--      = 阶段 2 删掉的 3 行（先快照后删除，SOP §1）
-- 幂等：重放 0 行变更（ON CONFLICT (user_id, role_id) DO NOTHING）
-- 说明：token_version 一并还原，否则回滚后对方手上的旧令牌还是全失效状态
--       （这是可逆性的一部分，不是额外改动）。
-- ============================================================================
\set ON_ERROR_STOP on

DO $$
DECLARE
  v_batch text := 'SUPERCONV-20261105';
  v_ins   int;
  v_have  int;
  v_ver   int;
BEGIN
  IF to_regclass('public.datafix_superconv_snapshot') IS NULL THEN
    RAISE EXCEPTION '找不到快照表 datafix_superconv_snapshot —— 先跑过 20_revoke_super.up.sql 才谈回滚';
  END IF;

  SELECT count(*) INTO v_have FROM datafix_superconv_snapshot
   WHERE batch = v_batch AND tbl = 'sys_user_role';
  IF v_have <> 3 THEN
    RAISE EXCEPTION '快照里 sys_user_role 期望 3 行，实际 % 行，停手', v_have;
  END IF;

  INSERT INTO sys_user_role (id, user_id, role_id, expire_time, created_at)
  SELECT (s.row_data->>'id')::uuid,
         (s.row_data->>'user_id')::uuid,
         (s.row_data->>'role_id')::uuid,
         (s.row_data->>'expire_time')::timestamptz,
         coalesce((s.row_data->>'created_at')::timestamptz, now())
    FROM datafix_superconv_snapshot s
   WHERE s.batch = v_batch AND s.tbl = 'sys_user_role'
  ON CONFLICT (user_id, role_id) DO NOTHING;
  GET DIAGNOSTICS v_ins = ROW_COUNT;

  -- token_version 还原
  UPDATE sys_user u
     SET token_version = (s.row_data->>'token_version')::int,
         updated_at    = now()
    FROM datafix_superconv_snapshot s
   WHERE s.batch = v_batch AND s.tbl = 'sys_user' AND s.pk = u.id::text;
  GET DIAGNOSTICS v_ver = ROW_COUNT;

  -- 硬断言：3 行必须都在位
  SELECT count(*) INTO v_have
    FROM sys_user_role ur
    JOIN sys_user u ON u.id = ur.user_id
    JOIN sys_role r ON r.id = ur.role_id
   WHERE (u.username = 'laoliu_test' AND r.role_code IN ('SUPER_ADMIN', 'GENERAL_MANAGER'))
      OR (u.username = 'laoliu'     AND r.role_code = 'SUPER_ADMIN');
  IF v_have <> 3 THEN
    RAISE EXCEPTION '回滚后断言失败：目标绑定只有 % 行（期望 3）', v_have;
  END IF;

  RAISE NOTICE '阶段 2 已回退：补回 % 行绑定，还原 % 个账号 token_version', v_ins, v_ver;
END $$;
