-- ============================================================================
-- V0267：销售模块菜单树归位（「销售管理」目录）+ 角色授权（ARCH-7 §10.2 五角色矩阵）
--
-- 依据：kanban 卡 t_30f9d01b（SO-25 销售菜单种子 + 角色授权）。
--       ARCH-7《销售模块设计》§10.1 权限码 / §10.2 角色授权 / §10.3 菜单树。
--
-- # 一、施工前实测（cloudpivot_so23fresh，226 条迁移全量应用；非推测）
--   六个销售页**都已存在且已可见**，但全是**顶层**节点（parent_id IS NULL），
--   sort_order 76/77/78/79/80/85 与「采购管理 / 仓库管理 / 财务管理 / 基础数据」
--   四个既有目录**平级** ⇒ 侧边栏里销售链的 6 个页面散落在根级，没有归属目录。
--
--     …2601 客户档案 /business/erp/customer       sort=76
--     …2651 销售订单 /business/erp/sales-order    sort=77
--     …2841 发货单   /business/erp/shipment       sort=78
--     …2880 应收账款 /business/erp/receivable     sort=79   （title=应收台账）
--     …2890 收款单   /business/erp/receipt        sort=80
--     …2895 销售发票 /business/erp/invoice        sort=85
--
--   19 个按钮（menu_type=3）已全部挂在各自页面下，且 auth_code 与后端
--   `#[nik::permission(permission = "…")]` **逐字对应**（本迁移不新造码，见第三节）。
--
-- # 二、本迁移做三件事
--   ① 种目录节点「销售管理」（menu_type=1，path=/business/erp/sales-mgmt，无 auth_code），
--      与 采购管理(62) / 仓库管理(66) / 财务管理(72) 同形，排序插在 62 与 66 之间的 64；
--   ② 六个页面 reparent 到该目录下，sort_order 收敛成 10/20/30/40/50/60（= 卡面菜单树顺序）；
--   ③ 按 ARCH-7 §10.2 五角色矩阵授权（含**祖先目录**闭包，否则非超管侧边栏整棵子树被静默丢掉
--      —— 见 V0258 头注释第五节）。
--
-- # 三、为什么不种卡面列的每一个码（重要，别照着卡面补行）
--   卡面列的 6 组码里有 9 个**后端不存在**（本仓实测 `grep -oE 'permission = "[^"]+"'`）：
--
--     business:erp:customer:detail      后端无此码（客户详情由 :list 覆盖，V0261 头注释同口径）
--     business:erp:sale-order:delete    后端无此码（销售订单只走 LEGAL_EDGES 状态机，无物理/软删）
--     business:erp:sale-order:approve   后端无此码（审批走流程引擎：so_approve 节点审批人角色）
--     business:erp:shipment:update      后端无此码（发货单明细只在 create 时落，如要改走作废重开）
--     business:erp:shipment:delete      后端无此码（同上，作废 = status 边）
--     business:erp:receipt:update       后端无此码（收款单只登记 + 核销 + 状态，无编辑）
--     business:erp:receipt:delete       后端无此码（作废 = status 边）
--     business:erp:invoice:update       后端无此码（发票不可编辑，作废 = status 边 + 红冲）
--     business:erp:sales-order:*        真实资源名是 **sale-order**（单数 sale，V0265 已落地；
--                                       卡面的 sales-order 是设计稿笔误，改名会让 SO-05 接口全 403）
--
--   AGENTS.md 约定 7：权限码与 `#[alun::permission]` **一一对应**；`perm_menu_gate.py` 的
--   反向档（菜单码 − 注解码 = unused_in_code）虽不判红，但给一个「点了必然 403」的码建行，
--   等于在权限树里放一颗哑弹。因此本迁移**只授真实存在的 19 个码**，不补上面 9 个。
--   反向地，卡面漏列但**真实存在**的 4 个码也一并纳入矩阵（否则页面上的按钮拿不到授权）：
--     business:erp:receivable:create / business:erp:receipt:detail / business:erp:invoice:detail
--     （清点见自检 6.1：菜单侧销售 6 模块的码 = 19，与后端注解码集合**完全相等**）
--
-- # 四、角色矩阵（ARCH-7 §10.2，落到本仓真实角色码上）
--   · SALES_MANAGER（销售经理）→ **复用已有 `SALES_SUPERVISOR`（销售主管）**。
--     该角色由 V0281 为 `so_approve` 流程种下（n_sales 节点审批人），再新建一个同义的
--     SALES_MANAGER 会出现「两个销售管理角色、审批流只认其中一个」的裂缝。矩阵内容按
--     §10.2 的「销售经理」行落：客户读写 / 订单读写+提交 / 发货只读 / 应收只读。
--   · SALES_CLERK（销售员）本仓**不存在** ⇒ 本迁移新建（role_code=SALES_CLERK）。
--     `role_code_gate.py` 的合法路径之一就是「先 INSERT INTO sys_role 建角色再授」，
--     该脚本会自动把本文件种出的角色并入白名单（无需手改白名单）。
--   · WAREHOUSE_MANAGER / FINANCE_MANAGER / GENERAL_MANAGER 已存在，直接授。
--   · 不含 SUPER_ADMIN：超管拿 `*:*:*` 通配，授权行对功能零影响（V0258 同口径）。
--
--   | 角色                | 客户 | 订单 | 发货 | 应收 | 收款 | 发票 |
--   |---------------------|------|------|------|------|------|------|
--   | SALES_SUPERVISOR    | 读写 | 读写+提交 | 只读 | 只读 | — | — |
--   | SALES_CLERK         | 只读 | 建单+提交 | — | — | — | — |
--   | WAREHOUSE_MANAGER   | —    | 只读 | 读写+过账 | — | — | — |
--   | FINANCE_MANAGER     | 只读 | 审批 | 只读 | 读写 | 读写 | 读写 |
--   | GENERAL_MANAGER     | 只读 | 审批 | 只读 | 只读 | 只读 | 只读 |
--
--   §10.2 的「订单·审批」在本仓**没有 sale-order:approve 码**：审批动作的鉴权由流程引擎的
--   `business:flow:approve`（V0258 已授 FINANCE_MANAGER / GENERAL_MANAGER）+ `so_approve`
--   节点的审批人角色（SALES_SUPERVISOR / FINANCE_MANAGER / GENERAL_MANAGER）共同完成。
--   所以「审批」这一列在本迁移里落成「必须能看单」= sale-order:list（否则审批中心点进去 404）。
--   实测佐证：docs/evidence/sales-e2e/README.md（SO-24）用 FINANCE_MANAGER 审批通过两节点。
--
--   §10.2 的「销售员 = 建单+提交」在本仓是 create + status（提交是 status 的一条 LEGAL_EDGES 边），
--   外加 sale-order:list —— 列表页本身要 list 才能进，且 create 按钮挂在 list 页下
--   （不授 list 会被 orphan_button_gate 判成孤儿按钮）。同理各「只读」都带上该页的
--   detail / aging / 列表码。
--
-- # 五、幂等 / 回滚 / 影响面
--   幂等：目录 `ON CONFLICT (id) DO UPDATE`（收敛终态）；reparent 带 `IS DISTINCT FROM`；
--        授权按 (role_id, menu_id) NOT EXISTS + 确定性 id（md5 折叠，与 V0258 同一套公式，
--        盐换成 `v0267-sales-menu`）。重放 = 0 行插入、0 行更新、不踢在线会话。
--   回滚：`V0267__sales_menu.down.sql` ① 按同一公式精确删本迁移插入的授权行；
--        ② 六个页面 parent_id 还原 NULL、sort_order 还原 76/77/78/79/80/85；
--        ③ 删目录节点；④ SALES_CLERK 只在「无用户绑定」时删（有绑定就 NOTICE 保留）。
--   影响面：**零授权变化之外零副作用** —— 不改任何页面的 path/component/is_visible
--        （所以 `system::menu::tests::n08`「菜单路径必须能在前端路由解析」不受影响：
--        六条路径在这次 reparent 里一个字符都没动，前端 router/index.ts 早已注册）。
--
-- # 六、自检（任一不符 → 迁移失败，宁可启动失败也不要「半套菜单」上线）
--   6.1 菜单侧「销售 6 模块的码」集合 = 后端注解码集合（19 个），一个不多一个不少；
--   6.2 目录节点存在、menu_type=1、无 auth_code；
--   6.3 六个页面 parent_id = 目录、sort_order = 10…60；
--   6.4 矩阵 47 对：角色码全部存在、权限码全部在 sys_menu 有承载；
--   6.5 施工后：每个 (角色, 码) 都真持有（空跑 0 行 = 报警而非静默）；
--   6.6 无孤儿：被授的按钮码，其父页的码必须也授给了同一角色。
-- ============================================================================

-- ---------- ① 目录节点「销售管理」 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002701'::uuid, '销售管理', 1, '',
     '/business/erp/sales-mgmt', NULL, NULL,
     64, 1, 'shop-5', '销售管理', TRUE, 'business')
ON CONFLICT (id) DO UPDATE
   SET name        = EXCLUDED.name,
       menu_type   = EXCLUDED.menu_type,
       auth_code   = EXCLUDED.auth_code,
       path        = EXCLUDED.path,
       parent_id   = EXCLUDED.parent_id,
       component   = EXCLUDED.component,
       sort_order  = EXCLUDED.sort_order,
       status      = EXCLUDED.status,
       icon        = EXCLUDED.icon,
       title       = EXCLUDED.title,
       is_visible  = EXCLUDED.is_visible,
       module_code = EXCLUDED.module_code,
       updated_at  = now();

-- ---------- ② 六个销售页面归位到「销售管理」下（顺序 = 卡面菜单树） ----------
UPDATE sys_menu m
   SET parent_id  = '20000000-0000-0000-0000-000000002701'::uuid,
       sort_order = v.sort_order,
       updated_at = now()
  FROM (VALUES
          ('20000000-0000-0000-0000-000000002601'::uuid, 10),  -- 客户档案
          ('20000000-0000-0000-0000-000000002651'::uuid, 20),  -- 销售订单
          ('20000000-0000-0000-0000-000000002841'::uuid, 30),  -- 发货单
          ('20000000-0000-0000-0000-000000002880'::uuid, 40),  -- 应收账款（title=应收台账）
          ('20000000-0000-0000-0000-000000002890'::uuid, 50),  -- 收款单
          ('20000000-0000-0000-0000-000000002895'::uuid, 60)   -- 销售发票
       ) AS v(id, sort_order)
 WHERE m.id = v.id
   AND m.deleted_at IS NULL
   AND (m.parent_id  IS DISTINCT FROM '20000000-0000-0000-0000-000000002701'::uuid
     OR m.sort_order IS DISTINCT FROM v.sort_order);

-- ---------- ③ 角色 SALES_CLERK（销售员）：不存在才建（幂等，与 V0281 同一写法） ----------
-- ⚠ 必须是 `VALUES` + `ON CONFLICT (role_code) WHERE deleted_at IS NULL DO NOTHING` 这种形状：
--   `role_code_gate.py::parse_seeded_roles` 只从 `INSERT INTO sys_role … VALUES …` 里解析
--   新角色（实测 `INSERT … SELECT … WHERE NOT EXISTS` 解析不到 ⇒ 同一文件里的授权段会被判
--   「未知角色码」而闸红）。本仓的「先建角色再授」合法路径就靠这条被自动并入白名单。
INSERT INTO sys_role (id, role_code, role_name, description, status, sort_order, remark, created_at, updated_at)
VALUES
    ('20000000-0000-4000-8000-0000000f0008'::uuid, 'SALES_CLERK', '销售员',
     '销售链执行角色：客户只读、销售订单建单+提交（ARCH-7 §10.2 SALES_CLERK）', 1, 80,
     'V0267 种：销售员（与 SALES_SUPERVISOR 销售主管区分 —— 后者是 so_approve 的 n_sales 审批角色）',
     now(), now())
ON CONFLICT (role_code) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- ④ 授权矩阵（角色 × 权限码，47 对） ----------
DROP TABLE IF EXISTS _sm_matrix;
CREATE TEMP TABLE _sm_matrix (
    role_code text NOT NULL,
    auth_code text NOT NULL,
    PRIMARY KEY (role_code, auth_code)
);

INSERT INTO _sm_matrix (role_code, auth_code) VALUES
-- ---- SALES_SUPERVISOR（销售经理；ARCH-7 的 SALES_MANAGER，复用 V0281 已有角色）12 个码
    ('SALES_SUPERVISOR', 'business:erp:customer:list'),
    ('SALES_SUPERVISOR', 'business:erp:customer:create'),
    ('SALES_SUPERVISOR', 'business:erp:customer:update'),
    ('SALES_SUPERVISOR', 'business:erp:customer:delete'),
    ('SALES_SUPERVISOR', 'business:erp:sale-order:list'),
    ('SALES_SUPERVISOR', 'business:erp:sale-order:create'),
    ('SALES_SUPERVISOR', 'business:erp:sale-order:update'),
    ('SALES_SUPERVISOR', 'business:erp:sale-order:status'),
    ('SALES_SUPERVISOR', 'business:erp:shipment:list'),
    ('SALES_SUPERVISOR', 'business:erp:receivable:list'),
    ('SALES_SUPERVISOR', 'business:erp:receivable:detail'),
    ('SALES_SUPERVISOR', 'business:erp:receivable:aging'),
-- ---- SALES_CLERK（销售员）4 个码：客户只读 / 订单 建单+提交（list 是进页面与按钮父页的必需码）
    ('SALES_CLERK', 'business:erp:customer:list'),
    ('SALES_CLERK', 'business:erp:sale-order:list'),
    ('SALES_CLERK', 'business:erp:sale-order:create'),
    ('SALES_CLERK', 'business:erp:sale-order:status'),
-- ---- WAREHOUSE_MANAGER（仓库主管）4 个码：订单只读 / 发货读写+过账
    ('WAREHOUSE_MANAGER', 'business:erp:sale-order:list'),
    ('WAREHOUSE_MANAGER', 'business:erp:shipment:list'),
    ('WAREHOUSE_MANAGER', 'business:erp:shipment:create'),
    ('WAREHOUSE_MANAGER', 'business:erp:shipment:status'),
-- ---- FINANCE_MANAGER（财务经理）17 个码：客户/订单/发货只读 + 应收/收款/发票读写
    ('FINANCE_MANAGER', 'business:erp:customer:list'),
    ('FINANCE_MANAGER', 'business:erp:sale-order:list'),
    ('FINANCE_MANAGER', 'business:erp:shipment:list'),
    ('FINANCE_MANAGER', 'business:erp:receivable:list'),
    ('FINANCE_MANAGER', 'business:erp:receivable:detail'),
    ('FINANCE_MANAGER', 'business:erp:receivable:aging'),
    ('FINANCE_MANAGER', 'business:erp:receivable:create'),
    ('FINANCE_MANAGER', 'business:erp:receivable:write-off'),
    ('FINANCE_MANAGER', 'business:erp:receipt:list'),
    ('FINANCE_MANAGER', 'business:erp:receipt:detail'),
    ('FINANCE_MANAGER', 'business:erp:receipt:create'),
    ('FINANCE_MANAGER', 'business:erp:receipt:allocate'),
    ('FINANCE_MANAGER', 'business:erp:receipt:status'),
    ('FINANCE_MANAGER', 'business:erp:invoice:list'),
    ('FINANCE_MANAGER', 'business:erp:invoice:detail'),
    ('FINANCE_MANAGER', 'business:erp:invoice:create'),
    ('FINANCE_MANAGER', 'business:erp:invoice:status'),
-- ---- GENERAL_MANAGER（总经理）10 个码：全线只读（审批走流程引擎权限）
    ('GENERAL_MANAGER', 'business:erp:customer:list'),
    ('GENERAL_MANAGER', 'business:erp:sale-order:list'),
    ('GENERAL_MANAGER', 'business:erp:shipment:list'),
    ('GENERAL_MANAGER', 'business:erp:receivable:list'),
    ('GENERAL_MANAGER', 'business:erp:receivable:detail'),
    ('GENERAL_MANAGER', 'business:erp:receivable:aging'),
    ('GENERAL_MANAGER', 'business:erp:receipt:list'),
    ('GENERAL_MANAGER', 'business:erp:receipt:detail'),
    ('GENERAL_MANAGER', 'business:erp:invoice:list'),
    ('GENERAL_MANAGER', 'business:erp:invoice:detail');

DO $$
DECLARE
    v_dir     uuid := '20000000-0000-0000-0000-000000002701'::uuid;
    v_roles   text[] := ARRAY['SALES_SUPERVISOR', 'SALES_CLERK', 'WAREHOUSE_MANAGER',
                              'FINANCE_MANAGER', 'GENERAL_MANAGER'];
    v_codes   text[] := ARRAY[
                'business:erp:customer:list', 'business:erp:customer:create',
                'business:erp:customer:update', 'business:erp:customer:delete',
                'business:erp:sale-order:list', 'business:erp:sale-order:create',
                'business:erp:sale-order:update', 'business:erp:sale-order:status',
                'business:erp:shipment:list', 'business:erp:shipment:create',
                'business:erp:shipment:status',
                'business:erp:receivable:list', 'business:erp:receivable:detail',
                'business:erp:receivable:aging', 'business:erp:receivable:create',
                'business:erp:receivable:write-off',
                'business:erp:receipt:list', 'business:erp:receipt:detail',
                'business:erp:receipt:create', 'business:erp:receipt:allocate',
                'business:erp:receipt:status',
                'business:erp:invoice:list', 'business:erp:invoice:detail',
                'business:erp:invoice:create', 'business:erp:invoice:status'];
    v_pages   uuid[] := ARRAY['20000000-0000-0000-0000-000000002601'::uuid,
                              '20000000-0000-0000-0000-000000002651'::uuid,
                              '20000000-0000-0000-0000-000000002841'::uuid,
                              '20000000-0000-0000-0000-000000002880'::uuid,
                              '20000000-0000-0000-0000-000000002890'::uuid,
                              '20000000-0000-0000-0000-000000002895'::uuid];
    v_n       integer;
    v_bad     integer;
    v_str     text;
    v_n_ins   integer := 0;
    v_n_ver   integer := 0;
BEGIN
    -- ---------- 6.1 菜单侧销售码集合 = 后端注解码集合（19 个，逐字比对） ----------
    SELECT count(DISTINCT m.auth_code) INTO v_n
      FROM sys_menu m
     WHERE m.deleted_at IS NULL
       AND m.auth_code = ANY (v_codes);
    IF v_n <> array_length(v_codes, 1) THEN
        SELECT string_agg(want, ', ') INTO v_str
          FROM unnest(v_codes) AS want
         WHERE NOT EXISTS (SELECT 1 FROM sys_menu m
                            WHERE m.auth_code = want AND m.deleted_at IS NULL);
        RAISE EXCEPTION 'V0267 自检 6.1 失败：以下销售权限码在 sys_menu 里没有承载 → %', v_str;
    END IF;

    -- 反向：六个销售页下不得出现「本迁移清点之外」的码（插进来的哑弹砲要炸）
    SELECT string_agg(DISTINCT c.auth_code, ', ') INTO v_str
      FROM sys_menu p
      JOIN sys_menu c ON c.parent_id = p.id AND c.deleted_at IS NULL
     WHERE p.id = ANY (v_pages)
       AND c.auth_code <> ''
       AND NOT (c.auth_code = ANY (v_codes));
    IF v_str IS NOT NULL THEN
        RAISE EXCEPTION 'V0267 自检 6.1 失败：销售页下出现矩阵外的权限码 → %', v_str;
    END IF;

    -- ---------- 6.2 目录节点 ----------
    SELECT count(*) INTO v_bad
      FROM sys_menu
     WHERE id = v_dir AND deleted_at IS NULL
       AND menu_type = 1 AND coalesce(auth_code, '') = '' AND parent_id IS NULL;
    IF v_bad <> 1 THEN
        RAISE EXCEPTION 'V0267 自检 6.2 失败：「销售管理」目录节点不存在或形状不对（menu_type/无码/根级）';
    END IF;

    -- ---------- 6.3 六个页面归位 ----------
    SELECT count(*) INTO v_bad
      FROM sys_menu m
      JOIN (VALUES ('20000000-0000-0000-0000-000000002601'::uuid, 10),
                   ('20000000-0000-0000-0000-000000002651'::uuid, 20),
                   ('20000000-0000-0000-0000-000000002841'::uuid, 30),
                   ('20000000-0000-0000-0000-000000002880'::uuid, 40),
                   ('20000000-0000-0000-0000-000000002890'::uuid, 50),
                   ('20000000-0000-0000-0000-000000002895'::uuid, 60)) AS v(id, sort_order)
        ON m.id = v.id
     WHERE m.deleted_at IS NULL
       AND m.parent_id = v_dir
       AND m.sort_order = v.sort_order
       AND m.menu_type = 2;
    IF v_bad <> 6 THEN
        RAISE EXCEPTION 'V0267 自检 6.3 失败：归位到「销售管理」下的页面 % 个（期望 6）', v_bad;
    END IF;

    -- ---------- 6.4 矩阵：条数 / 角色码 / 权限码 ----------
    SELECT count(*) INTO v_n FROM _sm_matrix;
    IF v_n <> 47 THEN
        RAISE EXCEPTION 'V0267 自检 6.4 失败：矩阵 % 对（期望 47 —— 改了矩阵请同步改本断言）', v_n;
    END IF;

    SELECT string_agg(DISTINCT mx.role_code, ', ') INTO v_str
      FROM _sm_matrix mx
     WHERE NOT EXISTS (SELECT 1 FROM sys_role r
                        WHERE r.role_code = mx.role_code AND r.deleted_at IS NULL);
    IF v_str IS NOT NULL THEN
        RAISE EXCEPTION 'V0267 自检 6.4 失败：矩阵里的角色码不存在（授权会静默 0 行）→ %', v_str;
    END IF;

    SELECT string_agg(DISTINCT mx.auth_code, ', ') INTO v_str
      FROM _sm_matrix mx
     WHERE NOT EXISTS (SELECT 1 FROM sys_menu m
                        WHERE m.auth_code = mx.auth_code AND m.deleted_at IS NULL);
    IF v_str IS NOT NULL THEN
        RAISE EXCEPTION 'V0267 自检 6.4 失败：矩阵里的权限码在 sys_menu 无承载 → %', v_str;
    END IF;

    -- 越界红线：销售矩阵不得命中平台治理节点 / 非销售模块的码（防复制粘贴串行）
    SELECT string_agg(DISTINCT mx.auth_code, ', ') INTO v_str
      FROM _sm_matrix mx
     WHERE mx.auth_code NOT LIKE 'business:erp:%'
        OR mx.auth_code NOT SIMILAR TO
           'business:erp:(customer|sale-order|shipment|receivable|receipt|invoice):%';
    IF v_str IS NOT NULL THEN
        RAISE EXCEPTION 'V0267 自检 6.4 失败：矩阵越界（命中非销售 6 模块的码）→ %', v_str;
    END IF;

    -- ---------- 6.5 展开 + 插入（码的全部节点 + 无码祖先目录闭包，确定性 id） ----------
    DROP TABLE IF EXISTS _sm_sel;
    CREATE TEMP TABLE _sm_sel AS
    WITH RECURSIVE hit AS (
        -- 码对应的**全部**节点（页面与其按钮共码时整组给，权限树勾选才一致）
        SELECT mx.role_code, r.id AS role_id, m.id AS menu_id
          FROM _sm_matrix mx
          JOIN sys_role r ON r.role_code = mx.role_code AND r.deleted_at IS NULL
          JOIN sys_menu m ON m.auth_code = mx.auth_code AND m.deleted_at IS NULL
    ),
    anc AS (
        -- 祖先闭包：**只补无码目录**（有码的祖先要出现在矩阵里才会被授，见 V0258 第五节）
        SELECT h.role_code, h.role_id, p.id AS menu_id
          FROM hit h
          JOIN sys_menu c ON c.id = h.menu_id
          JOIN sys_menu p ON p.id = c.parent_id AND p.deleted_at IS NULL
         WHERE coalesce(p.auth_code, '') = ''
        UNION
        SELECT a.role_code, a.role_id, p.id AS menu_id
          FROM anc a
          JOIN sys_menu c ON c.id = a.menu_id
          JOIN sys_menu p ON p.id = c.parent_id AND p.deleted_at IS NULL
         WHERE coalesce(p.auth_code, '') = ''
    )
    SELECT DISTINCT role_code, role_id, menu_id
      FROM (SELECT * FROM hit UNION ALL SELECT * FROM anc) u;

    INSERT INTO sys_role_menu (id, role_id, menu_id, created_at)
    SELECT (
               SELECT (substr(h, 1, 8) || '-' || substr(h, 9, 4) || '-4' || substr(h, 14, 3)
                       || '-8' || substr(h, 18, 3) || '-' || substr(h, 21, 12))::uuid
                 FROM (SELECT md5(s.role_id::text || '|' || s.menu_id::text || '|v0267-sales-menu') AS h) _h
           ), s.role_id, s.menu_id, now()
      FROM _sm_sel s
     WHERE NOT EXISTS (SELECT 1 FROM sys_role_menu rm
                        WHERE rm.role_id = s.role_id AND rm.menu_id = s.menu_id)
    ON CONFLICT (id) DO NOTHING;
    GET DIAGNOSTICS v_n_ins = ROW_COUNT;

    -- 有变更才推进 token_version（纯重放不踢在线会话）
    IF v_n_ins > 0 THEN
        UPDATE sys_user u
           SET token_version = u.token_version + 1, updated_at = now()
         WHERE u.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM sys_user_role ur
                        WHERE ur.user_id = u.id
                          AND ur.role_id IN (SELECT role_id FROM _sm_sel));
        GET DIAGNOSTICS v_n_ver = ROW_COUNT;
    END IF;

    -- ---------- 6.5 施工后：每个 (角色,码) 必须真持有 ----------
    SELECT count(*) INTO v_bad
      FROM _sm_matrix mx
     WHERE NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm
              JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
              JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
             WHERE r.role_code = mx.role_code AND m.auth_code = mx.auth_code);
    IF v_bad <> 0 THEN
        RAISE EXCEPTION 'V0267 自检 6.5 失败：施工后仍有 % 个 (角色,码) 未授权', v_bad;
    END IF;

    -- ---------- 6.6 无孤儿按钮：矩阵里被授的**按钮码**，其父页的码必须也授给了同一角色 ----------
    -- 只查本迁移矩阵内的 (角色,码) 对，不扩散到其它模块既有的授权形状（那些不归本卡管）。
    SELECT count(*) INTO v_bad
      FROM _sm_matrix mx
      JOIN sys_role r ON r.role_code = mx.role_code AND r.deleted_at IS NULL
      JOIN sys_menu b ON b.auth_code = mx.auth_code AND b.deleted_at IS NULL AND b.menu_type = 3
      JOIN sys_menu p ON p.id = b.parent_id AND p.deleted_at IS NULL
     WHERE coalesce(p.auth_code, '') <> ''
       AND NOT EXISTS (
            SELECT 1 FROM _sm_matrix mx2
             WHERE mx2.role_code = mx.role_code AND mx2.auth_code = p.auth_code);
    IF v_bad <> 0 THEN
        RAISE EXCEPTION 'V0267 自检 6.6 失败：矩阵里有 % 个按钮码的父页码没一起授（权限树上不可达）', v_bad;
    END IF;

    RAISE NOTICE 'V0267 完成：销售管理目录 + 6 页面归位；矩阵 47 对，本次新增授权 % 行，推进 token_version % 人',
        v_n_ins, v_n_ver;
END $$;

DROP TABLE IF EXISTS _sm_sel;
DROP TABLE IF EXISTS _sm_matrix;
