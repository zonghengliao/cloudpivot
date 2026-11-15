-- ======================================================
-- V0185：现金流量表（ERP-R3，简化版）—— 对方科目分类表 + 权限节点
--
-- 依据：kanban 卡 t_e2f972bd §一 / §二 / §七
--   · §一 简化口径：按「现金及现金等价物收支流水」**按对方科目**直接分类，
--           不做间接法调整，不建现金流量项目手工指定表
--   · §二 三大活动（经营 / 投资 / 筹资）的明细行 + 小计 + 净额
--   · §七 硬红线 1：复用 ERP-R1 的框架（同一套期间口径 / 科目匹配语法 /
--           金额格式 / 导出 / REPEATABLE READ 快照 / 权限码体系），本迁移只加配置，不碰凭证
--
-- 回滚：`V0185__erp_cashflow.down.sql`（删本迁移种的 12 条分类行 + 表 + 权限节点；
--       科目 / 凭证 / 资产负债表与利润表的配置一行不动）
--
-- ── 只读不写 ────────────────────────────────────────────────────────
--   现金流量表是**只读**报表：本迁移不碰 `biz_voucher` / `biz_voucher_entry` /
--   `biz_account` / `biz_accounting_period` / `biz_report_item`（硬红线 4）。
--
-- ── 为什么不复用 `biz_report_item`（与 ERP-R2 的差异，必须读完）──────────
--   资产负债表的报表项是「按科目余额取数」的一维结构（`side` 只有 ASSET / LIABILITY /
--   EQUITY 三值，`ck_biz_report_side` 约束锁死）。现金流量表的每一行是**二维**的：
--     · 属于哪个活动（经营 / 投资 / 筹资）—— 结构维度
--     · 是流入还是流出 —— 方向维度
--   硬塞进 `side` 只能二选一（要么活动丢失、要么方向丢失），会让「投资活动的流出」
--   与「经营活动的流出」混成一行 —— 报表语义当场失真。
--   所以本迁移新建一张**专用分类表** `biz_cashflow_item`：
--   匹配语法、金额格式、期间口径、导出、权限体系仍然全复用 R1 框架（`match_account` /
--   `resolve_period` / `diff_text` / `money_text` / `write_sheet`），只有配置结构不同。
--
-- ── 一笔现金分录怎么分类（`cash_flow.rs::classify` 的口径）─────────────
--   同一张凭证里：现金类分录（`1001*` / `1002*`）= 现金流本身；
--   非现金分录 = **对方科目**，按它的净额方向决定流入 / 流出：
--     对方科目贷方净额（贷 − 借 > 0）→ 现金流入，金额 = 该净额
--     对方科目借方净额（借 − 贷 > 0）→ 现金流出，金额 = 该净额绝对值
--   多借多贷的复合凭证按**对方科目**逐组拆分（不做比例分摊，金额天然等于现金净额，
--   因为复式记账下 Σ非现金(贷−借) = Σ现金(借−贷)）。
--   同一活动内 `INFLOW` / `OUTFLOW` 两张清单互不重叠；同一科目可以同时出现在
--   流入清单（收到借款）和流出清单（偿还借款）里 —— 那是同一科目的两个方向，不是重复映射。
--
-- ── 幂等 ──────────────────────────────────────────────────────────
--   表：`CREATE TABLE IF NOT EXISTS`
--   分类行：`ON CONFLICT (item_code) DO NOTHING`
--   菜单：`ON CONFLICT (id) DO NOTHING`；授权用 `NOT EXISTS` 去重
--   自检块只断言「行数够、关键行在位、同方向不重叠、非现金科目全覆盖」
-- ======================================================

-- ======================================================
-- ① 对方科目分类表
-- ======================================================
CREATE TABLE IF NOT EXISTS biz_cashflow_item (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    item_code       VARCHAR(32)  NOT NULL,                  -- 明细行编码：OPER_SALES_RECEIPTS
    item_name       VARCHAR(64)  NOT NULL,                  -- 明细行名称（纸面报表原文）
    activity        VARCHAR(12)  NOT NULL,                  -- 活动：OPERATING / INVESTING / FINANCING
    direction       VARCHAR(8)   NOT NULL,                  -- 方向：INFLOW / OUTFLOW
    account_pattern VARCHAR(255) NOT NULL,                  -- **对方科目**匹配模式（复用 report::match_pattern 语法）
    sort_order      INT          NOT NULL DEFAULT 0,        -- 显示顺序（决定明细行顺序）
    is_active       SMALLINT     NOT NULL DEFAULT 1,        -- 启用位：1 启用 / 0 停用
    remark          TEXT,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT uk_biz_cashflow_item   UNIQUE (item_code),
    CONSTRAINT ck_biz_cashflow_activity CHECK (activity IN ('OPERATING', 'INVESTING', 'FINANCING')),
    CONSTRAINT ck_biz_cashflow_direction CHECK (direction IN ('INFLOW', 'OUTFLOW')),
    CONSTRAINT ck_biz_cashflow_active CHECK (is_active IN (0, 1)),
    -- 取数方式必须给（没有 pattern 的行永远出 0，等于配错）
    CONSTRAINT ck_biz_cashflow_pattern CHECK (length(btrim(account_pattern)) > 0)
);

CREATE INDEX IF NOT EXISTS idx_biz_cashflow_item_order
    ON biz_cashflow_item (activity, direction, sort_order);

COMMENT ON TABLE biz_cashflow_item IS
    '现金流量表对方科目分类（ERP-R3 简化版）：每一行 = 一个报表明细行 + 它接受哪些对方科目';

-- ======================================================
-- ② 标准结构（12 行：经营 2+4 / 投资 1+1 / 筹资 2+2）
--
--   `*` 前缀语法在 `report::match_account` 里表示编码前缀；纯编码清单是**精确匹配 + 下级**。
--   2221 命中 2221 / 2221.01 / 2221.02（`match_account` 的「科目 + 其全部下级」规则）。
--   1012 其他货币资金**刻意不进现金类科目**（卡面 §一 定义现金类 = 1001/1002 前缀）：
--   它一旦真有钱进出，报表会把它归「其他」并在 warnings 里点名，绝不静默吞掉。
-- ======================================================
INSERT INTO biz_cashflow_item (item_code, item_name, activity, direction, account_pattern, sort_order, remark)
VALUES
    -- ---------- 一、经营活动 ----------
    ('OPER_SALES_RECEIPTS',  '销售商品、提供劳务收到的现金',       'OPERATING', 'INFLOW',
     '1122,1121,2203,5001,5051',                                            101,
     '应收账款 / 应收票据收回 + 预收账款增加 + 收入直接收现'),
    ('OPER_OTHER_INFLOW',    '收到的其他与经营活动有关的现金',     'OPERATING', 'INFLOW',
     '1221,2241,2401,5301,1901',                                            102,
     '其他应收款收回 / 其他应付款借入 / 递延收益 / 营业外收入 / 待处理财产损溢'),

    ('OPER_PURCHASE_PAYMENTS', '购买商品、接受劳务支付的现金',     'OPERATING', 'OUTFLOW',
     '2202,1123,1401,1402,1403,1404,1405,1408,4001,4101,5401,5402,6401',    201,
     '应付账款 / 预付账款 + 材料与库存商品 + 生产成本 / 制造费用 + 成本类损益科目'),
    ('OPER_STAFF_PAYMENTS',  '支付给职工以及为职工支付的现金',     'OPERATING', 'OUTFLOW',
     '2211',                                                                202,
     '应付职工薪酬（直接借费用贷现金的工资也走「其他」并在 warnings 提示）'),
    ('OPER_TAX_PAYMENTS',    '支付的各项税费',                     'OPERATING', 'OUTFLOW',
     '2221,5801,5403',                                                      203,
     '应交税费（含进项 / 销项下级）+ 所得税费用 + 营业税金及附加'),
    ('OPER_OTHER_OUTFLOW',   '支付的其他与经营活动有关的现金',     'OPERATING', 'OUTFLOW',
     '5601,5602,5603,5711,1221,2241,1901',                                  204,
     '销售 / 管理 / 财务费用 + 营业外支出 + 其他应收应付'),

    -- ---------- 二、投资活动 ----------
    ('INVEST_RECOVERY',      '收回投资、取得投资收益和处置长期资产收到的现金', 'INVESTING', 'INFLOW',
     '1601,1602,1604,1605,1606,1701,1702,1801,5111,1131,1132',              301,
     '处置固定资产 / 无形资产 / 在建工程 + 投资收益 + 应收股利利息'),
    ('INVEST_PURCHASES',     '购建固定资产、无形资产和其他长期资产支付的现金', 'INVESTING', 'OUTFLOW',
     '1601,1602,1604,1605,1606,1701,1702,1801',                             351,
     '购置长期资产（含备抵科目 1602 / 1702，处置与购置共用同一组科目）'),

    -- ---------- 三、筹资活动 ----------
    ('FIN_CAPITAL_IN',       '吸收投资收到的现金',                 'FINANCING', 'INFLOW',
     '3001,3101,3102',                                                      401, '实收资本 + 盈余公积 + 资本公积'),
    ('FIN_LOANS_IN',         '取得借款收到的现金',                 'FINANCING', 'INFLOW',
     '2101,2501,2701',                                                      402, '短期借款 + 长期借款 + 长期应付款'),
    ('FIN_DEBT_REPAY',       '偿还债务支付的现金',                 'FINANCING', 'OUTFLOW',
     '2101,2501,2701',                                                      451, '短期借款 + 长期借款 + 长期应付款（与「取得借款」同科目不同方向）'),
    ('FIN_DISTRIBUTION',     '分配股利、利润或偿付利息支付的现金', 'FINANCING', 'OUTFLOW',
     '3104,3103,2231,3001',                                                 452, '利润分配 / 本年利润 + 应付利息 + 减资')
ON CONFLICT (item_code) DO NOTHING;

-- ======================================================
-- ③ 权限节点：现金流量表页面
--
--   导出按钮**复用** R1 的 `business:erp:report:export`（V0183 …1016 已存在、已授权），
--   不再种第二个同码节点（`sys_menu.auth_code` 是权限码的唯一来源，一码一节点）。
--   菜单 id：1018（V0183 用 1015/1016，V0184 用 1017）；parent 挂「财务管理」…0e02。
-- ======================================================
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001018'::uuid, 'business:erp:report:cash-flow', 2,
     'business:erp:report:cash-flow', '/business/erp/report/cash-flow',
     '20000000-0000-0000-0000-000000000e02'::uuid, 'business/erp/report/cash-flow/index',
     47, 1, NULL, '现金流量表', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

DO $$
DECLARE
    n_roles  integer;
    n_menus  integer;
    n_grants integer;
    n_item   integer;
    n_dup    integer;
    n_miss   integer;
BEGIN
    -- ---------- 分类行自检 ----------
    SELECT count(*) INTO n_item FROM biz_cashflow_item WHERE is_active = 1;
    IF n_item <> 12 THEN
        RAISE EXCEPTION 'V0185 自检失败：biz_cashflow_item 应有 12 条启用行，实际 %', n_item;
    END IF;

    -- 每个活动都必须有流入和流出，否则净额会一边倒
    IF EXISTS (
        SELECT 1 FROM (VALUES ('OPERATING'), ('INVESTING'), ('FINANCING')) AS a(activity)
         WHERE NOT EXISTS (SELECT 1 FROM biz_cashflow_item i
                            WHERE i.activity = a.activity AND i.direction = 'INFLOW' AND i.is_active = 1)
            OR NOT EXISTS (SELECT 1 FROM biz_cashflow_item i
                            WHERE i.activity = a.activity AND i.direction = 'OUTFLOW' AND i.is_active = 1)
    ) THEN
        RAISE EXCEPTION 'V0185 自检失败：有活动缺流入或流出明细行';
    END IF;

    -- 同一方向内不许两行匹配同一科目（同一笔钱算两遍 = 静默多计）。
    -- 匹配语义与 `report::match_account` 一致：编码相等，或一方是另一方的上级
    -- （`2221` 覆盖 `2221.01`），所以按「前缀关系」比对，而不是字符串相等。
    SELECT count(*) INTO n_dup
      FROM biz_cashflow_item a
      JOIN biz_cashflow_item b
        ON b.direction = a.direction AND a.item_code < b.item_code AND b.is_active = 1
     CROSS JOIN LATERAL unnest(string_to_array(a.account_pattern, ',')) AS ca(code)
      CROSS JOIN LATERAL unnest(string_to_array(b.account_pattern, ',')) AS cb(code)
      WHERE a.is_active = 1
        AND (   btrim(ca.code) = btrim(cb.code)
             OR btrim(ca.code) LIKE btrim(cb.code) || '.%'
             OR btrim(cb.code) LIKE btrim(ca.code) || '.%');
    IF n_dup > 0 THEN
        RAISE EXCEPTION 'V0185 自检失败：同方向内有 % 组重复映射的对方科目', n_dup;
    END IF;

    -- 非现金科目必须全覆盖；**1012 其他货币资金例外** ——
    --   它不属于卡面 §一 定义的现金类（1001/1002 前缀），也不该被当成「对方科目」去分类：
    --   钱在 1002 ↔ 1012 之间划转时，报表会把 1012 的净额归「其他」并在 warnings 里点名
    --   （口径差异必须可见），勾稽会差这一块 —— 这是刻意留的闹钟，不是漏配。
    SELECT count(*) INTO n_miss
      FROM biz_account c
     WHERE c.deleted_at IS NULL
       AND LEFT(c.account_code, 4) NOT IN ('1001', '1002')
       AND c.account_code NOT IN ('1012')
       AND NOT EXISTS (
            SELECT 1 FROM biz_cashflow_item i
             CROSS JOIN LATERAL unnest(string_to_array(i.account_pattern, ',')) AS ca(code)
             WHERE i.is_active = 1
               AND (c.account_code = btrim(ca.code) OR c.account_code LIKE btrim(ca.code) || '.%')
       );
    IF n_miss > 0 THEN
        RAISE EXCEPTION 'V0185 自检失败：有 % 个非现金科目没有被任何现金流量表行覆盖（会全部落到「其他」）', n_miss;
    END IF;

    -- 现金类科目自己不许进分类清单（否则现金内部划转会被当成业务现金流）
    IF EXISTS (
        SELECT 1 FROM biz_cashflow_item i
          CROSS JOIN LATERAL unnest(string_to_array(i.account_pattern, ',')) AS ca(code)
         WHERE LEFT(btrim(ca.code), 4) IN ('1001', '1002')
    ) THEN
        RAISE EXCEPTION 'V0185 自检失败：现金类科目（1001/1002）不许出现在对方科目清单里';
    END IF;

    -- ---------- 菜单 / 授权自检 ----------
    SELECT count(*) INTO n_menus
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001018'::uuid;
    IF n_menus <> 1 THEN
        RAISE EXCEPTION 'V0185 自检失败：现金流量表权限节点没种上（实际 % 行）', n_menus;
    END IF;

    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0185：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
    ELSE
        INSERT INTO sys_role_menu (id, role_id, menu_id)
        SELECT gen_random_uuid(), r.id, m.id
          FROM sys_role r
          CROSS JOIN (VALUES
                ('20000000-0000-0000-0000-000000001018'::uuid)) AS m(id)
         WHERE r.deleted_at IS NULL
           AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'TEST_UADMIN', 'FINANCE', 'ACCOUNTANT')
           AND NOT EXISTS (
                SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
           );
        GET DIAGNOSTICS n_grants = ROW_COUNT;
        RAISE NOTICE 'V0185：现金流量表权限码已种，授权新增 % 行（角色数 %）', n_grants, n_roles;
    END IF;

    RAISE NOTICE 'V0185 自检通过：现金流量表分类行 % 个、权限节点 1 个', n_item;
END $$;
