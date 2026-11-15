-- ============================================================================
-- V0409__voucher_entry_foreign_amounts（M1-04 · 多币种：凭证录入外币金额 + 汇率 + 本位币换算）
--
-- 【本卡做什么】只给 `biz_voucher_entry` 加两列，不建表、不改已应用的迁移、不动 Rust：
--   ① `fc_debit_amount`  NUMERIC(20,4) NULL —— 借方**原币**金额
--   ② `fc_credit_amount` NUMERIC(20,4) NULL —— 贷方**原币**金额
--   单头的 `currency_code` / `exchange_rate` 已由 V0406（M1-01）就位，本卡只补**分录级**原币。
--
-- 【为什么分录要存原币，而不是只留单头币种 + 本位币金额】
--   会计上「外币核算科目」的余额是**双口径**的：原币余额（USD 100）+ 本位币余额（CNY 720）。
--   只存本位币金额，M1-05 期末调汇就无从算出差额（差额 = 原币余额 × 期末汇率 − 本位币余额），
--   M1-07 外币余额表 / M1-08 外币明细账也拿不到原币发生额。原币金额是**录入时用户的输入**，
--   属于业务事实，必须落库；本位币金额才是换算结果（`round(原币 × 汇率, 4)`）。
--   ⇒ 分录口径（唯一真相，改一处必须同步本注释 / M1-04 handler / 卡内汇报）：
--     `debit_amount` / `credit_amount` = **本位币**金额（总账、报表、试算平衡、年结全部按它取数，
--       与本卡之前逐行相同，**不回写、不改口径**）；
--     `fc_debit_amount` / `fc_credit_amount` = **原币**金额（外币凭证才有值）。
--
-- 【为什么是 NULL 语义，而不是 `NOT NULL DEFAULT 0`】
--   NULL 表示「该分录没有原币口径」（本位币凭证：原币 ≡ 本位币，汇率恒 1）。
--   三条硬理由：
--   ① 本位币凭证占 99%+，`NOT NULL DEFAULT 0` 要么逼所有写入口显式回填（本仓有 11 处裸 SQL
--      写 `biz_voucher_entry`：年结结转、报表夹具、期间夹具、成本/佣金/期初……改漏一处就是
--      运行期 500），要么让「原币 0 ≠ 本位币 720」这种自相矛盾的行存在；
--   ② 回填是**有损假设**：把存量行 `fc = debit/credit` 写死，一旦某天账套本位币被切成其它
--      币种，历史行就自动变成「外币行」，M1-05 会对它们做调汇 —— 凭空生损益；
--   ③ `NULL` 让「未回填」这件事**可探测**：M1-05 取数写
--      `SUM(COALESCE(fc_debit_amount, debit_amount))` 时，NULL 行自动退化成「原币 = 本位币」，
--      正是本位币凭证的正确语义，不需要任何回填脚本、也不需要重放幂等的 `IS NULL` 守卫。
--   ⇒ 本迁移**不做数据回填**（0 行数据改写）：重放天然幂等，与 V0406 的「恒等变换回填」不同。
--
-- 【CHECK 口径：一条约束同时管「成对 / 同边 / 非负」】
--   `ck_biz_voucher_entry_fc_side` 的三种合法形态：
--     ① `fc_debit_amount IS NULL AND fc_credit_amount IS NULL`（本位币分录 / 未记原币）
--     ② `fc_debit_amount > 0 AND debit_amount > 0 AND fc_credit_amount = 0`（借方原币，与借方本位币同边）
--     ③ `fc_credit_amount > 0 AND credit_amount > 0 AND fc_debit_amount = 0`（贷方原币，与贷方本位币同边）
--   拦住的脏数据（全部是「账面会错且没人发现」的类型）：
--   * **半对原币**（只写 fc_debit 不写 fc_credit）：M1-05 的 `fc_debit − fc_credit` 变 NULL，
--     SUM 直接把这行漏掉 ⇒ 原币余额少一行、调汇差额偏大；
--   * **原币与本位币不同边**（本位币记借方、原币记贷方）：冲销 / 余额方向全部反了；
--   * **负原币**：红冲走的是「借贷互换」，不是负数（V0149 的分录 CHECK 已如此），原币照同口径。
--   ★ PG 的 CHECK 在**求值为 NULL 时放行**（不是拒绝）—— 这是本条约束的取命门：
--   写成 `(fc_debit IS NULL AND fc_credit IS NULL) OR (fc_debit > 0 AND … fc_credit = 0) OR …`
--   时，「半对原币」那一支求值为 NULL，CHECK 直接放行（实测：第一次落库就漏进来了）。
--   所以每个比较都必须放在 `IS NOT NULL` 守卫**之后**，让整体只有 TRUE / FALSE 两种取值。
--   ⇒ 形态 ② 与 ③ 共用一个 `fc_debit_amount IS NOT NULL AND fc_credit_amount IS NOT NULL`
--   的前置守卫（见 DDL）。
--
-- 【为什么**不**在 `biz_voucher` 上加「原币合计」两列】
--   合计可由分录 `SUM` 得出（M1-07/08 本来就是按分录聚合取数）。单头再存一份 = 双真相：
--   分录被修正（期初覆盖式更新走 `replace_entries`）而合计忘同步，账就对不上，且库层
--   无法用 CHECK 跨表断言。本卡刻意不加。
--
-- 【索引】`idx_biz_voucher_entry_fc (account_code) WHERE fc_debit_amount IS NOT NULL`
--   M1-05 调汇 / M1-07 外币余额表的取数形态是「按科目扫外币分录」；部分索引把 99% 的本位币
--   分录排除在外，索引体积只有全表的零头。谓词与查询同形（`fc_debit_amount IS NOT NULL`），
--   规划器能直接用；不建 `(voucher_id)` 的重复索引（V0149 已有 `uk_biz_voucher_entry_line`）。
--
-- 【下游口径（写给 M1-05 / M1-07 / M1-08 / M1-09，别再自己发明一套）】
--   原币发生额 = `SUM(fc_debit_amount)` / `SUM(fc_credit_amount)`（`COALESCE(列, 本位币列)` 兜底）；
--   本位币发生额 = `SUM(debit_amount)` / `SUM(credit_amount)`（不变）；
--   原币余额按**币种**分组要连单头：`biz_voucher.currency_code`，即
--   「某科目在 USD 下的原币余额 = JOIN biz_voucher v ON v.id = e.voucher_id AND v.currency_code = 'USD'
--     THEN SUM(COALESCE(e.fc_debit_amount, e.debit_amount) − COALESCE(e.fc_credit_amount, e.credit_amount))」。
--   调汇凭证本身是**本位币凭证**（差额是 CNY，没有原币发生额），落 `currency_code = 本位币`、`exchange_rate = 1`。
--
-- 【幂等】
--   `ADD COLUMN IF NOT EXISTS` / 约束先查 `pg_constraint` 再 ADD / 索引 `CREATE INDEX IF NOT EXISTS`；
--   末尾自检块可反复执行，探针行自清场（残留必须为 0）。无数据回填 ⇒ 重放是纯 no-op。
--
-- 【向下兼容】
--   down 只在「库里还没有任何外币分录」时才允许执行（有一行就 RAISE EXCEPTION）：那些原币金额
--   是录入时的业务事实，回滚等于把它们扔掉（重放 up 只能得到 NULL）。与 V0406/V0408 同口径。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：两张表必须在（缺表说明 ERP 底座迁移没跑，报出来比 PG 的
--    `relation does not exist` 好排查）
-- ---------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.biz_voucher') IS NULL THEN
        RAISE EXCEPTION 'V0409: biz_voucher 不存在，请先应用 V0149（凭证与分录）';
    END IF;
    IF to_regclass('public.biz_voucher_entry') IS NULL THEN
        RAISE EXCEPTION 'V0409: biz_voucher_entry 不存在，请先应用 V0149（凭证与分录）';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'biz_voucher'
           AND column_name = 'currency_code'
    ) THEN
        RAISE EXCEPTION 'V0409: biz_voucher.currency_code 不存在，请先应用 V0406（M1-01 单头币种/汇率）';
    END IF;
    RAISE NOTICE 'V0409: 前置表与 V0406 列齐备，开始加列';
END $$;

-- ---------------------------------------------------------------------------
-- ② 加列（可空、无默认值：NULL = 本位币分录，见头注释第「NULL 语义」节）
-- ---------------------------------------------------------------------------
ALTER TABLE biz_voucher_entry ADD COLUMN IF NOT EXISTS fc_debit_amount  NUMERIC(20,4) NULL;
ALTER TABLE biz_voucher_entry ADD COLUMN IF NOT EXISTS fc_credit_amount NUMERIC(20,4) NULL;

COMMENT ON COLUMN biz_voucher_entry.fc_debit_amount IS
    '借方原币金额（外币凭证才有值）。NULL = 本位币分录（原币 ≡ 本位币）。本位币金额仍看 debit_amount（M1-04）';
COMMENT ON COLUMN biz_voucher_entry.fc_credit_amount IS
    '贷方原币金额（外币凭证才有值）。NULL = 本位币分录（原币 ≡ 本位币）。本位币金额仍看 credit_amount（M1-04）';

-- ---------------------------------------------------------------------------
-- ③ 约束：成对 / 同边 / 非负（一条表达，重放安全）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n integer;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'ck_biz_voucher_entry_fc_side'
          AND conrelid = 'public.biz_voucher_entry'::regclass
    ) THEN
        ALTER TABLE biz_voucher_entry
            ADD CONSTRAINT ck_biz_voucher_entry_fc_side CHECK (
                (fc_debit_amount IS NULL AND fc_credit_amount IS NULL)
                OR (
                    fc_debit_amount IS NOT NULL AND fc_credit_amount IS NOT NULL
                    AND (
                        (fc_debit_amount > 0 AND debit_amount > 0 AND fc_credit_amount = 0)
                        OR (fc_credit_amount > 0 AND credit_amount > 0 AND fc_debit_amount = 0)
                    )
                )
            );
    END IF;
    -- 建完**按名字 + 表**点名验一次：同名约束落在别的表上时上面的 `IF NOT EXISTS` 会静默跳过，
    -- 本迁移就成了「什么都没做但登记成功」。
    SELECT count(*) INTO n
      FROM pg_constraint
     WHERE conname = 'ck_biz_voucher_entry_fc_side'
       AND conrelid = 'public.biz_voucher_entry'::regclass;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0409: ck_biz_voucher_entry_fc_side 没落在 biz_voucher_entry 上（实际 % 条）', n;
    END IF;
    RAISE NOTICE 'V0409: ck_biz_voucher_entry_fc_side 在位';
END $$;

-- ---------------------------------------------------------------------------
-- ④ 部分索引：只收外币分录（M1-05 调汇 / M1-07 外币余额表的取数入口）
-- ---------------------------------------------------------------------------
CREATE INDEX IF NOT EXISTS idx_biz_voucher_entry_fc
    ON biz_voucher_entry (account_code)
    WHERE fc_debit_amount IS NOT NULL;

-- ---------------------------------------------------------------------------
-- ⑤ 自检块（结构 + 行为探针，可反复执行，探针行自清场）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n         integer;
    v_voucher uuid;
BEGIN
    -- ===== 结构：两列在位、类型 numeric(20,4)、可空、无默认 =====
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_voucher_entry'
       AND column_name IN ('fc_debit_amount', 'fc_credit_amount')
       AND data_type = 'numeric'
       AND numeric_precision = 20
       AND numeric_scale = 4
       AND is_nullable = 'YES'
       AND column_default IS NULL;
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0409 自检失败：原币两列只有 % 列符合 numeric(20,4)/NULL/无default（期望 2）', n;
    END IF;

    -- ===== 结构：CHECK 在位且定义里同时引用四列 =====
    SELECT count(*) INTO n
      FROM pg_constraint
     WHERE contype = 'c'
       AND conrelid = 'public.biz_voucher_entry'::regclass
       AND conname = 'ck_biz_voucher_entry_fc_side'
       AND pg_get_constraintdef(oid) LIKE '%fc_debit_amount%'
       AND pg_get_constraintdef(oid) LIKE '%fc_credit_amount%'
       AND pg_get_constraintdef(oid) LIKE '%debit_amount%';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0409 自检失败：ck_biz_voucher_entry_fc_side 缺失或没有同时引用原币/本位币列';
    END IF;

    -- ===== 结构：部分索引在位且谓词对 =====
    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname = 'idx_biz_voucher_entry_fc'
       AND indexdef LIKE '%WHERE%fc_debit_amount IS NOT NULL%';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0409 自检失败：idx_biz_voucher_entry_fc 缺失或谓词不是 fc_debit_amount IS NOT NULL';
    END IF;

    -- ===== 结构：存量分录全部是 NULL（本迁移不做回填）=====
    SELECT count(*) INTO n
      FROM biz_voucher_entry
     WHERE fc_debit_amount IS NOT NULL OR fc_credit_amount IS NOT NULL;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0409 自检失败：biz_voucher_entry 有 % 行带原币金额（本迁移不该回填任何行）', n;
    END IF;

    -- ===== 行为探针：造一张探针凭证（2 行分录，借 1 / 贷 1，均不带原币）=====
    INSERT INTO biz_voucher (voucher_word, voucher_no, period, voucher_date, voucher_type, source_type,
                             summary, total_debit, total_credit, entry_count, status)
    VALUES ('记', 'V0409PROBE', '1999-01', DATE '1999-01-01', 'MANUAL', 'MANUAL',
            '__V0409_PROBE__', 1, 1, 2, 'DRAFT')
    RETURNING id INTO v_voucher;

    -- P1：不写原币 ⇒ 两列 NULL（本位币分录的默认形态，写入口不必显式给值）
    INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name,
                                   debit_amount, credit_amount)
    VALUES (v_voucher, 1, '__V0409_P1__', gen_random_uuid(), '1999', '__V0409_PROBE__', 1, 0);
    SELECT count(*) INTO n
      FROM biz_voucher_entry
     WHERE voucher_id = v_voucher AND line_no = 1
       AND fc_debit_amount IS NULL AND fc_credit_amount IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0409 自检失败：本位币分录的原币列不是 NULL（写出了不该有的默认值）';
    END IF;

    -- P2：正常外币行（借方原币 7.2 / 本位币 1）可以写入
    INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name,
                                   debit_amount, credit_amount, fc_debit_amount, fc_credit_amount)
    VALUES (v_voucher, 2, '__V0409_P2__', gen_random_uuid(), '1999', '__V0409_PROBE__', 1, 0, 7.2, 0);

    -- P3：半对原币（只写 fc_debit）必须被拒
    BEGIN
        INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name,
                                       debit_amount, credit_amount, fc_debit_amount)
        VALUES (v_voucher, 3, '__V0409_P3__', gen_random_uuid(), '1999', '__V0409_PROBE__', 1, 0, 7.2);
        RAISE EXCEPTION 'V0409 自检失败：ck_biz_voucher_entry_fc_side 没生效（半对原币写入成功）';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- P4：原币与本位币不同边（本位币借、原币贷）必须被拒
    BEGIN
        INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name,
                                       debit_amount, credit_amount, fc_debit_amount, fc_credit_amount)
        VALUES (v_voucher, 4, '__V0409_P4__', gen_random_uuid(), '1999', '__V0409_PROBE__', 1, 0, 0, 7.2);
        RAISE EXCEPTION 'V0409 自检失败：ck_biz_voucher_entry_fc_side 没生效（原币与本位币不同边写入成功）';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- P5：负原币必须被拒
    BEGIN
        INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name,
                                       debit_amount, credit_amount, fc_debit_amount, fc_credit_amount)
        VALUES (v_voucher, 5, '__V0409_P5__', gen_random_uuid(), '1999', '__V0409_PROBE__', 1, 0, -7.2, 0);
        RAISE EXCEPTION 'V0409 自检失败：ck_biz_voucher_entry_fc_side 没生效（负原币写入成功）';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- ===== 探针清场（重复执行零残留）=====
    DELETE FROM biz_voucher_entry WHERE voucher_id = v_voucher;
    DELETE FROM biz_voucher WHERE voucher_no = 'V0409PROBE';

    SELECT count(*) INTO n FROM biz_voucher WHERE voucher_no = 'V0409PROBE';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0409 自检失败：凭证探针行残留 % 行', n;
    END IF;
    SELECT count(*) INTO n FROM biz_voucher_entry WHERE summary LIKE '__V0409\_%';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0409 自检失败：分录探针行残留 % 行', n;
    END IF;

    RAISE NOTICE 'V0409 自检通过：分录原币两列（numeric(20,4)，可空，NULL=本位币分录）+ 1 条 CHECK（成对/同边/非负）+ 部分索引 + 行为探针 5 组';
END $$;
