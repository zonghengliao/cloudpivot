-- ============================================================================
-- SUPERCONV（P0-FIX-2B-C）· 阶段 1：建 OPS_ADMIN 角色 + 授予 laoliu
-- ----------------------------------------------------------------------------
-- 本阶段**不摘任何绑定**：laoliu 在这一步之后仍是 SUPER_ADMIN，
-- 所以「角色建错 / 少授权」不会造成任何不可运维窗口。摘权在 20_ 里做。
--
-- 幂等：重放 0 行变更
--   * 角色：固定 id + ON CONFLICT (id) DO NOTHING；若同 code 但不同 id → 抛异常停手
--   * 授权：唯一约束 (role_id, menu_id) + ON CONFLICT DO NOTHING
--   * 绑定：唯一约束 (user_id, role_id) + ON CONFLICT DO NOTHING
--
-- 口径（为什么是「sys_menu 里所有 system:/support: 菜单」而不是「SUPER_ADMIN 的 274 个码里取前缀」）：
--   生产实测（2026-11-05 16:04 dump）：SUPER_ADMIN 只有 30 行 sys_role_menu，
--   其中 system:* 26 行 + support:* 4 行 = 30 行，**全部是 change-log/field-mask/help/
--   preference/role-mutex/dashboard/invariant**；`system:user:*`、`system:role:*`、
--   `system:dept:*`、`system:menu:*`、`support:dict:*`、`support:config:*`、`support:file:*`、
--   `support:log:*`、`support:online:*`、`support:job:*` 在它名下**一行都没有**
--   ——因为 SUPER_ADMIN 走的是「通配短路」（`TokenClaims::is_super_admin()`），不靠授权行。
--   照卡面字面取「SUPER_ADMIN 的 system/support 子集」，OPS_ADMIN 会拿到 30 行、
--   且一个用户管理码都没有 ⇒ laoliu 摘超管后当场不可运维，直接违反 C3。
--   故按卡面**语义**（用户/角色/菜单/部门/字典/配置/文件/日志/在线用户/定时任务）取
--   `sys_menu` 全量的 system:* + support:*（70 行 / 65 个码）+ 两个目录节点 = 72 行。
-- ============================================================================
\set ON_ERROR_STOP on

-- ---------------------------------------------------------------- ① 快照表
-- 形状照 docs/architecture/数据修复与清场SOP.md §1（批次一张表，pk 用 text）
CREATE TABLE IF NOT EXISTS datafix_superconv_snapshot (
  batch       text        NOT NULL,
  tbl         text        NOT NULL,
  pk          text        NOT NULL,
  row_data    jsonb       NOT NULL,
  captured_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (batch, tbl, pk)
);

-- ------------------------------------------------- ② 前置断言（不满足就整段回滚）
DO $$
DECLARE
  v_role_id   uuid := '20000000-0000-4000-8000-0000000f0016';
  v_laoliu    uuid;
  v_admin_sup int;
  v_n         int;
  v_menus     int;
  v_codes     int;
BEGIN
  -- 账号必须在（不建号）
  SELECT id INTO v_laoliu FROM sys_user WHERE username = 'laoliu' AND deleted_at IS NULL;
  IF v_laoliu IS NULL THEN
    RAISE EXCEPTION '前置失败：找不到账号 laoliu';
  END IF;

  -- admin 必须持有 SUPER_ADMIN：它是 D-2 的唯一真超管，缺了说明库被人动过
  SELECT count(*) INTO v_admin_sup
    FROM sys_user_role ur
    JOIN sys_user u ON u.id = ur.user_id
    JOIN sys_role r ON r.id = ur.role_id
   WHERE u.username = 'admin' AND r.role_code = 'SUPER_ADMIN'
     AND r.deleted_at IS NULL AND (ur.expire_time IS NULL OR ur.expire_time > now());
  IF v_admin_sup <> 1 THEN
    RAISE EXCEPTION '前置失败：admin 不是 SUPER_ADMIN 成员（期望 1 行，实际 %）', v_admin_sup;
  END IF;

  -- 同名角色但不同 id：不许静默续用，停手交人判断
  SELECT count(*) INTO v_n FROM sys_role
   WHERE role_code = 'OPS_ADMIN' AND deleted_at IS NULL AND id <> v_role_id;
  IF v_n > 0 THEN
    RAISE EXCEPTION '前置失败：已存在 role_code=OPS_ADMIN 但 id 不是 %，停手', v_role_id;
  END IF;

  -- 候选菜单集不能是空的（菜单被清过就别静默建一个空壳角色）
  SELECT count(*) INTO v_menus FROM sys_menu
   WHERE deleted_at IS NULL AND status = 1
     AND (auth_code LIKE 'system:%' OR auth_code LIKE 'support:%'
          OR id IN ('10000000-0000-0000-0000-000000000001',
                    '10000000-0000-0000-0000-000000000022'));
  IF v_menus <> 72 THEN
    RAISE EXCEPTION '前置失败：OPS_ADMIN 候选菜单期望 72 行（70 条 system/support + 2 个目录），实际 % 行', v_menus;
  END IF;
END $$;

-- ------------------------------------------------------------ ③ 建角色
INSERT INTO sys_role (id, role_code, role_name, description, status, sort_order, remark, data_scope)
VALUES (
  '20000000-0000-4000-8000-0000000f0016',
  'OPS_ADMIN',
  '运维管理员',
  'P0-FIX-2B-C 决策 D-2：平台/系统面运维角色（system:* + support:*），不含业务/ERP/流程配置码',
  1,
  5,
  E'运维管理员（P0-FIX-2B-C 决策 D-2）。权限码 = sys_menu 全量 system:* + support:*（用户/角色/菜单/部门/字典/配置/文件/日志/在线用户/定时任务），不含 business:* / erp:* / flow:*。\n【data_scope=1（全部）是刻意的】用户管理要看到全量 sys_user，而 sys_user 本身是受行级范围控制的表（shared/scope.rs::SCOPED_TABLES 之一），给 3/4 档会让运维看不到自己要管的人。\n【业务数据的边界靠权限码收，不靠行级范围收】——本角色没有任何业务权限码，所以 data_scope=1 不等于「运维能看全部业务数据」；不要因为这一格把范围改小（改了用户管理会瞎），也不要因为这一格给本角色加业务码。',
  1
)
ON CONFLICT (id) DO NOTHING;

-- ------------------------------------------------------------ ④ 授权
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), '20000000-0000-4000-8000-0000000f0016', m.id
  FROM sys_menu m
 WHERE m.deleted_at IS NULL AND m.status = 1
   AND (m.auth_code LIKE 'system:%' OR m.auth_code LIKE 'support:%'
        OR m.id IN ('10000000-0000-0000-0000-000000000001',
                    '10000000-0000-0000-0000-000000000022'))
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ------------------------------------------------------------ ⑤ 绑定 laoliu
-- ⑤a OPS_ADMIN（本批次新建）
INSERT INTO sys_user_role (id, user_id, role_id)
SELECT '5c0e0000-0000-4000-8000-000000000001', u.id, '20000000-0000-4000-8000-0000000f0016'
  FROM sys_user u
 WHERE u.username = 'laoliu' AND u.deleted_at IS NULL
ON CONFLICT (user_id, role_id) DO NOTHING;

-- ⑤b GENERAL_MANAGER（卡面目标态：laoliu = GENERAL_MANAGER + OPS_ADMIN）
--   今天 laoliu 只挂 SUPER_ADMIN 一个角色 —— 摘了超管他会连「总经理」这一档也没有，
--   而 payable_pay.n_board 的会签候选是 ROLE{FINANCE_DIRECTOR, GENERAL_MANAGER}（B 卡已改），
--   所以这一档必须在**摘权之前**补上，否则摘权当天审批人会掉一个。
INSERT INTO sys_user_role (id, user_id, role_id)
SELECT '5c0e0000-0000-4000-8000-000000000002', u.id, '20000000-0000-4000-8000-0000000f0003'
  FROM sys_user u
 WHERE u.username = 'laoliu' AND u.deleted_at IS NULL
ON CONFLICT (user_id, role_id) DO NOTHING;

-- ------------------------------------------- ⑥ 授权后硬断言（形状 + C3 必需码）
DO $$
DECLARE
  v_role_id uuid := '20000000-0000-4000-8000-0000000f0016';
  v_menus   int;
  v_codes   int;
  v_scope   smallint;
  v_missing text;
  v_bound   int;
BEGIN
  SELECT data_scope INTO v_scope FROM sys_role WHERE id = v_role_id AND deleted_at IS NULL;
  IF v_scope IS DISTINCT FROM 1 THEN
    RAISE EXCEPTION '授权后断言失败：OPS_ADMIN.data_scope 期望 1，实际 %', v_scope;
  END IF;

  SELECT count(*) INTO v_menus FROM sys_role_menu WHERE role_id = v_role_id;
  IF v_menus <> 72 THEN
    RAISE EXCEPTION '授权后断言失败：OPS_ADMIN 菜单行数期望 72，实际 %', v_menus;
  END IF;

  SELECT count(DISTINCT m.auth_code) INTO v_codes
    FROM sys_role_menu rm JOIN sys_menu m ON m.id = rm.menu_id
   WHERE rm.role_id = v_role_id AND m.auth_code <> '';
  IF v_codes <> 65 THEN
    RAISE EXCEPTION '授权后断言失败：OPS_ADMIN 权限码期望 65 个，实际 %', v_codes;
  END IF;

  -- C3 清单逐条点名：少一个就停手（菜单改名/被删会在这里炸，而不是上线后才发现）
  SELECT string_agg(code, ', ') INTO v_missing
    FROM (VALUES ('system:user:list'), ('system:user:update'),
                 ('system:role:list'), ('system:role:menu'),
                 ('system:dept:list'), ('system:menu:list'),
                 ('support:config:list'), ('support:config:create'),
                 ('support:log:list'), ('support:dict:list'),
                 ('support:file:list'), ('support:online:list'),
                 ('support:job:list')) AS t(code)
   WHERE NOT EXISTS (
           SELECT 1 FROM sys_role_menu rm JOIN sys_menu m ON m.id = rm.menu_id
            WHERE rm.role_id = v_role_id AND m.auth_code = t.code);
  IF v_missing IS NOT NULL THEN
    RAISE EXCEPTION '授权后断言失败：OPS_ADMIN 缺少 C3 必需权限码：%', v_missing;
  END IF;

  SELECT count(*) INTO v_bound
    FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
   WHERE ur.role_id = v_role_id AND u.username = 'laoliu';
  IF v_bound <> 1 THEN
    RAISE EXCEPTION '授权后断言失败：laoliu 未绑定 OPS_ADMIN（期望 1 行，实际 %）', v_bound;
  END IF;

  -- ⑤b 也断言：laoliu 必须同时持 GENERAL_MANAGER（摘超管后审批面靠它）
  IF NOT EXISTS (SELECT 1 FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
                  JOIN sys_role r ON r.id = ur.role_id
                  WHERE u.username = 'laoliu' AND r.role_code = 'GENERAL_MANAGER') THEN
    RAISE EXCEPTION '授权后断言失败：laoliu 未绑定 GENERAL_MANAGER';
  END IF;

  RAISE NOTICE 'OPS_ADMIN 就绪：菜单 % 行 / 权限码 % 个 / laoliu 已绑定（本阶段未摘任何超管绑定）', v_menus, v_codes;
END $$;
