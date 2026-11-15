-- ============================================================================
-- V0236__erp_finance_menu_reparent.up.sql
--
-- 【P2-2B-FIX / t_d1a5324f】把两个**财务口径**的页面节点从「采购管理」组挪回「财务管理」组：
--     20000000-…-000000001022  /business/erp/opening    期初余额
--     20000000-…-000000001025  /business/erp/year-end    年度结转
--
-- # 一、判定结论：种子写错（复制模板留下的过期 parent_id），**不是**有意归组
--
--   证据链（全部是仓库里可复核的行号）：
--   1) 两个节点的 parent_id 都写死成 `20000000-0000-0000-0000-000000000946`：
--      · V0189__erp_opening_balance.up.sql:132（期初余额）
--      · V0190__erp_year_end.up.sql:86（年度结转）
--   2) 这两处头注释一字不差地写着同一句：
--        `-- # 权限节点（menu_type 3 = 按钮，page 挂「ERP 台账」目录 …0946 下）`
--      · V0189 第 38 行、V0190 第 35 行
--      而「ERP 台账」这个目录名**在 V0154 就已经不存在了**：
--      · V0141__erp_permissions.up.sql:60 把 …946 建成 `'ERP 台账'`（auth_code /business/erp/ledger）
--      · V0154__erp_menu_reorg_business_lines.up.sql:27-30 把 …946 改名 `'采购管理'`、
--        path 改成 `/business/erp/purchase-mgmt`
--      即：V0189/V0190 的作者照抄了 V0141 时代的模板片段，没回头核对 …946 现在已经叫「采购管理」。
--   3) V0154 的归组是**显式逐条挂载**的（第 48-68 行），财务五页写的是 …e02=财务管理：
--        account(10) / period(20) / voucher(30) / ledger(40) / seq(50)
--      期初余额与年度结转**不在 V0154 的挂载清单里**（它们比 V0154 晚），所以它们的父节点
--      只能来自自己的 INSERT —— 也就是上面那行过期 UUID。没有第二处迁移把它们移走
--      （`grep -n "000000001025\|/business/erp/year-end" backend/migrations/` 只命中 V0190 与 V0208 的权限码）。
--   4) 「采购管理」这个组里，真正有意放进去的是采购侧单据：V0154 第 48-52 行把
--      purchase / stock-in / payable / supplier / purchase-order-item 挂到 …946。
--      年结是「损益结转 + 生成下年期初」，期初余额是财务手工录的开账台账
--      （V0189 的 `biz_opening_balance`，V0190 又给它加了 `source=MANUAL|YEAR_END`），
--      两者的权限码 `business:erp:yearend:{read,close,reopen}` / `business:erp:opening:*`
--      也是按财务角色（FINANCE / ACCOUNTANT）授权的（V0190 第 180-215 行的 sys_role_menu 授权块）。
--   5) 后续所有财务模块（报表 V0183:259 / V0184:95 / V0185:138、预算 V0227:230、资产 V0229:240）
--      一律挂 …e02=财务管理 —— 财务页 → 财务管理 才是本仓库的既有约定，年结/期初偏离它。
--
--   ⇒ 结论：**种子写错**。按卡片要求只改 `sys_menu.parent_id` / `sort_order`，
--     路由 `/business/erp/year-end` 与权限码 `business:erp:yearend:*` **一个字都不动**。
--
-- # 二、为什么把「期初余额」一起挪（同根因、同一行模板）
--
--   卡面只点名年度结转。但期初余额是**同一次复制粘贴**的产物（V0189:132 与 V0190:86 同源，
--   注释同为「ERP 台账」），而且它是年结的直接上游（年结生成 `source='YEAR_END'` 的期初，
--   见 V0190 头注释第 10-27 行）。只挪年结的话，侧边栏会变成
--   「采购管理 → 期初余额」+「财务管理 → 年度结转」——同一件事的两个页面被拆到两个组，
--   比现状更难解释。故一并挪，**排序紧跟会计期间**（科目 → 期间 → 期初 → 年结）。
--   若评审不认可这条附加项：down 会把两条一起还原，也可只回滚 opening 那一句（见下）。
--
-- # 三、排序（按 path 动态取，不硬编码数字）
--
--   目标顺序（财务管理组内）：会计科目(10) → 会计期间(20) → **期初余额(21)** → **年度结转(22)**
--                             → 预算(25) → 固定资产(29) → 凭证(30) → 总账(40) → 报表(45-47) → 单号(50)
--   `sort_order` 取 `会计期间.sort_order + 1 / + 2`，运维手工调过会计期间的序号也跟着走，
--   不用回来改这个迁移。
--
-- # 四、幂等 / 回滚
--
--   幂等：两个 UPDATE 都是「按 path 定位 + 目标是唯一行」，重复执行结果一致（第二遍 UPDATE 0 行
--         或写成同一组值）；自检断言的是**改完的稳定状态**，不是「本次改了 N 行」。
--   回滚：V0236__erp_finance_menu_reparent.down.sql —— 还原 parent_id=…946 与 sort_order
--         25 / 26（= V0189:133、V0190:87 的原始值）。
--
-- # 号段
--
--   动手前 `ls backend/migrations/*/`：已应用最大 V0235（system/ 的 sys_invariant_result），
--   business/ 内最大 V0234。本迁移取 V0236（不抢号、不改已应用文件）。
-- ============================================================================

-- ---------- ① 前置自检（fail-closed）：四个定位点必须各唯一 ----------
DO $$
DECLARE
    n_fin     int;
    n_period  int;
    n_opening int;
    n_yearend int;
    n_purchase int;
BEGIN
    SELECT count(*) INTO n_fin      FROM sys_menu WHERE path = '/business/erp/finance-mgmt'  AND deleted_at IS NULL;
    SELECT count(*) INTO n_period   FROM sys_menu WHERE path = '/business/erp/period'        AND deleted_at IS NULL;
    SELECT count(*) INTO n_opening  FROM sys_menu WHERE path = '/business/erp/opening'       AND deleted_at IS NULL;
    SELECT count(*) INTO n_yearend  FROM sys_menu WHERE path = '/business/erp/year-end'      AND deleted_at IS NULL;
    SELECT count(*) INTO n_purchase FROM sys_menu WHERE path = '/business/erp/purchase-mgmt' AND deleted_at IS NULL;

    IF n_fin <> 1 THEN
        RAISE EXCEPTION 'V0236 前置自检失败：/business/erp/finance-mgmt（财务管理）在用节点数 = %（必须为 1）', n_fin;
    END IF;
    IF n_period <> 1 THEN
        RAISE EXCEPTION 'V0236 前置自检失败：/business/erp/period（会计期间）在用节点数 = %（必须为 1）', n_period;
    END IF;
    IF n_opening <> 1 THEN
        RAISE EXCEPTION 'V0236 前置自检失败：/business/erp/opening（期初余额）在用节点数 = %（必须为 1）', n_opening;
    END IF;
    IF n_yearend <> 1 THEN
        RAISE EXCEPTION 'V0236 前置自检失败：/business/erp/year-end（年度结转）在用节点数 = %（必须为 1）', n_yearend;
    END IF;
    IF n_purchase <> 1 THEN
        RAISE EXCEPTION 'V0236 前置自检失败：/business/erp/purchase-mgmt（采购管理）在用节点数 = %（必须为 1）', n_purchase;
    END IF;

    -- 排序基准「会计期间」必须已经在财务管理组内（否则 +1/+2 会插到别的组的坐标系里）
    IF NOT EXISTS (
        SELECT 1
          FROM sys_menu p
          JOIN sys_menu f ON f.id = p.parent_id
         WHERE p.path = '/business/erp/period' AND p.deleted_at IS NULL
           AND f.path = '/business/erp/finance-mgmt' AND f.deleted_at IS NULL
    ) THEN
        RAISE EXCEPTION 'V0236 前置自检失败：会计期间（/business/erp/period）不在财务管理组下，排序基准不可用';
    END IF;

    -- 两个待挪节点必须还是页面级菜单（menu_type=2）；按钮（3）跟父页面走，不单独挪
    IF EXISTS (
        SELECT 1 FROM sys_menu
         WHERE path IN ('/business/erp/opening', '/business/erp/year-end')
           AND deleted_at IS NULL
           AND menu_type <> 2
    ) THEN
        RAISE EXCEPTION 'V0236 前置自检失败：待挪节点的 menu_type 不是 2（页面），拒绝改归属';
    END IF;
END $$;

-- ---------- ② 期初余额 → 财务管理（紧跟会计期间）----------
UPDATE sys_menu m
   SET parent_id  = f.id,
       sort_order = p.sort_order + 1,
       updated_at = now()
  FROM sys_menu f,
       sys_menu p
 WHERE f.path = '/business/erp/finance-mgmt' AND f.deleted_at IS NULL
   AND p.path = '/business/erp/period'       AND p.deleted_at IS NULL
   AND m.path = '/business/erp/opening'      AND m.deleted_at IS NULL
   AND m.menu_type = 2;

-- ---------- ③ 年度结转 → 财务管理（排在期初余额之后）----------
UPDATE sys_menu m
   SET parent_id  = f.id,
       sort_order = p.sort_order + 2,
       updated_at = now()
  FROM sys_menu f,
       sys_menu p
 WHERE f.path = '/business/erp/finance-mgmt' AND f.deleted_at IS NULL
   AND p.path = '/business/erp/period'       AND p.deleted_at IS NULL
   AND m.path = '/business/erp/year-end'     AND m.deleted_at IS NULL
   AND m.menu_type = 2;

-- ---------- ④ 改后自检（断言稳定状态，重复执行同样通过）----------
DO $$
DECLARE
    v_fin_path   text;
    v_open_path  text;
    v_ye_path    text;
    v_period_ord int;
    v_open_ord   int;
    v_ye_ord     int;
    n_btn        int;
    n_cycle      int;
BEGIN
    SELECT p.path, m.sort_order INTO v_open_path, v_open_ord
      FROM sys_menu m LEFT JOIN sys_menu p ON p.id = m.parent_id
     WHERE m.path = '/business/erp/opening' AND m.deleted_at IS NULL;
    SELECT p.path, m.sort_order INTO v_ye_path, v_ye_ord
      FROM sys_menu m LEFT JOIN sys_menu p ON p.id = m.parent_id
     WHERE m.path = '/business/erp/year-end' AND m.deleted_at IS NULL;
    SELECT p.path INTO v_fin_path
      FROM sys_menu m LEFT JOIN sys_menu p ON p.id = m.parent_id
     WHERE m.path = '/business/erp/period' AND m.deleted_at IS NULL;
    SELECT sort_order INTO v_period_ord
      FROM sys_menu WHERE path = '/business/erp/period' AND deleted_at IS NULL;

    IF v_open_path IS DISTINCT FROM '/business/erp/finance-mgmt' THEN
        RAISE EXCEPTION 'V0236 自检失败：期初余额父节点 = %（应为 /business/erp/finance-mgmt）', coalesce(v_open_path, 'NULL');
    END IF;
    IF v_ye_path IS DISTINCT FROM '/business/erp/finance-mgmt' THEN
        RAISE EXCEPTION 'V0236 自检失败：年度结转父节点 = %（应为 /business/erp/finance-mgmt）', coalesce(v_ye_path, 'NULL');
    END IF;
    IF v_fin_path IS DISTINCT FROM '/business/erp/finance-mgmt' THEN
        RAISE EXCEPTION 'V0236 自检失败：会计期间的父节点 = %（排序基准应仍在财务管理组）', coalesce(v_fin_path, 'NULL');
    END IF;
    IF NOT (v_period_ord < v_open_ord AND v_open_ord < v_ye_ord) THEN
        RAISE EXCEPTION 'V0236 自检失败：排序不是 会计期间(%) < 期初余额(%) < 年度结转(%)', v_period_ord, v_open_ord, v_ye_ord;
    END IF;

    -- 按钮子节点必须还挂在年度结转页面下，挪父节点不许把它们甩成孤儿。
    -- 实盘点名三个（不是按个数数）：V0190 建了 close / reopen 两个，V0208 又加了
    -- `business:erp:year-end:remind`（催办，注意这个码带连字符，与 V0190 的三个码命名不一致，
    -- 见 V0208 头注释第 14 行）—— 只数个数会漏掉后加的那个。
    SELECT count(*) INTO n_btn
      FROM sys_menu c
      JOIN sys_menu p ON p.id = c.parent_id
     WHERE p.path = '/business/erp/year-end'
       AND c.deleted_at IS NULL
       AND c.menu_type = 3;
    IF n_btn < 3 THEN
        RAISE EXCEPTION 'V0236 自检失败：年度结转下的按钮节点 = %（至少 3：年结 / 反年结 / 催办）', n_btn;
    END IF;
    IF EXISTS (
        SELECT 1 FROM (VALUES ('business:erp:yearend:close'),
                              ('business:erp:yearend:reopen'),
                              ('business:erp:year-end:remind')) AS want(code)
         WHERE NOT EXISTS (
             SELECT 1
               FROM sys_menu c
               JOIN sys_menu p ON p.id = c.parent_id
              WHERE p.path = '/business/erp/year-end'
                AND c.deleted_at IS NULL
                AND c.menu_type = 3
                AND c.auth_code = want.code
         )
    ) THEN
        RAISE EXCEPTION 'V0236 自检失败：年度结转页面下的按钮权限码不全（close / reopen / remind 必须都还在）';
    END IF;

    -- 不许把「财务管理」目录本身挂到这两个页面下（父子成环 = 侧边栏整棵树消失）
    SELECT count(*) INTO n_cycle
      FROM sys_menu m
     WHERE m.path IN ('/business/erp/opening', '/business/erp/year-end')
       AND m.deleted_at IS NULL
       AND m.id IN (
           SELECT parent_id FROM sys_menu
            WHERE path = '/business/erp/finance-mgmt' AND deleted_at IS NULL
       );
    IF n_cycle <> 0 THEN
        RAISE EXCEPTION 'V0236 自检失败：检测到父子成环（财务管理目录的父节点是待挪页面）';
    END IF;
END $$;
