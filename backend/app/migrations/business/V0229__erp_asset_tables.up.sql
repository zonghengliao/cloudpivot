-- ======================================================
-- V0229：固定资产三表 + 5 条资产分类种子 + 11 个权限码 + 固定资产菜单
--       （ARCH-6 §5.2.1 / §5.2.2 / §5.2.3 / §6.7）
--
-- 表（DDL 逐字取自 ARCH-6 §5.2，未自创列名）：
--   · biz_asset_category     资产分类（折旧方法 / 年限 / 残值率 / 三个科目）
--   · biz_fixed_asset        资产卡片（一台设备一张卡）
--   · biz_fixed_asset_change 变动留痕（append-only：**没有** updated_at / deleted_at）
--
-- ── 本章最要紧的四条设计约束（照着建表就能落地，不靠代码自觉）──────
-- ① **折旧方法只有一个值** `STRAIGHT_LINE`（ARCH-6 D6）：CHECK 只放一个值。
--    将来加方法 = 一次显式迁移 + 一组新单测，不是「先把枚举放开再说」。
-- ② **净值恒等** `ck_biz_fa_net`：`net_value = original_value − accumulated_depreciation`。
--    与 `ck_biz_payable_unpaid` 同款 —— 程序算错会被库挡住，不会静默错账。
-- ③ **累计折旧不许超过可折旧总额**（`ck_biz_fa_accum`）：提足即停。
-- ④ **变动留痕只增不改**：`biz_fixed_asset_change` 没有 `updated_at` / `deleted_at`，
--    主表当前值可以被覆盖，但每次覆盖必须同事务写一行（ARCH-6 §5.2.3）。
--
-- ── 科目（本迁移**不建科目**）──────────────────────────────
--   1601 固定资产 / 1602 累计折旧 / 1606 固定资产清理 由 V0183 种子已有；
--   5602.01 管理费用-折旧费 由 V0226（FIN-1）种好。
--   卡片还缺 `6602 管理费用-折旧`（卡面原文）——**库里没有 6602**，
--   按 ARCH-6 §8.1 ① 的结论用 5602.01（利润表 ADMIN_EXPENSE 只映射 5602）。
--   末尾自检：三个科目任一缺失 → 迁移失败（宁可起不来，也不要种出一张没有科目的分类）。
--
-- ── 权限码（11 个，唯一来源就是 sys_menu.auth_code）────────────────
--   asset: list / create / update / delete / transfer / dispose / detail   (7)
--   asset-category: list / create / update / delete                        (4)
--   审批不需要新码：走流程引擎的 `business:flow:approve`（与 po_approve 同款）。
--
-- ── 菜单 ─────────────────────────────────────────────────
--   固定资产（目录 1400）→ 资产卡片（1401）/ 资产分类（1402）
--   + 卡片页的按钮码（menu_type=3：新建 / 编辑 / 删除 / 转移 / 处置 / 详情）
--   折旧计提页（1403）在 V0230（它与折旧表、折旧权限码同批）。
--   菜单 id 段 14xx 是开工前扫空段选的（13xx 已被 FIN-1 预算占用）。
--
-- ── 幂等与回滚 ───────────────────────────────────────────
-- 建表 `IF NOT EXISTS`；索引 `IF NOT EXISTS`；分类种子 `NOT EXISTS`；
-- 菜单 / 权限 `ON CONFLICT (id) DO NOTHING`。回滚见 down（先删菜单，再删三表）。
-- ======================================================

-- ---------- ① biz_asset_category（资产分类，决定折旧参数） ----------
CREATE TABLE IF NOT EXISTS biz_asset_category (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    category_code         VARCHAR(40)   NOT NULL,
    category_name         VARCHAR(120)  NOT NULL,
    depreciation_method   VARCHAR(24)   NOT NULL DEFAULT 'STRAIGHT_LINE',
    useful_life_months    INT           NOT NULL,
    salvage_rate          NUMERIC(7,4)  NOT NULL DEFAULT 0.05,
    asset_account_id      UUID          NOT NULL,
    accum_account_id      UUID          NOT NULL,
    expense_account_id    UUID          NOT NULL,
    status                SMALLINT      NOT NULL DEFAULT 1,
    sort_order            INT           NOT NULL DEFAULT 0,
    remark                TEXT,
    created_at            TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at            TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by             UUID,
    update_by             UUID,
    deleted_at            TIMESTAMPTZ,

    -- D6：本期只有一种方法，CHECK 只放一个值
    CONSTRAINT ck_biz_asset_cat_method CHECK (depreciation_method IN ('STRAIGHT_LINE')),
    CONSTRAINT ck_biz_asset_cat_life   CHECK (useful_life_months BETWEEN 1 AND 600),
    CONSTRAINT ck_biz_asset_cat_salv   CHECK (salvage_rate >= 0 AND salvage_rate < 1),
    CONSTRAINT ck_biz_asset_cat_status CHECK (status IN (0,1))
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_asset_cat_code
    ON biz_asset_category (category_code) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_asset_cat_status
    ON biz_asset_category (status, sort_order);

COMMENT ON TABLE biz_asset_category IS
    '资产分类：折旧方法/年限/残值率/三个科目都在这里配；本期折旧方法唯一合法值 STRAIGHT_LINE';

-- ---------- ② biz_fixed_asset（资产卡片） ----------
CREATE TABLE IF NOT EXISTS biz_fixed_asset (
    id                       UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    asset_no                 VARCHAR(32)   NOT NULL,
    asset_name               VARCHAR(200)  NOT NULL,
    category_id              UUID          NOT NULL,
    category_code            VARCHAR(40)   NOT NULL,
    spec                     VARCHAR(200),
    use_dept_id              UUID          NOT NULL,
    use_dept_name            VARCHAR(120)  NOT NULL,
    custodian_id             UUID,
    custodian_name           VARCHAR(50),
    location                 VARCHAR(200),
    original_value           NUMERIC(18,4) NOT NULL,
    tax_amount               NUMERIC(18,4) NOT NULL DEFAULT 0,
    residual_value           NUMERIC(18,4) NOT NULL DEFAULT 0,
    depreciable_base         NUMERIC(18,4) NOT NULL DEFAULT 0,
    depreciation_method      VARCHAR(24)   NOT NULL DEFAULT 'STRAIGHT_LINE',
    useful_life_months       INT           NOT NULL,
    salvage_rate             NUMERIC(7,4)  NOT NULL DEFAULT 0.05,
    monthly_depreciation     NUMERIC(18,4) NOT NULL DEFAULT 0,
    start_depreciation_date  DATE          NOT NULL,
    accumulated_depreciation NUMERIC(18,4) NOT NULL DEFAULT 0,
    net_value                NUMERIC(18,4) NOT NULL DEFAULT 0,
    depreciation_months_done INT           NOT NULL DEFAULT 0,
    last_depreciation_period VARCHAR(7),
    purchase_date            DATE          NOT NULL,
    source_type              VARCHAR(16)   NOT NULL DEFAULT 'MANUAL',
    source_id                UUID,
    source_no                VARCHAR(32),
    buy_mode                 VARCHAR(16),
    supplier_id              UUID,
    supplier_name            VARCHAR(200),
    voucher_id               UUID,
    status                   VARCHAR(16)   NOT NULL DEFAULT 'IN_USE',
    disposal_type            VARCHAR(16),
    disposal_date            DATE,
    disposal_voucher_id      UUID,
    remark                   TEXT,
    created_at               TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at               TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by                UUID,
    update_by                UUID,
    deleted_at               TIMESTAMPTZ,

    CONSTRAINT ck_biz_fa_status CHECK (status IN
        ('IN_USE','IDLE','DISPOSED','CANCELLED')),
    CONSTRAINT ck_biz_fa_method CHECK (depreciation_method IN ('STRAIGHT_LINE')),
    CONSTRAINT ck_biz_fa_source CHECK (source_type IN ('MANUAL','PURCHASE_ORDER','STOCK_IN')),
    CONSTRAINT ck_biz_fa_buy_mode CHECK (buy_mode IS NULL OR buy_mode IN ('DIRECT_BUY','FROM_STOCK')),
    CONSTRAINT ck_biz_fa_disposal CHECK (disposal_type IS NULL OR disposal_type IN
        ('SALE','SCRAP','LOSS')),
    CONSTRAINT ck_biz_fa_value  CHECK (original_value > 0 AND tax_amount >= 0
                                       AND residual_value >= 0
                                       AND depreciable_base = original_value - residual_value),
    CONSTRAINT ck_biz_fa_rate   CHECK (salvage_rate >= 0 AND salvage_rate < 1),
    CONSTRAINT ck_biz_fa_life   CHECK (useful_life_months BETWEEN 1 AND 600),
    -- 累计折旧不许超过可折旧总额（提足即停）
    CONSTRAINT ck_biz_fa_accum  CHECK (accumulated_depreciation >= 0
                                       AND accumulated_depreciation <= depreciable_base),
    -- 净值恒等（与 ck_biz_payable_unpaid 同款：程序算错会被库挡住）
    CONSTRAINT ck_biz_fa_net    CHECK (net_value = original_value - accumulated_depreciation),
    CONSTRAINT ck_biz_fa_months CHECK (depreciation_months_done >= 0
                                       AND depreciation_months_done <= useful_life_months + 1),
    -- 已处置必须有处置日与类型；未处置不许有
    CONSTRAINT ck_biz_fa_disposed CHECK (
        (status = 'DISPOSED') = (disposal_date IS NOT NULL AND disposal_type IS NOT NULL)),
    -- 来源非手工时必须给 source_id（避免「标记了从采购来，却查不到哪张单」）
    CONSTRAINT ck_biz_fa_source_ref CHECK (
        (source_type = 'MANUAL' AND source_id IS NULL)
     OR (source_type <> 'MANUAL' AND source_id IS NOT NULL))
);

COMMENT ON TABLE biz_fixed_asset IS
    '固定资产卡片（一台设备/一处房产一张卡；同型号多台请建多张卡，不做「卡片数量」字段）';
COMMENT ON COLUMN biz_fixed_asset.tax_amount IS
    '可抵扣进项税：只用于购入凭证拆分（借 2221.01），不减少原值（小企业按含税价入账）';
COMMENT ON COLUMN biz_fixed_asset.accumulated_depreciation IS
    '缓存列：唯一写入者是折旧过账（§5.5.4），巡检 K2 用 biz_depreciation 求和反查';
COMMENT ON COLUMN biz_fixed_asset.monthly_depreciation IS
    '月折旧额 = round(可折旧总额 / 使用月数, 4)；最后一个月用「差额补齐」而不是本位（§5.4.3）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_fa_no
    ON biz_fixed_asset (asset_no) WHERE deleted_at IS NULL;
-- 一张来源单只能建一张卡（防止同一收货单重复转固）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_fa_source
    ON biz_fixed_asset (source_type, source_id)
    WHERE source_id IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_fa_category  ON biz_fixed_asset (category_id);
CREATE INDEX IF NOT EXISTS idx_biz_fa_dept      ON biz_fixed_asset (use_dept_id, status);
CREATE INDEX IF NOT EXISTS idx_biz_fa_custodian ON biz_fixed_asset (custodian_id) WHERE custodian_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_fa_status    ON biz_fixed_asset (status, purchase_date DESC);
-- 折旧批跑的核心扫描：在役 + 已到计提起始日 + 未提足
CREATE INDEX IF NOT EXISTS idx_biz_fa_dep_scan
    ON biz_fixed_asset (start_depreciation_date, last_depreciation_period)
    WHERE status IN ('IN_USE','IDLE') AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_fa_voucher
    ON biz_fixed_asset (voucher_id) WHERE voucher_id IS NOT NULL;

-- ---------- ③ biz_fixed_asset_change（变动留痕，append-only） ----------
CREATE TABLE IF NOT EXISTS biz_fixed_asset_change (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    asset_id      UUID          NOT NULL REFERENCES biz_fixed_asset(id),
    change_type   VARCHAR(16)   NOT NULL,
    change_date   DATE          NOT NULL,
    from_dept_id  UUID,  from_dept_name  VARCHAR(120),
    to_dept_id    UUID,  to_dept_name    VARCHAR(120),
    from_user_id  UUID,  from_user_name  VARCHAR(50),
    to_user_id    UUID,  to_user_name    VARCHAR(50),
    from_status   VARCHAR(16),
    to_status     VARCHAR(16),
    reason        VARCHAR(255)  NOT NULL,
    voucher_id    UUID,
    approval_instance_id UUID,
    operator_id   UUID,
    operator_name VARCHAR(50),
    remark        TEXT,
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT ck_biz_fa_chg_type CHECK (change_type IN
        ('TRANSFER','CUSTODIAN','DISPOSAL','ENABLE','IDLE')),
    -- 转移必须有前后部门；使用人变更必须有前后人
    CONSTRAINT ck_biz_fa_chg_transfer CHECK (
        change_type <> 'TRANSFER' OR (from_dept_id IS NOT NULL AND to_dept_id IS NOT NULL)),
    CONSTRAINT ck_biz_fa_chg_custodian CHECK (
        change_type <> 'CUSTODIAN' OR (to_user_id IS NOT NULL)),
    CONSTRAINT ck_biz_fa_chg_disposal CHECK (
        change_type <> 'DISPOSAL' OR (to_status = 'DISPOSED'))
);

CREATE INDEX IF NOT EXISTS idx_biz_fa_chg_asset ON biz_fixed_asset_change (asset_id, change_date DESC);
CREATE INDEX IF NOT EXISTS idx_biz_fa_chg_type  ON biz_fixed_asset_change (change_type, change_date DESC);

COMMENT ON TABLE biz_fixed_asset_change IS
    '资产变动留痕（append-only，无 updated_at / deleted_at）：主表存当前值，本表存每一次变化';

-- ---------- ④ 5 条资产分类种子（ARCH-6 §5.2.1） ----------
-- 年限取「税法最低年限」（多数小企业的实际选择）；要按会计年限调，改分类即可（不改代码）。
-- 科目一律按编码解析：1601 / 1602 / 5602.01（5602.01 由 V0226 种子；6602 不存在）。
INSERT INTO biz_asset_category (
    category_code, category_name, depreciation_method, useful_life_months, salvage_rate,
    asset_account_id, accum_account_id, expense_account_id, status, sort_order, remark)
SELECT v.code, v.name, 'STRAIGHT_LINE', v.months, 0.05,
       (SELECT id FROM biz_account WHERE account_code = '1601' AND deleted_at IS NULL),
       (SELECT id FROM biz_account WHERE account_code = '1602' AND deleted_at IS NULL),
       (SELECT id FROM biz_account WHERE account_code = '5602.01' AND deleted_at IS NULL),
       1, v.sort, v.remark
  FROM (VALUES
        ('ELEC',      '电子设备',   36,  10, '电脑 / 服务器 / 打印机，税法最低 3 年'),
        ('FURNITURE', '办公家具',   60,  20, '桌椅 / 文件柜，税法最低 5 年'),
        ('VEHICLE',   '运输工具',   48,  30, '车辆，税法最低 4 年'),
        ('MACHINE',   '机器设备',   120, 40, '生产设备，税法最低 10 年'),
        ('BUILDING',  '房屋建筑物', 240, 50, '厂房 / 办公用房，税法最低 20 年')
       ) AS v(code, name, months, sort, remark)
 WHERE NOT EXISTS (SELECT 1 FROM biz_asset_category c
                    WHERE c.category_code = v.code AND c.deleted_at IS NULL);

-- ---------- ⑤ 权限码与菜单 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    -- 目录：固定资产（挂在 财务管理 e02 下）
    ('20000000-0000-0000-0000-000000001400'::uuid, '固定资产', 1, '',
     '/business/erp/asset', '20000000-0000-0000-0000-000000000e02'::uuid, NULL,
     29, 1, 'server', '固定资产（卡片 / 分类 / 折旧）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001401'::uuid, '资产卡片', 2, 'business:erp:asset:list',
     '/business/erp/asset', '20000000-0000-0000-0000-000000001400'::uuid,
     'business/erp/asset/index', 10, 1, 'server', '资产卡片（建卡 / 转移 / 处置）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001402'::uuid, '资产分类', 2, 'business:erp:asset-category:list',
     '/business/erp/asset-category', '20000000-0000-0000-0000-000000001400'::uuid,
     'business/erp/asset-category/index', 20, 1, 'folder', '资产分类（年限 / 残值率 / 科目）', TRUE, 'business'),
    -- 资产卡片页的按钮权限（menu_type=3）
    ('20000000-0000-0000-0000-000000001411'::uuid, 'business:erp:asset:create', 3, 'business:erp:asset:create',
     NULL, '20000000-0000-0000-0000-000000001401'::uuid, NULL, 10, 1, NULL, '新建资产卡片（含购入凭证）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001412'::uuid, 'business:erp:asset:update', 3, 'business:erp:asset:update',
     NULL, '20000000-0000-0000-0000-000000001401'::uuid, NULL, 20, 1, NULL, '编辑资产卡片（仅未处置）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001413'::uuid, 'business:erp:asset:delete', 3, 'business:erp:asset:delete',
     NULL, '20000000-0000-0000-0000-000000001401'::uuid, NULL, 30, 1, NULL, '删除资产卡片（软删）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001414'::uuid, 'business:erp:asset:transfer', 3, 'business:erp:asset:transfer',
     NULL, '20000000-0000-0000-0000-000000001401'::uuid, NULL, 40, 1, NULL, '部门转移（不产生凭证，留痕）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001415'::uuid, 'business:erp:asset:dispose', 3, 'business:erp:asset:dispose',
     NULL, '20000000-0000-0000-0000-000000001401'::uuid, NULL, 50, 1, NULL, '资产处置（出售 / 报废 / 盘亏，走审批）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001416'::uuid, 'business:erp:asset:detail', 3, 'business:erp:asset:detail',
     NULL, '20000000-0000-0000-0000-000000001401'::uuid, NULL, 60, 1, NULL, '资产详情（变动时间线 / 折旧明细 / 凭证）', FALSE, 'business'),
    -- 资产分类页的按钮权限
    ('20000000-0000-0000-0000-000000001421'::uuid, 'business:erp:asset-category:create', 3, 'business:erp:asset-category:create',
     NULL, '20000000-0000-0000-0000-000000001402'::uuid, NULL, 10, 1, NULL, '新建资产分类', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001422'::uuid, 'business:erp:asset-category:update', 3, 'business:erp:asset-category:update',
     NULL, '20000000-0000-0000-0000-000000001402'::uuid, NULL, 20, 1, NULL, '编辑资产分类', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001423'::uuid, 'business:erp:asset-category:delete', 3, 'business:erp:asset-category:delete',
     NULL, '20000000-0000-0000-0000-000000001402'::uuid, NULL, 30, 1, NULL, '删除资产分类', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 授权：与 ARCH-6 §6.7.2 同口径（超管 + 管理 + 财务两档 + 部门主管）
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, m.id
  FROM sys_role r
  CROSS JOIN (VALUES
        ('20000000-0000-0000-0000-000000001400'::uuid),
        ('20000000-0000-0000-0000-000000001401'::uuid),
        ('20000000-0000-0000-0000-000000001402'::uuid),
        ('20000000-0000-0000-0000-000000001411'::uuid),
        ('20000000-0000-0000-0000-000000001412'::uuid),
        ('20000000-0000-0000-0000-000000001413'::uuid),
        ('20000000-0000-0000-0000-000000001414'::uuid),
        ('20000000-0000-0000-0000-000000001415'::uuid),
        ('20000000-0000-0000-0000-000000001416'::uuid),
        ('20000000-0000-0000-0000-000000001421'::uuid),
        ('20000000-0000-0000-0000-000000001422'::uuid),
        ('20000000-0000-0000-0000-000000001423'::uuid)) AS m(id)
 WHERE r.deleted_at IS NULL
   AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'FINANCE_MANAGER', 'FINANCE_DIRECTOR', 'DEPT_LEADER')
   AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id);

-- ---------- ⑥ 自检 ----------
DO $$
DECLARE
    n int;
BEGIN
    -- 6.1 三张表
    IF to_regclass('public.biz_asset_category') IS NULL
       OR to_regclass('public.biz_fixed_asset') IS NULL
       OR to_regclass('public.biz_fixed_asset_change') IS NULL THEN
        RAISE EXCEPTION 'V0229 自检失败：固定资产三表未全部建成';
    END IF;

    -- 6.2 关键索引（单号 / 来源单防重复转固 / 折旧扫描）
    IF NOT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'uk_biz_fa_no')
       OR NOT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'uk_biz_fa_source')
       OR NOT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'idx_biz_fa_dep_scan') THEN
        RAISE EXCEPTION 'V0229 自检失败：固定资产关键索引缺失';
    END IF;

    -- 6.3 净值恒等 / 提足即停 / 折旧方法单值 三条约束
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_fa_net')
       OR NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_fa_accum')
       OR NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_asset_cat_method') THEN
        RAISE EXCEPTION 'V0229 自检失败：固定资产关键 CHECK 缺失';
    END IF;

    -- 6.4 科目必须先存在（本迁移不代建科目 —— ARCH-6 §6.6 / 卡面硬约束）
    IF NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code = '1601' AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0229 自检失败：科目 1601 固定资产不存在（请先补科目，不要在本迁移里代建）';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code = '1602' AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0229 自检失败：科目 1602 累计折旧不存在';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code = '1606' AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0229 自检失败：科目 1606 固定资产清理不存在';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code = '5602.01' AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0229 自检失败：科目 5602.01 管理费用-折旧费不存在（V0226 种子缺失）';
    END IF;

    -- 6.5 5 条分类种子（费用科目必须解析到 5602.01，否则分类建了也用不了）
    SELECT count(*) INTO n FROM biz_asset_category
     WHERE deleted_at IS NULL AND category_code IN ('ELEC','FURNITURE','VEHICLE','MACHINE','BUILDING');
    IF n <> 5 THEN
        RAISE EXCEPTION 'V0229 自检失败：资产分类种子只种了 % 条（期望 5）', n;
    END IF;
    IF EXISTS (SELECT 1 FROM biz_asset_category c
                WHERE c.deleted_at IS NULL
                  AND (c.asset_account_id IS NULL OR c.accum_account_id IS NULL
                       OR c.expense_account_id IS NULL)) THEN
        RAISE EXCEPTION 'V0229 自检失败：分类种子的科目 id 有空值（1601/1602/5602.01 解析失败）';
    END IF;

    -- 6.6 11 个权限码
    SELECT count(*) INTO n FROM sys_menu
     WHERE auth_code IN ('business:erp:asset:list','business:erp:asset:create','business:erp:asset:update','business:erp:asset:delete',
                         'business:erp:asset:transfer','business:erp:asset:dispose','business:erp:asset:detail',
                         'business:erp:asset-category:list','business:erp:asset-category:create',
                         'business:erp:asset-category:update','business:erp:asset-category:delete')
       AND deleted_at IS NULL;
    IF n <> 11 THEN
        RAISE EXCEPTION 'V0229 自检失败：固定资产权限码种了 % 个（期望 11）', n;
    END IF;

    -- 6.7 3 个页面级菜单（1 目录 + 2 页面）
    SELECT count(*) INTO n FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001400'::uuid,
                  '20000000-0000-0000-0000-000000001401'::uuid,
                  '20000000-0000-0000-0000-000000001402'::uuid)
       AND deleted_at IS NULL;
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0229 自检失败：固定资产菜单只种了 % 个（期望 3）', n;
    END IF;
END $$;
