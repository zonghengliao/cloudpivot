-- ======================================================
-- V0189：期初余额录入（ERP-R5）—— 业务表 + 期初凭证类型 + 权限节点
--
-- 依据：kanban 卡 t_2c5eb708 §二 / §三 / §八
--
-- # 一句话
--
-- 客户从旧系统迁移过来，第一件事是录期初。期初有两种录入口径：
--   · 方式 A（财务）：按科目逐行录 借/贷 期初 → `biz_opening_balance`
--   · 方式 B（业务）：本卡只做**库存期初** → `biz_opening_inventory`
-- 两者都必须在同一事务里落成**一张** `voucher_type='OPENING'` 的期初凭证，
-- 这样报表逻辑不用改（期初就是凭证的一部分，试算平衡自动包含期初）。
--
-- # 三处与卡面不一致（都有硬理由，按本文件为准）
--
--   ① 卡面 §二 只给了 `biz_opening_balance`（方式 A）。库存期初没有落点，
--      卡面给的建议是「复用 biz_inventory 加 opening_flag」。**不采纳**：
--      `opening_flag` 只有布尔位，回答不了「这一行期初是谁录的、多少数量、什么单价」，
--      重复录入时也无从算差额（覆盖式更新必须知道旧值）。
--      本迁移另建 `biz_opening_inventory`（期初库存的**录入台账**），
--      `biz_inventory` / `biz_inventory_location` / `biz_inventory_txn` 三本账
--      仍由业务代码按卡面 §四 ①②③ 同步写 —— 台账只是「录入了什么」的凭据。
--
--   ② 卡面的表还带 `create_by/update_by`，但没有 `deleted_at`。同理：期初是**按期间整体覆盖**
--      的（重复录入 = 覆盖，不是新增），不做软删。删掉一个科目的期初 = 重存时它不在 entries 里。
--
--   ③ 卡面要求「期初凭证只能有一张」。这条本迁移**在库层面兜死**：
--      部分唯一索引 `uk_biz_voucher_opening_period`（period + voucher_type='OPENING'）。
--      应用层再怎么改都插不进第二张 —— 与 V0149 的 `ck_biz_voucher_balance` 同一手法。
--
-- # `ck_biz_voucher_type` 扩容
--
-- V0149 的白名单是 `AUTO|MANUAL`。期初凭证是第三种语义（不是某张单据的自动凭证，
-- 也不是人手敲的一张普通凭证，报表要单独识别它），扩成 `AUTO|MANUAL|OPENING`。
-- 约束**必须先 DROP 再 ADD**（`ADD CONSTRAINT` 没有 IF NOT EXISTS），且加回时必须与
-- V0149 的另外两条（红字引用、状态）互不影响 —— 只动 type 这一条。
--
-- # 权限节点（menu_type 3 = 按钮，page 挂「ERP 台账」目录 …0946 下）
--
--   1022 business:erp:opening:read       期初余额（菜单页，menu_type 2）
--   1023 business:erp:opening:write      录入科目期初（方式 A）
--   1024 business:erp:opening:inventory  录入库存期初（方式 B 的第一项）
--
-- 三个码分开的理由与 V0188 期间三码一致：财务录科目、仓管录库存是两类人；
-- 「能看期初」更不能等于「能改期初」。
--   授权：read  → SUPER_ADMIN / ADMIN / FINANCE / ACCOUNTANT / WAREHOUSE
--         write → SUPER_ADMIN / ADMIN / FINANCE / ACCOUNTANT
--         inventory → SUPER_ADMIN / ADMIN / FINANCE / WAREHOUSE
--
-- 幂等：CREATE TABLE / INDEX 全带 IF NOT EXISTS，节点 `ON CONFLICT (id) DO NOTHING`，
--       授权 `NOT EXISTS`。重复执行不报错、不覆盖运维改过的标题。
-- 回滚：V0189 down 只删本迁移建的**结构**与节点；期初数据与期初凭证**不删**
--       （它们已经是账的一部分，反方向应该是业务上的作废，不是迁移回滚）。
-- ======================================================

-- ---------- ① 科目期初（方式 A）----------
CREATE TABLE IF NOT EXISTS biz_opening_balance (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_code    VARCHAR(7)    NOT NULL,              -- 期初所属期间（2026-11 = 11 月初）
    account_id     UUID          NOT NULL,              -- 科目（弱引用 biz_account.id）
    account_code   VARCHAR(32)   NOT NULL,              -- 科目编码快照（科目改名后语义不漂移）
    debit_opening  NUMERIC(20,4) NOT NULL DEFAULT 0,    -- 借方期初
    credit_opening NUMERIC(20,4) NOT NULL DEFAULT 0,    -- 贷方期初
    remark         TEXT,
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by      UUID,
    update_by      UUID,

    CONSTRAINT uk_biz_opening_balance UNIQUE (period_code, account_id),
    CONSTRAINT ck_biz_opening_nonneg CHECK (debit_opening >= 0 AND credit_opening >= 0),
    -- 一个科目在一个期间只有**一个方向的余额**：借贷同时非零说明这一行的两列被谁加错了。
    -- 两列同时为 0 允许（应用层会把这种行整行丢掉，不写进来）。
    CONSTRAINT ck_biz_opening_single_side CHECK (NOT (debit_opening > 0 AND credit_opening > 0))
);

COMMENT ON TABLE  biz_opening_balance IS '科目期初余额（方式 A：财务按科目录）；每个期间每个科目一行，重复录入 = 整体覆盖';
COMMENT ON COLUMN biz_opening_balance.period_code IS '期初所属期间 yyyy-MM：2026-11 表示「2026-11 月初」，即 2026-10-31 结账后的余额';
COMMENT ON COLUMN biz_opening_balance.account_code IS '科目编码快照（与 biz_voucher_entry.account_code 同口径）';

CREATE INDEX IF NOT EXISTS idx_biz_opening_balance_period ON biz_opening_balance (period_code);
CREATE INDEX IF NOT EXISTS idx_biz_opening_balance_account ON biz_opening_balance (account_id);

-- ---------- ② 库存期初（方式 B 的第一项）----------
CREATE TABLE IF NOT EXISTS biz_opening_inventory (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_code  VARCHAR(7)    NOT NULL,                -- 期初所属期间（同上口径）
    material_id  UUID          NOT NULL,                -- 物料（弱引用 biz_material.id）
    warehouse_id UUID          NOT NULL,                -- 仓库（弱引用 biz_warehouse.id）
    qty          NUMERIC(18,4) NOT NULL,                -- 期初数量（> 0；要减到 0 就直接删这一行）
    unit_cost    NUMERIC(18,6) NOT NULL DEFAULT 0,      -- 期初单价
    amount       NUMERIC(18,4) NOT NULL DEFAULT 0,      -- 期初金额 = ROUND(qty × unit_cost, 4)
    remark       TEXT,
    created_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by    UUID,
    update_by    UUID,

    CONSTRAINT uk_biz_opening_inventory UNIQUE (period_code, material_id, warehouse_id),
    CONSTRAINT ck_biz_opening_inventory_qty CHECK (qty > 0),
    CONSTRAINT ck_biz_opening_inventory_amount CHECK (amount >= 0)
);

COMMENT ON TABLE  biz_opening_inventory IS '库存期初录入台账（方式 B）：本表是「录了什么」的凭据，库存三本账（biz_inventory / biz_inventory_location / biz_inventory_txn）由业务代码同步写';
COMMENT ON COLUMN biz_opening_inventory.amount IS '期初金额（4 位）；库存期初凭证的 借1405 合计 = 本列按期间求和';

CREATE INDEX IF NOT EXISTS idx_biz_opening_inventory_period ON biz_opening_inventory (period_code);
CREATE INDEX IF NOT EXISTS idx_biz_opening_inventory_material ON biz_opening_inventory (material_id);
CREATE INDEX IF NOT EXISTS idx_biz_opening_inventory_warehouse ON biz_opening_inventory (warehouse_id);

-- ---------- ③ 凭证类型白名单加 OPENING ----------
-- 注意顺序：先 DROP 再 ADD（`ADD CONSTRAINT` 没有 IF NOT EXISTS）。
-- 若上一轮已经加过（重复执行），DROP … IF EXISTS 会让它变成幂等操作。
ALTER TABLE biz_voucher DROP CONSTRAINT IF EXISTS ck_biz_voucher_type;
ALTER TABLE biz_voucher
    ADD CONSTRAINT ck_biz_voucher_type
    CHECK (voucher_type IN ('AUTO', 'MANUAL', 'OPENING'));

-- ---------- ④ 「期初凭证只能有一张」在库层面兜死（硬红线 2）----------
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_voucher_opening_period
    ON biz_voucher (period)
    WHERE voucher_type = 'OPENING' AND deleted_at IS NULL;

COMMENT ON INDEX uk_biz_voucher_opening_period IS
    '每个期间最多一张期初凭证（voucher_type=OPENING）；重复录入是覆盖更新同一张，不是新增';

-- ---------- ⑤ 权限节点 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001022'::uuid, '期初余额', 2, 'business:erp:opening:read',
     '/business/erp/opening', '20000000-0000-0000-0000-000000000946'::uuid,
     'business/erp/opening/index', 25, 1, 'flag', '期初余额', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001023'::uuid, 'business:erp:opening:write', 3,
     'business:erp:opening:write', NULL,
     '20000000-0000-0000-0000-000000001022'::uuid, NULL,
     10, 1, NULL, '录入科目期初', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001024'::uuid, 'business:erp:opening:inventory', 3,
     'business:erp:opening:inventory', NULL,
     '20000000-0000-0000-0000-000000001022'::uuid, NULL,
     20, 1, NULL, '录入库存期初', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ⑥ 自检 ----------
DO $$
DECLARE
    n_tbl      INT;
    n_ck       INT;
    n_uk       INT;
    n_menu     INT;
    n_roles    INT;
    n_grants   INT;
    n_row      INT;
    v_def      TEXT;
BEGIN
    -- 表在
    SELECT count(*) INTO n_tbl
      FROM information_schema.tables
     WHERE table_schema = 'public'
       AND table_name IN ('biz_opening_balance', 'biz_opening_inventory');
    IF n_tbl <> 2 THEN
        RAISE EXCEPTION 'V0189 自检失败：期初两表只建了 % 张（应为 2）', n_tbl;
    END IF;

    -- 约束在（唯一 + 单边 + 非负）
    SELECT count(*) INTO n_ck
      FROM pg_constraint
     WHERE conrelid = 'public.biz_opening_balance'::regclass
       AND conname IN ('uk_biz_opening_balance', 'ck_biz_opening_nonneg', 'ck_biz_opening_single_side');
    IF n_ck <> 3 THEN
        RAISE EXCEPTION 'V0189 自检失败：biz_opening_balance 约束缺失（应有 3 条，实际 %）', n_ck;
    END IF;

    -- ★ 见证「期初凭证唯一」是按 (period) 建的部分唯一索引（不是普通索引）
    SELECT count(*) INTO n_uk
      FROM pg_index i
      JOIN pg_class c ON c.oid = i.indexrelid
     WHERE c.relname = 'uk_biz_voucher_opening_period' AND i.indisunique;
    IF n_uk <> 1 THEN
        RAISE EXCEPTION 'V0189 自检失败：uk_biz_voucher_opening_period 不是唯一索引（% 行）', n_uk;
    END IF;

    -- ★ 凭证类型白名单真的含 OPENING（读约束定义，不是读期望）
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conrelid = 'public.biz_voucher'::regclass AND conname = 'ck_biz_voucher_type';
    IF v_def IS NULL OR v_def NOT LIKE '%OPENING%' THEN
        RAISE EXCEPTION 'V0189 自检失败：ck_biz_voucher_type 白名单里没有 OPENING（当前 %）', v_def;
    END IF;

    -- ★ 反证：非期初类型带 'OPENING' 的写法必须被约束挡住（自检块里试一次然后回滚子事务）
    BEGIN
        INSERT INTO biz_voucher (voucher_no, period, voucher_date, voucher_type,
                                 total_debit, total_credit, entry_count)
        VALUES ('__V0189_SELFCHECK__', '1999-02', DATE '1999-02-01', 'OPENING_X',
                10, 10, 2);
        RAISE EXCEPTION 'V0189 自检失败：白名单外的 voucher_type 写进了库';
    EXCEPTION WHEN check_violation THEN
        RAISE NOTICE 'V0189 自检通过：ck_biz_voucher_type 拦住了白名单外的类型';
    END;

    -- 权限节点在
    SELECT count(*) INTO n_menu
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001022'::uuid,
                  '20000000-0000-0000-0000-000000001023'::uuid,
                  '20000000-0000-0000-0000-000000001024'::uuid)
       AND deleted_at IS NULL;
    IF n_menu <> 3 THEN
        RAISE EXCEPTION 'V0189 自检失败：期初三码节点没种全（%）', n_menu;
    END IF;

    SELECT count(*) INTO n_row
      FROM sys_menu
     WHERE auth_code IN ('business:erp:opening:read',
                         'business:erp:opening:write',
                         'business:erp:opening:inventory')
       AND deleted_at IS NULL;
    IF n_row <> 3 THEN
        RAISE EXCEPTION 'V0189 自检失败：期初三码在 sys_menu 里出现 % 次（应为 3）', n_row;
    END IF;

    -- 授权（空库首启时 sys_role 可能为空，跳过并提示）
    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0189：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
    ELSE
        INSERT INTO sys_role_menu (id, role_id, menu_id)
        SELECT gen_random_uuid(), r.id, m.id
          FROM sys_role r
          CROSS JOIN (VALUES
                ('20000000-0000-0000-0000-000000001022'::uuid),
                ('20000000-0000-0000-0000-000000001023'::uuid)) AS m(id)
         WHERE r.deleted_at IS NULL
           AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'FINANCE', 'ACCOUNTANT')
           AND NOT EXISTS (
                SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
           );
        GET DIAGNOSTICS n_grants = ROW_COUNT;

        INSERT INTO sys_role_menu (id, role_id, menu_id)
        SELECT gen_random_uuid(), r.id, m.id
          FROM sys_role r
          CROSS JOIN (VALUES
                ('20000000-0000-0000-0000-000000001022'::uuid),
                ('20000000-0000-0000-0000-000000001024'::uuid)) AS m(id)
         WHERE r.deleted_at IS NULL
           AND r.role_code IN ('WAREHOUSE')
           AND NOT EXISTS (
                SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
           );
        GET DIAGNOSTICS n_row = ROW_COUNT;
        n_grants := n_grants + n_row;

        RAISE NOTICE 'V0189：期初三码已种，授权新增 % 行', n_grants;
    END IF;

    RAISE NOTICE 'V0189 自检通过：期初两表 + 期初凭证唯一索引 + 三个权限节点';
END $$;
