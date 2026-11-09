-- ============================================================================
-- 10_svc_selfcheck.up.sql —— 建「最小权限自查账号」svc_selfcheck + 只读角色
--
-- 卡：t_882d48c8（替掉借用的真人账号 henry.liao）
-- 为什么必须 HUMAN：`system/auth/handler.rs` → `ai_actor::login_forbidden`
--   禁止 `user_type='ROBOT'` 走网页登录；ROBOT 还与 `sys_ai_agent` 有不变式。
--
-- 幂等：重复执行不再插入任何行（apply.sh 打印 Δ=0）；已存在的账号**不覆盖口令**，
--   除非显式 `:svc_rotate = '1'`（改口令会让 env 里的 CLOUDPIVOT_OPS_PASSWORD 立刻失效）。
-- 原子：整篇在一个事务里；任何断言不成立 → RAISE EXCEPTION → 整笔回滚，绝不留半套权限。
--
-- 不要直接 psql -f 跑：用它上层的 apply.sh（负责生成 argon2id 哈希、备份、改 env、打证据）。
-- 手动跑的口径见 README.md §3。
--
-- 两个 psql 变量（apply.sh 注入）：
--   :svc_pwhash  argon2id PHC 串（$argon2id$v=19$m=…,t=…,p=…$salt$hash）
--   :svc_rotate  '1' = 连口令一起写；'0' = 只补角色/授权（已存在账号不动口令）
-- ============================================================================
\set ON_ERROR_STOP on

BEGIN;

-- psql 不做 dollar-quote 体内的变量替换，所以先把入参落到临时表（DO 块里再读）
CREATE TEMP TABLE _svc_input (pwhash text, rotate boolean) ON COMMIT DROP;
INSERT INTO _svc_input (pwhash, rotate) VALUES (:'svc_pwhash', :'svc_rotate' = '1');

DO $$
DECLARE
  v_hash        text;
  v_rotate      boolean;
  v_dept        uuid;
  v_role_id     uuid;
  v_user_id     uuid;
  v_menu_id     uuid;
  v_path        text;
  v_hits        int;
  v_n           int;
  v_bad         text;
BEGIN
  SELECT pwhash, rotate INTO v_hash, v_rotate FROM _svc_input;

  -- ① 入参自检：只接受 argon2id PHC（拒绝明文/短串被误写成口令）
  IF v_hash IS NULL
     OR v_hash !~ '^\$argon2id\$v=19\$m=[0-9]+,t=[0-9]+,p=[0-9]+\$[A-Za-z0-9+/=]+\$[A-Za-z0-9+/=]+$' THEN
    RAISE EXCEPTION 'svc_pwhash 必须是 argon2id PHC 串（拿到长度 %）', coalesce(length(v_hash), 0);
  END IF;

  -- ② 目标部门：跟 admin 同一个（不硬编码 uuid；admin 没部门就停下，不猜）
  SELECT dept_id INTO v_dept FROM sys_user WHERE username = 'admin' AND deleted_at IS NULL;
  IF NOT FOUND THEN
    RAISE EXCEPTION '库内没有 admin 账号：拒绝猜部门，先确认库对不对';
  END IF;
  IF v_dept IS NULL THEN
    RAISE EXCEPTION 'admin 没有 dept_id：先按 docs/待决策.md 的口径落位，本脚本不猜部门';
  END IF;

  -- ③ 角色（幂等）：OPS_SELFCHECK / data_scope=1 / 启用
  SELECT id INTO v_role_id FROM sys_role WHERE role_code = 'OPS_SELFCHECK' AND deleted_at IS NULL;
  IF v_role_id IS NULL THEN
    -- 复活路径：曾被 down.sql 软删 —— 复用原行，避免同一 role_code 堆积多个软删副本
    SELECT id INTO v_role_id FROM sys_role WHERE role_code = 'OPS_SELFCHECK' AND deleted_at IS NOT NULL
     ORDER BY created_at DESC LIMIT 1;
    IF v_role_id IS NOT NULL THEN
      UPDATE sys_role SET deleted_at = NULL WHERE id = v_role_id;
      RAISE NOTICE '复活软删的 OPS_SELFCHECK（%）', v_role_id;
    ELSE
      INSERT INTO sys_role (role_code, role_name, description, status, sort_order, data_scope, remark)
      VALUES ('OPS_SELFCHECK', '运维自查（只读）',
              '自动化自查账号专用：只授列表/页面级查看权限，无任何 create/update/delete/export/approve',
              1, 90, 1, '卡 t_882d48c8 / svc_selfcheck 专用，勿扩权')
      RETURNING id INTO v_role_id;
      RAISE NOTICE '插入角色 OPS_SELFCHECK（%）', v_role_id;
    END IF;
  END IF;
  -- 只收紧不扩权：把可能的漂移拽回「启用 + 本人数据范围」
  UPDATE sys_role
     SET status = 1, data_scope = 1
   WHERE id = v_role_id
     AND (status IS DISTINCT FROM 1 OR data_scope IS DISTINCT FROM 1);

  -- ④ 用户（幂等；软删过就复活）
  SELECT id INTO v_user_id FROM sys_user WHERE username = 'svc_selfcheck' AND deleted_at IS NULL;
  IF v_user_id IS NULL THEN
    -- 复活路径：曾被我方 down.sql 软删
    SELECT id INTO v_user_id FROM sys_user WHERE username = 'svc_selfcheck' AND deleted_at IS NOT NULL
     ORDER BY created_at DESC LIMIT 1;
    IF v_user_id IS NOT NULL THEN
      UPDATE sys_user
         SET deleted_at = NULL, status = 1, must_change_password = false, dept_id = v_dept,
             password = CASE WHEN v_rotate THEN v_hash ELSE password END
       WHERE id = v_user_id;
      RAISE NOTICE '复活软删的 svc_selfcheck（%）', v_user_id;
    ELSE
      INSERT INTO sys_user (username, password, real_name, dept_id, status,
                            login_fail_count, is_locked, must_change_password, user_type, remark)
      VALUES ('svc_selfcheck', v_hash, '自动化自查账号（勿删/勿改密）', v_dept, 1,
              0, false, false, 'HUMAN',
              '卡 t_882d48c8：最小权限只读自查账号；口令见 /etc/cloudpivot/cloudpivot.env 的 CLOUDPIVOT_OPS_PASSWORD')
      RETURNING id INTO v_user_id;
      RAISE NOTICE '插入账号 svc_selfcheck（%）', v_user_id;
    END IF;
  ELSE
    UPDATE sys_user
       SET status = 1, must_change_password = false, dept_id = v_dept,
           password = CASE WHEN v_rotate THEN v_hash ELSE password END
     WHERE id = v_user_id
       AND (status IS DISTINCT FROM 1 OR must_change_password IS DISTINCT FROM false
            OR dept_id IS DISTINCT FROM v_dept OR v_rotate);
  END IF;

  -- ⑤ 绑定角色（幂等）：**只挂这一个**，多挂一个都会被下面的断言打回
  INSERT INTO sys_user_role (user_id, role_id)
  VALUES (v_user_id, v_role_id)
  ON CONFLICT (user_id, role_id) DO NOTHING;

  -- ⑥ 账号属性硬断言（错了就整笔回滚）
  SELECT count(*) INTO v_n
    FROM sys_user
   WHERE id = v_user_id AND deleted_at IS NULL AND user_type = 'HUMAN'
     AND status = 1 AND must_change_password = false AND is_locked = false
     AND password LIKE '$argon2id$%';
  IF v_n <> 1 THEN
    RAISE EXCEPTION 'svc_selfcheck 属性不符：要求 user_type=HUMAN / status=1 / must_change_password=false / is_locked=false / 口令为 argon2id（命中 % 行）', v_n;
  END IF;

  -- ⑦ 授权节点：按 path 白名单解析（命中数必须恰好 1，否则拒绝猜）
  --    策略：只给「列表/只读页面」+ 它们的分组目录节点（目录 auth_code 为空，本身不授权任何接口）
  FOREACH v_path IN ARRAY ARRAY[
      '/task',                            -- 工作任务（目录，码 business:task:read）
      '/task/list',                       -- 任务列表         → /api/business/task/page
      '/business/erp/sales-mgmt',         -- 销售管理（目录）
      '/business/erp/sales-order',        -- 销售订单         → /api/business/erp/sales-order/page
      '/business/erp/finance-mgmt',       -- 财务管理（目录）
      '/business/erp/voucher',            -- 记账凭证         → /api/business/erp/voucher/page
      '/business/erp/master-data',        -- 基础数据（目录）
      '/business/erp/seq',                -- 单号序列（只读页；prod_selfcheck 观察它挂在哪）
      '/business/erp/warehouse-mgmt',     -- 仓库管理（目录）
      '/business/erp/inventory-location'  -- 库位库存（只读页；prod_selfcheck 观察它挂在哪）
  ] LOOP
    SELECT count(*) INTO v_hits
      FROM sys_menu WHERE path = v_path AND deleted_at IS NULL AND status = 1;
    IF v_hits <> 1 THEN
      RAISE EXCEPTION '菜单 path=% 命中 % 行（要求恰好 1 行），拒绝猜授权目标', v_path, v_hits;
    END IF;
    SELECT id INTO v_menu_id
      FROM sys_menu WHERE path = v_path AND deleted_at IS NULL AND status = 1;
    INSERT INTO sys_role_menu (role_id, menu_id)
    SELECT v_role_id, v_menu_id
     WHERE NOT EXISTS (
       SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = v_role_id AND rm.menu_id = v_menu_id);
  END LOOP;

  -- ⑧ 最小权限硬断言 1：只挂这一个角色（挂到 SUPER_ADMIN / 业务角色一律回滚）
  SELECT count(*) INTO v_n FROM sys_user_role WHERE user_id = v_user_id;
  IF v_n <> 1 THEN
    RAISE EXCEPTION 'svc_selfcheck 挂着 % 个角色（要求恰好 1 个 = OPS_SELFCHECK）', v_n;
  END IF;
  SELECT count(*) INTO v_n
    FROM sys_user_role ur JOIN sys_role r ON r.id = ur.role_id
   WHERE ur.user_id = v_user_id AND ur.role_id = v_role_id AND r.role_code = 'OPS_SELFCHECK';
  IF v_n <> 1 THEN
    RAISE EXCEPTION 'svc_selfcheck 没有绑定 OPS_SELFCHECK';
  END IF;

  -- ⑨ 最小权限硬断言 2：授权码里不许出现任何写/导出/审批码
  --    白名单只认「读」结尾：:list / :view / :read / :page / :detail
  SELECT string_agg(m.auth_code, ', ' ORDER BY m.auth_code) INTO v_bad
    FROM sys_role_menu rm JOIN sys_menu m ON m.id = rm.menu_id
   WHERE rm.role_id = v_role_id AND m.deleted_at IS NULL AND m.auth_code <> ''
     AND m.auth_code !~ ':(list|view|read|page|detail)$';
  IF v_bad IS NOT NULL THEN
    RAISE EXCEPTION 'OPS_SELFCHECK 里出现非只读权限码：%', v_bad;
  END IF;

  -- ⑩ 最小权限硬断言 3：自查必需的三条列表码必须都在
  SELECT count(*) INTO v_n
    FROM (VALUES ('business:task:read'),
                 ('business:erp:sale-order:list'),
                 ('business:erp:voucher:list')) AS need(code)
   WHERE NOT EXISTS (
     SELECT 1 FROM sys_role_menu rm JOIN sys_menu m ON m.id = rm.menu_id
      WHERE rm.role_id = v_role_id AND m.deleted_at IS NULL AND m.status = 1
        AND m.auth_code = need.code);
  IF v_n > 0 THEN
    RAISE EXCEPTION '自查必需的只读权限码缺 % 条（对应接口会 403）', v_n;
  END IF;

  RAISE NOTICE 'svc_selfcheck 就绪：user=% role=% 授权节点=% 个',
    v_user_id, v_role_id, (SELECT count(*) FROM sys_role_menu WHERE role_id = v_role_id);
END $$;

COMMIT;
