-- ============================================================================
-- ARCH-2 · biz_contract 数据模型 POC（可重复执行）
--
-- 用途：在「与仓库主干同构」的库上证明三件事
--   ① DDL 可直接执行（不依赖任何不存在的表）
--   ② 每条 DB 级校验真的会拦（逐条触发，断言 SQLSTATE）
--   ③ 软删除下的唯一约束、以及「合同换乙方」对下游快照的偏移量与可检出性
--
-- 运行：psql -v ON_ERROR_STOP=1 -f model-poc.sql
-- 前提：目标库已具备主干业务表（biz_supplier / biz_purchase_order / biz_stock_in /
--       biz_payable / biz_seq / sys_dept / sys_user），且尚无 biz_contract。
--       本仓 3B 期的库副本（cloudpivot_3b_sql → arch2_contract_poc）满足。
-- ============================================================================

\set ON_ERROR_STOP on
\pset pager off

\echo ''
\echo '══════════════ P0 前置事实（实测，非印象）══════════════'

SELECT current_database() AS db, version() AS pg;
SELECT to_regclass('public.biz_contract') IS NULL AS biz_contract_absent;
SELECT count(*) AS triggers_in_schema FROM information_schema.triggers WHERE trigger_schema = 'public';
SELECT count(*) AS fk_total FROM pg_constraint WHERE contype = 'f';
-- 单据链外键基线：PO / IN / PAYABLE 三张表有没有外键？
SELECT c.conrelid::regclass::text AS tbl, count(*) AS fk_count
  FROM pg_constraint c
 WHERE c.contype = 'f'
   AND c.conrelid::regclass::text IN ('biz_purchase_order', 'biz_stock_in', 'biz_payable', 'biz_supplier')
 GROUP BY 1 ORDER BY 1;
-- 主数据侧外键先例（决定 party_b 是否用 FK 的关键事实）
SELECT c.conrelid::regclass::text AS tbl, a.attname AS col, c.confrelid::regclass::text AS ref
  FROM pg_constraint c
  JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = c.conkey[1]
 WHERE c.contype = 'f' AND c.confrelid::regclass::text = 'biz_warehouse'
 ORDER BY 1, 2;
-- 金额 / 币种精度基线（决定合同金额精度）
SELECT table_name, column_name, data_type, numeric_precision, numeric_scale, character_maximum_length
  FROM information_schema.columns
 WHERE table_schema = 'public'
   AND column_name IN ('total_amount', 'amount', 'currency_code')
   AND table_name IN ('biz_purchase_order', 'biz_stock_in', 'biz_payable')
 ORDER BY 1, 2;
-- 唯一约束的软删写法先例
SELECT indexname, indexdef FROM pg_indexes
 WHERE schemaname = 'public' AND indexname IN ('uk_biz_po_no', 'uk_biz_supplier_code', 'uk_biz_seq_type')
 ORDER BY 1;

\echo ''
\echo '══════════════ P1 执行 DDL（合同主表 + 取号规则）══════════════'

-- #region DDL
CREATE TABLE biz_contract (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    contract_no          VARCHAR(32)   NOT NULL,
    contract_name        VARCHAR(200)  NOT NULL,
    contract_type        VARCHAR(16)   NOT NULL,
    party_a_id           UUID,
    party_a_name         VARCHAR(200),
    party_b_type         VARCHAR(16)   NOT NULL,
    party_b_id           UUID          NOT NULL,
    party_b_name         VARCHAR(200)  NOT NULL,
    amount               NUMERIC(18,4) NOT NULL,
    currency_code        CHAR(3)       NOT NULL DEFAULT 'CNY',
    sign_date            DATE,
    start_date           DATE,
    end_date             DATE,
    status               VARCHAR(16)   NOT NULL DEFAULT 'DRAFT',
    owner_id             UUID,
    dept_id              UUID,
    remark               TEXT,
    created_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by            UUID,
    update_by            UUID,
    deleted_at           TIMESTAMPTZ,

    CONSTRAINT ck_biz_contract_type
        CHECK (contract_type IN ('PURCHASE','SALES','SERVICE','LEASE')),
    CONSTRAINT ck_biz_contract_party_b_type
        CHECK (party_b_type IN ('SUPPLIER','CUSTOMER')),
    CONSTRAINT ck_biz_contract_type_party_pair
        CHECK (CASE contract_type
                 WHEN 'PURCHASE' THEN party_b_type = 'SUPPLIER'
                 WHEN 'SALES'    THEN party_b_type = 'CUSTOMER'
                 ELSE party_b_type IN ('SUPPLIER','CUSTOMER')
               END),
    CONSTRAINT ck_biz_contract_amount
        CHECK (amount > 0),
    CONSTRAINT ck_biz_contract_date_order
        CHECK (start_date IS NULL OR end_date IS NULL OR end_date >= start_date),
    CONSTRAINT ck_biz_contract_signed_dates
        CHECK (CASE WHEN status IN ('SIGNED','ACTIVE','COMPLETED')
                    THEN sign_date IS NOT NULL AND start_date IS NOT NULL AND end_date IS NOT NULL
                    ELSE TRUE END),
    CONSTRAINT ck_biz_contract_status
        CHECK (status IN ('DRAFT','SIGNED','ACTIVE','COMPLETED','TERMINATED')),
    CONSTRAINT ck_biz_contract_currency
        CHECK (currency_code ~ '^[A-Z]{3}$'),
    CONSTRAINT ck_biz_contract_no_charset
        CHECK (contract_no ~ '^[A-Za-z0-9][A-Za-z0-9_-]{5,31}$')
);

COMMENT ON TABLE  biz_contract IS '合同（采购/销售/服务/租赁）：乙方是供应商/客户的唯一真相；下游单据只引用合同，不各自维护乙方';
COMMENT ON COLUMN biz_contract.contract_no       IS '合同号：biz_seq(seq_type=CT) 取号，格式 HT-yyyyMMdd-000001；DB 只校验字符集与长度';
COMMENT ON COLUMN biz_contract.contract_name     IS '合同名称';
COMMENT ON COLUMN biz_contract.contract_type     IS 'PURCHASE 采购 / SALES 销售 / SERVICE 服务 / LEASE 租赁';
COMMENT ON COLUMN biz_contract.party_a_id        IS '甲方：NULL=本组织本体；非空=sys_dept.id（弱引用，本库无“公司”表）';
COMMENT ON COLUMN biz_contract.party_a_name      IS '甲方名称【快照】：签约时名称，随合同冻结';
COMMENT ON COLUMN biz_contract.party_b_type      IS '乙方类型：SUPPLIER→biz_supplier / CUSTOMER→biz_customer（客户表属批次2）';
COMMENT ON COLUMN biz_contract.party_b_id        IS '乙方 id：弱引用（非外键）。存在性由应用层写入校验 + 每日巡检兜底';
COMMENT ON COLUMN biz_contract.party_b_name      IS '乙方名称【快照】：签约时名称；主档改名不回溯历史合同（审计要“当时是谁”）';
COMMENT ON COLUMN biz_contract.amount            IS '合同金额（不含税口径与采购单 total_amount 一致），精度 NUMERIC(18,4)；必须 > 0';
COMMENT ON COLUMN biz_contract.currency_code     IS '币种 ISO 4217 三位大写';
COMMENT ON COLUMN biz_contract.sign_date         IS '签订日期；status>=SIGNED 时必填';
COMMENT ON COLUMN biz_contract.start_date        IS '生效日；status>=SIGNED 时必填，且 <= end_date';
COMMENT ON COLUMN biz_contract.end_date          IS '到期日；下游建单必须落在 [start_date, end_date] 内';
COMMENT ON COLUMN biz_contract.status            IS '状态机：DRAFT→SIGNED→ACTIVE→COMPLETED/TERMINATED，另有 SIGNED→TERMINATED；边表在应用层';
COMMENT ON COLUMN biz_contract.owner_id          IS '负责人（弱引用 sys_user.id）';
COMMENT ON COLUMN biz_contract.dept_id           IS '归属部门（弱引用 sys_dept.id），数据权限过滤用';
COMMENT ON COLUMN biz_contract.updated_at        IS '最后更新时间：由模型层写入（本库零触发器，不靠 DB 自动维护）';
COMMENT ON COLUMN biz_contract.deleted_at        IS '软删除时间；合同号唯一约束只对未删除行生效（软删后可复用号码，审计仍可查旧行）';

CREATE UNIQUE INDEX uk_biz_contract_no
    ON biz_contract (contract_no) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_contract_party_b
    ON biz_contract (party_b_id, sign_date DESC) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_contract_status
    ON biz_contract (status) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_contract_end
    ON biz_contract (end_date) WHERE deleted_at IS NULL;

INSERT INTO biz_seq (seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT 'CT', '合同号', 'HT', 'yyyyMMdd', '', 0, 1, '-', 1,
       '合同号：HT-yyyyMMdd-000001，按天滚动，与 PO/IN/TR/SN 同款取号器'
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'CT' AND deleted_at IS NULL);
-- #endregion DDL

SELECT count(*) AS columns FROM information_schema.columns
 WHERE table_schema = 'public' AND table_name = 'biz_contract';
SELECT count(*) AS check_constraints FROM pg_constraint
 WHERE conrelid = 'public.biz_contract'::regclass AND contype = 'c';
SELECT indexname, indexdef FROM pg_indexes
 WHERE schemaname = 'public' AND tablename = 'biz_contract' ORDER BY 1;
SELECT seq_type, prefix, date_pattern FROM biz_seq WHERE seq_type = 'CT' AND deleted_at IS NULL;
SELECT count(*) AS contract_fk_targets FROM pg_constraint
 WHERE confrelid = 'public.biz_contract'::regclass AND contype = 'f';

\echo ''
\echo '══════════════ P2 DB 级校验逐条触发（每条都必须被拦）══════════════'

DO $$
DECLARE
    v_supplier UUID := gen_random_uuid();
    v_desc     TEXT;
    v_state    TEXT;
BEGIN
    -- 逐条：期望 SQLSTATE 23514（check_violation）
    -- V-01 contract_type 非枚举
    BEGIN
        INSERT INTO biz_contract (contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount)
        VALUES ('HT-20261102-000001', '非法类型合同', 'SERVICE_XX', 'SUPPLIER', v_supplier, '供应商A', 100);
        RAISE EXCEPTION 'V-01 未被拦：contract_type 非法值写入成功';
    EXCEPTION WHEN check_violation THEN
        GET STACKED DIAGNOSTICS v_state = RETURNED_SQLSTATE, v_desc = CONSTRAINT_NAME;
        RAISE NOTICE 'V-01 PASS  非枚举类型被拦 [%] %', v_state, v_desc;
    END;

    -- V-02 party_b_type 非枚举
    BEGIN
        INSERT INTO biz_contract (contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount)
        VALUES ('HT-20261102-000001', '非法乙方类型', 'SERVICE', 'PARTNER', v_supplier, '合作方A', 100);
        RAISE EXCEPTION 'V-02 未被拦';
    EXCEPTION WHEN check_violation THEN
        GET STACKED DIAGNOSTICS v_state = RETURNED_SQLSTATE, v_desc = CONSTRAINT_NAME;
        RAISE NOTICE 'V-02 PASS  非枚举乙方类型被拦 [%] %', v_state, v_desc;
    END;

    -- V-03a 采购合同配客户乙方
    BEGIN
        INSERT INTO biz_contract (contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount)
        VALUES ('HT-20261102-000001', '采购合同配客户', 'PURCHASE', 'CUSTOMER', v_supplier, '客户A', 100);
        RAISE EXCEPTION 'V-03a 未被拦';
    EXCEPTION WHEN check_violation THEN
        GET STACKED DIAGNOSTICS v_state = RETURNED_SQLSTATE, v_desc = CONSTRAINT_NAME;
        RAISE NOTICE 'V-03a PASS  采购合同配 CUSTOMER 被拦 [%] %', v_state, v_desc;
    END;

    -- V-03b 销售合同配供应商乙方
    BEGIN
        INSERT INTO biz_contract (contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount)
        VALUES ('HT-20261102-000001', '销售合同配供应商', 'SALES', 'SUPPLIER', v_supplier, '供应商A', 100);
        RAISE EXCEPTION 'V-03b 未被拦';
    EXCEPTION WHEN check_violation THEN
        GET STACKED DIAGNOSTICS v_state = RETURNED_SQLSTATE, v_desc = CONSTRAINT_NAME;
        RAISE NOTICE 'V-03b PASS  销售合同配 SUPPLIER 被拦 [%] %', v_state, v_desc;
    END;

    -- V-04a amount = 0（框架合同/不限额在本批次不支持）
    BEGIN
        INSERT INTO biz_contract (contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount)
        VALUES ('HT-20261102-000001', '零金额合同', 'PURCHASE', 'SUPPLIER', v_supplier, '供应商A', 0);
        RAISE EXCEPTION 'V-04a 未被拦：amount=0 写入成功';
    EXCEPTION WHEN check_violation THEN
        GET STACKED DIAGNOSTICS v_state = RETURNED_SQLSTATE, v_desc = CONSTRAINT_NAME;
        RAISE NOTICE 'V-04a PASS  amount=0 被拦 [%] %', v_state, v_desc;
    END;

    -- V-04b amount < 0
    BEGIN
        INSERT INTO biz_contract (contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount)
        VALUES ('HT-20261102-000001', '负金额合同', 'PURCHASE', 'SUPPLIER', v_supplier, '供应商A', -1);
        RAISE EXCEPTION 'V-04b 未被拦';
    EXCEPTION WHEN check_violation THEN
        GET STACKED DIAGNOSTICS v_state = RETURNED_SQLSTATE, v_desc = CONSTRAINT_NAME;
        RAISE NOTICE 'V-04b PASS  amount<0 被拦 [%] %', v_state, v_desc;
    END;

    -- V-05 end_date < start_date
    BEGIN
        INSERT INTO biz_contract (contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount,
                                  sign_date, start_date, end_date, status)
        VALUES ('HT-20261102-000001', '期限倒挂', 'PURCHASE', 'SUPPLIER', v_supplier, '供应商A', 100,
                DATE '2026-01-01', DATE '2026-06-01', DATE '2026-05-31', 'DRAFT');
        RAISE EXCEPTION 'V-05 未被拦';
    EXCEPTION WHEN check_violation THEN
        GET STACKED DIAGNOSTICS v_state = RETURNED_SQLSTATE, v_desc = CONSTRAINT_NAME;
        RAISE NOTICE 'V-05 PASS  end_date<start_date 被拦 [%] %', v_state, v_desc;
    END;

    -- V-06 非法状态
    BEGIN
        INSERT INTO biz_contract (contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount, status)
        VALUES ('HT-20261102-000001', '非法状态', 'PURCHASE', 'SUPPLIER', v_supplier, '供应商A', 100, 'ARCHIVED');
        RAISE EXCEPTION 'V-06 未被拦';
    EXCEPTION WHEN check_violation THEN
        GET STACKED DIAGNOSTICS v_state = RETURNED_SQLSTATE, v_desc = CONSTRAINT_NAME;
        RAISE NOTICE 'V-06 PASS  非法状态被拦 [%] %', v_state, v_desc;
    END;

    -- V-07 币种小写
    BEGIN
        INSERT INTO biz_contract (contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount, currency_code)
        VALUES ('HT-20261102-000001', '小写币种', 'PURCHASE', 'SUPPLIER', v_supplier, '供应商A', 100, 'cny');
        RAISE EXCEPTION 'V-07 未被拦';
    EXCEPTION WHEN check_violation THEN
        GET STACKED DIAGNOSTICS v_state = RETURNED_SQLSTATE, v_desc = CONSTRAINT_NAME;
        RAISE NOTICE 'V-07 PASS  小写币种被拦 [%] %', v_state, v_desc;
    END;

    -- V-08 SIGNED 缺签日/生效日/到期日
    BEGIN
        INSERT INTO biz_contract (contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount, status)
        VALUES ('HT-20261102-000001', '已签却无日期', 'PURCHASE', 'SUPPLIER', v_supplier, '供应商A', 100, 'SIGNED');
        RAISE EXCEPTION 'V-08 未被拦：SIGNED 缺日期写入成功';
    EXCEPTION WHEN check_violation THEN
        GET STACKED DIAGNOSTICS v_state = RETURNED_SQLSTATE, v_desc = CONSTRAINT_NAME;
        RAISE NOTICE 'V-08 PASS  SIGNED 缺签日/期限被拦 [%] %', v_state, v_desc;
    END;

    -- V-09 合同号含中文/空白
    BEGIN
        INSERT INTO biz_contract (contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount)
        VALUES ('HT-2026 1102-1', '合同号含空格', 'PURCHASE', 'SUPPLIER', v_supplier, '供应商A', 100);
        RAISE EXCEPTION 'V-09 未被拦';
    EXCEPTION WHEN check_violation THEN
        GET STACKED DIAGNOSTICS v_state = RETURNED_SQLSTATE, v_desc = CONSTRAINT_NAME;
        RAISE NOTICE 'V-09 PASS  合同号含空白被拦 [%] %', v_state, v_desc;
    END;

    RAISE NOTICE '---- 以上 10 条 CHECK 全部按预期拦截 ----';
END $$;

\echo ''
\echo '══════════════ P3 软删除下的唯一约束语义 ══════════════'

-- 供应商主数据（后续 P3/P5 共用；DEC-04 的「乙方存在性」靠它验证）
INSERT INTO biz_supplier (id, supplier_code, supplier_name, status) VALUES
  ('aaaaaaa1-0000-4000-8000-000000000001', 'SUP-A', '供应商A', 1),
  ('aaaaaaa2-0000-4000-8000-000000000002', 'SUP-B', '供应商B', 1)
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount)
VALUES ('11111111-1111-4111-8111-111111111111', 'HT-20261102-000001', '一号采购合同', 'PURCHASE', 'SUPPLIER',
        'aaaaaaa1-0000-4000-8000-000000000001', '供应商A', 100000);

DO $$
DECLARE v_state TEXT; v_desc TEXT;
BEGIN
    -- 活跃行重号 → 23505
    BEGIN
        INSERT INTO biz_contract (contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount)
        VALUES ('HT-20261102-000001', '重号合同', 'PURCHASE', 'SUPPLIER', gen_random_uuid(), '供应商A', 100);
        RAISE EXCEPTION 'P3-1 未被拦：活跃行重号写入成功';
    EXCEPTION WHEN unique_violation THEN
        GET STACKED DIAGNOSTICS v_state = RETURNED_SQLSTATE, v_desc = CONSTRAINT_NAME;
        RAISE NOTICE 'P3-1 PASS  活跃行重号被拦 [%] %', v_state, v_desc;
    END;
END $$;

UPDATE biz_contract SET deleted_at = CURRENT_TIMESTAMP WHERE contract_no = 'HT-20261102-000001';
INSERT INTO biz_contract (contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount)
VALUES ('HT-20261102-000001', '一号采购合同（重签）', 'PURCHASE', 'SUPPLIER', 'aaaaaaa1-0000-4000-8000-000000000001', '供应商A', 200000);

SELECT contract_no, contract_name, deleted_at IS NOT NULL AS is_deleted
  FROM biz_contract WHERE contract_no = 'HT-20261102-000001' ORDER BY created_at;

\echo ''
\echo '══════════════ P4 状态机：DB 拦不住，必须应用层把门（反证）══════════════'

-- 模拟「有人绕过应用层直接改库」：DRAFT → COMPLETED 这条被明令禁止的边
INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name,
                          amount, sign_date, start_date, end_date, status)
VALUES ('33333333-3333-4333-8333-333333333333', 'HT-20261102-000009', '状态机反证合同', 'PURCHASE', 'SUPPLIER',
        'aaaaaaa1-0000-4000-8000-000000000001', '供应商A', 5000,
        DATE '2026-01-01', DATE '2026-01-01', DATE '2026-12-31', 'DRAFT');
UPDATE biz_contract SET status = 'COMPLETED' WHERE id = '33333333-3333-4333-8333-333333333333';
SELECT 'P4 结论：DRAFT→COMPLETED 直改库成功（DB 无 CHECK 可表达边）⇒ 边表必须由应用层 state_machine 统一入口把关' AS note,
       status FROM biz_contract WHERE id = '33333333-3333-4333-8333-333333333333';

\echo ''
\echo '══════════════ P5 合同换乙方：下游偏移量与可检出性（20,000 张采购单）══════════════'

-- 模拟 t_52f3cb32 的目标形态（本卡不交付这几列，只为验证漂移 SQL 可跑）
ALTER TABLE biz_purchase_order ADD COLUMN IF NOT EXISTS contract_id            UUID;
ALTER TABLE biz_purchase_order ADD COLUMN IF NOT EXISTS purchase_type          VARCHAR(8) NOT NULL DEFAULT 'SPOT';
ALTER TABLE biz_purchase_order ADD COLUMN IF NOT EXISTS contract_no_snapshot   VARCHAR(32);
ALTER TABLE biz_purchase_order ADD COLUMN IF NOT EXISTS supplier_name_snapshot VARCHAR(200);

INSERT INTO biz_supplier (id, supplier_code, supplier_name, status) VALUES
  ('aaaaaaa1-0000-4000-8000-000000000001', 'SUP-A', '供应商A', 1),
  ('aaaaaaa2-0000-4000-8000-000000000002', 'SUP-B', '供应商B', 1)
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name,
                          amount, sign_date, start_date, end_date, status)
VALUES ('44444444-4444-4444-8444-444444444444', 'HT-20261102-000100', '年度采购框架合同', 'PURCHASE', 'SUPPLIER',
        'aaaaaaa1-0000-4000-8000-000000000001', '供应商A', 99999999.9999,
        DATE '2026-01-01', DATE '2026-01-01', DATE '2026-12-31', 'ACTIVE');

INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status, currency_code,
                                total_amount, total_tax, total_with_tax, contract_id, purchase_type, contract_no_snapshot, supplier_name_snapshot)
SELECT gen_random_uuid(),
       'PO-SEED-' || lpad(g::text, 6, '0'),
       'aaaaaaa1-0000-4000-8000-000000000001',
       DATE '2026-03-01', gen_random_uuid(), 'APPROVED', 'CNY',
       100, 0, 100, '44444444-4444-4444-8444-444444444444', 'CONTRACT',
       'HT-20261102-000100', '供应商A'
  FROM generate_series(1, 20000) AS g;

-- 2,000 张采购入库单（COPY 链：supplier_id 由采购单带出）
INSERT INTO biz_stock_in (id, in_no, in_type, source_order_id, supplier_id, warehouse_id, in_date, status, currency_code,
                          total_amount, total_tax, total_with_tax)
SELECT gen_random_uuid(), 'IN' || substring(o.order_no from 3), 'PURCHASE', o.id, o.supplier_id,
       gen_random_uuid(), DATE '2026-03-05', 'POSTED', 'CNY', 100, 0, 100
  FROM (SELECT id, order_no, supplier_id FROM biz_purchase_order WHERE purchase_type = 'CONTRACT' ORDER BY order_no LIMIT 2000) o;

-- 2,000 张应付单（再下一环 COPY）
INSERT INTO biz_payable (id, payable_no, supplier_id, source_type, source_id, source_no, currency_code, amount, paid_amount, unpaid_amount, due_date, period_code, status)
SELECT gen_random_uuid(), 'AP' || substring(s.in_no from 3), s.supplier_id, 'STOCK_IN', s.id, s.in_no,
       'CNY', 100, 0, 100, DATE '2026-04-05', '2026-03', 'OPEN'
  FROM (SELECT id, in_no, supplier_id FROM biz_stock_in WHERE in_no LIKE 'IN-SEED-%' ORDER BY in_no LIMIT 2000) s;

ANALYZE biz_purchase_order;
ANALYZE biz_stock_in;
ANALYZE biz_payable;

\echo '-- 变更前：漂移应为 0 --'
SELECT count(*) AS po_drift_before
  FROM biz_purchase_order o JOIN biz_contract c ON c.id = o.contract_id
 WHERE o.deleted_at IS NULL AND c.deleted_at IS NULL AND o.supplier_id IS DISTINCT FROM c.party_b_id;

\echo '-- 执行「合同换乙方」（本卡设计上只允许 DRAFT 阶段改，这里模拟绕过守卫的直改库）--'
UPDATE biz_contract SET party_b_id = 'aaaaaaa2-0000-4000-8000-000000000002'
 WHERE id = '44444444-4444-4444-8444-444444444444';

\echo '-- 变更后：① 采购单快照 vs 合同真相 = 20,000 行偏移（SNAPSHOT 合法历史）--'
SELECT count(*) AS po_drift_after
  FROM biz_purchase_order o JOIN biz_contract c ON c.id = o.contract_id
 WHERE o.deleted_at IS NULL AND c.deleted_at IS NULL AND o.supplier_id IS DISTINCT FROM c.party_b_id;

\echo '-- 变更后：② 入库单 vs 其采购单 = 0 行（COPY 链内部仍然自洽，不算缺陷）--'
SELECT count(*) AS in_vs_po_defect
  FROM biz_stock_in s JOIN biz_purchase_order o ON o.id = s.source_order_id
 WHERE s.deleted_at IS NULL AND o.deleted_at IS NULL AND s.supplier_id IS DISTINCT FROM o.supplier_id;

\echo '-- 变更后：③ 应付单 vs 其入库单 = 0 行 --'
SELECT count(*) AS payable_vs_in_defect
  FROM biz_payable p JOIN biz_stock_in s ON s.id = p.source_id AND p.source_type = 'STOCK_IN'
 WHERE p.deleted_at IS NULL AND s.deleted_at IS NULL AND p.supplier_id IS DISTINCT FROM s.supplier_id;

\echo '-- 人为打断一环（1 张入库单的供应商被改成供应商B）：必须被检出且只检出 1 行 --'
UPDATE biz_stock_in SET supplier_id = 'aaaaaaa2-0000-4000-8000-000000000002'
 WHERE in_no = 'IN-SEED-000001';
SELECT count(*) AS in_vs_po_defect_after_break
  FROM biz_stock_in s JOIN biz_purchase_order o ON o.id = s.source_order_id
 WHERE s.deleted_at IS NULL AND o.deleted_at IS NULL AND s.supplier_id IS DISTINCT FROM o.supplier_id;

\echo '-- 巡检 SQL 的执行计划（20,000 行级，确认不全表笛卡尔积）--'
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF)
SELECT o.id
  FROM biz_purchase_order o JOIN biz_contract c ON c.id = o.contract_id
 WHERE o.deleted_at IS NULL AND c.deleted_at IS NULL AND o.supplier_id IS DISTINCT FROM c.party_b_id;

\echo '-- 孤儿乙方巡检：SUPPLIER 分支指向不存在的供应商 --'
INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount)
VALUES ('55555555-5555-4555-8555-555555555555', 'HT-20261102-000101', '孤儿乙方合同', 'PURCHASE', 'SUPPLIER',
        'deadbeef-0000-4000-8000-000000000000', '已消失的供应商', 1000);
SELECT c.contract_no, c.party_b_id
  FROM biz_contract c
 WHERE c.deleted_at IS NULL AND c.party_b_type = 'SUPPLIER'
   AND NOT EXISTS (SELECT 1 FROM biz_supplier s WHERE s.id = c.party_b_id AND s.deleted_at IS NULL);

\echo ''
\echo '══════════════ P6 汇总（供文档引用）══════════════'
SELECT (SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='biz_contract') AS cols,
       (SELECT count(*) FROM pg_constraint WHERE conrelid='public.biz_contract'::regclass AND contype='c') AS checks,
       (SELECT count(*) FROM pg_indexes WHERE schemaname='public' AND tablename='biz_contract') AS idx_incl_pk,
       (SELECT count(*) FROM biz_purchase_order WHERE purchase_type='CONTRACT') AS seeded_po,
       (SELECT count(*) FROM biz_stock_in WHERE in_no LIKE 'IN-SEED-%') AS seeded_in,
       (SELECT count(*) FROM biz_payable WHERE payable_no LIKE 'AP-SEED-%') AS seeded_payable;
