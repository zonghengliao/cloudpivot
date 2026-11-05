-- ============================================================================
-- SUPERCONV（P0-FIX-2B-C）· 阶段 1：给 laoliu 补 5 个业务角色
-- ----------------------------------------------------------------------------
-- 授权：卡面评论 `laoliu` 2026-11-05 16:36（用户原话「确认，同意」）+ 16:45 补充批准。
--   用户批准的执行方案（**取代卡面的「OPS_ADMIN 单角色」方案**）：
--     laoliu 摘 SUPER_ADMIN + 补 5 个业务角色（覆盖业务页全集）
--       DEPT_LEADER 46 行 / FINANCE_MANAGER 56 行 / WAREHOUSE_MANAGER 18 行 /
--       PURCHASE_MANAGER 12 行 / GENERAL_MANAGER 12 行（行数含按钮）
--   用户同时要求「laoliu 不能再访问 SUPER_ADMIN 专属功能（系统设置里的高危项）」⇒
--   **不建 OPS_ADMIN**（那一版留在 `90_ops_admin_reserved.up.sql`，未执行）。
--   代价：摘权后 laoliu 不再有 /system/* 权限，生产运维账号只剩 admin（唯一超管）。
--   这一点已在卡上向 user 明说（见 evidence README §风险）。
--
-- 为什么先授后摘：授角色时他还是超管（通配短路），不会出现「两边都没有」的窗口；
--   摘权在 20_ 里做，且 20_ 把这个阶段的产出作为**硬前置**。
--
-- 幂等：5 行全在 ⇒ NOTICE 返回；部分在 ⇒ 抛异常停手（形状不明，交人判断）。
-- 快照：插入的 5 行写进 batch=SUPERCONV-20261105-ROLES，down 按 (user_id, role_id) 精确删除。
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
  v_batch text := 'SUPERCONV-20261105-ROLES';
  v_uid   uuid;
  v_st    int2;
  v_sup   int;
  v_have  int;
  v_ins   int;
  v_snap  int;
  v_codes int;
  v_orphan int;
  v_rec   record;
  -- 期望的角色形状（data_scope 决定数据范围，必须精确匹配）
  c_codes CONSTANT text[]   := ARRAY['DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER'];
  c_scope CONSTANT int[]    := ARRAY[3, 5, 3, 3, 1];
  c_ids   CONSTANT uuid[]   := ARRAY['7c1e0000-0000-4000-8000-0000000000b1'::uuid,
                                     '7c1e0000-0000-4000-8000-0000000000b2'::uuid,
                                     '7c1e0000-0000-4000-8000-0000000000b3'::uuid,
                                     '7c1e0000-0000-4000-8000-0000000000b4'::uuid,
                                     '7c1e0000-0000-4000-8000-0000000000b5'::uuid];
BEGIN
  SELECT u.id, u.status INTO v_uid, v_st FROM sys_user u
   WHERE u.username = 'laoliu' AND u.deleted_at IS NULL;
  IF v_uid IS NULL THEN
    RAISE EXCEPTION '前置失败：找不到账号 laoliu';
  END IF;
  IF v_st <> 1 THEN
    RAISE EXCEPTION '前置失败：laoliu status=%（期望 1=启用）', v_st;
  END IF;

  -- 起始形状：他必须还是超管（否则说明本卡已被执行过 / 被人动过）
  SELECT count(*) INTO v_sup
    FROM sys_user_role ur JOIN sys_role r ON r.id = ur.role_id
   WHERE ur.user_id = v_uid AND r.role_code = 'SUPER_ADMIN';
  IF v_sup = 0 THEN
    RAISE EXCEPTION '前置失败：laoliu 当前不是 SUPER_ADMIN —— 本脚本只能在「摘权前」跑';
  END IF;

  -- 角色形状：5 个角色都在、启用、data_scope 精确匹配
  FOR i IN 1..5 LOOP
    SELECT r.id, r.data_scope, r.status INTO v_rec
      FROM sys_role r WHERE r.role_code = c_codes[i] AND r.deleted_at IS NULL;
    IF NOT FOUND THEN
      RAISE EXCEPTION '前置失败：角色 % 不存在（或已软删）', c_codes[i];
    END IF;
    IF v_rec.status <> 1 THEN
      RAISE EXCEPTION '前置失败：角色 % status=%（期望 1）', c_codes[i], v_rec.status;
    END IF;
    IF v_rec.data_scope <> c_scope[i] THEN
      RAISE EXCEPTION '前置失败：角色 % data_scope=%（期望 %）—— 数据范围口径变了，停手', c_codes[i], v_rec.data_scope, c_scope[i];
    END IF;
  END LOOP;

  -- 已有绑定：0 行 = 正常首跑；5 行 = 幂等重放；1..4 行 = 形状不明
  SELECT count(*) INTO v_have
    FROM sys_user_role ur JOIN sys_role r ON r.id = ur.role_id
   WHERE ur.user_id = v_uid AND r.role_code = ANY (c_codes);
  IF v_have = 5 THEN
    RAISE NOTICE 'laoliu 已持全部 5 个业务角色，本脚本无变更（幂等重放）';
    RETURN;
  END IF;
  IF v_have <> 0 THEN
    RAISE EXCEPTION '前置失败：laoliu 已持有 % 个目标角色（期望 0 或 5），形状不明，停手', v_have;
  END IF;

  -- 授权面复核：并集业务码 ≥ 48（生产实测 48 business + 1 support = 49）
  SELECT count(DISTINCT m.auth_code) INTO v_codes
    FROM sys_role r
    JOIN sys_role_menu rm ON rm.role_id = r.id
    JOIN sys_menu m ON m.id = rm.menu_id
   WHERE r.role_code = ANY (c_codes) AND r.deleted_at IS NULL
     AND m.deleted_at IS NULL AND m.status = 1 AND m.auth_code <> '';
  IF v_codes < 48 THEN
    RAISE EXCEPTION '授权面不足：5 角色并集权限码 % 个（期望 ≥ 48）—— 摘权会把他变成瞎子，停手', v_codes;
  END IF;

  -- 菜单树复核：每个角色不允许出现「页面在、祖先目录不在」（否则侧边栏是孤儿）
  -- 阶段 1.5 负责把目录补齐；这里只校验，不改数据。
  SELECT count(*) INTO v_orphan
  FROM (
    WITH RECURSIVE tr AS (
      SELECT id FROM sys_role WHERE role_code = ANY (c_codes) AND deleted_at IS NULL
    ), seed AS (
      SELECT rm.role_id, m.id AS menu_id, m.parent_id
        FROM sys_role_menu rm JOIN sys_menu m ON m.id = rm.menu_id
       WHERE rm.role_id IN (SELECT id FROM tr) AND m.deleted_at IS NULL AND m.menu_type = 2
    ), up AS (
      SELECT s.role_id, s.parent_id AS node_id FROM seed s WHERE s.parent_id IS NOT NULL
      UNION
      SELECT u.role_id, m.parent_id FROM up u JOIN sys_menu m ON m.id = u.node_id
       WHERE m.parent_id IS NOT NULL AND m.deleted_at IS NULL
    )
    SELECT u.role_id, u.node_id
      FROM up u JOIN sys_menu d ON d.id = u.node_id AND d.deleted_at IS NULL AND d.status = 1
     WHERE d.menu_type = 1
       AND NOT EXISTS (SELECT 1 FROM sys_role_menu x WHERE x.role_id = u.role_id AND x.menu_id = u.node_id)
  ) o;
  IF v_orphan > 0 THEN
    RAISE EXCEPTION '菜单树有 % 处「页面缺祖先目录」—— 先跑 15_grant_menu_dirs.up.sql，否则侧边栏会是空的', v_orphan;
  END IF;

  -- 插入 5 行绑定（固定 id，便于审计与人工核对）
  INSERT INTO sys_user_role (id, user_id, role_id, expire_time, created_at)
  SELECT c_ids[i], v_uid, r.id, NULL, now()
    FROM generate_series(1, 5) AS i
    JOIN sys_role r ON r.role_code = c_codes[i] AND r.deleted_at IS NULL
  ON CONFLICT (user_id, role_id) DO NOTHING;
  GET DIAGNOSTICS v_ins = ROW_COUNT;
  IF v_ins <> 5 THEN
    RAISE EXCEPTION '插入命中 % 行（期望 5），本事务回滚', v_ins;
  END IF;

  INSERT INTO datafix_superconv_snapshot (batch, tbl, pk, row_data)
  SELECT v_batch, 'sys_user_role', ur.id::text, to_jsonb(ur)
    FROM sys_user_role ur
   WHERE ur.user_id = v_uid AND ur.id = ANY (c_ids)
  ON CONFLICT DO NOTHING;
  SELECT count(*) INTO v_snap FROM datafix_superconv_snapshot WHERE batch = v_batch AND tbl = 'sys_user_role';
  IF v_snap <> 5 THEN
    RAISE EXCEPTION '快照 % 行 ≠ 5 行（SOP §1 红线）', v_snap;
  END IF;

  RAISE NOTICE '阶段 1 完成：laoliu 绑定 5 个业务角色；并集权限码 % 个；快照 5 行', v_codes;
END $$;
