-- ======================================================
-- V0183：资产负债表（ERP-R1）—— 标准会计科目补齐 + 报表项映射 + 权限码
--
-- 依据：kanban 卡 t_17c44185 §一 / §二 / §七
--   · §一 补标准小企业会计制度核心科目（卡面约 40 个，本迁移落 49 个新科目）
--   · §二 新建 `biz_report_item`（报表项 ↔ 科目映射）并种标准资产负债表结构
--   · §七 硬红线 2：**不许覆盖已有科目**（全部 `ON CONFLICT DO NOTHING`）
--
-- 回滚：`V0183__erp_balance_sheet.down.sql`（删报表项 / 表 / 本次新种科目 / 权限码；
--       已有的 7 个科目一行不动）
--
-- ── 开工实测（2026-11-01，生产库 cloudpivot）───────────────────────────
--   biz_account 只有 7 行：
--     1002 银行存款(ASSET/DEBIT) | 1405 库存商品(ASSET/DEBIT) | 1901 待处理财产损溢(ASSET/DEBIT)
--     2202 应付账款(LIABILITY/CREDIT) | 2221 应交税费(LIABILITY/CREDIT, 非末级)
--     2221.01 应交税费-进项税(LIABILITY/CREDIT) | 6401 主营业务成本(PROFIT_LOSS/DEBIT)
--   biz_accounting_period 只有 2026-10（OPEN）；biz_voucher 11 张，Σ借 = Σ贷 = 1144.17
--   sys_menu 最大 id 尾号 1014 → 本迁移取 **1015 / 1016**
--
-- ── 为什么科目要在这里补，而不是"用到再补"──────────────────────────────
--   报表项是按**科目**取数的（`account_pattern`），科目不存在 ⇒ 该报表项永远是 0，
--   用户看不出"是本期没发生"还是"科目根本没建"。科目表是报表的前置条件。
--
-- ── 为什么备抵科目要显式 direction=CREDIT 而不是负数 ─────────────────────
--   1602 累计折旧 / 1702 累计摊销 在会计上是**资产类备抵科目**：类别仍是 ASSET，
--   但余额在贷方。`biz_account.balance_direction` 只有 DEBIT/CREDIT 两值，
--   记 `CREDIT` 即表达"贷方余额"，报表里按 `is_deduction = true` 作减项
--   （口径与 `ck_biz_account_direction` 一致，不需要新增列）。
--
-- ── 幂等 ──────────────────────────────────────────────────────────────
--   科目：`ON CONFLICT (account_code) DO NOTHING`（撞 `uk_biz_account_code`）
--   报表项：`ON CONFLICT (report_code, item_code) DO NOTHING`
--   菜单：`ON CONFLICT (id) DO NOTHING`；授权用 `NOT EXISTS` 去重
--   自检块只断言"行数够、关键行在位"，重复应用不抛错
-- ======================================================

-- ======================================================
-- ① 标准会计科目（49 个新科目；2221.01 已在库，冲突即跳过）
--
--   一级科目：level=1 / is_leaf=1；二级科目（编码含 `.`）：level=2 / is_leaf=1，
--   parent_id 由 `split_part(code, '.', 1)` 找上级。
--   已存在科目一律不动（level/parent/name 都不覆盖）。
-- ======================================================
INSERT INTO biz_account (account_code, account_name, parent_id, level, account_category,
                         balance_direction, is_leaf, status, remark)
SELECT v.code,
       v.name,
       CASE
           WHEN v.code LIKE '%.%' THEN (
               SELECT p.id FROM biz_account p
                WHERE p.account_code = split_part(v.code, '.', 1)
                  AND p.deleted_at IS NULL
           )
           ELSE NULL
       END,
       CASE WHEN v.code LIKE '%.%' THEN 2::smallint ELSE 1::smallint END,
       v.category,
       v.direction,
       1::smallint,
       1::smallint,
       'V0183 标准科目（小企业会计制度核心科目，ERP-R1 补种）'
  FROM (VALUES
        -- ---------- 资产类（1xxx）----------
        ('1001', '库存现金',           'ASSET',       'DEBIT'),
        ('1002', '银行存款',           'ASSET',       'DEBIT'),
        ('1012', '其他货币资金',       'ASSET',       'DEBIT'),
        ('1121', '应收票据',           'ASSET',       'DEBIT'),
        ('1122', '应收账款',           'ASSET',       'DEBIT'),
        ('1123', '预付账款',           'ASSET',       'DEBIT'),
        ('1131', '应收股利',           'ASSET',       'DEBIT'),
        ('1132', '应收利息',           'ASSET',       'DEBIT'),
        ('1221', '其他应收款',         'ASSET',       'DEBIT'),
        ('1401', '材料采购',           'ASSET',       'DEBIT'),
        ('1402', '在途物资',           'ASSET',       'DEBIT'),
        ('1403', '原材料',             'ASSET',       'DEBIT'),
        ('1404', '材料成本差异',       'ASSET',       'DEBIT'),
        ('1405', '库存商品',           'ASSET',       'DEBIT'),
        ('1408', '委托加工物资',       'ASSET',       'DEBIT'),
        ('1601', '固定资产',           'ASSET',       'DEBIT'),
        ('1602', '累计折旧',           'ASSET',       'CREDIT'),  -- 备抵：资产类贷方余额
        ('1604', '在建工程',           'ASSET',       'DEBIT'),
        ('1605', '工程物资',           'ASSET',       'DEBIT'),
        ('1606', '固定资产清理',       'ASSET',       'DEBIT'),
        ('1701', '无形资产',           'ASSET',       'DEBIT'),
        ('1702', '累计摊销',           'ASSET',       'CREDIT'),  -- 备抵：资产类贷方余额
        ('1801', '长期待摊费用',       'ASSET',       'DEBIT'),
        ('1901', '待处理财产损溢',     'ASSET',       'DEBIT'),
        -- ---------- 负债类（2xxx）----------
        ('2101', '短期借款',           'LIABILITY',   'CREDIT'),
        ('2202', '应付账款',           'LIABILITY',   'CREDIT'),
        ('2203', '预收账款',           'LIABILITY',   'CREDIT'),
        ('2211', '应付职工薪酬',       'LIABILITY',   'CREDIT'),
        ('2221', '应交税费',           'LIABILITY',   'CREDIT'),
        ('2221.01', '应交税费-进项税', 'LIABILITY',   'CREDIT'),
        ('2221.02', '应交税费-销项税', 'LIABILITY',   'CREDIT'),
        ('2231', '应付利息',           'LIABILITY',   'CREDIT'),
        ('2241', '其他应付款',         'LIABILITY',   'CREDIT'),
        ('2401', '递延收益',           'LIABILITY',   'CREDIT'),
        ('2501', '长期借款',           'LIABILITY',   'CREDIT'),
        ('2701', '长期应付款',         'LIABILITY',   'CREDIT'),
        -- ---------- 所有者权益（3xxx）----------
        ('3001', '实收资本',           'EQUITY',      'CREDIT'),
        ('3101', '盈余公积',           'EQUITY',      'CREDIT'),
        ('3102', '资本公积',           'EQUITY',      'CREDIT'),
        ('3103', '本年利润',           'EQUITY',      'CREDIT'),
        ('3104', '利润分配',           'EQUITY',      'CREDIT'),
        -- ---------- 成本类（4xxx）----------
        ('4001', '生产成本',           'COST',        'DEBIT'),
        ('4101', '制造费用',           'COST',        'DEBIT'),
        -- ---------- 损益类（5xxx 收入 / 6xxx 费用）----------
        ('5001', '主营业务收入',       'PROFIT_LOSS', 'CREDIT'),
        ('5051', '其他业务收入',       'PROFIT_LOSS', 'CREDIT'),
        ('5111', '投资收益',           'PROFIT_LOSS', 'CREDIT'),
        ('5301', '营业外收入',         'PROFIT_LOSS', 'CREDIT'),
        ('5401', '主营业务成本',       'PROFIT_LOSS', 'DEBIT'),
        ('5402', '其他业务成本',       'PROFIT_LOSS', 'DEBIT'),
        ('5403', '营业税金及附加',     'PROFIT_LOSS', 'DEBIT'),
        ('5601', '销售费用',           'PROFIT_LOSS', 'DEBIT'),
        ('5602', '管理费用',           'PROFIT_LOSS', 'DEBIT'),
        ('5603', '财务费用',           'PROFIT_LOSS', 'DEBIT'),
        ('5711', '营业外支出',         'PROFIT_LOSS', 'DEBIT'),
        ('5801', '所得税费用',         'PROFIT_LOSS', 'DEBIT'),
        ('6401', '主营业务成本',       'PROFIT_LOSS', 'DEBIT')
       ) AS v(code, name, category, direction)
ON CONFLICT (account_code) DO NOTHING;

-- ======================================================
-- ② 报表项 ↔ 科目映射表
--
--   资产负债表不是"科目列表"，而是"报表项"：一个报表项可能汇总多个科目
--   （货币资金 = 1001 + 1002 + 1012），也可能只是小计（流动负债合计）。
--   所以取数规则（`account_pattern`）与结构（`item_level` / `parent_code` /
--   `sort_order`）分开存：**改结构不用改代码，改映射不用发版**。
--
--   `account_pattern` 支持四种写法（解析见 `business/erp/report.rs::match_pattern`）：
--     · `1001,1002`        科目编码清单
--     · `2221`             科目 + 其全部下级（2221 / 2221.01 / 2221.02）
--     · `1*` / `1%`        编码前缀
--     · `@CATEGORY:PROFIT_LOSS`  按科目类别整类匹配
--   `formula` 用 `+` / `-` 引用其它 `item_code`（只允许引用**排在前面的**项，防循环）。
-- ======================================================
CREATE TABLE IF NOT EXISTS biz_report_item (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    report_code     VARCHAR(32)  NOT NULL,                  -- 报表编码：BALANCE_SHEET
    item_code       VARCHAR(32)  NOT NULL,                  -- 报表项编码：TOTAL_ASSETS
    item_name       VARCHAR(64)  NOT NULL,                  -- 报表项名称：资产总计
    item_level      SMALLINT     NOT NULL,                  -- 层级：1 大类 / 2 小类 / 3 明细
    parent_code     VARCHAR(32),                            -- 上级 item_code（弱引用）
    side            VARCHAR(12)  NOT NULL,                  -- 所属方：ASSET / LIABILITY / EQUITY
                                                            -- 【与卡面的差异】卡面写 VARCHAR(8)，但 'LIABILITY' 是 9 个字符，
                                                            -- 照抄会在第 20 行（CURRENT_LIABILITIES）直接 22001 溢出 → 取 12。
    account_pattern VARCHAR(255),                           -- 科目匹配模式（见文件头说明）
    formula         VARCHAR(255),                           -- 计算式（引用其它 item_code）
    is_deduction    BOOLEAN      NOT NULL DEFAULT FALSE,     -- 是否减项（累计折旧 / 累计摊销）
    is_active       SMALLINT     NOT NULL DEFAULT 1,         -- 启用位：1 启用 / 0 停用
    sort_order      INT          NOT NULL DEFAULT 0,         -- 显示顺序（同时决定公式求值顺序）
    remark          TEXT,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT uk_biz_report_item   UNIQUE (report_code, item_code),
    CONSTRAINT ck_biz_report_side   CHECK (side IN ('ASSET', 'LIABILITY', 'EQUITY')),
    CONSTRAINT ck_biz_report_level  CHECK (item_level BETWEEN 1 AND 3),
    CONSTRAINT ck_biz_report_active CHECK (is_active IN (0, 1)),
    -- 取数方式必须二选一（既没模式又没公式的项只会出 0，等于配错）；
    -- 例外：`item_level = 1` 的大类标题行（流动资产 / 负债 …）是纯分组标题，金额恒为 NULL。
    CONSTRAINT ck_biz_report_source CHECK (account_pattern IS NOT NULL
                                           OR formula IS NOT NULL
                                           OR item_level = 1)
);

CREATE INDEX IF NOT EXISTS idx_biz_report_item_code
    ON biz_report_item (report_code, sort_order);

-- ======================================================
-- ③ 标准资产负债表结构（小企业）
--
--   UNMAPPED_ASSETS / UNMAPPED_LIABILITIES / UNMAPPED_EQUITY 三行不在这里种：
--   它们是**运行时按类别差额补出来的**（科目表新增了一个还没映射的科目时，
--   钱不会凭空消失，而是落在"未归类"行上并给出告警，见 report.rs 模块文档）。
-- ======================================================
INSERT INTO biz_report_item (report_code, item_code, item_name, item_level, parent_code, side,
                             account_pattern, formula, is_deduction, sort_order, remark)
VALUES
    -- ---------- 资产 ----------
    ('BALANCE_SHEET', 'CURRENT_ASSETS',          '流动资产',         1, NULL,                    'ASSET', NULL, NULL, FALSE, 10, '大类标题（金额为 NULL）'),
    ('BALANCE_SHEET', 'MONETARY_FUNDS',          '货币资金',         2, 'CURRENT_ASSETS',        'ASSET', '1001,1002,1012',                          NULL, FALSE, 11, '库存现金 + 银行存款 + 其他货币资金'),
    ('BALANCE_SHEET', 'NOTES_RECEIVABLE',        '应收票据',         2, 'CURRENT_ASSETS',        'ASSET', '1121',                                    NULL, FALSE, 12, NULL),
    ('BALANCE_SHEET', 'ACCOUNTS_RECEIVABLE',     '应收账款',         2, 'CURRENT_ASSETS',        'ASSET', '1122',                                    NULL, FALSE, 13, NULL),
    ('BALANCE_SHEET', 'PREPAYMENTS',             '预付账款',         2, 'CURRENT_ASSETS',        'ASSET', '1123',                                    NULL, FALSE, 14, NULL),
    ('BALANCE_SHEET', 'OTHER_RECEIVABLES',       '其他应收款',       2, 'CURRENT_ASSETS',        'ASSET', '1131,1132,1221',                          NULL, FALSE, 15, '应收股利 + 应收利息 + 其他应收款'),
    ('BALANCE_SHEET', 'INVENTORY',               '存货',             2, 'CURRENT_ASSETS',        'ASSET', '1401,1402,1403,1404,1405,1408,4001,4101',  NULL, FALSE, 16, '材料 / 商品 / 委托加工 + 生产成本、制造费用（在产品）'),
    ('BALANCE_SHEET', 'OTHER_CURRENT_ASSETS',    '其他流动资产',     2, 'CURRENT_ASSETS',        'ASSET', '1901',                                    NULL, FALSE, 17, '待处理财产损溢'),
    ('BALANCE_SHEET', 'CURRENT_ASSETS_TOTAL',    '流动资产合计',     2, 'CURRENT_ASSETS',        'ASSET', NULL,
     'MONETARY_FUNDS + NOTES_RECEIVABLE + ACCOUNTS_RECEIVABLE + PREPAYMENTS + OTHER_RECEIVABLES + INVENTORY + OTHER_CURRENT_ASSETS',
     FALSE, 18, '小计'),

    ('BALANCE_SHEET', 'NONCURRENT_ASSETS',       '非流动资产',       1, NULL,                    'ASSET', NULL, NULL, FALSE, 20, '大类标题（金额为 NULL）'),
    ('BALANCE_SHEET', 'FIXED_ASSETS_ORIGINAL',   '固定资产原价',     2, 'NONCURRENT_ASSETS',     'ASSET', '1601',                                    NULL, FALSE, 21, NULL),
    ('BALANCE_SHEET', 'ACCUM_DEPRECIATION',      '减：累计折旧',     2, 'NONCURRENT_ASSETS',     'ASSET', '1602',                                    NULL, TRUE,  22, '★ 备抵科目：贷方余额，作资产减项'),
    ('BALANCE_SHEET', 'FIXED_ASSETS_NET',        '固定资产净值',     2, 'NONCURRENT_ASSETS',     'ASSET', NULL, 'FIXED_ASSETS_ORIGINAL - ACCUM_DEPRECIATION', FALSE, 23, '原价 − 累计折旧'),
    ('BALANCE_SHEET', 'INTANGIBLE_ORIGINAL',     '无形资产原价',     2, 'NONCURRENT_ASSETS',     'ASSET', '1701',                                    NULL, FALSE, 24, NULL),
    ('BALANCE_SHEET', 'ACCUM_AMORTIZATION',      '减：累计摊销',     2, 'NONCURRENT_ASSETS',     'ASSET', '1702',                                    NULL, TRUE,  25, '★ 备抵科目：贷方余额，作资产减项'),
    ('BALANCE_SHEET', 'INTANGIBLE_ASSETS',       '无形资产净值',     2, 'NONCURRENT_ASSETS',     'ASSET', NULL, 'INTANGIBLE_ORIGINAL - ACCUM_AMORTIZATION', FALSE, 26, '原价 − 累计摊销'),
    ('BALANCE_SHEET', 'OTHER_NONCURRENT_ASSETS', '其他非流动资产',   2, 'NONCURRENT_ASSETS',     'ASSET', '1604,1605,1606,1801',                     NULL, FALSE, 27, '在建工程 / 工程物资 / 固定资产清理 / 长期待摊费用'),
    ('BALANCE_SHEET', 'NONCURRENT_ASSETS_TOTAL', '非流动资产合计',   2, 'NONCURRENT_ASSETS',     'ASSET', NULL,
     'FIXED_ASSETS_NET + INTANGIBLE_ASSETS + OTHER_NONCURRENT_ASSETS', FALSE, 28, '小计'),
    ('BALANCE_SHEET', 'TOTAL_ASSETS',            '资产总计',         1, NULL,                    'ASSET', NULL,
     'CURRENT_ASSETS_TOTAL + NONCURRENT_ASSETS_TOTAL + UNMAPPED_ASSETS', FALSE, 30, '★ 勾稽左式'),

    -- ---------- 负债 ----------
    ('BALANCE_SHEET', 'CURRENT_LIABILITIES',         '流动负债',       1, NULL,                      'LIABILITY', NULL, NULL, FALSE, 40, '大类标题（金额为 NULL）'),
    ('BALANCE_SHEET', 'SHORT_TERM_LOANS',            '短期借款',       2, 'CURRENT_LIABILITIES',     'LIABILITY', '2101',  NULL, FALSE, 41, NULL),
    ('BALANCE_SHEET', 'ACCOUNTS_PAYABLE',            '应付账款',       2, 'CURRENT_LIABILITIES',     'LIABILITY', '2202',  NULL, FALSE, 42, NULL),
    ('BALANCE_SHEET', 'ADVANCES_RECEIVED',           '预收账款',       2, 'CURRENT_LIABILITIES',     'LIABILITY', '2203',  NULL, FALSE, 43, NULL),
    ('BALANCE_SHEET', 'PAYROLL_PAYABLE',             '应付职工薪酬',   2, 'CURRENT_LIABILITIES',     'LIABILITY', '2211',  NULL, FALSE, 44, NULL),
    ('BALANCE_SHEET', 'TAXES_PAYABLE',               '应交税费',       2, 'CURRENT_LIABILITIES',     'LIABILITY', '2221',  NULL, FALSE, 45, '含 2221.01 进项税 / 2221.02 销项税等全部下级'),
    ('BALANCE_SHEET', 'INTEREST_PAYABLE',            '应付利息',       2, 'CURRENT_LIABILITIES',     'LIABILITY', '2231',  NULL, FALSE, 46, NULL),
    ('BALANCE_SHEET', 'OTHER_PAYABLES',              '其他应付款',     2, 'CURRENT_LIABILITIES',     'LIABILITY', '2241',  NULL, FALSE, 47, NULL),
    ('BALANCE_SHEET', 'CURRENT_LIABILITIES_TOTAL',   '流动负债合计',   2, 'CURRENT_LIABILITIES',     'LIABILITY', NULL,
     'SHORT_TERM_LOANS + ACCOUNTS_PAYABLE + ADVANCES_RECEIVED + PAYROLL_PAYABLE + TAXES_PAYABLE + INTEREST_PAYABLE + OTHER_PAYABLES',
     FALSE, 48, '小计'),

    ('BALANCE_SHEET', 'NONCURRENT_LIABILITIES',      '非流动负债',     1, NULL,                      'LIABILITY', NULL, NULL, FALSE, 50, '大类标题（金额为 NULL）'),
    ('BALANCE_SHEET', 'LONG_TERM_LOANS',             '长期借款',       2, 'NONCURRENT_LIABILITIES',  'LIABILITY', '2501',  NULL, FALSE, 51, NULL),
    ('BALANCE_SHEET', 'LONG_TERM_PAYABLES',          '长期应付款',     2, 'NONCURRENT_LIABILITIES',  'LIABILITY', '2701',  NULL, FALSE, 52, NULL),
    ('BALANCE_SHEET', 'DEFERRED_INCOME',             '递延收益',       2, 'NONCURRENT_LIABILITIES',  'LIABILITY', '2401',  NULL, FALSE, 53, NULL),
    ('BALANCE_SHEET', 'NONCURRENT_LIABILITIES_TOTAL', '非流动负债合计', 2, 'NONCURRENT_LIABILITIES',  'LIABILITY', NULL,
     'LONG_TERM_LOANS + LONG_TERM_PAYABLES + DEFERRED_INCOME', FALSE, 54, '小计'),
    ('BALANCE_SHEET', 'TOTAL_LIABILITIES',           '负债合计',       1, NULL,                      'LIABILITY', NULL,
     'CURRENT_LIABILITIES_TOTAL + NONCURRENT_LIABILITIES_TOTAL + UNMAPPED_LIABILITIES', FALSE, 60, NULL),

    -- ---------- 所有者权益 ----------
    ('BALANCE_SHEET', 'OWNERS_EQUITY',               '所有者权益',     1, NULL,                      'EQUITY', NULL, NULL, FALSE, 70, '大类标题（金额为 NULL）'),
    ('BALANCE_SHEET', 'PAID_IN_CAPITAL',             '实收资本',       2, 'OWNERS_EQUITY',           'EQUITY', '3001', NULL, FALSE, 71, NULL),
    ('BALANCE_SHEET', 'CAPITAL_RESERVE',             '资本公积',       2, 'OWNERS_EQUITY',           'EQUITY', '3102', NULL, FALSE, 72, NULL),
    ('BALANCE_SHEET', 'SURPLUS_RESERVE',             '盈余公积',       2, 'OWNERS_EQUITY',           'EQUITY', '3101', NULL, FALSE, 73, NULL),
    ('BALANCE_SHEET', 'UNDISTRIBUTED_PROFIT',        '未分配利润',     2, 'OWNERS_EQUITY',           'EQUITY',
     '@CATEGORY:PROFIT_LOSS,3103,3104', NULL, FALSE, 74,
     '利润分配 + 本年利润 + 全部损益类科目净额（期间未结账时利润还留在 5xxx/6xxx）'),
    ('BALANCE_SHEET', 'TOTAL_EQUITY',                '所有者权益合计', 2, 'OWNERS_EQUITY',           'EQUITY', NULL,
     'PAID_IN_CAPITAL + CAPITAL_RESERVE + SURPLUS_RESERVE + UNDISTRIBUTED_PROFIT + UNMAPPED_EQUITY', FALSE, 75, '小计'),
    ('BALANCE_SHEET', 'TOTAL_LIABILITIES_AND_EQUITY', '负债和所有者权益总计', 1, NULL,              'EQUITY', NULL,
     'TOTAL_LIABILITIES + TOTAL_EQUITY', FALSE, 80, '★ 勾稽右式：必须等于 TOTAL_ASSETS')
ON CONFLICT (report_code, item_code) DO NOTHING;

-- ======================================================
-- ④ 权限码：报表查看 + 导出
--
--   授权段与 V0150 同款：迁移先于 bootstrap 跑（此刻可能还没有角色）→ 空操作 + NOTICE。
--   菜单 id：1015（资产负债表页面，auth_code 即 `:balance-sheet`）
--            1016（导出 Excel 按钮，auth_code `:export`）
-- ======================================================
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001015'::uuid, 'business:erp:report:balance-sheet', 2,
     'business:erp:report:balance-sheet', '/business/erp/report/balance-sheet',
     '20000000-0000-0000-0000-000000000e02'::uuid, 'business/erp/report/balance-sheet/index',
     45, 1, NULL, '资产负债表', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001016'::uuid, 'business:erp:report:export', 3,
     'business:erp:report:export', NULL,
     '20000000-0000-0000-0000-000000001015'::uuid, NULL,
     1, 1, NULL, '导出资产负债表（Excel）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

DO $$
DECLARE
    n_roles  integer;
    n_menus  integer;
    n_grants integer;
    n_acct   integer;
    n_item   integer;
    n_contra integer;
BEGIN
    -- ---------- 科目自检 ----------
    SELECT count(*) INTO n_acct FROM biz_account WHERE deleted_at IS NULL;
    IF n_acct < 56 THEN
        RAISE EXCEPTION 'V0183 自检失败：科目总数应 ≥ 56（7 既有 + 49 新种，2221.01 已存在故去重），实际 %', n_acct;
    END IF;

    -- 备抵科目必须是"资产类别 + 贷方余额"
    SELECT count(*) INTO n_contra
      FROM biz_account
     WHERE account_code IN ('1602', '1702')
       AND account_category = 'ASSET'
       AND balance_direction = 'CREDIT'
       AND deleted_at IS NULL;
    IF n_contra <> 2 THEN
        RAISE EXCEPTION 'V0183 自检失败：备抵科目 1602/1702 应为 ASSET+CREDIT 共 2 行，实际 %', n_contra;
    END IF;

    -- 二级科目必须挂上上级（2221.02 → 2221）
    IF NOT EXISTS (SELECT 1 FROM biz_account c
                     JOIN biz_account p ON p.id = c.parent_id
                    WHERE c.account_code = '2221.02'
                      AND p.account_code = '2221'
                      AND c.level = 2
                      AND c.deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0183 自检失败：2221.02 未正确挂到 2221 下';
    END IF;

    -- ---------- 报表项自检 ----------
    SELECT count(*) INTO n_item FROM biz_report_item WHERE report_code = 'BALANCE_SHEET' AND is_active = 1;
    IF n_item <> 41 THEN
        RAISE EXCEPTION 'V0183 自检失败：BALANCE_SHEET 应有 41 个启用报表项，实际 %', n_item;
    END IF;

    IF NOT EXISTS (SELECT 1 FROM biz_report_item
                    WHERE report_code = 'BALANCE_SHEET' AND item_code = 'ACCUM_DEPRECIATION'
                      AND is_deduction = TRUE AND account_pattern = '1602') THEN
        RAISE EXCEPTION 'V0183 自检失败：累计折旧项必须是 1602 的减项（硬红线 5）';
    END IF;

    -- 公式里引用的每个 item_code 都必须在同一张报表里存在（否则报表一跑就 500）。
    -- 例外：UNMAPPED_* 三行是**运行时补出来的**未归类差额行（不落库，见 report.rs），
    -- 公式可以引用它们。
    IF EXISTS (
        SELECT 1
          FROM biz_report_item i,
               LATERAL regexp_split_to_table(replace(replace(COALESCE(i.formula, ''), '-', '+'), ' ', ''), '\+') AS ref(code)
         WHERE i.report_code = 'BALANCE_SHEET'
           AND COALESCE(i.formula, '') <> ''
           AND ref.code <> ''
           AND ref.code NOT IN ('UNMAPPED_ASSETS', 'UNMAPPED_LIABILITIES', 'UNMAPPED_EQUITY')
           AND NOT EXISTS (SELECT 1 FROM biz_report_item r
                            WHERE r.report_code = 'BALANCE_SHEET' AND r.item_code = ref.code)
    ) THEN
        RAISE EXCEPTION 'V0183 自检失败：报表项公式引用了不存在的 item_code';
    END IF;

    -- ---------- 菜单 / 授权自检 ----------
    SELECT count(*) INTO n_menus
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001015'::uuid,
                  '20000000-0000-0000-0000-000000001016'::uuid);
    IF n_menus <> 2 THEN
        RAISE EXCEPTION 'V0183 自检失败：预期 2 个权限节点，实际 %', n_menus;
    END IF;

    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0183：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
    ELSE
        INSERT INTO sys_role_menu (id, role_id, menu_id)
        SELECT gen_random_uuid(), r.id, m.id
          FROM sys_role r
          CROSS JOIN (VALUES
                ('20000000-0000-0000-0000-000000001015'::uuid),
                ('20000000-0000-0000-0000-000000001016'::uuid)) AS m(id)
         WHERE r.deleted_at IS NULL
           AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'TEST_UADMIN', 'FINANCE', 'ACCOUNTANT')
           AND NOT EXISTS (
                SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
           );
        GET DIAGNOSTICS n_grants = ROW_COUNT;
        RAISE NOTICE 'V0183：资产负债表 2 个权限码已种，授权新增 % 行（角色数 %）', n_grants, n_roles;
    END IF;

    RAISE NOTICE 'V0183 自检通过：科目 % 个、报表项 % 个、权限节点 2 个', n_acct, n_item;
END $$;
