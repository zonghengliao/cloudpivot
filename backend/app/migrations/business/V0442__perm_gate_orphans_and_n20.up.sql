-- ============================================================================
-- V0442__perm_gate_orphans_and_n20（FIX-权限闸 · 孤儿按钮 4 对 + N20 六码零授予 · 2026-11-17）
--
-- 卡：t_fa41e0e1（FIX-权限闸：M1-23 账龄页面码死码 + 4 对孤儿按钮）
-- 上游：M2-04（t_f1bce846）施工时在**空库首启路径**上实测到两道闸的既有红，本卡收口。
--       其中 N14（死权限码）已由 M2-07（t_10d54ac1，51afe645）补例外台账后转绿；
--       本迁移只收剩下两件：**孤儿按钮 4 对** + **N20 六码零授予**（见 ④ 的来由）。
--
-- 【闸的判据（先读，否则会以为本迁移在「绕闸」）】
--   · `scripts/verify/orphan_button_gate.py`：对每个（角色,码），把**该角色被授的按钮节点**
--     逐层向上走祖先链；链上只要有一个「带码祖先」的码没授给该角色 ⇒ 该节点不可达；
--     该码的**全部**按钮节点都不可达 ⇒ 判红（真孤儿）。修法只有两条：
--     ① 该角色确实该看该页面 ⇒ 补授父页码；② 不该看 ⇒ 按钮挂到别的可达父页（V0259 范本）。
--   · `permission_audit_tests::n20`：`business:erp:<资源>:(create|list|status)` 形态的核心码
--     **必须至少授予 1 个非超管角色**（超管 `*:*:*` 短路，授了等于没授）。
--   · `scripts/verify/role_code_gate.py`：迁移授权段里不许出现**生产不存在的角色码**
--     （`WHERE role_code IN ('ADMIN','TEST_UADMIN',…)` 会静默 0 行 = 授权从没发生）。
--     本迁移因此**不写任何非生产角色字面量**：能数据驱动的一律数据驱动（见 ③ A1/A2）。
--
-- 【本迁移做四件事，逐条写明选的是哪条修法、为什么】
--
-- ① 父页归位（闸的修法 ②，零授权变化）：`business:notice:mine` 的按钮节点 …414
--    「我的通知 / 未读数 / 已读」现在挂在「消息通知」页（…410，码 business:notice:list）下，
--    但它语义上属于「消息中心」页（…0010）—— 而 …0010 的页面行挂的**就是** business:notice:mine
--    （同一码：页面显形 + 该页三个接口）。
--    实测（装配库）：持有 business:notice:mine 的 7 个角色里 5 个同时有 business:notice:list，
--    所以闸不判红；`DEV_MONITOR_VIEWER`（只读监控）与 `TEST_UADMIN`（装配库遗留验收角色）
--    只给了「消息中心」这一页 ⇒ 按钮挂在它们看不见的模板管理页下。
--    修法取舍：
--      · 补授 business:notice:list 给这两个角色 = 把「消息通知」模板管理页（含新建/修改/删除
--        模板三个按钮）送给只读监控角色 —— 明显过头，否决；
--      · 把 …414 归位到它真正所属的「消息中心」页 = 零授权变化，且 7 个持有者**全部**已持有
--        …0010（自检 b 用结构断言钉死）⇒ 归位后一律可达。取这条。
--
-- ② 补授父页码（闸的修法 ①），两对都走**数据驱动**（不写角色名，客户库同样成立）：
--    · `FINANCE_DIRECTOR` × business:erp:receivable:aging → 补「应收台账」页
--      business:erp:receivable:list（…2880）：
--      凡是**持有账龄取数码**的角色，就把承载它的台账页一并授出 —— 这正是「调得动 ⇒ 看得到」。
--      为什么不挪按钮：取数码 …2882 是「应收台账」页里的账龄看板 tab（M1-23 V0418 头注释写明的
--      既有口径），台账页就是它的家；挪到 V0418 新建的报表页 …1830 会让**只持有台账页**的角色
--      （SALES_SUPERVISOR）变成新孤儿，等于把缺陷挪个位置。而 FINANCE_DIRECTOR 在 V0418 里拿到了
--      「应收账龄分析」报表页 + 取数码，却没有台账页 ⇒ 它调得动接口、看不见挂载点，
--      本来就是「补页面」的事，不是「挪按钮」的事。
--    · `TEST_UADMIN` × system:role-mutex:conflict → 补「角色互斥」页 system:role-mutex:list
--      （…620）：凡是持有「冲突用户预检」按钮码的角色，就把它的父页授出。
--      角色互斥只有一个页面，按钮没有第二个可归位的父页 ⇒ 只能补授父页码。
--    `TEST_UADMIN` 的去留：**本迁移不删角色**。它是装配库里的遗留角色（`docs/architecture/
--    P2-新装配库权限矩阵-决策.md` §2.2 已列明「生产不存在的历史角色码」），装配库有 1 个账号
--    绑着它、它持有 87 个码（含 system:user:* 整套）。删角色会连带账号、越出「修闸」的范围
--    ⇒ 只按闸的建议补授父页码；是否从装配数据下线另开卡决定。
--
-- ③ 组合与顺序
--    A1 持有 aging 取数码 ⇒ 授「应收台账」页（…2880）
--    A2 持有 conflict 按钮码 ⇒ 授「角色互斥」页（…620）
--    B  N20 六码矩阵（角色是生产真角色，见 ④ 的表）
--    C  祖先闭包（见 ③ 之后那段）
--
-- 【③ 祖先闭包为什么必做（否则上面的补授是纸面修复）】
--   `system/auth/handler.rs::build_menu_tree` 用 `utils::build_tree` 建树，
--   **父节点不在授权集合里的子节点会被静默丢弃**（不进 roots、也没人 attach）。
--   实测：装配库里「系统管理」（…0001）与「销售管理」（…2701）在补授前**授权行为 0 或不全**
--   —— 这意味着 TEST_UADMIN 早就持有的「用户管理」页（system:user:list，父 = …0001）
--   一直没在侧栏出现过。所以补授页面码必须连带补父目录（通用写法，按「谁持有该页面行」自愈）。
--
-- 【④ N20 六码零授予（本卡体检时新红的既有缺陷，属 M3 报价/返利三卡）】
--    `cargo test --lib system::menu::permission_audit_tests` 实测 7 通过 / 1 失败：
--      business:erp:quotation:{create,list,status}   （V0433 M3-02 报价单）
--      business:erp:rebate-policy:{create,list}      （V0438 M3-15 返利政策）
--      business:erp:rebate-settlement:list           （V0439 M3-16 返利结算）
--    三条迁移都**刻意不写** `sys_role_menu`（原文：「授给哪些角色是管理决策，不静默扩权」），
--    与 N20 的「核心码必须至少授予 1 个非超管角色」直接冲突 —— 冲突总得有人收口，本卡就是那个人。
--    授权口径（**只补 N20 点名的 6 个码，update/delete/convert 一律不动**）：见 ③B 的 VALUES 注释。
--
-- 【本迁移不新增任何 sys_menu 行】
--   装配库活跃菜单 540 行，正顶在 `menu_invariants.sh` 第 ⑤ 条的上界（400–540）——
--   新增一行就判红。本迁移只做 UPDATE（① 改 parent_id）+ INSERT sys_role_menu（②③④）。
--
-- 【2026-11-17 修订（t_08e1816a，P0）：空库首启不得依赖种子行】
--   现象：`ERP_DB=<新库> make_full_suite_db.sh` 在 V0442 处启动失败
--   （内核只报 `query failed: : db error`；手工 psql 应用本文件才看到
--    `V0442: 祖先目录「系统管理」(1000…0001) 不存在`）。实测失败那一刻
--   `schema_migrations` 里 `kind='versioned'` 361 条、`kind='seed'` 0 条。
--   真因：内核执行次序是 **versioned 先、seed 后**（kind.rs::execution_rank），
--   而 …0001 只由种子 S__menu_data.sql 种下 ⇒ 任何空库首启必炸，
--   连带 make_full_suite_db.sh（全量套件标准库）/L3 干净检出闸/新客户部署全被挡。
--   改法（只改本文件，不加 V0443 —— 442 先炸，443 永远轮不到）：
--     · ③ 的「系统管理」护栏从「硬 RAISE」改为「缺席则 NOTICE + 跳过该层闭包」，
--       并把 ④ 祖先闭包**本来就是动态 JOIN** 这一点写进注释（本迁移不写死 0001）；
--     · 「销售管理」(…2701) 不变：它由 V0267（版本化）建出，空库首启确实存在。
--   为什么不在这条迁移里 INSERT 出 …0001：base 菜单行的唯一真相源是种子
--   （`ON CONFLICT (id) DO UPDATE` 自愈），在版本化侧再写一份等于制造第二个真相源；
--   而实测空库首启路径上没有任何角色会因该目录缺席而拿到「缺祖先的授权」
--   （见 evidence：missing_ancestor_pairs=0），所以「跳过」不等于「留下隐患」。
--   ⚠️ 代价：本文件（已应用迁移）被改写 ⇒ 内容指纹随之变化（新值不写死在本文件里，
--   由 `scripts/datafix/v0442-checksum/apply.sh` 现算，避免「改文件→指纹变→再改文件」的自指）。
--   已应用过旧版的库（当时只有 cloudpivot_dev）必须先跑
--   `scripts/datafix/v0442-checksum/apply.sh`，否则启动即 `MigrationChecksum` 失败。
--   规矩：**版本化迁移不得把「seed 行」当作前置条件**，静态闸
--   `scripts/lint/seed_dependency_gate.py`（L0）会抓这一类。
--
-- 【幂等】① `WHERE parent_id IS DISTINCT FROM …` ⇒ 重放 0 行；
--   ②③④ 的每一行都有**确定性 id**（`v0442.<role_id>:<menu_id>`）且插入前
--   `NOT EXISTS` 按「角色 + 码」判重（V0258 §4 口径：同一码可挂多节点，按码判重才不会重复授）。
--   重放不修改任何已存在的 title / path / sort_order / 父节点。
--
-- 【回滚】down 恢复 …414 的原父节点与 sort_order（40），并按同一套确定性 id 精确删除
--   本迁移插入的授权行（角色侧不按名单枚举 ⇒ 数据驱动那两段也能完整回滚）。
--   回滚 = 4 对孤儿与 N20 六红回到迁移前状态（不是关功能：接口、菜单、码都还在）。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：表 / 关键节点 / 六个 N20 码都在，且形状与预期一致
--    （宁可启动失败，也不要「改了一半」—— 尤其 parent_id 归位动的是存量行）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n integer;
BEGIN
    IF to_regclass('public.sys_menu') IS NULL THEN
        RAISE EXCEPTION 'V0442: sys_menu 不存在，请先应用系统底座迁移';
    END IF;
    IF to_regclass('public.sys_role_menu') IS NULL THEN
        RAISE EXCEPTION 'V0442: sys_role_menu 不存在，请先应用系统底座迁移';
    END IF;

    -- 「消息中心」页（…0010，码 business:notice:mine）—— ① 的落点
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '23000000-0000-0000-0000-000000000010'::uuid
       AND menu_type = 2
       AND auth_code = 'business:notice:mine'
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0442: 「消息中心」页 (2300…0010) 不存在或不是 business:notice:mine 的页面行，先应用 notice 系列迁移';
    END IF;

    -- `business:notice:mine` 的按钮节点（…414）—— ① 要归位的那一行
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000000414'::uuid
       AND menu_type = 3
       AND auth_code = 'business:notice:mine'
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0442: business:notice:mine 的按钮节点 (2000…0414) 不存在或形状不符（应为 menu_type=3），先应用 notice 菜单迁移';
    END IF;

    -- ② 的两个父页
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000002880'::uuid
       AND menu_type = 2
       AND auth_code = 'business:erp:receivable:list'
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0442: 「应收台账」页 (2000…2880) 不存在或不是 business:erp:receivable:list 的页面行，先应用 V0288';
    END IF;

    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000000620'::uuid
       AND menu_type = 2
       AND auth_code = 'system:role-mutex:list'
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0442: 「角色互斥」页 (2000…0620) 不存在或不是 system:role-mutex:list 的页面行，先应用系统菜单迁移';
    END IF;

    -- ③ 祖先目录（无码目录，menu_type=1）：销售管理来自 V0267（版本化）⇒ 必须存在；
    --    系统管理只由**种子** S__menu_data.sql 种下 ⇒ 空库首启时必然缺席，见下方说明。
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '10000000-0000-0000-0000-000000000001'::uuid
       AND menu_type = 1
       AND deleted_at IS NULL;
    IF n = 0 THEN
        -- ⚠️ 空库首启的正常路径，不是异常：
        --   内核执行次序 fixed 为 baseline(0) → versioned(1) → repeatable(2) → seed(3)
        --   （vendor/nik-db/src/migrate/kind.rs::execution_rank），而「系统管理」(…0001)
        --   这一行**只**由种子迁移 migrations/seed/S__menu_data.sql 种下
        --   （`grep -rln 10000000-0000-0000-0000-000000000001 migrations/` 的版本化侧
        --   只有 V0319 的 UPDATE parent_id 与 V0322 的 parent_id 引用，都不建行）。
        --   ⇒ 任何空库首启 / 新环境 / 演练库跑到本迁移时，这一行一定还不存在。
        --
        --   原版这里硬 RAISE EXCEPTION，代价是【新库根本起不来】（P0，t_08e1816a 实测：
        --   make_full_suite_db.sh 造库失败、bootstrap 跑不完、库里没有 admin+SUPER_ADMIN，
        --   而内核只打 `query failed: : db error` 把真因吞掉）。
        --
        --   本迁移对该行**没有写死依赖**：④ 的祖先闭包是 `JOIN sys_menu dir ON dir.id = p.parent_id`
        --   的动态形态（不写死 0001），⑤c 的 touched 集合也由确定性 id 反查得到 —— 目录缺席时
        --   这两段自然授不到任何行（0 行 = 空操作），不会产生半成品。
        --   种子随后建出该行；本迁移的目标角色（持有「角色互斥」页 …0620 的角色）在
        --   **已有种子的库**（装配库 / 生产）上仍然照常补授这层目录。
        RAISE NOTICE 'V0442：祖先目录「系统管理」(1000…0001) 尚不存在（空库首启：种子迁移排在版本化迁移之后）→ 跳过该层祖先闭包，不失败';
    END IF;

    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000002701'::uuid
       AND menu_type = 1
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0442: 祖先目录「销售管理」(2000…2701) 不存在，先应用 V0265 系列销售迁移';
    END IF;

    -- ④ 六个 N20 码必须各有恰好 1 个承载行
    SELECT count(*) INTO n
      FROM (VALUES
              ('business:erp:quotation:list'),
              ('business:erp:quotation:create'),
              ('business:erp:quotation:status'),
              ('business:erp:rebate-policy:list'),
              ('business:erp:rebate-policy:create'),
              ('business:erp:rebate-settlement:list')
           ) AS w(auth_code)
     WHERE (SELECT count(*) FROM sys_menu m
             WHERE m.auth_code = w.auth_code AND m.deleted_at IS NULL) <> 1;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0442: % 个 N20 码的承载行不是恰好 1 行（先应用 V0433 / V0438 / V0439）', n;
    END IF;

    RAISE NOTICE 'V0442: 前置接点齐备（消息中心 / 应收台账 / 角色互斥页 + 销售管理目录 + 六个 N20 码；系统管理目录按空库首启情况可能缺席）';
END $$;

-- ---------------------------------------------------------------------------
-- ② 父页归位：business:notice:mine 的按钮 …414「消息通知」…410 → 「消息中心」…0010
--    sort_order 取同父最大值 +10（当前「消息中心」下无子节点 ⇒ 落 10；将来若有子节点也不撞号，
--    约定 12：同父 sort 必须唯一）。重放时父节点已是 …0010 ⇒ 0 行，不动 sort_order。
-- ---------------------------------------------------------------------------
UPDATE sys_menu m
   SET parent_id = '23000000-0000-0000-0000-000000000010'::uuid,
       sort_order = COALESCE(
                        (SELECT max(s.sort_order)
                           FROM sys_menu s
                          WHERE s.parent_id = '23000000-0000-0000-0000-000000000010'::uuid
                            AND s.deleted_at IS NULL
                            AND s.id <> m.id),
                        0) + 10,
       updated_at = now()
 WHERE m.id = '20000000-0000-0000-0000-000000000414'::uuid
   AND m.parent_id IS DISTINCT FROM '23000000-0000-0000-0000-000000000010'::uuid;

-- ---------------------------------------------------------------------------
-- ③ A1 数据驱动补授：「持有应收账龄取数码的角色」⇒ 授「应收台账」页（business:erp:receivable:list）
--    不写角色名：客户库/装配库同一套规则都成立；按「角色 + 码」判重（已持有该码则跳过）。
-- ---------------------------------------------------------------------------
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT DISTINCT
       md5('v0442.' || rm.role_id || ':' || pg.id)::uuid,
       rm.role_id, pg.id
  FROM sys_role_menu rm
  JOIN sys_role r   ON r.id = rm.role_id AND r.deleted_at IS NULL
  JOIN sys_menu src ON src.id = rm.menu_id AND src.deleted_at IS NULL
                   AND src.auth_code = 'business:erp:receivable:aging'
  JOIN sys_menu pg  ON pg.id = '20000000-0000-0000-0000-000000002880'::uuid
                   AND pg.deleted_at IS NULL
 WHERE NOT EXISTS (
        -- 按码判重：角色已持有 business:erp:receivable:list（任一行）则跳过
        SELECT 1
          FROM sys_role_menu x
          JOIN sys_menu mx ON mx.id = x.menu_id AND mx.deleted_at IS NULL
         WHERE x.role_id = rm.role_id
           AND mx.auth_code = 'business:erp:receivable:list'
   )
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu y
         WHERE y.id = md5('v0442.' || rm.role_id || ':' || pg.id)::uuid
   );

-- ---------------------------------------------------------------------------
-- ③ A2 数据驱动补授：「持有冲突预检按钮码的角色」⇒ 授「角色互斥」页（system:role-mutex:list）
-- ---------------------------------------------------------------------------
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT DISTINCT
       md5('v0442.' || rm.role_id || ':' || pg.id)::uuid,
       rm.role_id, pg.id
  FROM sys_role_menu rm
  JOIN sys_role r   ON r.id = rm.role_id AND r.deleted_at IS NULL
  JOIN sys_menu src ON src.id = rm.menu_id AND src.deleted_at IS NULL
                   AND src.auth_code = 'system:role-mutex:conflict'
  JOIN sys_menu pg  ON pg.id = '20000000-0000-0000-0000-000000000620'::uuid
                   AND pg.deleted_at IS NULL
 WHERE NOT EXISTS (
        SELECT 1
          FROM sys_role_menu x
          JOIN sys_menu mx ON mx.id = x.menu_id AND mx.deleted_at IS NULL
         WHERE x.role_id = rm.role_id
           AND mx.auth_code = 'system:role-mutex:list'
   )
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu y
         WHERE y.id = md5('v0442.' || rm.role_id || ':' || pg.id)::uuid
   );

-- ---------------------------------------------------------------------------
-- ③ B  N20 六码矩阵（角色全部是**生产真角色**，role_code_gate 白名单内）
--    | 码 | 角色 | 依据 |
--    |----|------|------|
--    | quotation:list   | SALES_CLERK / SALES_SUPERVISOR / FINANCE_MANAGER / GENERAL_MANAGER | 同型单据销售订单（V0265）的 sale-order:list 给了 SALES_CLERK+SALES_SUPERVISOR+FINANCE_MANAGER+GENERAL_MANAGER+WAREHOUSE_MANAGER；报价单是成交前的单，仓库不需要 ⇒ 去掉 WAREHOUSE_MANAGER。FINANCE_MANAGER 是 V0435 报价审批流「财务经理复核」节点的指派人（看不见单就审不了） |
--    | quotation:create | SALES_CLERK / SALES_SUPERVISOR | 与 sale-order:create 完全同款（同两个角色） |
--    | quotation:status | SALES_CLERK / SALES_SUPERVISOR | 与 sale-order:status 同款；**审批动作不靠这个码**：审批走 business:flow:approve（V0435 绑 SALES_SUPERVISOR + FINANCE_MANAGER），SALES_CLERK 一个 approve 都没有（自检 e 钉死）⇒ 建单与审批分属不同角色 |
--    | rebate-policy:list   | SALES_SUPERVISOR / SALES_CLERK / FINANCE_MANAGER | V0438 头注释原话「销售经理建政策 / 销售员只读」；财务要算返利，只读政策是前提 |
--    | rebate-policy:create | SALES_SUPERVISOR | 政策由销售经理维护（同上） |
--    | rebate-settlement:list | SALES_SUPERVISOR / FINANCE_MANAGER | 结算单 = 期间净销售 × 命中档算出的应返金额，销售经理与财务经理都要看；**不给** SALES_CLERK（返利金额不该下到业务员） |
--    这 6 个码 0 行授权时除超管外**全体 403**：报价单建不了、审批人看不见单、返利政策没人能建
--    —— 不是「少配几个勾」，是功能上线即死锁（N20 头注释的 E2E-02 现场同类）。
-- ---------------------------------------------------------------------------
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT DISTINCT
       md5('v0442.' || r.id || ':' || m.id)::uuid,
       r.id, m.id
  FROM sys_role r
  JOIN (VALUES
            ('SALES_CLERK',      'business:erp:quotation:list'),
            ('SALES_SUPERVISOR', 'business:erp:quotation:list'),
            ('FINANCE_MANAGER',  'business:erp:quotation:list'),
            ('GENERAL_MANAGER',  'business:erp:quotation:list'),
            ('SALES_CLERK',      'business:erp:quotation:create'),
            ('SALES_SUPERVISOR', 'business:erp:quotation:create'),
            ('SALES_CLERK',      'business:erp:quotation:status'),
            ('SALES_SUPERVISOR', 'business:erp:quotation:status'),
            ('SALES_SUPERVISOR', 'business:erp:rebate-policy:list'),
            ('SALES_CLERK',      'business:erp:rebate-policy:list'),
            ('FINANCE_MANAGER',  'business:erp:rebate-policy:list'),
            ('SALES_SUPERVISOR', 'business:erp:rebate-policy:create'),
            ('SALES_SUPERVISOR', 'business:erp:rebate-settlement:list'),
            ('FINANCE_MANAGER',  'business:erp:rebate-settlement:list')
       ) AS w(rc, ac) ON w.rc = r.role_code
  JOIN sys_menu m ON m.auth_code = w.ac AND m.deleted_at IS NULL
 WHERE r.deleted_at IS NULL
   AND NOT EXISTS (
        -- 按码判重：角色已持有该码（任一行）则跳过
        SELECT 1
          FROM sys_role_menu x
          JOIN sys_menu mx ON mx.id = x.menu_id AND mx.deleted_at IS NULL
         WHERE x.role_id = r.id AND mx.auth_code = w.ac
   )
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu y
         WHERE y.id = md5('v0442.' || r.id || ':' || m.id)::uuid
   );

-- ---------------------------------------------------------------------------
-- ④ 祖先闭包：凡持有本迁移涉及的页面行的角色，补其父目录（缺席者才插）
--    涉及页面行：应收台账 …2880（父=销售管理）/ 角色互斥 …620（父=系统管理）/
--                报价单 …4331（父=销售管理）/ 返利政策 …4380（父=销售管理）
--    通用写法（不写死角色名单）：按「当前持有这些页面行」反查，重放零变化。
-- ---------------------------------------------------------------------------
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT DISTINCT
       md5('v0442c.' || rm.role_id || ':' || dir.id)::uuid,
       rm.role_id, dir.id
  FROM sys_role_menu rm
  JOIN sys_menu p   ON p.id = rm.menu_id AND p.deleted_at IS NULL
  JOIN sys_menu dir ON dir.id = p.parent_id AND dir.deleted_at IS NULL
  JOIN sys_role r   ON r.id = rm.role_id AND r.deleted_at IS NULL
 WHERE p.id IN (
        '20000000-0000-0000-0000-000000002880'::uuid,   -- 应收台账（父=销售管理）
        '20000000-0000-0000-0000-000000000620'::uuid,   -- 角色互斥（父=系统管理）
        '20000000-0000-0000-0000-000000004331'::uuid,   -- 报价单（父=销售管理）
        '20000000-0000-0000-0000-000000004380'::uuid    -- 返利政策（父=销售管理）
   )
   AND dir.menu_type = 1
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu x WHERE x.role_id = rm.role_id AND x.menu_id = dir.id
   )
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu y
         WHERE y.id = md5('v0442c.' || rm.role_id || ':' || dir.id)::uuid
   );

-- ---------------------------------------------------------------------------
-- ⑤ 迁移自检（任一不符即整笔回滚）
--   a) 父页归位到位 + 同父按钮 sort 不重号
--   b) 结构断言：business:notice:mine 的按钮节点，其带码祖先只能是 business:notice:mine 本身
--      （还挂在别人的页面下 ⇒ 闸的孤儿判据仍会红）
--   c) 本迁移授过权的角色，不得存在「全部按钮节点不可达」的 (角色,码)
--      —— 用闸同款递归判据，范围只取本迁移动过的角色（按确定性 id 反查，不写角色名单）
--   d) N20：六个码各至少有 1 个非超管持有者（空库首启无业务角色时只打 NOTICE）
--   e) 职责分离：SALES_CLERK 不得持有 business:flow:approve（业务员不能自审自己的报价）
--   f) 本迁移不新增菜单行：business:notice:mine 仍恰好 2 行（1 页面 + 1 按钮）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n           integer;
    n_bad       integer;
    n_roles     integer;
BEGIN
    -- ===== a) …414 已归位到「消息中心」，同父按钮 sort 不重号 =====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000000414'::uuid
       AND parent_id = '23000000-0000-0000-0000-000000000010'::uuid
       AND menu_type = 3
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0442 自检失败：business:notice:mine 的按钮节点没有归位到「消息中心」页';
    END IF;

    SELECT count(*) INTO n
      FROM (SELECT sort_order
              FROM sys_menu
             WHERE parent_id = '23000000-0000-0000-0000-000000000010'::uuid
               AND menu_type = 3
               AND deleted_at IS NULL
             GROUP BY sort_order
            HAVING count(*) > 1) dup;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0442 自检失败：「消息中心」下有 % 组重复的按钮 sort_order（约定 12）', n;
    END IF;

    -- ===== b) 结构断言：notice:mine 的按钮不得再有「别的码」当祖先 =====
    SELECT count(*) INTO n_bad
      FROM (
        WITH RECURSIVE btn(btn_id, anc_id) AS (
            SELECT m.id, m.parent_id
              FROM sys_menu m
             WHERE m.auth_code = 'business:notice:mine'
               AND m.menu_type = 3
               AND m.deleted_at IS NULL
            UNION
            SELECT b.btn_id, p.parent_id
              FROM btn b
              JOIN sys_menu p ON p.id = b.anc_id AND p.deleted_at IS NULL
        )
        SELECT 1
          FROM btn b
          JOIN sys_menu a ON a.id = b.anc_id
         WHERE coalesce(a.auth_code, '') <> ''
           AND a.auth_code <> 'business:notice:mine'
      ) bad;
    IF n_bad <> 0 THEN
        RAISE EXCEPTION 'V0442 自检失败：business:notice:mine 的按钮仍有 % 个「别的码」祖先（孤儿判据仍会红）', n_bad;
    END IF;

    -- ===== c) 本迁移授过权的角色：不得有「全部按钮节点不可达」的 (角色,码) =====
    SELECT count(*) INTO n_bad
      FROM (
        WITH RECURSIVE touched(role_id) AS (
            SELECT DISTINCT rm.role_id
              FROM sys_role_menu rm
             WHERE (rm.id IN (SELECT md5('v0442.' || r2.id || ':' || m2.id)::uuid
                                FROM sys_role r2
                                CROSS JOIN sys_menu m2
                               WHERE m2.auth_code IN (
                                     'business:erp:receivable:list','system:role-mutex:list',
                                     'business:erp:quotation:list','business:erp:quotation:create',
                                     'business:erp:quotation:status','business:erp:rebate-policy:list',
                                     'business:erp:rebate-policy:create','business:erp:rebate-settlement:list')
                                 AND m2.deleted_at IS NULL)
                    OR rm.id IN (SELECT md5('v0442c.' || r2.id || ':' || d.dir_id)::uuid
                                   FROM sys_role r2
                                  CROSS JOIN (VALUES ('10000000-0000-0000-0000-000000000001'::uuid),
                                                     ('20000000-0000-0000-0000-000000002701'::uuid)) d(dir_id)))
        ),
        btn(role_id, btn_id, code, anc_id) AS (
            SELECT rm.role_id, m.id, m.auth_code, m.parent_id
              FROM sys_role_menu rm
              JOIN touched t ON t.role_id = rm.role_id
              JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
              JOIN sys_menu m ON m.id = rm.menu_id AND m.menu_type = 3 AND m.deleted_at IS NULL
            UNION
            SELECT b.role_id, b.btn_id, b.code, p.parent_id
              FROM btn b
              JOIN sys_menu p ON p.id = b.anc_id AND p.deleted_at IS NULL
        ),
        mark AS (
            SELECT b.role_id, b.btn_id, b.code,
                   bool_or(coalesce(a.auth_code, '') <> '' AND NOT EXISTS (
                       SELECT 1
                         FROM sys_role_menu rm2
                         JOIN sys_menu m2 ON m2.id = rm2.menu_id AND m2.deleted_at IS NULL
                        WHERE rm2.role_id = b.role_id AND m2.auth_code = a.auth_code)) AS blocked
              FROM btn b
              JOIN sys_menu a ON a.id = b.anc_id
             GROUP BY b.role_id, b.btn_id, b.code
        ),
        pair AS (
            SELECT k.role_id, k.code, bool_and(k.blocked) AS all_blocked
              FROM mark k
             GROUP BY k.role_id, k.code
        )
        SELECT r.role_code, p.code
          FROM pair p
          JOIN sys_role r ON r.id = p.role_id AND r.deleted_at IS NULL
         WHERE p.all_blocked
           -- 与闸口径一致：超管持 *:*:* 通配短路，它的孤儿行对功能零影响（闸只报告不判红）
           AND r.role_code <> 'SUPER_ADMIN'
      ) bad;
    IF n_bad <> 0 THEN
        RAISE EXCEPTION 'V0442 自检失败：本迁移动过的角色里仍有 % 组 (角色,码) 是孤儿按钮（必须为 0）', n_bad;
    END IF;

    -- ===== d/e) N20 与职责分离（空库首启 sys_role 为空时跳过）=====
    SELECT count(*) INTO n_roles
      FROM sys_role
     WHERE deleted_at IS NULL
       AND role_code IN ('FINANCE_DIRECTOR', 'TEST_UADMIN', 'SALES_CLERK',
                         'SALES_SUPERVISOR', 'FINANCE_MANAGER', 'GENERAL_MANAGER');

    IF n_roles = 0 THEN
        RAISE NOTICE 'V0442：sys_role 里没有本卡的目标角色（空库首启，迁移先于种子），授权待重放补齐';
    ELSE
        SELECT count(*) INTO n_bad
          FROM (VALUES
                    ('business:erp:quotation:list'),
                    ('business:erp:quotation:create'),
                    ('business:erp:quotation:status'),
                    ('business:erp:rebate-policy:list'),
                    ('business:erp:rebate-policy:create'),
                    ('business:erp:rebate-settlement:list')
               ) AS w(auth_code)
         WHERE NOT EXISTS (
                SELECT 1
                  FROM sys_menu m
                  JOIN sys_role_menu rm ON rm.menu_id = m.id
                  JOIN sys_role r ON r.id = rm.role_id
                                  AND r.deleted_at IS NULL
                                  AND r.role_code <> 'SUPER_ADMIN'
                 WHERE m.auth_code = w.auth_code
                   AND m.deleted_at IS NULL
               );
        IF n_bad <> 0 THEN
            RAISE EXCEPTION 'V0442 自检失败：% 个 N20 核心码仍只授给超管或谁都没授（除超管外全员 403）', n_bad;
        END IF;

        SELECT count(*) INTO n_bad
          FROM sys_role r
          JOIN sys_role_menu rm ON rm.role_id = r.id
          JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
         WHERE r.role_code = 'SALES_CLERK'
           AND r.deleted_at IS NULL
           AND m.auth_code = 'business:flow:approve';
        IF n_bad <> 0 THEN
            RAISE EXCEPTION 'V0442 自检失败：SALES_CLERK 持有 business:flow:approve —— 业务员能自审报价单，职责分离失效';
        END IF;

        -- 本迁移的目标角色必须真的拿到码（数据驱动两段的目标角色按「持有取数码」反查）
        SELECT count(*) INTO n_bad
          FROM (VALUES
                    ('business:erp:receivable:aging'),
                    ('system:role-mutex:conflict')
               ) AS w(btn_code)
         WHERE EXISTS (SELECT 1
                         FROM sys_menu m
                         JOIN sys_role_menu rm ON rm.menu_id = m.id
                         JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
                        WHERE m.auth_code = w.btn_code AND m.deleted_at IS NULL)
           AND NOT EXISTS (
                -- 凡持有按钮码的角色，父页码也必须持有（否则就是 c) 要抓的孤儿）
                SELECT 1
                  FROM sys_menu m
                  JOIN sys_role_menu rm ON rm.menu_id = m.id
                  JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
                  JOIN sys_menu p ON p.id = m.parent_id AND p.deleted_at IS NULL
                  JOIN sys_role_menu pr ON pr.role_id = r.id
                  JOIN sys_menu pm ON pm.id = pr.menu_id AND pm.deleted_at IS NULL
                 WHERE m.auth_code = w.btn_code
                   AND m.deleted_at IS NULL
                   AND coalesce(p.auth_code, '') <> ''
                   AND pm.auth_code = p.auth_code
               );
        IF n_bad <> 0 THEN
            RAISE EXCEPTION 'V0442 自检失败：% 个「有按钮码、无父页码」的组合没被补上（补授规则漏了角色）', n_bad;
        END IF;
    END IF;

    -- ===== f) 本迁移不新增菜单行 =====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE auth_code = 'business:notice:mine'
       AND deleted_at IS NULL;
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0442 自检失败：business:notice:mine 的承载行应为 2（1 页面 + 1 按钮），实得 %', n;
    END IF;

    RAISE NOTICE 'V0442 自检通过：notice:mine 按钮归位「消息中心」+ 账龄/互斥两对孤儿补授父页码 + N20 六码已授（目标角色 % 个）',
        n_roles;
END $$;
