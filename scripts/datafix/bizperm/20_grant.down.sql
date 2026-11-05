-- ============================================================================
-- BIZPERM · 阶段 1 回退：按**同一份计划**（plan.sql + expand.sql）算出的确定性 id 删除
-- ----------------------------------------------------------------------------
-- 只删「本卡插进去的行」：
--   * id = pg_temp.bp_id(role_id, menu_id) 的行（= up 必然生成的 id）
--   * 3 条新建按钮码（固定 uuid 20000000-...-0b000x）及其授权行
-- 既有授权（不是本卡插的）**一行不碰** —— 这是与「按时间戳删」相比唯一可靠的判据。
--
-- 幂等：重放 0 行删除，NOTICE 说明。
-- ============================================================================
\set ON_ERROR_STOP on

\i plan.sql
\i expand.sql

CREATE OR REPLACE FUNCTION pg_temp.bp_id(p_role uuid, p_menu uuid) RETURNS uuid
LANGUAGE sql IMMUTABLE AS $fn$
  SELECT (substr(h,1,8) || '-' || substr(h,9,4) || '-4' || substr(h,14,3)
          || '-8' || substr(h,18,3) || '-' || substr(h,21,12))::uuid
    FROM (SELECT md5(p_role::text || '|' || p_menu::text || '|bizperm-20261105') AS h) x
$fn$;

DO $$
DECLARE
  v_rm   int;
  v_menu int;
  v_ver  int;
  v_left int;
  v_pre  int;
  c_biz CONSTANT text[] := ARRAY['DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER'];
BEGIN
  SELECT count(*) INTO v_pre FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id
   WHERE r.role_code = ANY (c_biz);

  -- ① 删本卡插入的授权行（含 3 条新建按钮码的授权）
  DELETE FROM sys_role_menu rm
   USING sys_role r
   WHERE rm.role_id = r.id
     AND r.role_code = ANY (c_biz)
     AND EXISTS (SELECT 1 FROM _sel s WHERE s.role_code = r.role_code AND s.id = rm.menu_id)
     AND rm.id = pg_temp.bp_id(rm.role_id, rm.menu_id);
  GET DIAGNOSTICS v_rm = ROW_COUNT;

  -- ② 删 3 条新建按钮码（先删引用它的授权行，FK 是 RESTRICT）
  DELETE FROM sys_role_menu WHERE menu_id IN (SELECT id FROM _new_menus);
  DELETE FROM sys_menu WHERE id IN (SELECT id FROM _new_menus);
  GET DIAGNOSTICS v_menu = ROW_COUNT;

  -- ③ 推进 token_version（让还带着新权限码的令牌立刻作废）；无变更则不动
  v_ver := 0;
  IF (v_rm + v_menu) > 0 THEN
    UPDATE sys_user u
       SET token_version = u.token_version + 1, updated_at = now()
     WHERE u.deleted_at IS NULL
       AND EXISTS (
         SELECT 1 FROM sys_user_role ur JOIN sys_role r ON r.id = ur.role_id
          WHERE ur.user_id = u.id AND r.role_code = ANY (c_biz)
            AND (ur.expire_time IS NULL OR ur.expire_time > now()));
    GET DIAGNOSTICS v_ver = ROW_COUNT;
  END IF;

  -- ④ 回退后断言：本卡的行必须一条不剩
  SELECT count(*) INTO v_left
    FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id
   WHERE r.role_code = ANY (c_biz)
     AND rm.id = pg_temp.bp_id(rm.role_id, rm.menu_id);
  IF v_left <> 0 THEN
    RAISE EXCEPTION '回退后断言失败：仍有 % 行本卡授权未删', v_left;
  END IF;

  SELECT count(*) INTO v_left FROM sys_menu WHERE id IN (SELECT id FROM _new_menus);
  IF v_left <> 0 THEN
    RAISE EXCEPTION '回退后断言失败：新建按钮码没删干净（剩 % 条）', v_left;
  END IF;

  RAISE NOTICE '阶段 1 已回退：删授权 % 行 / 删按钮码 % 条 / 推进 token_version % 个账号（回退前 % 行）',
    v_rm, v_menu, v_ver, v_pre;
END $$;

-- 回退后对平（应与快照基线逐角色一致）
SELECT batch, tbl, count(*) AS rows FROM datafix_bizperm_snapshot
 WHERE batch LIKE 'BIZPERM-20261105-ROLEMENU%' GROUP BY 1,2;
SELECT r.role_code, count(*) AS granted_rows
  FROM sys_role r JOIN sys_role_menu rm ON rm.role_id = r.id
 WHERE r.role_code IN ('DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER')
 GROUP BY 1 ORDER BY 1;
