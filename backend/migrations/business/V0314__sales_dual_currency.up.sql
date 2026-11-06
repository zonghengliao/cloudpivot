-- ======================================================
-- V0314 up：销售链四张单据表「双币字段 + 汇率锁定」（SO-40 · ARCH-7 §15.6.3 ③ / §15.6.4 ①②④）
--
-- 改动（4 张表 × 5 列，逐表同名同型）：
--   currency_code     varchar(8)    NOT NULL DEFAULT 'CNY'   -- 原币币种（ISO 4217 三位大写）
--   exchange_rate     numeric(18,8) NOT NULL DEFAULT 1       -- **建单时锁定的**汇率：1 原币 = rate 本位币
--   base_total_amount numeric(18,4) NOT NULL DEFAULT 0       -- 本位币：不含税金额
--   base_total_tax    numeric(18,4) NOT NULL DEFAULT 0       -- 本位币：税额
--   base_total_with_tax numeric(18,4) NOT NULL DEFAULT 0     -- 本位币：价税合计
--
-- ── 为什么 base_* 是 NUMERIC(18,4) 而不是稿面的 (18,2) ────────────────────
--   ARCH-7 §15.6.3 的示例 DDL 写的是 `numeric(18,2)`，但同一章 §15.6.1 明写
--   「金额精度: numeric(18,4)」，本仓四张表的原币金额列全是 NUMERIC(18,4)
--   （V0264 total_amount / V0287 amount / V0292 amount / V0298 amount|tax_amount）。
--   用 (18,2) 会有两个硬伤：
--     ① **存量单据的 base 会被静默改写**：CNY 单的恒等式是 `base_* = 原币 *`，
--        原币 100.1234 的存量单回填成 100.12 —— 四舍五入吃掉了历史金额（审计不可接受）；
--     ② `base_total_with_tax = base_total_amount + base_total_tax` 这条恒等式在
--        逐项按 2 位舍入后**不成立**（amount/tax 各舍一次，与含税单独舍一次差 1~2 分），
--        于是只能放弃库层兜底，把尾差留给程序自觉 —— 与「单据金额恒等由 CHECK 兜底」
--        的全仓口径（ck_sales_order_amount / ck_biz_ar_unsettled / ck_biz_invoice_amount）不一致。
--   取 (18,4) 后：逐项 `round(原币 × rate, 4)`，含税列**取前两项之和**（尾差即落在含税列），
--   恒等式精确成立、可由 CHECK 兜底。
--
-- ── 汇率口径（与 SO-39 的 biz_exchange_rate 逐字对接，勿改 V0272）──────────
--   方向：`1 from_currency = rate to_currency`，只存「外币 → 本位币」；
--   取数：`from = 单据币种 AND to = 本位币 AND rate_type='SPOT' AND effective_date <= 单据日期
--          ORDER BY effective_date DESC LIMIT 1`（uk_biz_exchange_rate 最左前缀覆盖）；
--   锁定：**建单那一刻取到的 rate 落到单据行**，此后汇率表怎么变都不回写单据（§15.6.4 ①）。
--
-- ── 约束口径（4 表各 3 条，共 12 条）────────────────────────────────────
--   ck_*_currency_code  : `^[A-Z]{3}$`（与 ck_biz_currency_code / ck_biz_contract_currency 同正则）
--   ck_*_exchange_rate  : `exchange_rate > 0`
--                         —— ARCH §15.6.4 ④ 只要求「currency ≠ 本位币时 rate > 0」，
--                            这里收紧成**无条件 > 0**：本位币行的 rate 恒为 1，1 > 0 天然满足，
--                            而无条件约束才能拦住「外币单 + rate 0」这种最危险的脏数据
--                            （rate 0 会让 base_* 全 0：账面上这张单金额凭空消失）。
--   ck_*_base_amount    : `base_total_with_tax = base_total_amount + base_total_tax` + 符号
--                         （发票跟随 ck_biz_invoice_sign：正票非负 / 红字票非正）
--
-- ── biz_receivable.currency_code 的特殊处理 ─────────────────────────────
--   V0287 当年已经给 biz_receivable 加过 `currency_code CHAR(3) NOT NULL DEFAULT 'CNY'`，
--   本迁移**放宽为 varchar(8)**（与另外三张表、与 ARCH 稿面同型）：
--   CHAR(3) 在跨表 UNION / JOIN 时会带定长空格语义，且与 `biz_currency.code varchar(8)`
--   类型不一致（同码值比较要隐式转换）。放宽是 lossless 的（本仓全是 3 位码），
--   若库里另存过 >3 位的码（理论不可能，CHECK 会拦），ALTER 会直接报错而不是静默截断。
--
-- ── 幂等 ────────────────────────────────────────────────
--   ADD COLUMN IF NOT EXISTS / 约束先查 pg_constraint 再 ADD / 回填带「仍是默认值」谓词。
--   末尾 DO 块自检：4 表列与精度、12 条 CHECK、存量回填一致性，任一不符 → 迁移失败。
-- ======================================================

-- ---------- ① 四张单据表各加 5 列 ----------
-- 表用 DO 循环加，省得 20 条 ALTER 写四遍；缺表只 NOTICE 不红（未上销售模块的库）。
DO $$
DECLARE
    t text;
    tables text[] := ARRAY['biz_sales_order', 'biz_receivable', 'biz_receipt', 'biz_invoice'];
BEGIN
    FOREACH t IN ARRAY tables LOOP
        IF to_regclass('public.' || t) IS NULL THEN
            RAISE NOTICE 'SO-40: 表 % 不存在，跳过加列（前置销售链表迁移未应用？）', t;
            CONTINUE;
        END IF;
        EXECUTE format(
            'ALTER TABLE %I ADD COLUMN IF NOT EXISTS currency_code varchar(8) NOT NULL DEFAULT ''CNY''', t);
        EXECUTE format(
            'ALTER TABLE %I ADD COLUMN IF NOT EXISTS exchange_rate numeric(18,8) NOT NULL DEFAULT 1', t);
        EXECUTE format(
            'ALTER TABLE %I ADD COLUMN IF NOT EXISTS base_total_amount numeric(18,4) NOT NULL DEFAULT 0', t);
        EXECUTE format(
            'ALTER TABLE %I ADD COLUMN IF NOT EXISTS base_total_tax numeric(18,4) NOT NULL DEFAULT 0', t);
        EXECUTE format(
            'ALTER TABLE %I ADD COLUMN IF NOT EXISTS base_total_with_tax numeric(18,4) NOT NULL DEFAULT 0', t);
    END LOOP;
END $$;

-- biz_receivable 的既有 CHAR(3) 放宽为 varchar(8)（见文件头「特殊处理」）。
-- 放在 ADD COLUMN 之后：若该列不存在（理论上不会），ADD COLUMN 已建成 varchar(8)，此句 no-op。
DO $$
DECLARE
    t text;
BEGIN
    SELECT data_type INTO t FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_receivable'
       AND column_name = 'currency_code';
    IF t = 'character' THEN
        ALTER TABLE biz_receivable ALTER COLUMN currency_code TYPE varchar(8);
        RAISE NOTICE 'SO-40: biz_receivable.currency_code 已从 CHAR(3) 放宽为 varchar(8)';
    END IF;
END $$;

COMMENT ON COLUMN biz_sales_order.currency_code       IS '原币币种（ISO 4217 三位大写；默认 CNY）';
COMMENT ON COLUMN biz_sales_order.exchange_rate       IS '建单时锁定的汇率：1 原币 = rate 本位币（后续汇率变动不回写本列，ARCH §15.6.4 ①）';
COMMENT ON COLUMN biz_sales_order.base_total_amount   IS '本位币不含税金额 = round(total_amount × exchange_rate, 4)';
COMMENT ON COLUMN biz_sales_order.base_total_tax      IS '本位币税额 = round(total_tax × exchange_rate, 4)';
COMMENT ON COLUMN biz_sales_order.base_total_with_tax IS '本位币价税合计 = base_total_amount + base_total_tax（尾差落在这里）';
COMMENT ON COLUMN biz_receivable.currency_code       IS '原币币种（ISO 4217 三位大写；V0314 由 CHAR(3) 放宽为 varchar(8)）';
COMMENT ON COLUMN biz_receivable.exchange_rate       IS '建单时锁定的汇率：1 原币 = rate 本位币（后续汇率变动不回写本列）';
COMMENT ON COLUMN biz_receivable.base_total_amount   IS '本位币金额 = round(amount × exchange_rate, 4)';
COMMENT ON COLUMN biz_receivable.base_total_tax      IS '本位币税额（应收无税额拆分，恒 0；恒等式要求 base_amount + base_tax = base_with_tax）';
COMMENT ON COLUMN biz_receivable.base_total_with_tax IS '本位币价税合计 = base_total_amount + base_total_tax';
COMMENT ON COLUMN biz_receipt.currency_code          IS '原币币种（ISO 4217 三位大写；默认 CNY）';
COMMENT ON COLUMN biz_receipt.exchange_rate          IS '登记时锁定的汇率：1 原币 = rate 本位币（后续汇率变动不回写本列）';
COMMENT ON COLUMN biz_receipt.base_total_amount      IS '本位币收款金额 = round(amount × exchange_rate, 4)';
COMMENT ON COLUMN biz_receipt.base_total_tax         IS '本位币税额（收款无税额拆分，恒 0）';
COMMENT ON COLUMN biz_receipt.base_total_with_tax    IS '本位币价税合计 = base_total_amount + base_total_tax';
COMMENT ON COLUMN biz_invoice.currency_code          IS '原币币种（ISO 4217 三位大写；默认 CNY）';
COMMENT ON COLUMN biz_invoice.exchange_rate          IS '开票时锁定的汇率：1 原币 = rate 本位币（后续汇率变动不回写本列）';
COMMENT ON COLUMN biz_invoice.base_total_amount      IS '本位币不含税金额 = round(amount × exchange_rate, 4)（红字票为负）';
COMMENT ON COLUMN biz_invoice.base_total_tax         IS '本位币税额 = round(tax_amount × exchange_rate, 4)（红字票为负）';
COMMENT ON COLUMN biz_invoice.base_total_with_tax    IS '本位币价税合计 = base_total_amount + base_total_tax';

-- ---------- ② 约束（4 表 × 3 条） ----------
DO $$
DECLARE
    -- (表, 币种 CHECK, 汇率 CHECK, 本位币金额 CHECK)
    specs text[][] := ARRAY[
        ['biz_sales_order', 'ck_biz_sales_order_currency_code', 'ck_biz_sales_order_exchange_rate', 'ck_biz_sales_order_base_amount',
         'base_total_amount >= 0 AND base_total_tax >= 0 AND base_total_with_tax = base_total_amount + base_total_tax'],
        ['biz_receivable', 'ck_biz_ar_currency_code', 'ck_biz_ar_exchange_rate', 'ck_biz_ar_base_amount',
         'base_total_amount >= 0 AND base_total_with_tax = base_total_amount + base_total_tax'],
        ['biz_receipt', 'ck_biz_receipt_currency_code', 'ck_biz_receipt_exchange_rate', 'ck_biz_receipt_base_amount',
         'base_total_amount >= 0 AND base_total_with_tax = base_total_amount + base_total_tax'],
        -- 发票跟随 ck_biz_invoice_sign：正票非负、红字票非正（红冲要冲成负数，不能一律 >= 0）
        ['biz_invoice', 'ck_biz_invoice_currency_code', 'ck_biz_invoice_exchange_rate', 'ck_biz_invoice_base_amount',
         '(invoice_type = ''OUT''     AND base_total_amount >= 0 AND base_total_tax >= 0 AND base_total_with_tax = base_total_amount + base_total_tax) OR '
         '(invoice_type = ''OUT_RED'' AND base_total_amount <= 0 AND base_total_tax <= 0 AND base_total_with_tax = base_total_amount + base_total_tax)']
    ];
    s text[];
BEGIN
    FOREACH s SLICE 1 IN ARRAY specs LOOP
        IF to_regclass('public.' || s[1]) IS NULL THEN
            RAISE NOTICE 'SO-40: 表 % 不存在，跳过约束', s[1];
            CONTINUE;
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = s[2]) THEN
            EXECUTE format('ALTER TABLE %I ADD CONSTRAINT %I CHECK (currency_code ~ ''^[A-Z]{3}$'')', s[1], s[2]);
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = s[3]) THEN
            EXECUTE format('ALTER TABLE %I ADD CONSTRAINT %I CHECK (exchange_rate > 0)', s[1], s[3]);
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = s[4]) THEN
            EXECUTE format('ALTER TABLE %I ADD CONSTRAINT %I CHECK (%s)', s[1], s[4], s[5]);
        END IF;
    END LOOP;
END $$;

-- ---------- ③ 存量单据回填：CNY 单的 base_* = 原币 *（rate = 1） ----------
-- 谓词用「三列都还是默认 0」：真·零金额单回填后仍是 0（no-op），
-- 已由业务写入过的行（base 非 0）不会被二次覆盖 —— 重放安全。
DO $$
DECLARE
    n bigint;
BEGIN
    IF to_regclass('public.biz_sales_order') IS NOT NULL THEN
        UPDATE biz_sales_order
           SET base_total_amount   = total_amount,
               base_total_tax      = total_tax,
               base_total_with_tax = total_with_tax
         WHERE base_total_amount = 0 AND base_total_tax = 0 AND base_total_with_tax = 0;
        GET DIAGNOSTICS n = ROW_COUNT;
        RAISE NOTICE 'SO-40: biz_sales_order 回填 % 行（base_* = 原币）', n;
    END IF;

    IF to_regclass('public.biz_receivable') IS NOT NULL THEN
        UPDATE biz_receivable
           SET base_total_amount   = amount,
               base_total_tax      = 0,
               base_total_with_tax = amount
         WHERE base_total_amount = 0 AND base_total_tax = 0 AND base_total_with_tax = 0;
        GET DIAGNOSTICS n = ROW_COUNT;
        RAISE NOTICE 'SO-40: biz_receivable 回填 % 行（base_* = 原币）', n;
    END IF;

    IF to_regclass('public.biz_receipt') IS NOT NULL THEN
        UPDATE biz_receipt
           SET base_total_amount   = amount,
               base_total_tax      = 0,
               base_total_with_tax = amount
         WHERE base_total_amount = 0 AND base_total_tax = 0 AND base_total_with_tax = 0;
        GET DIAGNOSTICS n = ROW_COUNT;
        RAISE NOTICE 'SO-40: biz_receipt 回填 % 行（base_* = 原币）', n;
    END IF;

    IF to_regclass('public.biz_invoice') IS NOT NULL THEN
        UPDATE biz_invoice
           SET base_total_amount   = amount,
               base_total_tax      = tax_amount,
               base_total_with_tax = total_with_tax
         WHERE base_total_amount = 0 AND base_total_tax = 0 AND base_total_with_tax = 0;
        GET DIAGNOSTICS n = ROW_COUNT;
        RAISE NOTICE 'SO-40: biz_invoice 回填 % 行（base_* = 原币）', n;
    END IF;
END $$;

-- ---------- ④ 迁移自检（结构 + 精度 + 约束在位 + 回填一致） ----------
DO $$
DECLARE
    t      text;
    n      integer;
    txt    text;
    tbl    text;
    tables text[] := ARRAY['biz_sales_order', 'biz_receivable', 'biz_receipt', 'biz_invoice'];
    cols   text[] := ARRAY['currency_code', 'exchange_rate', 'base_total_amount', 'base_total_tax', 'base_total_with_tax'];
    c      text;
BEGIN
    FOREACH tbl IN ARRAY tables LOOP
        IF to_regclass('public.' || tbl) IS NULL THEN
            CONTINUE;
        END IF;

        -- ===== 结构：5 列齐 + NOT NULL =====
        SELECT count(*) INTO n FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = tbl AND column_name = ANY (cols);
        IF n <> 5 THEN
            RAISE EXCEPTION 'SO-40: % 双币列只有 % 个（期望 5：currency_code/exchange_rate/base_total_amount/base_total_tax/base_total_with_tax）', tbl, n;
        END IF;

        SELECT count(*) INTO n FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = tbl AND column_name = ANY (cols)
           AND is_nullable = 'NO';
        IF n <> 5 THEN
            RAISE EXCEPTION 'SO-40: % 双币列 NOT NULL 只有 % 列（期望 5：可空会让 base_* 出现 NULL，汇总静默丢行）', tbl, n;
        END IF;

        -- ===== 结构：精度（18,8 / 18,4）与稿面-修订口径一致 =====
        SELECT format('%s,%s', numeric_precision, numeric_scale) INTO txt
          FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = tbl AND column_name = 'exchange_rate';
        IF txt IS DISTINCT FROM '18,8' THEN
            RAISE EXCEPTION 'SO-40: %.exchange_rate 精度 = %（期望 18,8：6 位有效小数不够 USD/JPY 这类汇率的还原精度）', tbl, txt;
        END IF;

        FOREACH c IN ARRAY ARRAY['base_total_amount', 'base_total_tax', 'base_total_with_tax'] LOOP
            SELECT format('%s,%s', numeric_precision, numeric_scale) INTO txt
              FROM information_schema.columns
             WHERE table_schema = 'public' AND table_name = tbl AND column_name = c;
            IF txt IS DISTINCT FROM '18,4' THEN
                RAISE EXCEPTION 'SO-40: %.% 精度 = %（期望 18,4：与原币金额列同精度，见文件头）', tbl, c, txt;
            END IF;
        END LOOP;

        -- 币种列必须是 varchar（biz_receivable 由 CHAR(3) 放宽过）
        SELECT data_type INTO txt FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = tbl AND column_name = 'currency_code';
        IF txt IS DISTINCT FROM 'character varying' THEN
            RAISE EXCEPTION 'SO-40: %.currency_code 类型 = %（期望 character varying，与 biz_currency.code 同型）', tbl, txt;
        END IF;

        -- ===== 结构：3 条 CHECK 在位且定义含关键字（防「同名空壳」）=====
        -- 表名前缀不统一（销售订单 ck_biz_sales_order_* / 应收 ck_biz_ar_* / 其余跟表名），
        -- 所以按**定义内容**断言，而不是按名字拼：三条双币约束必须各命中一次。
        SELECT count(*) INTO n FROM pg_constraint
         WHERE conrelid = ('public.' || tbl)::regclass AND contype = 'c'
           AND pg_get_constraintdef(oid) LIKE '%currency_code%'
           AND pg_get_constraintdef(oid) LIKE '%[A-Z]%';
        IF n < 1 THEN
            RAISE EXCEPTION 'SO-40: % 缺币种码正则 CHECK（^[A-Z]{3}$，与 ck_biz_currency_code 同口径）', tbl;
        END IF;

        SELECT pg_get_constraintdef(oid) INTO txt FROM pg_constraint
         WHERE conrelid = ('public.' || tbl)::regclass AND contype = 'c'
           AND pg_get_constraintdef(oid) LIKE '%exchange_rate > (0)%';
        IF txt IS NULL THEN
            RAISE EXCEPTION 'SO-40: % 缺「exchange_rate > 0」CHECK（ARCH §15.6.4 ④）', tbl;
        END IF;

        SELECT pg_get_constraintdef(oid) INTO txt FROM pg_constraint
         WHERE conrelid = ('public.' || tbl)::regclass AND contype = 'c'
           AND pg_get_constraintdef(oid) LIKE '%base_total_with_tax%'
           AND pg_get_constraintdef(oid) LIKE '%base_total_amount%'
           AND pg_get_constraintdef(oid) LIKE '%base_total_tax%';
        IF txt IS NULL THEN
            RAISE EXCEPTION 'SO-40: % 缺「base_total_with_tax = base_total_amount + base_total_tax」CHECK', tbl;
        END IF;
    END LOOP;

    -- ===== 回填一致：CNY + rate 1 的单据 base_* 必须等于原币 =====
    IF to_regclass('public.biz_sales_order') IS NOT NULL THEN
        SELECT count(*) INTO n FROM biz_sales_order
         WHERE currency_code = 'CNY' AND exchange_rate = 1
           AND (base_total_amount <> total_amount OR base_total_tax <> total_tax
                OR base_total_with_tax <> total_with_tax);
        IF n > 0 THEN
            RAISE EXCEPTION 'SO-40: biz_sales_order 有 % 行 CNY 单的 base_* 与原币不符（回填没跑对）', n;
        END IF;
    END IF;

    IF to_regclass('public.biz_receivable') IS NOT NULL THEN
        SELECT count(*) INTO n FROM biz_receivable
         WHERE currency_code = 'CNY' AND exchange_rate = 1 AND base_total_with_tax <> amount;
        IF n > 0 THEN
            RAISE EXCEPTION 'SO-40: biz_receivable 有 % 行 CNY 单的 base_with_tax <> amount', n;
        END IF;
    END IF;

    IF to_regclass('public.biz_receipt') IS NOT NULL THEN
        SELECT count(*) INTO n FROM biz_receipt
         WHERE currency_code = 'CNY' AND exchange_rate = 1 AND base_total_with_tax <> amount;
        IF n > 0 THEN
            RAISE EXCEPTION 'SO-40: biz_receipt 有 % 行 CNY 单的 base_with_tax <> amount', n;
        END IF;
    END IF;

    IF to_regclass('public.biz_invoice') IS NOT NULL THEN
        SELECT count(*) INTO n FROM biz_invoice
         WHERE currency_code = 'CNY' AND exchange_rate = 1 AND base_total_with_tax <> total_with_tax;
        IF n > 0 THEN
            RAISE EXCEPTION 'SO-40: biz_invoice 有 % 行 CNY 单的 base_with_tax <> total_with_tax', n;
        END IF;
    END IF;

    RAISE NOTICE 'SO-40 自检通过：4 表 × 5 列（varchar(8) / 18,8 / 18,4×3）、12 条 CHECK 在位、存量回填一致';
END $$;
