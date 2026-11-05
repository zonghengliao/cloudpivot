-- ============================================================================
-- SUPERCONV（P0-FIX-2B-C）· 阶段 2：摘超管（4 行绑定 → 目标 = 只剩 admin 一个真超管）
-- ----------------------------------------------------------------------------
-- 授权（两句话都齐了）：
--   · 卡面评论 `laoliu` 2026-11-05 16:36「确认，同意」→ 同意摘 laoliu_test / laoliu 的超管
--   · 卡面评论 `laoliu` 2026-11-05 16:45「henry.liao 是我建的自己的测试号。可以调整」
--
-- 动 4 行 sys_user_role：
--   laoliu_test → 删 SUPER_ADMIN + GENERAL_MANAGER（保留 DEPT_LEADER / PURCHASE_MANAGER /
--                 WAREHOUSE_MANAGER 三个 3 档业务角色；A 卡已把他落在 TECH）
--   laoliu      → 删 SUPER_ADMIN（保留 10_ 补的 5 个业务角色）
--   henry.liao  → 删 SUPER_ADMIN（保留 DEPT_LEADER / GENERAL_MANAGER / PURCHASE_MANAGER /
--                 WAREHOUSE_MANAGER 四个业务角色；05_ 已把他落在 HQ）
--   admin       → 不动（D-2 的唯一真超管）
--
-- 为什么 laoliu_test 必须连 GENERAL_MANAGER 一起摘：它 data_scope=1（全部）。只摘超管留着它
--   = 留下「摘了超管但还能看全公司」的后门。摘完之后他的范围 = 三个 3 档角色 + TECH 部门
--   = TECH 子树（C2）。henry.liao 保留 GENERAL_MANAGER 是有意的：他是老板自己的测试号，
--   需要「总经理审批视角」；且落位 HQ 之后 data_scope=1 与 3 档在 HQ 的并集一样是 All
--   —— 不影响任何收敛目标（超管是权限码通配，不是数据范围）。
--
-- 为什么同时推进 token_version：路由闸 `#[nik::permission]` 只认**令牌里的权限码快照**。
--   直改库不走 app 的授权写点，不推进版本号 ⇒ 他手上已签发的令牌在有效期内仍带着全部权限码。
--   app 自己改角色授权时就是这么做的（system/user/model.rs::bump_token_version_of_role_holders_in）。
--   本卡另外要求 bump `perm:user_scope_epoch` + 删 `perm:user_perms:*`（见 README §缓存失效）。
--
-- 幂等：4 行全已删 ⇒ NOTICE 返回（0 行变更）；只删掉一部分 ⇒ 抛异常停手。
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
  v_batch text := 'SUPERCONV-20261105';
  v_tgt     int;
  v_snap    int;
  v_del     int;
  v_cnt     int;
  v_d       text;
  c_biz CONSTANT text[] := ARRAY['DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER'];
BEGIN
  -- --------------------------------------------------- ① 目标行形状（严进）
  SELECT count(*) INTO v_tgt
    FROM sys_user_role ur
    JOIN sys_user u ON u.id = ur.user_id
    JOIN sys_role r ON r.id = ur.role_id
   WHERE (u.username = 'laoliu_test' AND r.role_code IN ('SUPER_ADMIN', 'GENERAL_MANAGER'))
      OR (u.username = 'laoliu'      AND r.role_code = 'SUPER_ADMIN')
      OR (u.username = 'henry.liao'  AND r.role_code = 'SUPER_ADMIN');

  IF v_tgt = 0 THEN
    RAISE NOTICE '已摘过（目标 0 行），本脚本无变更（幂等重放）';
    RETURN;
  END IF;
  IF v_tgt <> 4 THEN
    RAISE EXCEPTION '目标行数期望 4（laoliu_test: SUPER_ADMIN+GENERAL_MANAGER、laoliu: SUPER_ADMIN、henry.liao: SUPER_ADMIN），实际 %，形状不对，停手', v_tgt;
  END IF;

  SELECT string_agg(fmt, ', ' ORDER BY fmt) INTO v_d FROM (
    SELECT u.username || '/' || r.role_code AS fmt
      FROM sys_user_role ur
      JOIN sys_user u ON u.id = ur.user_id
      JOIN sys_role r ON r.id = ur.role_id
     WHERE (u.username = 'laoliu_test' AND r.role_code IN ('SUPER_ADMIN', 'GENERAL_MANAGER'))
        OR (u.username = 'laoliu'      AND r.role_code = 'SUPER_ADMIN')
        OR (u.username = 'henry.liao'  AND r.role_code = 'SUPER_ADMIN')) t;
  RAISE NOTICE '待摘绑定：%', v_d;

  -- --------------------------------------- ② 摘权前置 1：laoliu 的业务角色在位
  SELECT count(*) INTO v_cnt
    FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id JOIN sys_role r ON r.id = ur.role_id
   WHERE u.username = 'laoliu' AND r.role_code = ANY (c_biz);
  IF v_cnt <> 5 THEN
    RAISE EXCEPTION '摘权前置失败：laoliu 只有 % 个业务角色（期望 5）→ 先跑 10_grant_laoliu_roles.up.sql', v_cnt;
  END IF;

  -- --------------------------------------- ③ 摘权前置 2：每个人都有部门（否则 S10/S11 降级 SelfOnly）
  SELECT count(*) INTO v_cnt FROM sys_user
   WHERE username IN ('laoliu', 'laoliu_test', 'henry.liao') AND deleted_at IS NULL AND dept_id IS NOT NULL;
  IF v_cnt <> 3 THEN
    RAISE EXCEPTION '摘权前置失败：三个账号里有 % 个没落部门（期望都有）→ 先跑 A 卡的落位与 05_land_henry_hq.up.sql', 3 - v_cnt;
  END IF;

  -- --------------------------------------- ④ 摘权前置 3：保留角色形状对得上
  SELECT count(*) INTO v_cnt
    FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id JOIN sys_role r ON r.id = ur.role_id
   WHERE u.username = 'laoliu_test'
     AND r.role_code IN ('DEPT_LEADER','PURCHASE_MANAGER','WAREHOUSE_MANAGER');
  IF v_cnt <> 3 THEN
    RAISE EXCEPTION '摘权前置失败：laoliu_test 的 3 档业务角色只有 % 个（期望 3）', v_cnt;
  END IF;
  SELECT count(*) INTO v_cnt
    FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id JOIN sys_role r ON r.id = ur.role_id
   WHERE u.username = 'henry.liao'
     AND r.role_code IN ('DEPT_LEADER','GENERAL_MANAGER','PURCHASE_MANAGER','WAREHOUSE_MANAGER');
  IF v_cnt <> 4 THEN
    RAISE EXCEPTION '摘权前置失败：henry.liao 的业务角色只有 % 个（期望 4）', v_cnt;
  END IF;

  -- --------------------------------------- ⑤ 摘权前置 4：admin 仍是超管 + 超管成员正好 4 个
  IF NOT EXISTS (
    SELECT 1 FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id JOIN sys_role r ON r.id = ur.role_id
     WHERE u.username = 'admin' AND r.role_code = 'SUPER_ADMIN'
       AND (ur.expire_time IS NULL OR ur.expire_time > now())
  ) THEN
    RAISE EXCEPTION '摘权前置失败：admin 不是 SUPER_ADMIN（D-2 的唯一真超管）';
  END IF;
  SELECT count(*) INTO v_cnt FROM sys_user_role ur JOIN sys_role r ON r.id = ur.role_id
   WHERE r.role_code = 'SUPER_ADMIN';
  IF v_cnt <> 4 THEN
    RAISE EXCEPTION '摘权前置失败：SUPER_ADMIN 当前成员 % 个（期望 4 = admin+laoliu+laoliu_test+henry.liao）', v_cnt;
  END IF;

  -- --------------------------------------------------------- ⑥ 快照（先于 DML）
  INSERT INTO datafix_superconv_snapshot (batch, tbl, pk, row_data)
  SELECT v_batch, 'sys_user_role', ur.id::text, to_jsonb(ur)
    FROM sys_user_role ur
    JOIN sys_user u ON u.id = ur.user_id
    JOIN sys_role r ON r.id = ur.role_id
   WHERE (u.username = 'laoliu_test' AND r.role_code IN ('SUPER_ADMIN', 'GENERAL_MANAGER'))
      OR (u.username = 'laoliu'      AND r.role_code = 'SUPER_ADMIN')
      OR (u.username = 'henry.liao'  AND r.role_code = 'SUPER_ADMIN')
  ON CONFLICT DO NOTHING;

  INSERT INTO datafix_superconv_snapshot (batch, tbl, pk, row_data)
  SELECT v_batch, 'sys_user', u.id::text, to_jsonb(u)
    FROM sys_user u WHERE u.username IN ('laoliu', 'laoliu_test', 'henry.liao') AND u.deleted_at IS NULL
  ON CONFLICT DO NOTHING;

  SELECT count(*) INTO v_snap FROM datafix_superconv_snapshot
   WHERE batch = v_batch AND tbl = 'sys_user_role';
  IF v_snap <> v_tgt THEN
    RAISE EXCEPTION '快照 % 行 ≠ 目标 % 行，停止修数（SOP §1 红线）', v_snap, v_tgt;
  END IF;

  -- ------------------------------------------------------------- ⑦ 摘权
  DELETE FROM sys_user_role ur
   USING sys_user u, sys_role r
   WHERE ur.user_id = u.id AND ur.role_id = r.id
     AND ((u.username = 'laoliu_test' AND r.role_code IN ('SUPER_ADMIN', 'GENERAL_MANAGER'))
       OR (u.username = 'laoliu'      AND r.role_code = 'SUPER_ADMIN')
       OR (u.username = 'henry.liao'  AND r.role_code = 'SUPER_ADMIN'));
  GET DIAGNOSTICS v_del = ROW_COUNT;
  IF v_del <> 4 THEN
    RAISE EXCEPTION '删除行数 % ≠ 4，本事务回滚', v_del;
  END IF;

  -- ⑧ 令牌失效（与 app 改角色授权的同一口径）
  UPDATE sys_user SET token_version = token_version + 1, updated_at = now()
   WHERE username IN ('laoliu', 'laoliu_test', 'henry.liao') AND deleted_at IS NULL;

  -- ------------------------------------------------------- ⑨ 摘权后硬断言
  -- ⑨.1 超管只剩 admin
  SELECT count(*) INTO v_cnt FROM sys_user_role ur JOIN sys_role r ON r.id = ur.role_id
   WHERE r.role_code = 'SUPER_ADMIN';
  IF v_cnt <> 1 THEN
    RAISE EXCEPTION '摘权后断言失败：SUPER_ADMIN 仍有 % 个成员（期望 1 = admin）', v_cnt;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
                  JOIN sys_role r ON r.id = ur.role_id
                  WHERE r.role_code = 'SUPER_ADMIN' AND u.username = 'admin') THEN
    RAISE EXCEPTION '摘权后断言失败：admin 丢了 SUPER_ADMIN';
  END IF;

  -- ⑨.2 laoliu_test = 3 个 3 档角色，且没有 data_scope=1 的后门
  SELECT count(*) INTO v_cnt FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
   WHERE u.username = 'laoliu_test';
  IF v_cnt <> 3 THEN
    RAISE EXCEPTION '摘权后断言失败：laoliu_test 角色数 %（期望 3）', v_cnt;
  END IF;
  IF EXISTS (SELECT 1 FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
              JOIN sys_role r ON r.id = ur.role_id
              WHERE u.username = 'laoliu_test' AND r.data_scope = 1) THEN
    RAISE EXCEPTION '摘权后断言失败：laoliu_test 仍有 data_scope=1 的角色（全公司可见后门）';
  END IF;

  -- ⑨.3 laoliu = 5 个业务角色
  SELECT count(*) INTO v_cnt FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
   WHERE u.username = 'laoliu';
  IF v_cnt <> 5 THEN
    RAISE EXCEPTION '摘权后断言失败：laoliu 角色数 %（期望 5）', v_cnt;
  END IF;

  -- ⑨.4 henry.liao = 4 个业务角色
  SELECT count(*) INTO v_cnt FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
   WHERE u.username = 'henry.liao';
  IF v_cnt <> 4 THEN
    RAISE EXCEPTION '摘权后断言失败：henry.liao 角色数 %（期望 4）', v_cnt;
  END IF;

  RAISE NOTICE '阶段 2 完成：删 % 行绑定；SUPER_ADMIN 成员 = admin（1 个）；三个账号 token_version 已推进', v_del;
END $$;
