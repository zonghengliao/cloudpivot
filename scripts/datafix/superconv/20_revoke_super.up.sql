-- ============================================================================
-- SUPERCONV（P0-FIX-2B-C）· 阶段 2：摘超管
-- ----------------------------------------------------------------------------
-- 动 3 行 sys_user_role（判据见下），另外推进 2 个账号的 token_version：
--   laoliu_test → 删 SUPER_ADMIN + GENERAL_MANAGER（保留 DEPT_LEADER /
--                 PURCHASE_MANAGER / WAREHOUSE_MANAGER 三个 3 档业务角色）
--   laoliu      → 删 SUPER_ADMIN（保留 GENERAL_MANAGER + 新增 OPS_ADMIN，
--                 由 10_ 先建好；本脚本开头硬断言 OPS_ADMIN 已在位）
--   henry.liao  → 本脚本**不动**（待老大二选一；那一支由 30_ 单独处理）
--
-- 为什么删 GENERAL_MANAGER：它的 data_scope=1（全部）。只摘 SUPER_ADMIN 而留着它，
--   等于留一个「摘了超管但还能看全公司」的后门。摘完后 laoliu_test 的范围 =
--   DEPT_LEADER/PURCHASE_MANAGER/WAREHOUSE_MANAGER 的 3 档（TECH 子树，A 卡已落位）。
--
-- 为什么同时推进 token_version（不是卡面要求的，但不做则摘权不生效）：
--   路由闸 `#[nik::permission]` 只认**令牌里的权限码快照**（nik-web has_permission）。
--   直改库不走 app 的授权写点，没人推进 token_version ⇒ 他手上已签发的令牌在有效期内
--   仍然带着全部 274 个码。app 自己改角色授权时就是这么做的
--   （system/user/model.rs::bump_token_version_of_role_holders_in），本脚本照同一口径补偿。
--
-- 幂等：重放 0 行变更（目标行数 0 → 只打 NOTICE）。
-- ============================================================================
\set ON_ERROR_STOP on

CREATE TABLE IF NOT EXISTS datafix_superconv_snapshot (
  batch       text        NOT NULL,
  tbl         text        NOT NULL,
  pk          text        NOT NULL,
  row_data    jsonb       NOT NULL,
  captured_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (batch, tbl, pk)
);

DO $$
DECLARE
  v_batch   text := 'SUPERCONV-20261105';
  v_ops     uuid := '20000000-0000-4000-8000-0000000f0016';
  v_tgt     int;
  v_snap    int;
  v_del     int;
  v_cnt     int;
  v_codes   int;
  v_d       text;
BEGIN
  -- --------------------------------------------------- ① 目标行数（严进）
  SELECT count(*) INTO v_tgt
    FROM sys_user_role ur
    JOIN sys_user u ON u.id = ur.user_id
    JOIN sys_role r ON r.id = ur.role_id
   WHERE (u.username = 'laoliu_test' AND r.role_code IN ('SUPER_ADMIN', 'GENERAL_MANAGER'))
      OR (u.username = 'laoliu'     AND r.role_code = 'SUPER_ADMIN');

  IF v_tgt = 0 THEN
    RAISE NOTICE '已摘过（目标 0 行），本脚本无变更';
    RETURN;
  END IF;
  IF v_tgt <> 3 THEN
    RAISE EXCEPTION '目标行数期望 3（laoliu_test: SUPER_ADMIN+GENERAL_MANAGER 两行 + laoliu: SUPER_ADMIN 一行，实际 %），形状不对，停手', v_tgt;
  END IF;

  -- 形状复核：逐 (账号, 角色) 点一遍，多一条少一条都不许
  SELECT string_agg(fmt, ', ') INTO v_d FROM (
    SELECT u.username || '/' || r.role_code AS fmt
      FROM sys_user_role ur
      JOIN sys_user u ON u.id = ur.user_id
      JOIN sys_role r ON r.id = ur.role_id
     WHERE (u.username = 'laoliu_test' AND r.role_code IN ('SUPER_ADMIN', 'GENERAL_MANAGER'))
        OR (u.username = 'laoliu'     AND r.role_code = 'SUPER_ADMIN')
     ORDER BY 1) t;
  RAISE NOTICE '待摘绑定：%', v_d;

  -- ------------------------------------------- ② 摘权前置：OPS_ADMIN 必须已在位
  SELECT count(DISTINCT m.auth_code) INTO v_codes
    FROM sys_role_menu rm JOIN sys_menu m ON m.id = rm.menu_id
   WHERE rm.role_id = v_ops AND m.auth_code <> '';
  IF v_codes <> 65 THEN
    RAISE EXCEPTION '摘权前置失败：OPS_ADMIN 权限码 % 个（期望 65）→ 先跑 10_grant_ops_admin.up.sql', v_codes;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
                  WHERE ur.role_id = v_ops AND u.username = 'laoliu') THEN
    RAISE EXCEPTION '摘权前置失败：laoliu 未绑定 OPS_ADMIN → 摘了就不可运维';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
                  JOIN sys_role r ON r.id = ur.role_id
                  WHERE u.username = 'laoliu' AND r.role_code = 'GENERAL_MANAGER') THEN
    RAISE EXCEPTION '摘权前置失败：laoliu 不持 GENERAL_MANAGER（会签/审批面会掉），停手';
  END IF;

  -- --------------------------------------------------------- ③ 快照（先于 DML）
  INSERT INTO datafix_superconv_snapshot (batch, tbl, pk, row_data)
  SELECT v_batch, 'sys_user_role', ur.id::text, to_jsonb(ur)
    FROM sys_user_role ur
    JOIN sys_user u ON u.id = ur.user_id
    JOIN sys_role r ON r.id = ur.role_id
   WHERE (u.username = 'laoliu_test' AND r.role_code IN ('SUPER_ADMIN', 'GENERAL_MANAGER'))
      OR (u.username = 'laoliu'     AND r.role_code = 'SUPER_ADMIN')
  ON CONFLICT DO NOTHING;

  INSERT INTO datafix_superconv_snapshot (batch, tbl, pk, row_data)
  SELECT v_batch, 'sys_user', u.id::text, to_jsonb(u)
    FROM sys_user u WHERE u.username IN ('laoliu', 'laoliu_test')
  ON CONFLICT DO NOTHING;

  SELECT count(*) INTO v_snap FROM datafix_superconv_snapshot
   WHERE batch = v_batch AND tbl = 'sys_user_role';
  IF v_snap <> v_tgt THEN
    RAISE EXCEPTION '快照 % 行 ≠ 目标 % 行，停止修数（SOP §1 红线）', v_snap, v_tgt;
  END IF;

  -- ------------------------------------------------------------- ④ 摘权
  DELETE FROM sys_user_role ur
   USING sys_user u, sys_role r
   WHERE ur.user_id = u.id AND ur.role_id = r.id
     AND ((u.username = 'laoliu_test' AND r.role_code IN ('SUPER_ADMIN', 'GENERAL_MANAGER'))
       OR (u.username = 'laoliu'     AND r.role_code = 'SUPER_ADMIN'));
  GET DIAGNOSTICS v_del = ROW_COUNT;
  IF v_del <> 3 THEN
    RAISE EXCEPTION '删除行数 % ≠ 3，本事务回滚', v_del;
  END IF;

  -- ⑤ 令牌失效（app 改角色授权的同一口径；不做则旧令牌在有效期内仍是超管）
  UPDATE sys_user SET token_version = token_version + 1, updated_at = now()
   WHERE username IN ('laoliu', 'laoliu_test') AND deleted_at IS NULL;

  -- ------------------------------------------------------- ⑥ 摘权后硬断言
  -- ⑥.1 超管集合只允许剩下 admin（+henry.liao：本卡未获确认时原样保留）
  SELECT count(*) INTO v_cnt
    FROM sys_user_role ur JOIN sys_role r ON r.id = ur.role_id
   WHERE r.role_code = 'SUPER_ADMIN';
  IF v_cnt > 2 THEN
    RAISE EXCEPTION '摘权后断言失败：SUPER_ADMIN 仍有 % 个成员（期望 1 或 2=admin+henry.liao）', v_cnt;
  END IF;
  IF EXISTS (SELECT 1 FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
              JOIN sys_role r ON r.id = ur.role_id
              WHERE r.role_code = 'SUPER_ADMIN' AND u.username IN ('laoliu', 'laoliu_test')) THEN
    RAISE EXCEPTION '摘权后断言失败：laoliu/laoliu_test 仍持 SUPER_ADMIN';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
                  JOIN sys_role r ON r.id = ur.role_id
                  WHERE r.role_code = 'SUPER_ADMIN' AND u.username = 'admin') THEN
    RAISE EXCEPTION '摘权后断言失败：admin 丢了 SUPER_ADMIN（D-2 的唯一真超管）';
  END IF;

  -- ⑥.2 laoliu_test 剩下必须是那 3 个 3 档角色（多/少都停）
  SELECT count(*) INTO v_cnt
    FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
   WHERE u.username = 'laoliu_test';
  IF v_cnt <> 3 THEN
    RAISE EXCEPTION '摘权后断言失败：laoliu_test 角色数 % （期望 3）', v_cnt;
  END IF;
  IF EXISTS (SELECT 1 FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
              JOIN sys_role r ON r.id = ur.role_id
              WHERE u.username = 'laoliu_test' AND r.data_scope = 1) THEN
    RAISE EXCEPTION '摘权后断言失败：laoliu_test 仍有 data_scope=1 的角色（全公司可见后门）';
  END IF;

  -- ⑥.3 laoliu 剩下 = GENERAL_MANAGER + OPS_ADMIN
  SELECT count(*) INTO v_cnt
    FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
   WHERE u.username = 'laoliu';
  IF v_cnt <> 2 THEN
    RAISE EXCEPTION '摘权后断言失败：laoliu 角色数 % （期望 2 = GENERAL_MANAGER + OPS_ADMIN）', v_cnt;
  END IF;

  RAISE NOTICE '摘权完成：删 % 行绑定；SUPER_ADMIN 成员 % 个；laoliu_test 保留 3 个业务角色；laoliu = GM + OPS_ADMIN', v_del, v_cnt;
END $$;
