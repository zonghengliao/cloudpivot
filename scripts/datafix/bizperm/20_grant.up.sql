-- ============================================================================
-- BIZPERM（P0-FIX-2B-C 跟进 · 业务角色权限覆盖）· 阶段 1：按职能归位补码
-- ----------------------------------------------------------------------------
-- 决策：方案 1（补码到既有角色），见
--   docs/architecture/P0-FIX-2B-C-业务角色权限覆盖-决策.md
--
-- 授权依据（本卡验收第 2 条「若补码」+ 上级卡 t_db2506d2 的用户批准链）：
--   用户 2026-11-05 16:36/16:45 批准「摘超管 + 补这 5 个角色」；本卡把该批准
--   在**不改数据范围、不建新角色、不扩超管**的前提下补齐页面级权限码。
--
-- 本阶段做三件事（全部只增不减）：
--   1. 新建 3 条真实存在但库里缺失的按钮码（记账凭证 审核/记账/反审核）
--   2. 按 plan.sql 的矩阵插入 sys_role_menu（up/down 共用同一份计划）
--   3. 推进角色持有者的 sys_user.token_version（app 改授权时同口径）
--
-- 幂等：每条插入前 EXISTS 判重 + 确定性 uuid + ON CONFLICT DO NOTHING；
--       重放全部命中「已存在」→ 0 行变更，NOTICE 说明。
-- 回退：20_grant.down.sql 按**同一份计划**算出的确定性 id 精确删除，绝不误删既有授权。
-- ============================================================================
\set ON_ERROR_STOP on

\i plan.sql
\i expand.sql

-- 确定性 uuid：同一 (role, menu, 批次) 恒得同一 id ⇒ 幂等 + 精确回退（不依赖快照表）
CREATE OR REPLACE FUNCTION pg_temp.bp_id(p_role uuid, p_menu uuid) RETURNS uuid
LANGUAGE sql IMMUTABLE AS $fn$
  SELECT (substr(h,1,8) || '-' || substr(h,9,4) || '-4' || substr(h,14,3)
          || '-8' || substr(h,18,3) || '-' || substr(h,21,12))::uuid
    FROM (SELECT md5(p_role::text || '|' || p_menu::text || '|bizperm-20261105') AS h) x
$fn$;

DO $$
DECLARE
  v_menu_ins  int;
  v_grant_ins int;
  v_sel       int;
  v_ver       int;
  v_pre       int;
  v_post      int;
  v_super     int;
  v_tmpl      int;
  c_biz CONSTANT text[] := ARRAY['DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER'];
BEGIN
  -- ------------------------------------------------ ① 前置：快照必须先跑
  IF to_regclass('public.datafix_bizperm_snapshot') IS NULL THEN
    RAISE EXCEPTION '先跑 05_snapshot.up.sql（SOP：无快照不动数据）';
  END IF;
  SELECT count(*) INTO v_sel
    FROM datafix_bizperm_snapshot WHERE batch = 'BIZPERM-20261105-ROLEMENU';
  IF v_sel = 0 THEN
    RAISE EXCEPTION '快照表里没有 sys_role_menu 基线（batch=BIZPERM-20261105-ROLEMENU）→ 先跑 05_snapshot.up.sql';
  END IF;

  -- ------------------------------------------------ ② 前置：超管仍只有 admin
  SELECT count(*) INTO v_super
    FROM sys_user_role ur JOIN sys_role r ON r.id = ur.role_id
   WHERE r.role_code = 'SUPER_ADMIN';
  IF v_super <> 1 THEN
    RAISE EXCEPTION '前置失败：SUPER_ADMIN 成员 % 个（目标态 = 1）', v_super;
  END IF;

  -- ------------------------------------------------ ③ 前置：5 个角色 data_scope 未被改动
  SELECT count(*) INTO v_pre FROM sys_role
   WHERE (role_code, data_scope) IN (('DEPT_LEADER',3),('FINANCE_MANAGER',5),('WAREHOUSE_MANAGER',3),
                                     ('PURCHASE_MANAGER',3),('GENERAL_MANAGER',1))
     AND deleted_at IS NULL;
  IF v_pre <> 5 THEN
    RAISE EXCEPTION '前置失败：5 个角色的 data_scope 形状不符（期望 DEPT_LEADER=3/FINANCE_MANAGER=5/WAREHOUSE=3/PURCHASE=3/GM=1），实际命中 % 行', v_pre;
  END IF;

  -- ------------------------------------------------ ④ 插入前基线（对平用）
  SELECT count(*) INTO v_pre FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id
   WHERE r.role_code = ANY (c_biz);

  -- ------------------------------------------------ ⑤ 新建 3 条凭证按钮码
  IF NOT EXISTS (SELECT 1 FROM sys_menu WHERE auth_code = 'business:erp:voucher:audit') THEN
    INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component, sort_order,
                          status, icon, title, is_frame, is_cache, is_visible, module_code,
                          created_at, updated_at)
    SELECT n.id, n.name, 3, n.auth_code, NULL, p.id, NULL, n.sort_order,
           1, NULL, n.auth_code, false, false, true, 'business', now(), now()
      FROM _new_menus n
      JOIN sys_menu p ON p.path = n.parent_path AND p.menu_type = 2 AND p.deleted_at IS NULL
     WHERE NOT EXISTS (SELECT 1 FROM sys_menu x WHERE x.id = n.id);
    GET DIAGNOSTICS v_menu_ins = ROW_COUNT;
  ELSE
    v_menu_ins := 0;
  END IF;

  -- ------------------------------------------------ ⑥ 按计划插授权（幂等）
  INSERT INTO sys_role_menu (id, role_id, menu_id, created_at)
  SELECT pg_temp.bp_id(r.id, s.id), r.id, s.id, now()
    FROM _sel s
    JOIN sys_role r ON r.role_code = s.role_code AND r.deleted_at IS NULL
   WHERE NOT EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = s.id)
  ON CONFLICT (id) DO NOTHING;
  GET DIAGNOSTICS v_grant_ins = ROW_COUNT;

  -- ------------------------------------------------ ⑦ 推进 token_version（只推进持有者）
  -- 仅当真有变更时才推进：纯重放不该把在线会话踢掉（幂等重放 = 0 变更 = 0 副作用）
  v_ver := 0;
  IF (v_grant_ins + v_menu_ins) > 0 THEN
    UPDATE sys_user u
       SET token_version = u.token_version + 1, updated_at = now()
     WHERE u.deleted_at IS NULL
       AND EXISTS (
         SELECT 1 FROM sys_user_role ur JOIN sys_role r ON r.id = ur.role_id
          WHERE ur.user_id = u.id AND r.role_code = ANY (c_biz)
            AND (ur.expire_time IS NULL OR ur.expire_time > now()));
    GET DIAGNOSTICS v_ver = ROW_COUNT;
  END IF;

  -- ------------------------------------------------ ⑧ 施工后硬断言
  -- ⑧.1 超管成员数不变（本卡不许碰超管）
  SELECT count(*) INTO v_post
    FROM sys_user_role ur JOIN sys_role r ON r.id = ur.role_id
   WHERE r.role_code = 'SUPER_ADMIN';
  IF v_post <> 1 THEN
    RAISE EXCEPTION '施工后断言失败：SUPER_ADMIN 成员 % 个（期望 1）', v_post;
  END IF;

  -- ⑧.2 计划全覆盖：每个 (role, menu) 都在库里
  SELECT count(*) INTO v_sel FROM (
    SELECT s.role_code, s.id, r.id AS rid FROM _sel s
      JOIN sys_role r ON r.role_code = s.role_code AND r.deleted_at IS NULL) t
   WHERE NOT EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = t.rid AND rm.menu_id = t.id);
  IF v_sel <> 0 THEN
    RAISE EXCEPTION '施工后断言失败：仍有 % 个计划节点未授权', v_sel;
  END IF;

  -- ⑧.3 凭证审核/记账/反审核三条码必须已授权给财务经理
  SELECT count(*) INTO v_tmpl
    FROM sys_role_menu rm
    JOIN sys_role r ON r.id = rm.role_id AND r.role_code = 'FINANCE_MANAGER'
    JOIN sys_menu m ON m.id = rm.menu_id
   WHERE m.auth_code IN ('business:erp:voucher:audit','business:erp:voucher:post','business:erp:voucher:un-audit');
  IF v_tmpl <> 3 THEN
    RAISE EXCEPTION '施工后断言失败：财务经理的凭证审核/记账/反审核码只有 % 条（期望 3）', v_tmpl;
  END IF;

  -- ⑧.4 平台治理页一条都不许进业务角色（再断一次，与 expand.sql 的 4.2 互为双保险）
  SELECT count(*) INTO v_tmpl
    FROM sys_role_menu rm
    JOIN sys_role r ON r.id = rm.role_id AND r.role_code = ANY (c_biz)
    JOIN sys_menu m ON m.id = rm.menu_id
   WHERE m.path IN ('/system/user','/system/role','/system/dept','/system/menu',
                    '/system/data-scope','/system/field-mask','/system/role-mutex',
                    '/system/change-log','/business/storage/backend',
                    '/support/config','/support/dict','/support/file','/support/job',
                    '/support/log','/support/online');
  IF v_tmpl <> 0 THEN
    RAISE EXCEPTION '施工后断言失败：业务角色越界持有 % 个平台治理节点', v_tmpl;
  END IF;

  -- ⑧.5 每个角色至少有一条业务页（防止计划写歪成只授按钮）
  SELECT count(*) INTO v_tmpl FROM (
    SELECT r.role_code
      FROM sys_role r JOIN sys_role_menu rm ON rm.role_id = r.id
      JOIN sys_menu m ON m.id = rm.menu_id AND m.menu_type = 2 AND m.deleted_at IS NULL
     WHERE r.role_code = ANY (c_biz)
     GROUP BY 1) t;
  IF v_tmpl <> 5 THEN
    RAISE EXCEPTION '施工后断言失败：只有 % 个角色有可见业务页（期望 5）', v_tmpl;
  END IF;

  RAISE NOTICE '阶段 1 完成：新建按钮码 % 条 / 新增授权 % 行 / 推进 token_version % 个账号（插入前基线 % 行）',
    v_menu_ins, v_grant_ins, v_ver, v_pre;
END $$;

-- 施工后对平：逐角色授权行数（up 前后用同一 SQL 对照）
SELECT r.role_code, r.data_scope,
       count(*) FILTER (WHERE m.menu_type = 1) AS dirs,
       count(*) FILTER (WHERE m.menu_type = 2) AS pages,
       count(*) FILTER (WHERE m.menu_type = 3) AS buttons,
       count(*) AS total
  FROM sys_role r
  JOIN sys_role_menu rm ON rm.role_id = r.id
  JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
 WHERE r.role_code IN ('DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER')
 GROUP BY 1, 2 ORDER BY 1;
