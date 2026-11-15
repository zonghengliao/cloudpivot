-- ============================================================================
-- V0406__account_voucher_currency（M1-01 · 多币种存储层第一步 · 2026-11-16）
--
-- 【本卡做什么】两张表加列，不建表、不动索引结构、不写 Rust：
--   ① `biz_account` 加 `currency_code`   —— 科目核算币种（默认 CNY）
--   ② `biz_voucher` 加 `currency_code`   —— 凭证原币币种（默认 CNY）
--   ③ `biz_voucher` 加 `exchange_rate`   —— 建单时锁定的汇率（默认 1）
--   存量行一律回填 CNY / 1（全仓历史账套都是人民币，回填是**恒等变换**，不改任何金额）。
--
-- 【为什么先只做存储层】
--   M1-04（凭证录入：外币金额 + 汇率 + 本位币换算）、M1-05（期末调汇）、
--   M1-07/08（外币余额表 / 外币明细账）、M1-09（报表按币种取数）都要求这两列**先存在**。
--   本卡刻意不碰业务代码：加列带 DEFAULT + NOT NULL，现有 INSERT（全部显式列名）与
--   现有查询（全部显式列表）行为**逐字节不变**，不重启也不需要改一行 Rust。
--
-- 【列名口径：卡面写 `currency`，本迁移取 `currency_code`】
--   依据是**全仓 12 处同类列**的既有写法，不是个人口味：
--     biz_contract / biz_payable / biz_sales_order / biz_receivable / biz_receipt /
--     biz_invoice / biz_purchase_order … 一律 `currency_code`，且 V0272 的
--     `biz_currency.code`（IS0 4217 三位大写）就是它们 join 的目标。
--   取名 `currency` 会让本仓出现两套列名，M1-05 调汇与 M1-09 报表折算每次都要记
--   「这张表叫 currency，那张表叫 currency_code」—— 而写错时 PG 只在运行期报
--   `column does not exist`，不会在编译期提醒（列名不参与类型检查）。
--   ⇒ 取值 `currency_code`，并在此处留下偏差记录，避免下游卡按卡面硬写 `currency`。
--
-- 【类型与精度：与 V0314（SO-40 销售链双币）逐字同口径】
--   `currency_code VARCHAR(8) NOT NULL DEFAULT 'CNY'`  —— varchar(8) 而非 CHAR(3)：
--       CHAR(3) 在 UNION / JOIN 里带定长空格语义，与 `biz_currency.code varchar(8)`
--       比较要隐式转换（V0314 已把 biz_receivable 从 CHAR(3) 放宽过，勿再倒退）。
--       长度 8 留出「未来出现 4 位非标码」的余量，正则仍是严格三位大写。
--   `exchange_rate NUMERIC(18,8) NOT NULL DEFAULT 1`   —— 与 biz_exchange_rate.rate、
--       V0314 四张销售单据同精度。汇率写窄（如 18,6）会让 JPY→CNY 这类 0.04x 量级
--       丢有效位，折算差额在月末调汇时变成「找不到来源的尾差」。
--
-- 【CHECK 口径】
--   ck_biz_account_currency_code  / ck_biz_voucher_currency_code : `^[A-Z]{3}$`
--       与 ck_biz_currency_code / ck_biz_contract_currency / V0314 六条 CHECK 同一正则。
--       **不带**「非 CNY 必须在 biz_currency 里」这类跨表断言：CHECK 不能跨表，
--       币种主数据校验属于写入口（M1-04 的凭证录入接口）在事务里做。
--   ck_biz_voucher_exchange_rate : `exchange_rate > 0`
--       卡面没点名，但这是本卡唯一的「会静默算错账」入口：rate = 0 会让折算后
--       本位币金额全 0（账面上金额凭空消失），负汇率会造出反向金额。
--       CNY 行恒为 1，1 > 0 天然满足 ⇒ 无条件收紧没有副作用。
--
-- 【为什么**不**加外键指向 biz_currency(code)】
--   ① 与 V0314 完全一致：销售链 4 张单据表的 currency_code 全是弱引用，外键只建在
--      `biz_exchange_rate.from/to_currency` 上（那是币种对的构成部分，不是单据字段）；
--   ② 加 FK 会把「币种主数据」变成单据表的**删除依赖**：biz_currency 走
--      `status=INACTIVE` 停用（V0272 明确不软删），FK 的 RESTRICT 在这里换不来安全性，
--      只会让「先建外币科目、再从主数据页补币种」的导入顺序变脆；
--   ③ 真正需要「币种必须存在」的是**录入**那一刻，写入口保证即可（同 ①）。
--
-- 【索引：为什么加、加什么】
--   idx_biz_account_currency (currency_code)
--       M1-05 期末调汇的取数入口 = 「按币种列为外币的末级科目」；M1-03 之后科目表还要
--       支持「只看外币核算科目」的筛选。biz_account 是几百行的极小表，但网关排序/
--       过滤都落在它上面，走索引比全表扫便宜。
--   idx_biz_voucher_currency (currency_code, voucher_date DESC) [WHERE currency_code <> 'CNY']
--       部分索引：只收外币凭证（历史 + 未来 CNY 凭证占绝大多数，收进索引纯属负担），
--       M1-08 外币明细账按「币种 + 日期区间」倒序取数正好打在这两列上。
--       谓词 `<> 'CNY'` 与查询条件同形，规划器能直接用；若只按 currency_code 等值查
--       CNY，会退回顺序扫（那是「查全部历史凭证」的量级，本来就该走 period 索引）。
--
-- 【幂等】
--   `ADD COLUMN IF NOT EXISTS` / 约束先查 `pg_constraint` 再 ADD /
--   索引 `CREATE INDEX IF NOT EXISTS` / 回填带 `IS NULL` 谓词（重放 = no-op）。
--   末尾自检块可反复执行，探针行自清场，残留必须为 0。
--
-- 【向下兼容】
--   down 只在「库里还没有任何外币数据」时才允许执行（有外币行就 RAISE EXCEPTION），
--   因为回滚会把这些列连同真实外币信息一起丢掉 —— 那属于业务数据，不是结构。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：两张表必须存在（缺表说明 ERP 底座迁移没跑，报出来比 PG 的
--    `relation does not exist` 好排查）
-- ---------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.biz_account') IS NULL THEN
        RAISE EXCEPTION 'V0406: biz_account 不存在，请先应用 V0137（ERP 主数据与仓库）';
    END IF;
    IF to_regclass('public.biz_voucher') IS NULL THEN
        RAISE EXCEPTION 'V0406: biz_voucher 不存在，请先应用 V0149（凭证与分录）';
    END IF;
    RAISE NOTICE 'V0406: 前置表齐备，开始加列';
END $$;

-- ---------------------------------------------------------------------------
-- ② 加列（NOT NULL + DEFAULT ⇒ 存量行自动回填，且新写入不必显式给值）
-- ---------------------------------------------------------------------------
ALTER TABLE biz_account ADD COLUMN IF NOT EXISTS currency_code VARCHAR(8)     NOT NULL DEFAULT 'CNY';
ALTER TABLE biz_voucher ADD COLUMN IF NOT EXISTS currency_code VARCHAR(8)     NOT NULL DEFAULT 'CNY';
ALTER TABLE biz_voucher ADD COLUMN IF NOT EXISTS exchange_rate NUMERIC(18,8)  NOT NULL DEFAULT 1;

COMMENT ON COLUMN biz_account.currency_code IS '科目核算币种（ISO 4217 三位大写，默认 CNY）。外币科目由 M1-03 的「是否外币核算」标记 + 期末调汇使用';
COMMENT ON COLUMN biz_voucher.currency_code IS '凭证原币币种（ISO 4217 三位大写，默认 CNY）。分录金额以该币种记账；本位币口径由 M1-04 落实';
COMMENT ON COLUMN biz_voucher.exchange_rate IS '建单时锁定的汇率：1 原币 = rate 本位币（CNY 恒 1）。锁定后汇率表变动不回写本列（同 ARCH-7 §15.6.4①）';

-- ---------------------------------------------------------------------------
-- ③ 约束（三条，全部「先查再 ADD」，重放安全）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n integer;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'ck_biz_account_currency_code'
          AND conrelid = 'public.biz_account'::regclass
    ) THEN
        ALTER TABLE biz_account
            ADD CONSTRAINT ck_biz_account_currency_code CHECK (currency_code ~ '^[A-Z]{3}$');
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'ck_biz_voucher_currency_code'
          AND conrelid = 'public.biz_voucher'::regclass
    ) THEN
        ALTER TABLE biz_voucher
            ADD CONSTRAINT ck_biz_voucher_currency_code CHECK (currency_code ~ '^[A-Z]{3}$');
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'ck_biz_voucher_exchange_rate'
          AND conrelid = 'public.biz_voucher'::regclass
    ) THEN
        ALTER TABLE biz_voucher
            ADD CONSTRAINT ck_biz_voucher_exchange_rate CHECK (exchange_rate > 0);
    END IF;

    -- 建完必须点名验一次：约束同名落在**别的表**上（历史遗留 / 别人手工加过）时上面的
    -- IF NOT EXISTS 会静默跳过，表上其实没有这条约束 —— 不在这里拦住，脏数据入口就打开了。
    SELECT count(*) INTO n
      FROM pg_constraint
     WHERE contype = 'c'
       AND ((conname = 'ck_biz_account_currency_code' AND conrelid = 'public.biz_account'::regclass)
         OR (conname = 'ck_biz_voucher_currency_code' AND conrelid = 'public.biz_voucher'::regclass)
         OR (conname = 'ck_biz_voucher_exchange_rate' AND conrelid = 'public.biz_voucher'::regclass));
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0406: 三条币种 / 汇率 CHECK 只建成 % 条（期望 3：可能同名约束落在别的表上）', n;
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- ④ 索引（见文件头「索引」段）
-- ---------------------------------------------------------------------------
CREATE INDEX IF NOT EXISTS idx_biz_account_currency
    ON biz_account (currency_code);

CREATE INDEX IF NOT EXISTS idx_biz_voucher_currency
    ON biz_voucher (currency_code, voucher_date DESC)
    WHERE currency_code <> 'CNY';

-- ---------------------------------------------------------------------------
-- ⑤ 存量回填（ADD COLUMN … NOT NULL DEFAULT 已把存量行填好，这里再显式收一遍
--    空值：万一列是被别人先手工加成可空的，本迁移也要把它收成 CNY / 1）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n_acc  bigint := 0;
    n_vou  bigint := 0;
    n_rate bigint := 0;
    n      bigint := 0;
BEGIN
    UPDATE biz_account
       SET currency_code = 'CNY', updated_at = now()
     WHERE currency_code IS NULL OR btrim(currency_code) = '';
    GET DIAGNOSTICS n_acc = ROW_COUNT;

    UPDATE biz_voucher
       SET currency_code = 'CNY', updated_at = now()
     WHERE currency_code IS NULL OR btrim(currency_code) = '';
    GET DIAGNOSTICS n_vou = ROW_COUNT;

    UPDATE biz_voucher
       SET exchange_rate = 1, updated_at = now()
     WHERE exchange_rate IS NULL;
    GET DIAGNOSTICS n_rate = ROW_COUNT;

    -- 回填后必须一条空值都不剩（NOT NULL 已兜底，这里是「真的收干净了」的实证）
    SELECT count(*) INTO n FROM biz_account WHERE currency_code IS NULL OR btrim(currency_code) = '';
    IF n > 0 THEN
        RAISE EXCEPTION 'V0406: biz_account 仍有 % 行 currency_code 为空（回填没跑对）', n;
    END IF;
    SELECT count(*) INTO n FROM biz_voucher WHERE currency_code IS NULL OR btrim(currency_code) = '';
    IF n > 0 THEN
        RAISE EXCEPTION 'V0406: biz_voucher 仍有 % 行 currency_code 为空（回填没跑对）', n;
    END IF;
    SELECT count(*) INTO n FROM biz_voucher WHERE exchange_rate IS NULL;
    IF n > 0 THEN
        RAISE EXCEPTION 'V0406: biz_voucher 仍有 % 行 exchange_rate 为空（回填没跑对）', n;
    END IF;

    RAISE NOTICE 'V0406 回填：biz_account.currency_code % 行、biz_voucher.currency_code % 行、biz_voucher.exchange_rate % 行（ADD COLUMN 已默认填好时这里全为 0）',
        n_acc, n_vou, n_rate;
END $$;

-- ---------------------------------------------------------------------------
-- ⑥ 迁移自检（结构 + 默认值 + 精度 + 约束在位 + 行为探针；任一不符即 RAISE
--    EXCEPTION 让整个迁移回滚 —— 「跑过了」和「验过了」是两件事）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n        integer;
    txt      text;
    def      text;
    v_num    numeric;
    v_acc_id uuid;
BEGIN
    -- ===== 结构：三列齐、NOT NULL =====
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_account' AND column_name = 'currency_code';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0406 自检失败：biz_account.currency_code 不存在';
    END IF;

    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_voucher'
       AND column_name IN ('currency_code', 'exchange_rate');
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0406 自检失败：biz_voucher 的 currency_code / exchange_rate 只有 % 列在位（期望 2）', n;
    END IF;

    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public'
       AND ((table_name = 'biz_account' AND column_name = 'currency_code')
         OR (table_name = 'biz_voucher' AND column_name IN ('currency_code', 'exchange_rate')))
       AND is_nullable = 'NO';
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0406 自检失败：三列 NOT NULL 只有 % 列（可空会让折算取到 NULL，汇总静默丢行）', n;
    END IF;

    -- ===== 结构：类型 / 默认值 =====
    SELECT data_type INTO txt
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_account' AND column_name = 'currency_code';
    IF txt IS DISTINCT FROM 'character varying' THEN
        RAISE EXCEPTION 'V0406 自检失败：biz_account.currency_code 类型 = %（期望 character varying，与 biz_currency.code 同型）', txt;
    END IF;
    SELECT data_type INTO txt
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_voucher' AND column_name = 'currency_code';
    IF txt IS DISTINCT FROM 'character varying' THEN
        RAISE EXCEPTION 'V0406 自检失败：biz_voucher.currency_code 类型 = %（期望 character varying）', txt;
    END IF;

    SELECT format('%s,%s', numeric_precision, numeric_scale) INTO txt
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_voucher' AND column_name = 'exchange_rate';
    IF txt IS DISTINCT FROM '18,8' THEN
        RAISE EXCEPTION 'V0406 自检失败：biz_voucher.exchange_rate 精度 = %（期望 18,8：与 biz_exchange_rate.rate / V0314 同精度，写窄会丢有效位）', txt;
    END IF;

    -- 默认值：CNY / 1（存量回填与新写入的兜底都靠它）
    SELECT column_default INTO def
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_account' AND column_name = 'currency_code';
    IF def IS DISTINCT FROM '''CNY''::character varying' THEN
        RAISE EXCEPTION 'V0406 自检失败：biz_account.currency_code 默认值 = %（期望 ''CNY''）', def;
    END IF;
    SELECT column_default INTO def
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_voucher' AND column_name = 'currency_code';
    IF def IS DISTINCT FROM '''CNY''::character varying' THEN
        RAISE EXCEPTION 'V0406 自检失败：biz_voucher.currency_code 默认值 = %（期望 ''CNY''）', def;
    END IF;
    SELECT column_default INTO def
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_voucher' AND column_name = 'exchange_rate';
    IF def IS DISTINCT FROM '1' THEN
        RAISE EXCEPTION 'V0406 自检失败：biz_voucher.exchange_rate 默认值 = %（期望 1）', def;
    END IF;

    -- ===== 结构：三条 CHECK 在位（按名字 + 定义内容双判，防「同名空壳」）=====
    SELECT count(*) INTO n
      FROM pg_constraint
     WHERE conname IN ('ck_biz_account_currency_code', 'ck_biz_voucher_currency_code', 'ck_biz_voucher_exchange_rate')
       AND contype = 'c';
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0406 自检失败：币种 / 汇率 CHECK 只有 % 条在位（期望 3）', n;
    END IF;

    SELECT count(*) INTO n
      FROM pg_constraint
     WHERE contype = 'c'
       AND ((conname = 'ck_biz_account_currency_code' AND pg_get_constraintdef(oid) LIKE '%currency_code%')
         OR (conname = 'ck_biz_voucher_currency_code' AND pg_get_constraintdef(oid) LIKE '%currency_code%')
         OR (conname = 'ck_biz_voucher_exchange_rate' AND pg_get_constraintdef(oid) LIKE '%exchange_rate > (0)%'));
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0406 自检失败：三条 CHECK 的定义内容不符（期望 2 条币种正则 + 1 条汇率 > 0），命中 % 条', n;
    END IF;

    -- ===== 结构：两条索引在位 =====
    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = 'public' AND indexname IN ('idx_biz_account_currency', 'idx_biz_voucher_currency');
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0406 自检失败：索引只有 % 条在位（期望 2：idx_biz_account_currency / idx_biz_voucher_currency）', n;
    END IF;

    -- ===== 结构：存量行全部有值（回填实证，与 ⑤ 互相印证）=====
    SELECT count(*) INTO n FROM biz_account WHERE currency_code IS NULL;
    IF n > 0 THEN
        RAISE EXCEPTION 'V0406 自检失败：biz_account 有 % 行 currency_code 为 NULL', n;
    END IF;
    SELECT count(*) INTO n FROM biz_voucher WHERE currency_code IS NULL OR exchange_rate IS NULL;
    IF n > 0 THEN
        RAISE EXCEPTION 'V0406 自检失败：biz_voucher 有 % 行币种 / 汇率为 NULL', n;
    END IF;

    -- ===== 行为探针 P1：显式非法币种码必须被拒（科目表；'usd' 小写）=====
    BEGIN
        INSERT INTO biz_account (account_code, account_name, level, account_category, balance_direction, currency_code)
        VALUES ('V0406PROBE-BAD', '__V0406_PROBE_BAD_CODE__', 1, 'ASSET', 'DEBIT', 'usd');
        RAISE EXCEPTION 'V0406 自检失败：ck_biz_account_currency_code 没生效（小写币种插入成功）';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO biz_account (account_code, account_name, level, account_category, balance_direction, currency_code)
        VALUES ('V0406PROBE-BAD', '__V0406_PROBE_BAD_CODE__', 1, 'ASSET', 'DEBIT', 'US');
        RAISE EXCEPTION 'V0406 自检失败：ck_biz_account_currency_code 没生效（两位币种插入成功）';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- ===== 行为探针 P2：科目表默认币种 = CNY（新写入不必显式给值）=====
    INSERT INTO biz_account (account_code, account_name, level, account_category, balance_direction)
    VALUES ('V0406PROBE', '__V0406_PROBE__', 1, 'ASSET', 'DEBIT')
    RETURNING id INTO v_acc_id;

    SELECT currency_code INTO txt FROM biz_account WHERE id = v_acc_id;
    IF txt IS DISTINCT FROM 'CNY' THEN
        RAISE EXCEPTION 'V0406 自检失败：新建科目的默认币种 = %（期望 CNY）', txt;
    END IF;

    -- ===== 行为探针 P3：凭证默认币种 / 汇率（CNY 恒 1）=====
    INSERT INTO biz_voucher (voucher_word, voucher_no, period, voucher_date, total_debit, total_credit, entry_count, remark)
    VALUES ('记', 'V0406PROBE', '1900-01', DATE '1900-01-01', 100, 100, 2, '__V0406_PROBE__');

    SELECT currency_code INTO txt FROM biz_voucher WHERE voucher_no = 'V0406PROBE' AND deleted_at IS NULL;
    IF txt IS DISTINCT FROM 'CNY' THEN
        RAISE EXCEPTION 'V0406 自检失败：新建凭证的默认币种 = %（期望 CNY）', txt;
    END IF;
    SELECT exchange_rate INTO v_num FROM biz_voucher WHERE voucher_no = 'V0406PROBE' AND deleted_at IS NULL;
    IF v_num IS DISTINCT FROM 1::numeric THEN
        RAISE EXCEPTION 'V0406 自检失败：新建凭证的默认汇率 = %（期望 1；外币行必须显式给 rate）', v_num;
    END IF;

    -- ===== 行为探针 P4：汇率 0 / 负数必须被拒 =====
    BEGIN
        UPDATE biz_voucher SET exchange_rate = 0
         WHERE voucher_no = 'V0406PROBE' AND deleted_at IS NULL;
        RAISE EXCEPTION 'V0406 自检失败：ck_biz_voucher_exchange_rate 没生效（rate = 0 写入成功 —— 折算后金额会全变 0）';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    BEGIN
        UPDATE biz_voucher SET exchange_rate = -7.1
         WHERE voucher_no = 'V0406PROBE' AND deleted_at IS NULL;
        RAISE EXCEPTION 'V0406 自检失败：ck_biz_voucher_exchange_rate 没生效（负汇率写入成功）';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- ===== 行为探针 P5：凭证币种必须三位大写 =====
    BEGIN
        UPDATE biz_voucher SET currency_code = 'RMB¥'
         WHERE voucher_no = 'V0406PROBE' AND deleted_at IS NULL;
        RAISE EXCEPTION 'V0406 自检失败：ck_biz_voucher_currency_code 没生效（带符号的币种写入成功）';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- ===== 行为探针 P6：外币行可以正常写入（约束不能把正常业务挡死）=====
    UPDATE biz_voucher
       SET currency_code = 'USD', exchange_rate = 7.12345678
     WHERE voucher_no = 'V0406PROBE' AND deleted_at IS NULL;
    SELECT exchange_rate INTO v_num FROM biz_voucher WHERE voucher_no = 'V0406PROBE' AND deleted_at IS NULL;
    IF v_num IS DISTINCT FROM 7.12345678::numeric THEN
        RAISE EXCEPTION 'V0406 自检失败：外币凭证汇率落库 = %（期望 7.12345678；精度不够会在这里暴露）', v_num;
    END IF;

    -- ===== 探针清场（重复执行零残留）=====
    DELETE FROM biz_voucher WHERE voucher_no = 'V0406PROBE';
    DELETE FROM biz_account WHERE account_code = 'V0406PROBE' OR account_code LIKE 'V0406PROBE%';

    SELECT count(*) INTO n FROM biz_voucher WHERE voucher_no = 'V0406PROBE';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0406 自检失败：凭证探针行残留 % 行', n;
    END IF;
    SELECT count(*) INTO n FROM biz_account WHERE account_code LIKE 'V0406PROBE%';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0406 自检失败：科目探针行残留 % 行', n;
    END IF;

    RAISE NOTICE 'V0406 自检通过：三列（varchar(8) / 18,8）+ 默认 CNY/1 + 3 条 CHECK + 2 条索引 + 存量回填无空值 + 行为探针 6 条';
END $$;
