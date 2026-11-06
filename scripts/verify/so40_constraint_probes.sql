-- ============================================================================
-- SO-40 库层约束探针：验证 V0314 的 12 条 CHECK 真的能拦住脏数据
--
-- 用法：psql "postgres://alun:alun@127.0.0.1:5432/<库>" -f scripts/verify/so40_constraint_probes.sql
-- 全部在事务里跑，末尾 ROLLBACK —— 不污染库。每条探针失败会抛 EXCEPTION（psql 输出可见）。
-- ============================================================================
\set ON_ERROR_STOP 0
BEGIN;

-- ---------- P1：汇率 = 0 必须被拒（会 23514 check_violation） ----------
DO $$
BEGIN
    BEGIN
        UPDATE biz_sales_order SET exchange_rate = 0 WHERE id = (SELECT id FROM biz_sales_order LIMIT 1);
        RAISE EXCEPTION 'P1 FAIL: exchange_rate = 0 竟然写进去了（ck_biz_sales_order_exchange_rate 没生效）';
    EXCEPTION WHEN check_violation THEN
        RAISE NOTICE 'P1 PASS: exchange_rate = 0 被 ck_biz_sales_order_exchange_rate 拒绝';
    END;
END $$;

-- ---------- P2：币种码非三位大写必须被拒 ----------
DO $$
BEGIN
    BEGIN
        UPDATE biz_sales_order SET currency_code = 'usd' WHERE id = (SELECT id FROM biz_sales_order LIMIT 1);
        RAISE EXCEPTION 'P2 FAIL: currency_code = usd 竟然写进去了';
    EXCEPTION WHEN check_violation THEN
        RAISE NOTICE 'P2 PASS: 小写币种码被 ck_biz_sales_order_currency_code 拒绝';
    END;
END $$;

-- ---------- P3：本位币三项恒等式破掉必须被拒（尾差不能赖账） ----------
DO $$
BEGIN
    BEGIN
        UPDATE biz_sales_order SET base_total_with_tax = base_total_with_tax + 1
         WHERE id = (SELECT id FROM biz_sales_order LIMIT 1);
        RAISE EXCEPTION 'P3 FAIL: 含税 <> 不含税+税额 竟然通过了（恒等式 CHECK 没生效）';
    EXCEPTION WHEN check_violation THEN
        RAISE NOTICE 'P3 PASS: base_with_tax <> base_amount + base_tax 被 ck_biz_sales_order_base_amount 拒绝';
    END;
END $$;

-- ---------- P4：收款 / 发票同样有约束 ----------
DO $$
BEGIN
    BEGIN
        UPDATE biz_receipt SET exchange_rate = 0 WHERE id = (SELECT id FROM biz_receipt LIMIT 1);
        RAISE EXCEPTION 'P4a FAIL: 收款单 exchange_rate = 0 通过';
    EXCEPTION WHEN check_violation THEN
        RAISE NOTICE 'P4a PASS: 收款单 exchange_rate = 0 被拒';
    END;
    BEGIN
        UPDATE biz_invoice SET base_total_with_tax = base_total_with_tax + 1
         WHERE id = (SELECT id FROM biz_invoice LIMIT 1);
        -- 没有发票夹具时 UPDATE 命中 0 行，不触发约束 —— 打印跳过
        IF NOT FOUND THEN
            RAISE NOTICE 'P4b SKIP: 库里没有发票行（没跑发票夹具）';
        ELSE
            RAISE EXCEPTION 'P4b FAIL: 发票本位币恒等式被破坏却通过';
        END IF;
    EXCEPTION WHEN check_violation THEN
        RAISE NOTICE 'P4b PASS: 发票本位币恒等式被 ck_biz_invoice_base_amount 拒绝';
    END;
END $$;

-- ---------- P5：迁移记账行（kind=versioned + applied_via ∈ {executed,imported}） ----------
DO $$
DECLARE
    rec record;
BEGIN
    SELECT version, kind, applied_via, success INTO rec FROM schema_migrations WHERE source LIKE '%V0314%';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'P5 FAIL: schema_migrations 里没有 V0314 的记账行';
    END IF;
    IF rec.kind <> 'versioned' OR rec.applied_via NOT IN ('executed','imported') OR rec.success IS NOT TRUE THEN
        RAISE EXCEPTION 'P5 FAIL: 记账行不对：version=% kind=% applied_via=% success=%',
            rec.version, rec.kind, rec.applied_via, rec.success;
    END IF;
    RAISE NOTICE 'P5 PASS: 迁移记账 version=% kind=% applied_via=% success=%',
        rec.version, rec.kind, rec.applied_via, rec.success;
END $$;

-- ---------- P6：四张表都有 5 列（结构兜底，与迁移自检同口径） ----------
DO $$
DECLARE
    t text;
    n integer;
BEGIN
    FOREACH t IN ARRAY ARRAY['biz_sales_order','biz_receivable','biz_receipt','biz_invoice'] LOOP
        SELECT count(*) INTO n FROM information_schema.columns
         WHERE table_schema='public' AND table_name=t
           AND column_name IN ('currency_code','exchange_rate','base_total_amount','base_total_tax','base_total_with_tax');
        IF n <> 5 THEN
            RAISE EXCEPTION 'P6 FAIL: % 只有 % 列（期望 5）', t, n;
        END IF;
    END LOOP;
    RAISE NOTICE 'P6 PASS: 四张单据表各 5 个双币列';
END $$;

ROLLBACK;
