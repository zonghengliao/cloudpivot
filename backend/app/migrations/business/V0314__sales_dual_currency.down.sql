-- ======================================================
-- V0314 down：逆序撤销「销售链四表双币字段」（SO-40 · ARCH-7 §15.6.3 ③）
--
--   ① 先删 12 条 CHECK（currency_code / exchange_rate / base_amount）
--   ② 再删 20 列（4 表 × 5 列）
--   ③ biz_receivable.currency_code **不删**：它是 V0287 建的原始列，本迁移只是把它
--      从 CHAR(3) 放宽为 varchar(8)。回滚时**不收窄回 CHAR(3)** —— 收窄会静默截断
--      任何 4~8 位的币种码（虽然本仓不会出现），且 CHAR(3) 与 biz_currency.code
--      类型不一致恰恰是本卡要修的问题；宁可留 varchar(8) 也不回退到类型不一致的状态。
--      若确需完全回滚，请手工执行：
--        ALTER TABLE biz_receivable ALTER COLUMN currency_code TYPE char(3);
--
-- ⚠ 数据丢失提示：本 down 会丢掉单据上**已锁定的汇率**与 base_* 金额。生产上执行前先
--   `COPY (SELECT id, currency_code, exchange_rate, base_total_amount, base_total_tax,
--          base_total_with_tax FROM <表>) TO '/tmp/…csv' CSV HEADER`。
--
-- 幂等：DROP CONSTRAINT / DROP COLUMN IF EXISTS，重复执行 no-op。
-- ======================================================

-- ---------- ① 12 条 CHECK ----------
DO $$
DECLARE
    names text[] := ARRAY[
        'ck_biz_sales_order_currency_code', 'ck_biz_sales_order_exchange_rate', 'ck_biz_sales_order_base_amount',
        'ck_biz_ar_currency_code',          'ck_biz_ar_exchange_rate',          'ck_biz_ar_base_amount',
        'ck_biz_receipt_currency_code',     'ck_biz_receipt_exchange_rate',     'ck_biz_receipt_base_amount',
        'ck_biz_invoice_currency_code',     'ck_biz_invoice_exchange_rate',     'ck_biz_invoice_base_amount'
    ];
    tables text[] := ARRAY['biz_sales_order', 'biz_receivable', 'biz_receipt', 'biz_invoice'];
    t text;
    c text;
BEGIN
    FOREACH t IN ARRAY tables LOOP
        IF to_regclass('public.' || t) IS NULL THEN
            CONTINUE;
        END IF;
        FOREACH c IN ARRAY names LOOP
            EXECUTE format('ALTER TABLE %I DROP CONSTRAINT IF EXISTS %I', t, c);
        END LOOP;
    END LOOP;
END $$;

-- ---------- ② 20 列（currency_code 对 biz_receivable 是 V0287 的原始列，DROP 前先判断） ----------
DO $$
DECLARE
    tables text[] := ARRAY['biz_sales_order', 'biz_receivable', 'biz_receipt', 'biz_invoice'];
    t text;
BEGIN
    FOREACH t IN ARRAY tables LOOP
        IF to_regclass('public.' || t) IS NULL THEN
            RAISE NOTICE 'SO-40 down: 表 % 不存在，跳过', t;
            CONTINUE;
        END IF;
        EXECUTE format('ALTER TABLE %I DROP COLUMN IF EXISTS exchange_rate', t);
        EXECUTE format('ALTER TABLE %I DROP COLUMN IF EXISTS base_total_amount', t);
        EXECUTE format('ALTER TABLE %I DROP COLUMN IF EXISTS base_total_tax', t);
        EXECUTE format('ALTER TABLE %I DROP COLUMN IF EXISTS base_total_with_tax', t);
        -- currency_code：只有不复用 V0287 原始列的三张表才删
        IF t <> 'biz_receivable' THEN
            EXECUTE format('ALTER TABLE %I DROP COLUMN IF EXISTS currency_code', t);
        END IF;
    END LOOP;
    RAISE NOTICE 'SO-40 down: 已撤销双币列与约束（biz_receivable.currency_code 保留，见文件头）';
END $$;
