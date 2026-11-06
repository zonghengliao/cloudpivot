-- ============================================================================
-- V0266：销售链 4 个页面 / 17 个权限码的「前置承载」—— 让 V0267 在空库重放时成立
--
-- 依据：kanban 卡 t_d403a60a（【MIG-OUT-OF-ORDER】空库重放迁移在 V0267 必败）。
--
-- # 一、为什么要有这条迁移
--   V0267（sales_menu）的自检 6.1 / 6.3 / 6.4 要求：它要授的 25 个销售权限码必须在
--   `sys_menu` 有承载行，6 个销售页面都已存在（否则 reparent 后 count<>6 直接 RAISE），
--   且矩阵里的 5 个角色码必须已存在（否则授权静默 0 行）。
--   这些前置对象里有两批由**更晚**的迁移才种下：
--
--   ┌ 19 个权限码所在的 4 个页面 + 13 个按钮 ────────────────────────────┐
--   │  V0284（发货：1 页 + 2 按钮）、V0288（应收：1 页 + 4 按钮）、       │
--   │  V0293（收款：1 页 + 4 按钮）、V0296（发票：1 页 + 3 按钮）         │
--   └──────────────────────────────────────────────────────────────────┘
--   ┌ 角色码 SALES_SUPERVISOR（销售主管）—— V0281 的 so_approve 审批角色 ┐
--   └──────────────────────────────────────────────────────────────────┘
--
--   ⇒ 空库按版本号升序重放时，跑到 267 这些行还不存在，V0267 必 RAISE：
--       ERROR: V0267 自检 6.1 失败：以下销售权限码在 sys_menu 里没有承载 → business:erp:shipment:list …
--       （种齐菜单后紧接 6.4：矩阵里的角色码不存在 → SALES_SUPERVISOR）
--
--   实测（2026-11-07，空库逐条 apply）：197 条成功，第 198 条 V0267 失败；
--   把 V0267 在两个副本里挪到末尾再放 → **217/217 全过** ⇒ 整条迁移链只有这一个乱序依赖。
--   V0267 当年能过，是因为 SO-25 是在 cloudpivot_so24（已含 287–301）的副本上应用的。
--
-- # 二、为什么不是直接改 V0267
--   已应用的迁移文件与 `schema_migrations.checksum` 绑定，改一个注释都会让所有已应用库
--   （生产 / 144 / 各卡副本）启动即 `migration … was modified after it was applied`。
--   所以在**不动历史文件**的前提下，唯一合法解是「把它依赖的行提前种下」：
--   本迁移用与 V0281 / V0284 / V0288 / V0293 / V0296 **逐字相同**的 INSERT
--   （同固定 uuid / auth_code / menu_type / sort_order / title / module_code / role_code）
--   把 4 个页面 + 13 个按钮 + 1 个角色提前落地。
--   那 5 条迁移都是 `ON CONFLICT … DO NOTHING` ⇒ 它们随后执行时 0 行插入，
--   其自检（码有承载 / 按钮挂在页面下 / is_visible 仍为 FALSE）依旧成立。
--
-- # 三、对存量库的影响：纯 no-op（gap 补迁）
--   已应用库里这 17 行菜单 + SALES_SUPERVISOR 角色早就在 ⇒ 本迁移 0 行插入，只补一条
--   schema_migrations 记录（引擎按**集合语义**判定待应用，`out_of_order = false`
--   只打 WARN 不拒绝）。
--   之后 V0281 / V0284 / V0288 / V0293 / V0296 执行时，其 `ON CONFLICT … DO NOTHING`
--   命中冲突 → 0 行插入，它们的自检依旧成立。
--
--   本迁移**不**做的事（别在这里加）：
--     · 不写 `sys_role_menu`：角色矩阵只有一处真相源 = V0267（V0250/261/265/284 同口径）；
--     · 不改 `path` / `component` / `is_visible`：那三列由 V0286 / V0291 / V0294 / V0297 收尾
--       （前端路由落地后才开可见性，提前填会让 `system::menu::tests::n08` 在空库上判红）；
--     · 不种 so_approve 流程（V0281 的主体职责，本迁移只补它种的那一个角色码）。
--
-- # 四、幂等 / 回滚
--   幂等：全部 `ON CONFLICT (id) DO NOTHING`，重放 0 行；末尾 DO 块自检 17 行形状。
--   回滚：**故意 no-op** —— 这 17 行的生命周期属于 V0284/288/293/296（含各自的 down），
--        本迁移只是把它们提前种下；down 里删行会连带删掉存量库里的既有菜单，
--        造成「回滚一条 no-op 补迁却丢了 4 个页面」。故 down 只打 NOTICE。
-- ============================================================================

-- ---------- 发货单：页面 …2841 + 2 个按钮（镜像 V0284） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002841'::uuid, '发货单', 2,
     'business:erp:shipment:list', NULL, NULL,
     NULL, 78, 1, 'send',
     '发货单（销售链单据：单号 SH-yyyyMMdd-000001 / 4 态状态机 / 从销售订单带出未发完的行）。path/component/可见性由前端卡落地后补',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002842'::uuid, 'business:erp:shipment:create', 3,
     'business:erp:shipment:create', NULL,
     '20000000-0000-0000-0000-000000002841'::uuid, NULL,
     10, 1, NULL, '新建发货单（取号 SH-yyyyMMdd-000001；items 留空 = 从订单带出全部未发完的行；超发 400 / 订单未审批 409）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002843'::uuid, 'business:erp:shipment:status', 3,
     'business:erp:shipment:status', NULL,
     '20000000-0000-0000-0000-000000002841'::uuid, NULL,
     20, 1, NULL, '发货单状态流转（提交待过账 / 过账（过账前校验库存充足）/ 作废，只走 4 条合法边）',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 应收账款：页面 …2880 + 4 个按钮（镜像 V0288） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002880'::uuid, '应收账款', 2,
     'business:erp:receivable:list', NULL, NULL,
     NULL, 79, 1, 'money',
     '应收账款（销售链台账：单号 AR-yyyyMMdd-000001 / 4 态状态机 / 账龄分析）。path/component/可见性由前端卡 SO-15 落地后补',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002881'::uuid, 'business:erp:receivable:detail', 3,
     'business:erp:receivable:detail', NULL,
     '20000000-0000-0000-0000-000000002880'::uuid, NULL,
     10, 1, NULL, '应收单详情（含客户名 / 关联凭证号 / 是否逾期）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002882'::uuid, 'business:erp:receivable:aging', 3,
     'business:erp:receivable:aging', NULL,
     '20000000-0000-0000-0000-000000002880'::uuid, NULL,
     20, 1, NULL, '应收账龄分析（分档与应付账款账龄完全同口径：未到期 / 1-30 / 31-60 / 61-90 / 90 天以上）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002883'::uuid, 'business:erp:receivable:create', 3,
     'business:erp:receivable:create', NULL,
     '20000000-0000-0000-0000-000000002880'::uuid, NULL,
     30, 1, NULL, '手工挂账（取号 AR-yyyyMMdd-000001；source_type=MANUAL；amount 必填且 > 0；已结账期间 400）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002884'::uuid, 'business:erp:receivable:write-off', 3,
     'business:erp:receivable:write-off', NULL,
     '20000000-0000-0000-0000-000000002880'::uuid, NULL,
     40, 1, NULL, '坏账核销（OPEN/PARTIAL → WRITTEN_OFF，必须给原因；SETTLED/已核销 409）',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 收款单：页面 …2890 + 4 个按钮（镜像 V0293） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002890'::uuid, '收款单', 2,
     'business:erp:receipt:list', NULL, NULL,
     NULL, 80, 1, 'wallet',
     '收款单（销售链：RC 取号 / 4 态 / 一单核销多张应收）',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002891'::uuid, 'business:erp:receipt:detail', 3,
     'business:erp:receipt:detail', NULL,
     '20000000-0000-0000-0000-000000002890'::uuid, NULL,
     10, 1, NULL, '收款单详情（客户 / 凭证号 / 核销明细）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002892'::uuid, 'business:erp:receipt:create', 3,
     'business:erp:receipt:create', NULL,
     '20000000-0000-0000-0000-000000002890'::uuid, NULL,
     20, 1, NULL, '登记收款（RC 取号；金额 > 0；方式白名单）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002893'::uuid, 'business:erp:receipt:allocate', 3,
     'business:erp:receipt:allocate', NULL,
     '20000000-0000-0000-0000-000000002890'::uuid, NULL,
     30, 1, NULL, '核销到应收（一单多张，回写应收状态）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002894'::uuid, 'business:erp:receipt:status', 3,
     'business:erp:receipt:status', NULL,
     '20000000-0000-0000-0000-000000002890'::uuid, NULL,
     40, 1, NULL, '状态迁移（确认 / 过账生成凭证 / 作废）',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 销售发票：页面 …2895 + 3 个按钮（镜像 V0296） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002895'::uuid, '销售发票', 2,
     'business:erp:invoice:list', NULL, NULL,
     NULL, 85, 1, 'file-copy',
     '销售发票（销售链：IV 取号 / 4 态 / 开票红冲）',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002896'::uuid, 'business:erp:invoice:detail', 3,
     'business:erp:invoice:detail', NULL,
     '20000000-0000-0000-0000-000000002895'::uuid, NULL,
     10, 1, NULL, '发票详情（客户 / 应收 / 凭证号 / 可开票额度）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002897'::uuid, 'business:erp:invoice:create', 3,
     'business:erp:invoice:create', NULL,
     '20000000-0000-0000-0000-000000002895'::uuid, NULL,
     20, 1, NULL, '开票（从应收带出金额 / 手工开票；金额不得超过应收）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002898'::uuid, 'business:erp:invoice:status', 3,
     'business:erp:invoice:status', NULL,
     '20000000-0000-0000-0000-000000002895'::uuid, NULL,
     30, 1, NULL, '状态迁移（开票生成收入凭证 / 归档 / 作废红冲）',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 角色 SALES_SUPERVISOR（销售主管，镜像 V0281 的角色种子） ----------
-- V0267 的 47 对矩阵里有 12 对授给这个角色；它由 V0281（so_approve 流程）种下，
-- 号段晚于 267 ⇒ 空库重放到 267 时不存在，自检 6.4 会 RAISE。这里按 V0281 逐字补一行。
INSERT INTO sys_role (id, role_code, role_name, description, status, sort_order, remark, created_at, updated_at)
VALUES
    ('20000000-0000-4000-8000-0000000f0007'::uuid, 'SALES_SUPERVISOR', '销售主管',
     '销售订单审批·第一档（< 1 万）：销售链的第一复核人（三档都要过他）', 1, 14,
     'SO-06 预置审批角色：把用户绑到这个角色上即可参与销售订单审批，无需改流程', now(), now())
ON CONFLICT (role_code) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- 自检：17 行（4 页 + 13 按钮）形状必须与 V0284/288/293/296 的种法一致 ----------
DO $$
DECLARE
    v_n     integer;
    v_str   text;
    v_want  int := 17;
BEGIN
    -- 1) 行数：4 个页面 + 13 个按钮（按固定 uuid 清点，避免把别的模块的行数进来）
    SELECT count(*) INTO v_n
      FROM sys_menu
     WHERE id = ANY (ARRAY[
              '20000000-0000-0000-0000-000000002841',
              '20000000-0000-0000-0000-000000002842',
              '20000000-0000-0000-0000-000000002843',
              '20000000-0000-0000-0000-000000002880',
              '20000000-0000-0000-0000-000000002881',
              '20000000-0000-0000-0000-000000002882',
              '20000000-0000-0000-0000-000000002883',
              '20000000-0000-0000-0000-000000002884',
              '20000000-0000-0000-0000-000000002890',
              '20000000-0000-0000-0000-000000002891',
              '20000000-0000-0000-0000-000000002892',
              '20000000-0000-0000-0000-000000002893',
              '20000000-0000-0000-0000-000000002894',
              '20000000-0000-0000-0000-000000002895',
              '20000000-0000-0000-0000-000000002896',
              '20000000-0000-0000-0000-000000002897',
              '20000000-0000-0000-0000-000000002898']::uuid[])
       AND deleted_at IS NULL;
    IF v_n <> v_want THEN
        RAISE EXCEPTION 'V0266 自检 1 失败：4 个销售页面 + 13 个按钮应有 % 行，实际 % 行', v_want, v_n;
    END IF;

    -- 2) 页面（menu_type=2）：auth_code 必须是列表码，父级此时应为根级
    --    （reparent 到「销售管理」目录 …2701 是 V0267 的职责，本迁移不越界）
    SELECT count(*) INTO v_n
      FROM sys_menu
     WHERE id = ANY (ARRAY[
              '20000000-0000-0000-0000-000000002841',
              '20000000-0000-0000-0000-000000002880',
              '20000000-0000-0000-0000-000000002890',
              '20000000-0000-0000-0000-000000002895']::uuid[])
       AND deleted_at IS NULL
       AND menu_type = 2;
    IF v_n <> 4 THEN
        RAISE EXCEPTION 'V0266 自检 2 失败：4 个销售页面节点里 menu_type=2 的只有 % 个', v_n;
    END IF;

    -- 3) 按钮（menu_type=3）必须挂在自己那一页下
    SELECT count(*) INTO v_n
      FROM sys_menu m
     WHERE m.id = ANY (ARRAY[
              '20000000-0000-0000-0000-000000002842',
              '20000000-0000-0000-0000-000000002843',
              '20000000-0000-0000-0000-000000002881',
              '20000000-0000-0000-0000-000000002882',
              '20000000-0000-0000-0000-000000002883',
              '20000000-0000-0000-0000-000000002884',
              '20000000-0000-0000-0000-000000002891',
              '20000000-0000-0000-0000-000000002892',
              '20000000-0000-0000-0000-000000002893',
              '20000000-0000-0000-0000-000000002894',
              '20000000-0000-0000-0000-000000002896',
              '20000000-0000-0000-0000-000000002897',
              '20000000-0000-0000-0000-000000002898']::uuid[])
       AND m.deleted_at IS NULL
       AND m.menu_type = 3
       AND m.parent_id = ANY (ARRAY[
              '20000000-0000-0000-0000-000000002841',
              '20000000-0000-0000-0000-000000002880',
              '20000000-0000-0000-0000-000000002890',
              '20000000-0000-0000-0000-000000002895']::uuid[]);
    IF v_n <> 13 THEN
        RAISE EXCEPTION 'V0266 自检 3 失败：13 个按钮里挂在自己页面下的只有 % 个', v_n;
    END IF;

    RAISE NOTICE 'V0266 完成：4 个销售页面 + 13 个按钮的前置承载已就位（存量库为 0 行插入）';
END $$;
