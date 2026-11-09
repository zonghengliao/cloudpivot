-- ============================================================================
-- 10_svc_selfcheck.down.sql —— 回滚：摘掉 svc_selfcheck 的权限与账号
--
-- 口径（为什么不是一把 DELETE）：
--   · `sys_user` / `sys_role` 走**软删**（`deleted_at = now()`）：审计要能回溯「这个机器账号
--     什么时候有过、什么时候没的」，和库里其它账号的处置方式一致；
--   · `sys_user_role` / `sys_role_menu` 是纯关系表，跟主体一起硬删（否则重建会撞唯一键）；
--   · 真删（物理删）只有一种情况：本脚本创建的、从没有过业务痕迹的账号 —— 由人工在确认后执行，
--     见 README §5。
--
-- 幂等：连跑两次第二次 0 行变更。
-- ============================================================================
\set ON_ERROR_STOP on

BEGIN;

DO $$
DECLARE
  v_uid  uuid;
  v_rid  uuid;
  v_u    int := 0;
  v_r    int := 0;
  v_ur   int := 0;
  v_rm   int := 0;
BEGIN
  SELECT id INTO v_uid FROM sys_user WHERE username = 'svc_selfcheck' AND deleted_at IS NULL;
  SELECT id INTO v_rid FROM sys_role WHERE role_code = 'OPS_SELFCHECK' AND deleted_at IS NULL;

  IF v_uid IS NOT NULL THEN
    DELETE FROM sys_user_role WHERE user_id = v_uid;
    GET DIAGNOSTICS v_ur = ROW_COUNT;
    UPDATE sys_user SET deleted_at = now(), is_locked = false, login_fail_count = 0, lock_time = NULL
     WHERE id = v_uid;
    GET DIAGNOSTICS v_u = ROW_COUNT;
  END IF;

  IF v_rid IS NOT NULL THEN
    DELETE FROM sys_role_menu WHERE role_id = v_rid;
    GET DIAGNOSTICS v_rm = ROW_COUNT;
    UPDATE sys_role SET deleted_at = now() WHERE id = v_rid;
    GET DIAGNOSTICS v_r = ROW_COUNT;
  END IF;

  RAISE NOTICE 'svc_selfcheck 回滚：用户软删 % 行 / 角色软删 % 行 / user_role 删 % 行 / role_menu 删 % 行',
    v_u, v_r, v_ur, v_rm;
END $$;

COMMIT;
