-- ============================================================================
-- SUPERCONV（P0-FIX-2B-C）· 阶段 1 回退：撤掉 OPS_ADMIN（角色 + 授权 + laoliu 绑定）
-- ----------------------------------------------------------------------------
-- 用途：阶段 1 建错 / 验证不过时一键回到「没有 OPS_ADMIN 这个角色」的原始库。
--       **不碰**任何 SUPER_ADMIN 绑定 —— 阶段 1 本来就没摘过。
-- 幂等：重放 0 行变更。
-- 为什么是物理删而不是软删（deleted_at）：本角色是本批次**新建**的，且 down→up 必须
--   能往返（软删会让 10_ 的 ON CONFLICT (id) DO NOTHING 命中一条已删行，得到「有角色但
--   不可用」的僵尸态）。角色没有被任何其它表引用（sys_field_mask / sys_role_mutex /
--   sys_role_dept 三张关联表在本批次都不写），先删关联行再删角色即可。
-- ============================================================================
\set ON_ERROR_STOP on

DO $$
DECLARE
  v_role_id uuid := '20000000-0000-4000-8000-0000000f0016';
  v_ur  int;
  v_rm  int;
  v_r   int;
BEGIN
  DELETE FROM sys_user_role WHERE role_id = v_role_id
     OR id IN ('5c0e0000-0000-4000-8000-000000000001',
               '5c0e0000-0000-4000-8000-000000000002');
  GET DIAGNOSTICS v_ur = ROW_COUNT;

  DELETE FROM sys_role_menu WHERE role_id = v_role_id;
  GET DIAGNOSTICS v_rm = ROW_COUNT;

  DELETE FROM sys_role WHERE id = v_role_id AND role_code = 'OPS_ADMIN';
  GET DIAGNOSTICS v_r = ROW_COUNT;

  RAISE NOTICE 'OPS_ADMIN 已撤：user_role % 行 / role_menu % 行 / role % 行', v_ur, v_rm, v_r;

  -- 残留检查：撤完不该再有任何引用
  IF EXISTS (SELECT 1 FROM sys_user_role WHERE role_id = v_role_id)
     OR EXISTS (SELECT 1 FROM sys_role_menu WHERE role_id = v_role_id)
     OR EXISTS (SELECT 1 FROM sys_role WHERE id = v_role_id) THEN
    RAISE EXCEPTION '回退失败：OPS_ADMIN 仍有残留引用';
  END IF;
END $$;
