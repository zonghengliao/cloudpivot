-- ============================================================================
-- V0416__cashflow_adjust_item（M1-15 · 现金流底稿：间接法调整项配置 · 2026-11-16）
--
-- 卡：t_ce938a2a（M1-15【M1·财务】现金流底稿：间接法调整项配置）
-- 拆卡依据 `docs/planning/20261113-新卡清单.md` §4 M1 财务 的 M1-15 行。
-- 上游：V0414（M1-13 底稿表）、V0415（M1-14 底稿接口码）；下游 M1-16（由底稿生成现金流量表）。
--
-- 【本卡只做一件事】给「间接法」（净利润 → 经营活动现金流量）建**一张配置表** + 12 行标准
-- 模板 + 两个接口权限码。**不算金额、不出表**（那是 M1-16 / M1-17），也不动 `biz_cashflow_item`
-- （直接法的对方科目分类，V0185）与 `biz_cashflow_worksheet`（底稿金额，V0414）。
--
-- ── 为什么另建一张表（而不是复用 biz_cashflow_item / biz_report_item）────────────
--   ① `biz_cashflow_item`（V0185）是**直接法**的分类规则：一行 = 一个报表明细行 +
--      它接受哪些「对方科目」，`activity` / `direction` 是必填的二维坐标。间接法的调整项
--      不属于任何一个活动（它们全部服务于「经营活动」），也没有 INFLOW / OUTFLOW ——
--      它们表达的是「净利润与经营现金流之间的**差异原因**」（折旧没付现、存货占款…）。
--      把调整项硬塞进 `biz_cashflow_item` 只能给 `activity` / `direction` 编造假值
--      （OPERATING / INFLOW），于是直接法的分类循环会把这些行也当成对方科目规则去匹配，
--      **同一笔业务被算两遍**，而且没有任何测试会发现。
--   ② `biz_report_item`（V0183，资产负债表 / 利润表用）是「按科目余额取数」的一维结构，
--      `side` 只有 ASSET / LIABILITY / EQUITY；间接法要的是「发生额（贷 / 借净额）」
--      与「余额变动（期末 − 期初 / 期初 − 期末）」四选一的取数口径，`side` 表达不了，
--      塞进去就得给报表引擎加分支 —— 那会动到 R1 / R2 的出数路径，本卡不夹带。
--   ③ 间接法与直接法是**两套并行口径**：同一期间可以同时存在「直接法底稿」与
--      「间接法调整项」，两者互不覆盖（M1-16 出表时并列展示，勾稽由 M1-17 验）。
--      两张表各管一件事，谁也别改谁。
--
-- ── 报表形状（`line_formula`）────────────────────────────────────────────────
--   净利润
--   ＋ 资产减值准备
--   ＋ 固定资产折旧
--   ＋ 无形资产摊销
--   ＋ 长期待摊费用摊销
--   ＋ 处置长期资产损失（收益以「−」号填列）
--   ＋ 财务费用
--   ＋ 投资损失（收益以「−」号填列）
--   ＋ 递延所得税调整
--   ＋ 存货的减少（增加以「−」号填列）
--   ＋ 经营性应收项目的减少（增加以「−」号填列）
--   ＋ 经营性应付项目的增加（减少以「−」号填列）
--   ＋ 其他
--   ＝ 经营活动产生的现金流量净额
--
--   起点「净利润」与小计「经营活动产生的现金流量净额」**不是配置行**：前者取利润表
--   （ERP-R2），后者是求和结果。配置表只装中间那 12 行「调整项」—— 让可配置的东西
--   可配置，让算出来的东西算出来，否则用户可以「配置」出一个与利润表对不上的净利润。
--
-- ── 取数口径（`calc_basis`，4 个值，全部是**本期金额**，不是余额）────────────────
--   四选一，写进 CHECK；每一行配一个口径之后，未来的出表代码只需
--   `amount = Σ科目(口径)`，不需要任何行内特判：
--
--   | 口径 | 值 | 什么时候用 |
--   |------|----|------------|
--   | `CREDIT_NET` | 贷方净额（贷 − 借） | 备抵科目计提：累计折旧 1602 / 累计摊销 1702 / 坏账与跌价准备 / 长期待摊费用 1801 |
--   | `DEBIT_NET`  | 借方净额（借 − 贷） | 费用与损失类：财务费用 5603 / 投资收益 5111（损失为正）/ 固定资产清理 1606（净损失为正） |
--   | `BALANCE_INCREASE` | 余额增加额（期末 − 期初） | 经营性**负债**：应付账款 / 应付职工薪酬 / 应交税费 —— 负债增加 = 现金少付 = 加项 |
--   | `BALANCE_DECREASE` | 余额减少额（期初 − 期末） | 经营性**资产**：存货 / 应收账款 —— 资产减少 = 现金多收 = 加项 |
--
--   **为什么没有「加 / 减」符号列**（这是一个决定，别再加回来）：中国企业会计准则的
--   间接法表里，每一行都印「加：」，方向由**数值的正负**表达（「减少（增加以「−」号
--   填列）」就是这个意思）。符号列会让「加/减」出现两个真相：符号列说「加」，而口径
--   算出来是负数 —— 到底听谁的？所以方向一律由**口径本身**决定：
--     存货增加 → `BALANCE_DECREASE` 算出负数 → 报表自动减；
--     处置有收益 → 1606 借方净额为负 → 报表自动减。
--   这也让 4 个口径就能完整表达 12 行标准模板（`ADJ_DISPOSAL_LOSS` 与 `ADJ_INVEST_LOSS`
--   都用 `DEBIT_NET`：损失为正、收益为负，正好是报表要求的「收益以负号填列」）。
--
-- ── `MANUAL`（手工填列）为什么必须存在 ────────────────────────────────────────
--   科目表里没有对应科目（递延所得税、公允价值变动）或口径需要会计判断的行，
--   只能手工填。配置行 `source_type = 'MANUAL'` ⇒ `calc_basis` 与 `account_pattern`
--   **必须都为空**（CHECK 钉死）：留着一个不参与计算的模式串，等于给用户一个
--   「看起来配了、其实不用」的摆设（`docs/ERP-摆设字段口径.md` 的反例）。
--
-- ── 幂等 ──────────────────────────────────────────────────────────────────
--   表 `CREATE TABLE IF NOT EXISTS`；索引 `CREATE INDEX IF NOT EXISTS`；
--   配置行 `ON CONFLICT (item_code) DO NOTHING`（**不覆盖**已有行：管理员改过的
--   名称 / 口径 / 顺序在重放时必须保得住）；菜单 `ON CONFLICT (id) DO NOTHING`；
--   授权 `NOT EXISTS` 去重。自检块可反复执行。
--
-- ── 回滚 ──────────────────────────────────────────────────────────────────
--   down 先删授权（`sys_role_menu` 有外键指向 `sys_menu`）再删菜单行，最后删表。
--   护栏：表里存在**非种子行**（== 管理员真的加过配置）时拒绝回滚 —— 回滚 = 把这些
--   配置永久丢掉，且界面上的「其他调整项」会凭空消失。要真回滚请先导出这些行。
--
-- ── id 段 ────────────────────────────────────────────────────────────────
--   …0017xx 是 V0408 起的「外币 / 报表」小段（1770 / 1780 = M1-14 底稿两码）。
--   本卡续 1790 / 1800（已 grep 核对：`20000000-0000-0000-0000-00000000179` 与
--   `…180` 在此之前无占用）。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：底稿链必须在（缺表说明 V0414 / V0185 没跑到）
-- ---------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.biz_cashflow_item') IS NULL THEN
        RAISE EXCEPTION 'V0416: biz_cashflow_item 不存在，请先应用 V0185（现金流量表分类配置）';
    END IF;
    IF to_regclass('public.sys_menu') IS NULL OR to_regclass('public.sys_role_menu') IS NULL THEN
        RAISE EXCEPTION 'V0416: sys_menu / sys_role_menu 不存在，请先应用系统底座迁移';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM sys_menu
         WHERE id = '20000000-0000-0000-0000-000000001018'::uuid
           AND deleted_at IS NULL
    ) THEN
        RAISE EXCEPTION 'V0416: 「现金流量表」页面节点 …001018 不存在（V0185 没跑到），按钮行无处可挂';
    END IF;
    IF to_regclass('public.biz_cashflow_adjust_item') IS NOT NULL THEN
        RAISE NOTICE 'V0416: biz_cashflow_adjust_item 已存在，本迁移按幂等路径重放（只补缺失行）';
    END IF;
    RAISE NOTICE 'V0416: 前置齐备，开始建间接法调整项配置';
END $$;

-- ---------------------------------------------------------------------------
-- ② 配置表：一行 = 一个调整项（不含起点净利润 / 小计，理由见头注释）
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cashflow_adjust_item (
    id              UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    item_code       VARCHAR(32)  NOT NULL,                  -- 调整项编码：ADJ_DEPRECIATION
    item_name       VARCHAR(64)  NOT NULL,                  -- 报表行名称（纸面报表原文）
    source_type     VARCHAR(16)  NOT NULL DEFAULT 'ACCOUNT',-- ACCOUNT 按科目取数 / MANUAL 手工填列
    calc_basis      VARCHAR(24),                            -- ACCOUNT 必填：CREDIT_NET / DEBIT_NET / BALANCE_INCREASE / BALANCE_DECREASE
    account_pattern VARCHAR(255),                           -- ACCOUNT 必填：复用 report::match_pattern 语法（编码清单 / 编码+下级 / 前缀 / @CATEGORY:）
    sort_order      INT          NOT NULL DEFAULT 0,        -- 报表行顺序（同父不必唯一，但前端按它排；新建默认 max+10）
    is_active       SMALLINT     NOT NULL DEFAULT 1,        -- 启用位：1 启用 / 0 停用
    remark          TEXT,                                   -- 取数口径说明（给会计看的「为什么这么配」）
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT uk_biz_cashflow_adjust_item      UNIQUE (item_code),
    CONSTRAINT ck_biz_cashflow_adjust_source    CHECK (source_type IN ('ACCOUNT', 'MANUAL')),
    CONSTRAINT ck_biz_cashflow_adjust_basis     CHECK (calc_basis IS NULL OR calc_basis IN
                                                   ('CREDIT_NET', 'DEBIT_NET', 'BALANCE_INCREASE', 'BALANCE_DECREASE')),
    CONSTRAINT ck_biz_cashflow_adjust_active    CHECK (is_active IN (0, 1)),
    CONSTRAINT ck_biz_cashflow_adjust_name      CHECK (length(btrim(item_name)) > 0),
    CONSTRAINT ck_biz_cashflow_adjust_code      CHECK (length(btrim(item_code)) > 0),
    -- 行形状：ACCOUNT 行必须给口径 + 模式；MANUAL 行两列**必须为空**（否则就是摆设字段）
    CONSTRAINT ck_biz_cashflow_adjust_shape     CHECK (
        (source_type = 'ACCOUNT'
            AND calc_basis IS NOT NULL
            AND account_pattern IS NOT NULL
            AND length(btrim(account_pattern)) > 0)
     OR (source_type = 'MANUAL'
            AND calc_basis IS NULL
            AND account_pattern IS NULL)
    )
);

CREATE INDEX IF NOT EXISTS idx_biz_cashflow_adjust_order
    ON biz_cashflow_adjust_item (sort_order, item_code);

COMMENT ON TABLE biz_cashflow_adjust_item IS
    '间接法调整项配置（M1-15）：一行 = 一个「净利润 → 经营活动现金流量」的调整项 + 它的取数口径；不算金额、不出表（M1-16 / M1-17 消费）';
COMMENT ON COLUMN biz_cashflow_adjust_item.calc_basis IS
    '取数口径：CREDIT_NET 贷方净额 / DEBIT_NET 借方净额 / BALANCE_INCREASE 余额增加(期末-期初) / BALANCE_DECREASE 余额减少(期初-期末)；方向由口径自然表达，没有独立的加减符号列';
COMMENT ON COLUMN biz_cashflow_adjust_item.account_pattern IS
    '科目匹配模式（report::match_pattern 语法）；ACCOUNT 行必填，MANUAL 行必须为 NULL';

-- ---------------------------------------------------------------------------
-- ③ 12 行标准模板（企业会计准则《现金流量表补充资料》间接法部分）
--   `ON CONFLICT (item_code) DO NOTHING`：重放不覆盖管理员改过的名称 / 口径 / 顺序。
--   顺序 10 起、间隔 10：留给「插到两行之间」用中间值（15 / 25）。
-- ---------------------------------------------------------------------------
INSERT INTO biz_cashflow_adjust_item
    (item_code, item_name, source_type, calc_basis, account_pattern, sort_order, remark)
VALUES
    ('ADJ_ASSET_IMPAIRMENT', '资产减值准备', 'ACCOUNT', 'CREDIT_NET', '1231,1471', 10,
     '坏账准备 1231 / 存货跌价准备 1471 的贷方净额（计提为正）。当前科目表没有这两个科目 ⇒ 该行会恒为 0，列表接口会点名，建了科目即自动生效'),
    ('ADJ_DEPRECIATION',    '固定资产折旧', 'ACCOUNT', 'CREDIT_NET', '1602', 20,
     '累计折旧贷方净额 = 本期计提折旧；生产用与管理用折旧一并在内（间接法不区分折旧去向）'),
    ('ADJ_AMORT_INTANGIBLE','无形资产摊销', 'ACCOUNT', 'CREDIT_NET', '1702', 30,
     '累计摊销贷方净额'),
    ('ADJ_AMORT_LONGTERM',  '长期待摊费用摊销', 'ACCOUNT', 'CREDIT_NET', '1801', 40,
     '长期待摊费用贷方净额（摊销时贷记本科目）'),
    ('ADJ_DISPOSAL_LOSS',   '处置长期资产损失', 'ACCOUNT', 'DEBIT_NET', '1606', 50,
     '固定资产清理借方净额 = 净损失（加回）；有净收益时借方净额为负 ⇒ 报表自然表现为减项（收益以负号填列）'),
    ('ADJ_FINANCE_COST',    '财务费用', 'ACCOUNT', 'DEBIT_NET', '5603', 60,
     '财务费用借方净额（利息支出）；利息收入会冲减该净额，净额为负时自然表现为减项'),
    ('ADJ_INVEST_LOSS',     '投资损失', 'ACCOUNT', 'DEBIT_NET', '5111', 70,
     '投资收益借方净额 = 投资损失（加回）；投资收益为贷方净额 ⇒ 负数，表现为减项（收益以负号填列）'),
    ('ADJ_DEFERRED_TAX',    '递延所得税调整', 'MANUAL', NULL, NULL, 80,
     '科目表暂未建递延所得税资产 / 负债科目，按账套实际手工填列；建好科目后可改成 ACCOUNT 行（余额增加 / 减少口径）'),
    ('ADJ_INVENTORY_DECREASE', '存货的减少', 'ACCOUNT', 'BALANCE_DECREASE', '1401,1402,1403,1404,1405,1408', 90,
     '存货类科目（材料采购 / 在途物资 / 原材料 / 材料成本差异 / 库存商品 / 委托加工物资）余额**减少**额（期初 − 期末）；存货增加时为负数，报表自然表现减项'),
    ('ADJ_OPER_RECEIVABLE_DECREASE', '经营性应收项目的减少', 'ACCOUNT', 'BALANCE_DECREASE', '1121,1122,1123,1221', 100,
     '应收票据 / 应收账款 / 预付账款 / 其他应收款余额减少额（期初 − 期末）；注意不含应收股利 1131 / 应收利息 1132（投资活动）'),
    ('ADJ_OPER_PAYABLE_INCREASE', '经营性应付项目的增加', 'ACCOUNT', 'BALANCE_INCREASE', '2202,2203,2211,2221,2241', 110,
     '应付账款 / 预收账款 / 应付职工薪酬 / 应交税费 / 其他应付款余额增加额（期末 − 期初）'),
    ('ADJ_OTHER',           '其他', 'MANUAL', NULL, NULL, 120,
     '未单独列示的调整项，金额在间接法底稿里手工填列（例如暂未建科目的公允价值变动损益）')
ON CONFLICT (item_code) DO NOTHING;

-- ---------------------------------------------------------------------------
-- ④ 两个按钮权限码（同父 = 「现金流量表」页面 …001018；sort 30 / 40 接在底稿两码之后）
--   读 `...:list`：看配置（财务经理 / 总监 / 审计都要看口径）
--   写 `...:manage`：改配置（改错一行 = 间接法算错一个调整项，且报表上看不出）
--   title 按约定 12：≤12 字、不带标点（「调整项查看」5 字 / 「调整项配置」5 字）。
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001790'::uuid,
     'business:erp:cashflow-adjust:list', 3,
     'business:erp:cashflow-adjust:list', NULL,
     '20000000-0000-0000-0000-000000001018'::uuid, NULL,
     30, 1, NULL, '调整项查看', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001800'::uuid,
     'business:erp:cashflow-adjust:manage', 3,
     'business:erp:cashflow-adjust:manage', NULL,
     '20000000-0000-0000-0000-000000001018'::uuid, NULL,
     40, 1, NULL, '调整项配置', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 授权：超管 / 财务经理（FINANCE_MANAGER 已有现金流量表页面码 ⇒ 按钮可达）。
--   刻意不给 FINANCE_DIRECTOR：无报表页码 ⇒ 孤儿按钮（orphan_button_gate 判红）。
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, m.id
  FROM sys_role r
  CROSS JOIN (VALUES
        ('20000000-0000-0000-0000-000000001790'::uuid),
        ('20000000-0000-0000-0000-000000001800'::uuid)
  ) AS m(id)
 WHERE r.deleted_at IS NULL
   AND r.role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER')
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm
         WHERE rm.role_id = r.id
           AND rm.menu_id = m.id
   );

-- ---------------------------------------------------------------------------
-- ⑤ 迁移自检：表形状 / 种子行 / 行形状约束真的在 / 菜单节点 / 授权
--   任一不符即 RAISE EXCEPTION 让整个迁移回滚 ——「跑过了」和「验过了」是两件事。
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n          integer;
    n_roles    integer;
    n_grant    integer;
    n_active   integer;
    n_manual   integer;
    n_basis    integer;
BEGIN
    -- ===== 表存在 =====
    IF to_regclass('public.biz_cashflow_adjust_item') IS NULL THEN
        RAISE EXCEPTION 'V0416 自检失败：biz_cashflow_adjust_item 没建出来';
    END IF;

    -- ===== 种子 12 行、全部启用；MANUAL 2 行；4 个口径至少各 1 行 =====
    SELECT count(*), count(*) FILTER (WHERE is_active = 1) INTO n, n_active
      FROM biz_cashflow_adjust_item WHERE item_code LIKE 'ADJ\_%';
    IF n < 12 THEN
        RAISE EXCEPTION 'V0416 自检失败：标准模板应有 12 行调整项，实际 % 行', n;
    END IF;
    IF n_active <> n THEN
        RAISE EXCEPTION 'V0416 自检失败：种子行应全部启用（% 行里只有 % 行启用）', n, n_active;
    END IF;
    SELECT count(*) INTO n_manual
      FROM biz_cashflow_adjust_item WHERE source_type = 'MANUAL' AND item_code LIKE 'ADJ\_%';
    IF n_manual < 2 THEN
        RAISE EXCEPTION 'V0416 自检失败：MANUAL 行只有 % 行（递延所得税 / 其他 两行是手工填列的样板）', n_manual;
    END IF;
    SELECT count(DISTINCT calc_basis) INTO n_basis
      FROM biz_cashflow_adjust_item WHERE source_type = 'ACCOUNT' AND item_code LIKE 'ADJ\_%';
    IF n_basis <> 4 THEN
        RAISE EXCEPTION 'V0416 自检失败：4 个取数口径应各被用一次，实际只用到 % 个（模板不全）', n_basis;
    END IF;

    -- ===== 行形状约束真的在（不是「写在注释里」）：插一条坏形状必须被数据库拒 =====
    BEGIN
        INSERT INTO biz_cashflow_adjust_item (item_code, item_name, source_type, calc_basis, account_pattern)
        VALUES ('ADJ_SELFCHECK_BAD', '自检坏行', 'MANUAL', 'CREDIT_NET', '1602');
        RAISE EXCEPTION 'V0416 自检失败：MANUAL 行带取数口径竟然插得进去（ck_biz_cashflow_adjust_shape 没生效）';
    EXCEPTION
        WHEN check_violation THEN
            RAISE NOTICE 'V0416 自检：MANUAL 行带口径被 CHECK 拒绝（符合预期）';
    END;

    -- ===== 菜单节点：码 / 类型 / 父节点 / 无 path / 启用 / 未软删 =====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001790'::uuid,
                  '20000000-0000-0000-0000-000000001800'::uuid)
       AND menu_type = 3
       AND parent_id = '20000000-0000-0000-0000-000000001018'::uuid
       AND path IS NULL
       AND status = 1
       AND deleted_at IS NULL
       AND auth_code = name;
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0416 自检失败：两个调整项按钮节点只建成 % 个', n;
    END IF;

    -- ===== 一码一节点（同码两行会让授权页出现两个同名勾选框）=====
    SELECT count(*) INTO n
      FROM (
            SELECT auth_code FROM sys_menu
             WHERE auth_code IN ('business:erp:cashflow-adjust:list',
                                 'business:erp:cashflow-adjust:manage')
               AND deleted_at IS NULL
             GROUP BY auth_code HAVING count(*) = 1
           ) one_node;
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0416 自检失败：两个码在 sys_menu 里应各恰好 1 行（实际只 % 个码满足）', n;
    END IF;

    -- ===== title 规范（≤12 字、不带标点/说明）=====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001790'::uuid,
                  '20000000-0000-0000-0000-000000001800'::uuid)
       AND length(title) <= 12
       AND title NOT LIKE '%，%'
       AND title NOT LIKE '%（%'
       AND btrim(title) <> '';
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0416 自检失败：调整项按钮的 title 不合菜单规范（≤12 字、不带标点）';
    END IF;

    -- ===== 同父 sort_order 唯一（V0415 用了 10/20，本卡 30/40）=====
    SELECT count(*) INTO n
      FROM (
            SELECT sort_order FROM sys_menu
             WHERE parent_id = '20000000-0000-0000-0000-000000001018'::uuid
               AND menu_type = 3 AND deleted_at IS NULL
             GROUP BY sort_order HAVING count(*) > 1
           ) dup;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0416 自检失败：「现金流量表」页面下有 % 组重复 sort_order（本卡用 30 / 40）', n;
    END IF;

    -- ===== 授权：有角色就该授到；空库首启（无角色）跳过 =====
    SELECT count(*) INTO n_roles
      FROM sys_role WHERE deleted_at IS NULL
       AND role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER');
    SELECT count(*) INTO n_grant
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE rm.menu_id IN ('20000000-0000-0000-0000-000000001790'::uuid,
                          '20000000-0000-0000-0000-000000001800'::uuid)
       AND r.role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER');
    IF n_grant <> n_roles * 2 THEN
        RAISE EXCEPTION 'V0416 自检失败：授权数 % ≠ 目标角色数 % × 2 个码', n_grant, n_roles;
    END IF;

    RAISE NOTICE 'V0416 自检通过：间接法调整项 12 行模板 + 2 个权限码（调整项查看 / 调整项配置）挂「现金流量表」页面，授权 % 个角色 × 2 码 = % 行',
        n_roles, n_grant;
END $$;
