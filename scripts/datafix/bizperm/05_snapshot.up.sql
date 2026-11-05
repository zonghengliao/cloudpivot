-- ============================================================================
-- BIZPERM（P0-FIX-2B-C 跟进）· 阶段 0：整行快照（**先于任何 DML**）
-- ----------------------------------------------------------------------------
-- 照 `docs/architecture/数据修复与清场SOP.md`：只读复现 → 行级快照 → 幂等 DML
-- → 对平 → 巡检 → 留档。本阶段只写 `datafix_bizperm_snapshot`（审计表），
-- 不碰 sys_role / sys_role_menu / sys_menu / sys_user。
--
-- 快照四类（batch = BIZPERM-20261105 / -ROLEMENU / -MENU / -USER）：
--   sys_role       5 行  —— 证明 data_scope 全程未被改动（本卡不动数据范围）
--   sys_role_menu  N 行  —— 5 个角色改动前的全部授权行（对平基准）
--   sys_menu       M 行  —— 计划命中的节点 + 记账凭证页（证明菜单内容未被改，
--                           除了本卡主动新建的 3 条凭证按钮码）
--   sys_user       K 行  —— 持有这 5 个角色的账号（token_version 改动前镜像）
-- ============================================================================
\set ON_ERROR_STOP on

\i plan.sql
\i expand.sql

CREATE TABLE IF NOT EXISTS datafix_bizperm_snapshot (
  batch       text        NOT NULL,
  tbl         text        NOT NULL,
  pk          text        NOT NULL,
  row_data    jsonb       NOT NULL,
  captured_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (batch, tbl, pk)
);

DO $$
DECLARE
  v_base   text := 'BIZPERM-20261105';
  v_roles  int;
  v_rm     int;
  v_menu   int;
  v_user   int;
  c_biz CONSTANT text[] := ARRAY['DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER'];
BEGIN
  -- ① 5 个业务角色必须在位（本卡只补码，不建角色、不改 data_scope）
  SELECT count(*) INTO v_roles FROM sys_role WHERE role_code = ANY (c_biz) AND deleted_at IS NULL;
  IF v_roles <> 5 THEN
    RAISE EXCEPTION '前置失败：5 个业务角色只有 % 个在位（期望 5）', v_roles;
  END IF;

  -- ② 目标态不变量：SUPER_ADMIN 成员必须仍是 1（admin）。快照阶段只断言、不修改
  SELECT count(*) INTO v_user
    FROM sys_user_role ur JOIN sys_role r ON r.id = ur.role_id
   WHERE r.role_code = 'SUPER_ADMIN';
  IF v_user <> 1 THEN
    RAISE EXCEPTION '前置失败：SUPER_ADMIN 成员 % 个（目标态 = 1，只有 admin）', v_user;
  END IF;

  -- ③ sys_role（5 行）
  INSERT INTO datafix_bizperm_snapshot (batch, tbl, pk, row_data)
  SELECT v_base || '-ROLES', 'sys_role', r.id::text, to_jsonb(r)
    FROM sys_role r WHERE r.role_code = ANY (c_biz) AND r.deleted_at IS NULL
  ON CONFLICT DO NOTHING;

  -- ④ sys_role_menu（5 个角色的全部授权行 = 对平基准）
  INSERT INTO datafix_bizperm_snapshot (batch, tbl, pk, row_data)
  SELECT v_base || '-ROLEMENU', 'sys_role_menu', rm.id::text, to_jsonb(rm)
    FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id
   WHERE r.role_code = ANY (c_biz)
  ON CONFLICT DO NOTHING;

  -- ⑤ sys_menu（计划命中的节点 + 凭证页 + 3 条待建按钮码的父页）
  INSERT INTO datafix_bizperm_snapshot (batch, tbl, pk, row_data)
  SELECT DISTINCT v_base || '-MENU', 'sys_menu', m.id::text, to_jsonb(m)
    FROM sys_menu m
   WHERE m.id IN (SELECT id FROM _sel WHERE id IN (SELECT id FROM sys_menu))
      OR m.id = (SELECT id FROM sys_menu WHERE path = '/business/erp/voucher' AND menu_type = 2 AND deleted_at IS NULL)
  ON CONFLICT DO NOTHING;

  -- ⑥ sys_user（这 5 个角色的持有者；token_version 改动前镜像）
  INSERT INTO datafix_bizperm_snapshot (batch, tbl, pk, row_data)
  SELECT DISTINCT v_base || '-USER', 'sys_user', u.id::text, to_jsonb(u)
    FROM sys_user u
    JOIN sys_user_role ur ON ur.user_id = u.id
    JOIN sys_role r ON r.id = ur.role_id
   WHERE r.role_code = ANY (c_biz)
  ON CONFLICT DO NOTHING;

  -- ⑦ 快照完整性断言
  SELECT count(*) INTO v_rm   FROM datafix_bizperm_snapshot WHERE batch = v_base || '-ROLEMENU';
  SELECT count(*) INTO v_menu FROM datafix_bizperm_snapshot WHERE batch = v_base || '-MENU';
  SELECT count(*) INTO v_user FROM datafix_bizperm_snapshot WHERE batch = v_base || '-USER';
  IF v_rm = 0 OR v_menu = 0 THEN
    RAISE EXCEPTION '快照失败：role_menu % 行 / menu % 行（都不能为 0）', v_rm, v_menu;
  END IF;

  RAISE NOTICE '快照完成：sys_role 5 行 / sys_role_menu % 行 / sys_menu % 行 / sys_user % 行',
    v_rm, v_menu, v_user;
END $$;

-- 快照目录（人肉可读）
SELECT batch, tbl, count(*) AS rows
  FROM datafix_bizperm_snapshot
 WHERE batch LIKE 'BIZPERM-20261105%'
 GROUP BY 1, 2 ORDER BY 1, 2;
