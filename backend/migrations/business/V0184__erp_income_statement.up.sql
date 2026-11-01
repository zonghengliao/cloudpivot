-- ======================================================
-- V0184：利润表（ERP-R2）—— 报表项映射 + 权限节点
--
-- 依据：kanban 卡 t_3e6d3220 §一 / §二 / §七
--   · §一 小企业标准利润表结构（营业收入 → 净利润）
--   · §二 科目映射（report_code = 'INCOME_STATEMENT'）
--   · §七 硬红线 1：**复用 ERP-R1 的框架**（V0183 建的 `biz_report_item` + 查询事务模式），
--           本迁移只加配置，不建表、不改取数逻辑
--
-- 回滚：`V0184__erp_income_statement.down.sql`（删本迁移种的 12 个报表项 + 1 个权限节点；
--       资产负债表配置 / 科目 / 既有行一行不动）
--
-- ── 只读不写 ────────────────────────────────────────────────────────
--   利润表是**只读**报表：本迁移不碰 `biz_voucher` / `biz_voucher_entry` /
--   `biz_account` / `biz_accounting_period`（硬红线 4）。
--
-- ── 为什么 side 填 'EQUITY' 而不是新造 'PROFIT_LOSS' ──────────────────
--   V0183 给 `biz_report_item.side` 定了 `ck_biz_report_side CHECK (side IN
--   ('ASSET','LIABILITY','EQUITY'))`。损益类科目本来就是权益的一部分（未结账时利润
--   留在 5xxx/6xxx，见 V0183 的 `UNDISTRIBUTED_PROFIT` 报表项），所以利润表的报表项
--   统一填 `EQUITY`：**不为一张报表放宽一张共用表的约束**。
--   两张报表靠 `report_code` 隔开，同一张表里 `side` 的取值语义不变。
--
-- ── 科目映射的三处口径说明（改配置前先读）────────────────────────────
--   ① **营业成本 = 5401 + 5402 + 6401**：`6401 主营业务成本` 是 V0149 时就有的存量科目，
--      也是 `business/erp/voucher.rs::ACCOUNT_MAIN_COST`（序列号出库 借 6401 / 贷 1405）
--      写死的成本科目。只映射 5401/5402 会让「出库成本」进不了利润表 ——
--      净利润与「未分配利润变动」当场对不上（假不平，真漏数）。
--   ② **税金及附加只认 5403**：卡面写「如果没有，先跳过或加科目」。V0183 已种 5403
--      营业税金及附加，直接用。
--   ③ **5111 投资收益 刻意不映射**：卡面 §一 的结构里没有这一行。硬塞进某个现有行会
--      让报表数字含义失真；不映射时，一旦真的发生投资收益，报表会多出一行
--      `UNMAPPED_PROFIT_LOSS`（运行时补出来的）并在 `check.hints` 里点名科目编码，
--      差额精确到分 —— 绝不静默吞掉。要显示它，加一个报表项即可（本表是配置驱动）。
--
-- ── 幂等 ──────────────────────────────────────────────────────────
--   报表项：`ON CONFLICT (report_code, item_code) DO NOTHING`
--   菜单：`ON CONFLICT (id) DO NOTHING`；授权用 `NOT EXISTS` 去重
--   自检块只断言「行数够、关键行在位、公式引用都存在、同一科目不被两行匹配」
-- ======================================================

-- ======================================================
-- ① 利润表结构（12 个报表项，全部落在 report_code = 'INCOME_STATEMENT'）
--
--   取数口径（见 `business/erp/income_statement.rs` 模块文档）：
--     收入类科目（贷方）  金额 = 贷方发生额 − 借方发生额
--     成本费用类（借方）  金额 = 借方发生额 − 贷方发生额
--   红字凭证是借贷互换（`is_red=1`，金额恒正），净额天然为负 → 红冲自动抵减，不用特判。
--   `formula` 只能引用 `sort_order` 排在**前面**的项（防循环，求值顺序即 sort_order 顺序）。
-- ======================================================
INSERT INTO biz_report_item (report_code, item_code, item_name, item_level, parent_code, side,
                             account_pattern, formula, is_deduction, sort_order, remark)
VALUES
    -- ---------- 一、营业收入 及其减项 ----------
    ('INCOME_STATEMENT', 'REVENUE',             '一、营业收入',     1, NULL,                 'EQUITY', '5001,5051', NULL, FALSE, 10, '主营业务收入 + 其他业务收入'),
    ('INCOME_STATEMENT', 'COST_OF_SALES',       '减：营业成本',     2, 'REVENUE',            'EQUITY', '5401,5402,6401', NULL, FALSE, 11, '主营业务成本 + 其他业务成本；6401 为存量成本科目（voucher.rs::ACCOUNT_MAIN_COST，序列号出库默认借贷方向 借6401/贷1405）'),
    ('INCOME_STATEMENT', 'TAX_SURCHARGE',       '减：税金及附加',   2, 'REVENUE',            'EQUITY', '5403', NULL, FALSE, 12, '营业税金及附加'),
    ('INCOME_STATEMENT', 'SELLING_EXPENSE',     '减：销售费用',     2, 'REVENUE',            'EQUITY', '5601', NULL, FALSE, 13, NULL),
    ('INCOME_STATEMENT', 'ADMIN_EXPENSE',       '减：管理费用',     2, 'REVENUE',            'EQUITY', '5602', NULL, FALSE, 14, NULL),
    ('INCOME_STATEMENT', 'FINANCE_EXPENSE',     '减：财务费用',     2, 'REVENUE',            'EQUITY', '5603', NULL, FALSE, 15, NULL),

    -- ---------- 二、营业利润 ----------
    ('INCOME_STATEMENT', 'OPERATING_PROFIT',    '二、营业利润',     1, NULL,                 'EQUITY', NULL,
     'REVENUE - COST_OF_SALES - TAX_SURCHARGE - SELLING_EXPENSE - ADMIN_EXPENSE - FINANCE_EXPENSE',
     FALSE, 20, '★ 小计：营业收入 − 营业成本 − 税金及附加 − 三项期间费用'),

    -- ---------- 营业外收支 ----------
    ('INCOME_STATEMENT', 'NON_OPERATING_INCOME',  '加：营业外收入', 2, 'OPERATING_PROFIT',   'EQUITY', '5301', NULL, FALSE, 21, NULL),
    ('INCOME_STATEMENT', 'NON_OPERATING_EXPENSE', '减：营业外支出', 2, 'OPERATING_PROFIT',   'EQUITY', '5711', NULL, FALSE, 22, NULL),

    -- ---------- 三、利润总额 ----------
    ('INCOME_STATEMENT', 'TOTAL_PROFIT',        '三、利润总额',     1, NULL,                 'EQUITY', NULL,
     'OPERATING_PROFIT + NON_OPERATING_INCOME - NON_OPERATING_EXPENSE',
     FALSE, 30, '★ 小计：营业利润 + 营业外收入 − 营业外支出'),

    -- ---------- 四、净利润 ----------
    ('INCOME_STATEMENT', 'INCOME_TAX',          '减：所得税费用',   2, 'TOTAL_PROFIT',       'EQUITY', '5801', NULL, FALSE, 31, NULL),
    ('INCOME_STATEMENT', 'NET_PROFIT',          '四、净利润',       1, NULL,                 'EQUITY', NULL,
     'TOTAL_PROFIT - INCOME_TAX',
     FALSE, 40, '★ 勾稽：净利润（本年累计）≡ 资产负债表「未分配利润」本年变动')
ON CONFLICT (report_code, item_code) DO NOTHING;

-- ======================================================
-- ② 权限节点：利润表页面
--
--   导出按钮**复用** R1 的 `business:erp:report:export`（V0183 …1016 已存在、已授权），
--   不再种第二个同码节点（`sys_menu.auth_code` 是权限码的唯一来源，一码一节点）。
--   菜单 id：1017（V0183 已用到 1016）；parent 挂「财务管理」目录 …0e02（与资产负债表同级）。
-- ======================================================
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001017'::uuid, 'business:erp:report:income-statement', 2,
     'business:erp:report:income-statement', '/business/erp/report/income-statement',
     '20000000-0000-0000-0000-000000000e02'::uuid, 'business/erp/report/income-statement/index',
     46, 1, NULL, '利润表', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

DO $$
DECLARE
    n_roles  integer;
    n_menus  integer;
    n_grants integer;
    n_item   integer;
    n_codes  integer;
BEGIN
    -- ---------- 报表项自检 ----------
    SELECT count(*) INTO n_item
      FROM biz_report_item
     WHERE report_code = 'INCOME_STATEMENT' AND is_active = 1;
    IF n_item <> 12 THEN
        RAISE EXCEPTION 'V0184 自检失败：INCOME_STATEMENT 应有 12 个启用报表项，实际 %', n_item;
    END IF;

    -- 结构必须完整：勾稽三小计 + 净利润一行都不能少
    IF NOT EXISTS (SELECT 1 FROM biz_report_item
                    WHERE report_code = 'INCOME_STATEMENT' AND item_code = 'NET_PROFIT'
                      AND formula = 'TOTAL_PROFIT - INCOME_TAX') THEN
        RAISE EXCEPTION 'V0184 自检失败：NET_PROFIT 必须是 TOTAL_PROFIT - INCOME_TAX';
    END IF;

    -- 公式引用的每个 item_code 都必须存在（否则报表一跑就 500）。
    -- 例外：`UNMAPPED_PROFIT_LOSS` 是运行时补出来的未归类损益行（不落库，见 income_statement.rs），
    -- 公式可以引用它（把未归类的钱算进某一行是允许的配置选择）。
    IF EXISTS (
        SELECT 1
          FROM biz_report_item i,
               LATERAL regexp_split_to_table(replace(replace(COALESCE(i.formula, ''), '-', '+'), ' ', ''), '\+') AS ref(code)
         WHERE i.report_code = 'INCOME_STATEMENT'
           AND COALESCE(i.formula, '') <> ''
           AND ref.code <> ''
           AND ref.code <> 'UNMAPPED_PROFIT_LOSS'
           AND NOT EXISTS (SELECT 1 FROM biz_report_item r
                            WHERE r.report_code = 'INCOME_STATEMENT' AND r.item_code = ref.code)
    ) THEN
        RAISE EXCEPTION 'V0184 自检失败：利润表公式引用了不存在的 item_code';
    END IF;

    -- 公式只能引用**前面**的项（求值顺序 = sort_order），否则运行期静默算 0 / 500
    IF EXISTS (
        SELECT 1
          FROM biz_report_item i
          JOIN biz_report_item r
            ON r.report_code = 'INCOME_STATEMENT' AND r.item_code = ANY (
                 string_to_array(replace(replace(COALESCE(i.formula, ''), '-', '+'), ' ', ''), '+'))
         WHERE i.report_code = 'INCOME_STATEMENT'
           AND COALESCE(i.formula, '') <> ''
           AND r.sort_order >= i.sort_order
    ) THEN
        RAISE EXCEPTION 'V0184 自检失败：利润表公式引用了 sort_order 不在前面的项';
    END IF;

    -- 同一个科目不许被两个报表项匹配（同一笔钱算两遍 = 静默多计）
    IF EXISTS (
        SELECT 1
          FROM biz_report_item a
          JOIN biz_report_item b
            ON a.report_code = 'INCOME_STATEMENT' AND b.report_code = 'INCOME_STATEMENT'
           AND a.item_code < b.item_code
          JOIN biz_account c ON c.deleted_at IS NULL
         WHERE a.account_pattern IS NOT NULL AND b.account_pattern IS NOT NULL
           AND c.account_code = ANY (string_to_array(a.account_pattern, ','))
           AND c.account_code = ANY (string_to_array(b.account_pattern, ','))
    ) THEN
        RAISE EXCEPTION 'V0184 自检失败：同一科目被两个利润表报项匹配';
    END IF;

    -- 标准损益类科目必须被映射（5111 投资收益按设计不映射，见文件头 ③）
    IF EXISTS (
        SELECT 1 FROM biz_account c
         WHERE c.deleted_at IS NULL
           AND c.account_category = 'PROFIT_LOSS'
           AND c.account_code IN ('5001','5051','5301','5401','5402','5403','5601','5602','5603','5711','5801','6401')
           AND NOT EXISTS (
                SELECT 1 FROM biz_report_item m
                 WHERE m.report_code = 'INCOME_STATEMENT'
                   AND m.account_pattern IS NOT NULL
                   AND c.account_code = ANY (string_to_array(m.account_pattern, ','))
           )
    ) THEN
        RAISE EXCEPTION 'V0184 自检失败：有标准损益类科目（5001/5051/5301/5401/5402/5403/5601/5602/5603/5711/5801/6401）没被利润表映射';
    END IF;

    -- ---------- 菜单 / 授权自检 ----------
    SELECT count(*) INTO n_menus
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001017'::uuid;
    IF n_menus <> 1 THEN
        RAISE EXCEPTION 'V0184 自检失败：利润表权限节点没种上（实际 % 行）', n_menus;
    END IF;

    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0184：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
    ELSE
        INSERT INTO sys_role_menu (id, role_id, menu_id)
        SELECT gen_random_uuid(), r.id, m.id
          FROM sys_role r
          CROSS JOIN (VALUES
                ('20000000-0000-0000-0000-000000001017'::uuid)) AS m(id)
         WHERE r.deleted_at IS NULL
           AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'TEST_UADMIN', 'FINANCE', 'ACCOUNTANT')
           AND NOT EXISTS (
                SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
           );
        GET DIAGNOSTICS n_grants = ROW_COUNT;
        RAISE NOTICE 'V0184：利润表权限码已种，授权新增 % 行（角色数 %）', n_grants, n_roles;
    END IF;

    SELECT count(*) INTO n_codes
      FROM biz_report_item
     WHERE report_code = 'INCOME_STATEMENT' AND account_pattern IS NOT NULL;
    RAISE NOTICE 'V0184 自检通过：利润表报表项 % 个（其中科目取数 % 个）、权限节点 1 个', n_item, n_codes;
END $$;
