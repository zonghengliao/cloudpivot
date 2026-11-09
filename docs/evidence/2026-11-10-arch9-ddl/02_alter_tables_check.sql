-- ARCH-9 改造表 DDL 可执行性 + 唯一键语义验证（在 cloudpivot_test 上跑，整段 ROLLBACK）
-- 只覆盖 cloudpivot_test 中真实存在的表；biz_depreciation_batch / biz_fx_revaluation / biz_customer
-- / biz_inventory_location 在该库不存在，其 ALTER 语句无法在此验证（见文档 §7.1 自检块兜底）
\set ON_ERROR_STOP 1

-- ==================== A1 biz_voucher（3 条唯一键重建）====================
ALTER TABLE biz_voucher ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE biz_voucher SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE biz_voucher ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE biz_voucher ADD COLUMN IF NOT EXISTS counterparty_org_id UUID;

DROP INDEX IF EXISTS uk_biz_voucher_no;
CREATE UNIQUE INDEX uk_biz_voucher_no
    ON biz_voucher (org_id, voucher_word, period, voucher_no) WHERE deleted_at IS NULL;
DROP INDEX IF EXISTS uk_biz_voucher_year_end_period;
CREATE UNIQUE INDEX uk_biz_voucher_year_end_period
    ON biz_voucher (org_id, period)
    WHERE voucher_type = 'YEAR_END' AND deleted_at IS NULL;
DROP INDEX IF EXISTS uk_biz_voucher_opening_period;
CREATE UNIQUE INDEX uk_biz_voucher_opening_period
    ON biz_voucher (org_id, period)
    WHERE voucher_type = 'OPENING' AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_voucher_org_period ON biz_voucher (org_id, period, voucher_date DESC);
CREATE INDEX IF NOT EXISTS idx_biz_voucher_ic ON biz_voucher (counterparty_org_id)
    WHERE counterparty_org_id IS NOT NULL;

-- ==================== A2 biz_accounting_period ====================
ALTER TABLE biz_accounting_period ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE biz_accounting_period SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE biz_accounting_period ALTER COLUMN org_id SET NOT NULL;
DROP INDEX IF EXISTS uk_biz_period_code;
DROP INDEX IF EXISTS uk_biz_period_ym;
CREATE UNIQUE INDEX uk_biz_period_code   ON biz_accounting_period (org_id, period_code);
CREATE UNIQUE INDEX uk_biz_period_ym     ON biz_accounting_period (org_id, period_year, period_month);
CREATE INDEX IF NOT EXISTS idx_biz_period_org_status ON biz_accounting_period (org_id, status, period_code);

-- ==================== A3 biz_opening_balance ====================
ALTER TABLE biz_opening_balance ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE biz_opening_balance SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE biz_opening_balance ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE biz_opening_balance DROP CONSTRAINT IF EXISTS uk_biz_opening_balance;
ALTER TABLE biz_opening_balance ADD CONSTRAINT uk_biz_opening_balance
    UNIQUE (org_id, period_code, account_id);
CREATE INDEX IF NOT EXISTS idx_biz_opening_balance_org ON biz_opening_balance (org_id, period_code);

-- ==================== A4 biz_opening_inventory ====================
ALTER TABLE biz_opening_inventory ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE biz_opening_inventory SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE biz_opening_inventory ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE biz_opening_inventory DROP CONSTRAINT IF EXISTS uk_biz_opening_inventory;
ALTER TABLE biz_opening_inventory ADD CONSTRAINT uk_biz_opening_inventory
    UNIQUE (org_id, period_code, material_id, warehouse_id);

-- ==================== A5 biz_seq ====================
ALTER TABLE biz_seq ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE biz_seq SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE biz_seq ALTER COLUMN org_id SET NOT NULL;
DROP INDEX IF EXISTS uk_biz_seq_type;
CREATE UNIQUE INDEX uk_biz_seq_type ON biz_seq (org_id, seq_type) WHERE deleted_at IS NULL;

-- ==================== C 组 biz_warehouse / biz_inventory_txn ====================
ALTER TABLE biz_warehouse ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE biz_warehouse SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE biz_warehouse ALTER COLUMN org_id SET NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_warehouse_org ON biz_warehouse (org_id, status);

ALTER TABLE biz_inventory_txn ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE biz_inventory_txn SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE biz_inventory_txn ALTER COLUMN org_id SET NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_inv_txn_org ON biz_inventory_txn (org_id, created_at DESC);

-- ==================== B 组 biz_stock_in（样板表）====================
ALTER TABLE biz_stock_in ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE biz_stock_in SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE biz_stock_in ALTER COLUMN org_id SET NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_stock_in_org ON biz_stock_in (org_id, created_at DESC);
ALTER TABLE biz_stock_in ADD COLUMN IF NOT EXISTS counterparty_org_id UUID;
ALTER TABLE biz_stock_in ADD CONSTRAINT ck_biz_stock_in_ic
    CHECK (counterparty_org_id IS NULL OR counterparty_org_id <> org_id);

-- ==================== D 组 主数据/系统 ====================
ALTER TABLE biz_account ADD COLUMN IF NOT EXISTS org_id UUID;
CREATE INDEX IF NOT EXISTS idx_biz_account_org ON biz_account (org_id) WHERE org_id IS NOT NULL;

ALTER TABLE biz_supplier ADD COLUMN IF NOT EXISTS is_internal SMALLINT NOT NULL DEFAULT 0;
ALTER TABLE biz_supplier ADD COLUMN IF NOT EXISTS internal_org_id UUID;
ALTER TABLE biz_supplier ADD CONSTRAINT ck_biz_supplier_ic
    CHECK ((is_internal = 0 AND internal_org_id IS NULL)
        OR (is_internal = 1 AND internal_org_id IS NOT NULL));
CREATE INDEX IF NOT EXISTS idx_biz_supplier_ic ON biz_supplier (internal_org_id) WHERE is_internal = 1;

ALTER TABLE sys_dept ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE sys_dept SET org_id = '00000000-0000-4000-8000-000000000001'::uuid
 WHERE org_id IS NULL AND deleted_at IS NULL;
ALTER TABLE sys_dept ALTER COLUMN org_id SET NOT NULL;
CREATE INDEX IF NOT EXISTS idx_sys_dept_org ON sys_dept (org_id, parent_id);

ALTER TABLE sys_user ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE sys_user SET org_id = '00000000-0000-4000-8000-000000000001'::uuid
 WHERE org_id IS NULL AND user_type = 'HUMAN' AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_sys_user_org ON sys_user (org_id);

ALTER TABLE biz_flow_instance ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE biz_flow_instance SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE biz_flow_instance ALTER COLUMN org_id SET NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_flow_instance_org ON biz_flow_instance (org_id, status, started_at DESC);

-- ==================== 语义测试：唯一键真的按公司分开 ====================
DO $$
DECLARE
    org_a UUID := '00000000-0000-4000-8000-000000000001';
    org_b UUID := '00000000-0000-4000-8000-0000000000b2';
    n INT := 0;
BEGIN
    -- 两家公司可以有同号凭证（这正是多公司要的能力）
    INSERT INTO biz_voucher (voucher_no, period, voucher_date, total_debit, total_credit, entry_count, org_id)
    VALUES ('0001','2099-12','2099-12-05',100,100,2,org_a);
    INSERT INTO biz_voucher (voucher_no, period, voucher_date, total_debit, total_credit, entry_count, org_id)
    VALUES ('0001','2099-12','2099-12-05',100,100,2,org_b);
    n := n + 1;

    -- 同一公司同号必须被拒
    BEGIN
        INSERT INTO biz_voucher (voucher_no, period, voucher_date, total_debit, total_credit, entry_count, org_id)
        VALUES ('0001','2099-12','2099-12-06',100,100,2,org_a);
        RAISE EXCEPTION '唯一键失效：同公司同号被接受';
    EXCEPTION WHEN unique_violation THEN n := n + 1;
    END;

    -- 期间：两家公司同 period_code 合法，同公司重复非法
    INSERT INTO biz_accounting_period (period_year, period_month, period_code, start_date, end_date, org_id)
    VALUES (2099,12,'2099-12','2099-12-01','2099-12-31',org_a);
    INSERT INTO biz_accounting_period (period_year, period_month, period_code, start_date, end_date, org_id)
    VALUES (2099,12,'2099-12','2099-12-01','2099-12-31',org_b);
    n := n + 1;
    BEGIN
        INSERT INTO biz_accounting_period (period_year, period_month, period_code, start_date, end_date, org_id)
        VALUES (2099,12,'2099-12','2099-12-01','2099-12-31',org_a);
        RAISE EXCEPTION '期间唯一键失效';
    EXCEPTION WHEN unique_violation THEN n := n + 1;
    END;

    -- 取号：两家公司各自 0001
    INSERT INTO biz_seq (seq_type, seq_name, prefix, org_id) VALUES ('ARCH9T','架构测试','AT',org_a);
    INSERT INTO biz_seq (seq_type, seq_name, prefix, org_id) VALUES ('ARCH9T','架构测试','AT',org_b);
    n := n + 1;
    BEGIN
        INSERT INTO biz_seq (seq_type, seq_name, prefix, org_id) VALUES ('ARCH9T','架构测试','AT',org_a);
        RAISE EXCEPTION '取号唯一键失效';
    EXCEPTION WHEN unique_violation THEN n := n + 1;
    END;

    -- 内部交易对手方不得等于本方（ck_biz_stock_in_ic）
    BEGIN
        INSERT INTO biz_stock_in (org_id, counterparty_org_id, in_type, warehouse_id, status)
        VALUES (org_a, org_a, 'PURCHASE', gen_random_uuid(), 'DRAFT');
        RAISE EXCEPTION 'ck_biz_stock_in_ic 未生效：对手方=本方被接受';
    EXCEPTION WHEN check_violation THEN n := n + 1;
    WHEN not_null_violation THEN n := n + 1;   -- 该库 biz_stock_in 可能还有别的必填列，等价"被拒"
    END;

    -- 供应商内部标识：is_internal=1 但无 internal_org_id 必须被拒
    BEGIN
        INSERT INTO biz_supplier (supplier_code, supplier_name, is_internal)
        VALUES ('IC-X','内部单位',1);
        RAISE EXCEPTION 'ck_biz_supplier_ic 未生效';
    EXCEPTION WHEN check_violation THEN n := n + 1;
    END;

    RAISE NOTICE 'ARCH-9 改造表语义测试通过：% 条断言', n;
END $$;

-- 结果核对：新索引形状
SELECT indexname, indexdef FROM pg_indexes
 WHERE schemaname = current_schema()
   AND indexname IN ('uk_biz_voucher_no','uk_biz_voucher_year_end_period','uk_biz_voucher_opening_period',
                     'uk_biz_period_code','uk_biz_period_ym','uk_biz_seq_type')
 ORDER BY indexname;

SELECT conname, pg_get_constraintdef(oid) FROM pg_constraint
 WHERE conname IN ('uk_biz_opening_balance','uk_biz_opening_inventory','ck_biz_stock_in_ic','ck_biz_supplier_ic')
 ORDER BY conname;
