-- ======================================================
-- V0315 up：汇兑损益**月末重估台账**（SO-41 · ARCH-7 §15.6.4 ②）
--
-- 新增一张表 biz_fx_revaluation（一行 = 一会计期间 × 一笔应收的重估结果），
-- 载体是「月末结账时对未核销外币应收按期末 SPOT 汇率重估」这件事的**唯一留痕**：
-- 重估凭证（source_type='FX_REVALUATION'）的合计差额在这张表里有逐笔明细可回溯。
--
-- ── 为什么必须落一张表，而不是「每次现算」────────────────────────────
--   重估会改 1122 的账面人民币：本月提了 +800，下月再算「账面人民币」时必须把它算进去，
--   否则每个月都会把同一块汇兑损益重提一遍（账越提越偏）。逐笔的「已提累计」
--   （Σ diff_base）只能落在台账里 —— 凭证分录是**科目级合计**（一张凭证只有 1122 / 5603 两行），
--   `biz_voucher_entry` 上没有 customer / receivable 辅助核算列，反查不到「这笔应收提过多少」。
--
-- ── 口径（与 `erp/fx_revaluation.rs` 头注释逐字一致，改一处必须同步）────────────
--   open_amount   = biz_receivable.unsettled_amount            （未核销原币，> 0）
--   book_base     = round(base_total_with_tax × open / amount, 4) + Σ历史 diff_base
--   revalued_base = round(open × 期末汇率, 4)                   （期末 = 期间码那月最后一天）
--   diff_base     = revalued_base − book_base                   （正 = 调增应收 / 贷汇兑收益）
--
-- ── 账号口径：卡面写「6603 财务费用-汇兑损益」，本库落 5603 ──────────────────
--   本仓科目表是**小企业准则**：损益类只有 5xxx，财务费用 = 5603（V0183 资产负债表映射、
--   V0184 利润表 FINANCE_EXPENSE 都按 5603 取数）。与既有三次同口径决策一致
--   （V0226 6602→5602、V0229 6602→5602.01、V0311 6601→5601）：新种 66xx 会让财务费用
--   在报表里多出一份、而 5603 那份恒为 0。故本迁移**不种 66xx 科目**，
--   凭证落 5603（末级），摘要写「汇兑损益」以便在总账里单独筛出。
--   ⇒ 本迁移不写 biz_account / biz_voucher_rule，也不动任何既有行。
--
-- ── 约束 ─────────────────────────────────────────────
--   ck_biz_fx_currency  : `^[A-Z]{3}$`（与 ck_biz_currency_code / ck_*_currency_code 同正则）
--   ck_biz_fx_rate      : book_rate > 0 AND close_rate > 0（汇率 ≤ 0 会把重估金额算成 0 / 负数）
--   ck_biz_fx_open      : open_amount > 0（未核销为 0 的应收不参与重估，不该有行）
--   ck_biz_fx_diff      : diff_base = revalued_base − book_base（程序算错立刻被库拦住）
--
-- ── 索引 ─────────────────────────────────────────────
--   uk_biz_fx_period_ar     (period_code, receivable_id)  ← 幂等键：同一期同一笔只许一行
--   idx_biz_fx_receivable   (receivable_id)               ← 下期取「历史已提累计」用
--   idx_biz_fx_period       (period_code)                 ← 结账时按期间列表 / 对账
--
-- ── 幂等 ─────────────────────────────────────────────
--   CREATE TABLE / INDEX IF NOT EXISTS；末尾 DO 块自检：表 / 14 列 / 4 条 CHECK / 3 个索引 /
--   4 组约束行为探针（坏数据必须被拒 + 正常行可写可读），任一不符 → 迁移失败。
-- ======================================================

CREATE TABLE IF NOT EXISTS biz_fx_revaluation (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),   -- 主键
    period_code    VARCHAR(7)    NOT NULL,                        -- 会计期间 yyyy-MM（重估归属期）
    receivable_id  UUID          NOT NULL,                        -- 应收 id（弱引用 biz_receivable.id）
    ar_no          VARCHAR(32)   NOT NULL,                        -- 应收单号（人的对账键，冗余存一份便于回溯）
    currency_code  VARCHAR(8)    NOT NULL,                        -- 原币币种（ISO 4217 三位大写）
    book_rate      NUMERIC(18,8) NOT NULL,                        -- 账面汇率 = book_base / open_amount（派生列，给人看）
    close_rate     NUMERIC(18,8) NOT NULL,                        -- 期末 SPOT 汇率（取自 biz_exchange_rate）
    open_amount    NUMERIC(18,4) NOT NULL,                        -- 未核销原币
    book_base      NUMERIC(18,4) NOT NULL,                        -- 重估前账面人民币（含历史重估）
    revalued_base  NUMERIC(18,4) NOT NULL,                        -- 重估后人民币 = round(open × close_rate, 4)
    diff_base      NUMERIC(18,4) NOT NULL,                        -- 差额 = revalued_base − book_base（正 = 调增应收）
    voucher_id     UUID,                                          -- 重估凭证 id（差额合计为 0 时为 NULL）
    create_by      UUID,                                          -- 审计：操作人（结账人）
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 审计：创建时间

    CONSTRAINT ck_biz_fx_currency
        CHECK (currency_code ~ '^[A-Z]{3}$'),
    CONSTRAINT ck_biz_fx_rate
        CHECK (book_rate > 0 AND close_rate > 0),
    CONSTRAINT ck_biz_fx_open
        CHECK (open_amount > 0),
    -- 差额恒等：程序算错（或用错口径）会被数据库直接拦住，不靠自觉
    CONSTRAINT ck_biz_fx_diff
        CHECK (diff_base = revalued_base - book_base)
);

COMMENT ON TABLE  biz_fx_revaluation IS '汇兑损益月末重估台账（SO-41 / ARCH-7 §15.6.4 ②）：一行 = 一期 × 一笔外币应收的重估结果';
COMMENT ON COLUMN biz_fx_revaluation.period_code   IS '重估归属会计期间 yyyy-MM（期末汇率取该月最后一天）';
COMMENT ON COLUMN biz_fx_revaluation.book_base     IS '重估前账面人民币 = round(应收本位币含税 × 未核销占比, 4) + Σ历史 diff_base';
COMMENT ON COLUMN biz_fx_revaluation.revalued_base IS '重估后人民币 = round(未核销原币 × 期末汇率, 4)';
COMMENT ON COLUMN biz_fx_revaluation.diff_base     IS '差额 = 重估后 − 重估前（正 = 调增应收 1122 / 贷 5603；负 = 反向）';
COMMENT ON COLUMN biz_fx_revaluation.voucher_id    IS '重估凭证 id（弱引用 biz_voucher.id；差额合计为 0 时无凭证，为 NULL）';

-- 幂等键：同一期同一笔应收只许一行（重跑走 ON CONFLICT DO UPDATE）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_fx_period_ar
    ON biz_fx_revaluation (period_code, receivable_id);
-- 下期算「历史已提累计」：按应收反查
CREATE INDEX IF NOT EXISTS idx_biz_fx_receivable
    ON biz_fx_revaluation (receivable_id);
-- 结账 / 对账：按期间列表
CREATE INDEX IF NOT EXISTS idx_biz_fx_period
    ON biz_fx_revaluation (period_code);

-- ---------- 自检：结构 + 约束行为 ----------
DO $$
DECLARE
    n        INT;
    probe_ok UUID := 'd0315000-0000-4000-8000-000000000001'::uuid;
    ar_probe UUID := 'd0315000-0000-4000-8000-000000000002'::uuid;
BEGIN
    -- 1.1 表在位
    IF to_regclass('public.biz_fx_revaluation') IS NULL THEN
        RAISE EXCEPTION 'V0315 自检失败：biz_fx_revaluation 未建';
    END IF;

    -- 1.2 列数（多一列少一列都算漂移）
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_fx_revaluation';
    IF n <> 14 THEN
        RAISE EXCEPTION 'V0315 自检失败：列数 = %（期望 14）', n;
    END IF;

    -- 1.3 点名列逐字在位
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_fx_revaluation'
       AND column_name IN ('id','period_code','receivable_id','ar_no','currency_code','book_rate',
                           'close_rate','open_amount','book_base','revalued_base','diff_base',
                           'voucher_id','create_by','created_at');
    IF n <> 14 THEN
        RAISE EXCEPTION 'V0315 自检失败：点名列只齐了 %/14', n;
    END IF;

    -- 1.4 金额列精度必须是 numeric(18,4) / 汇率 numeric(18,8)
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_fx_revaluation'
       AND ((column_name IN ('open_amount','book_base','revalued_base','diff_base')
             AND data_type = 'numeric' AND numeric_precision = 18 AND numeric_scale = 4)
            OR (column_name IN ('book_rate','close_rate')
                AND data_type = 'numeric' AND numeric_precision = 18 AND numeric_scale = 8));
    IF n <> 6 THEN
        RAISE EXCEPTION 'V0315 自检失败：金额/汇率列精度只对了 %/6', n;
    END IF;

    -- 1.5 4 条 CHECK 在位
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_fx_revaluation'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_fx_currency','ck_biz_fx_rate','ck_biz_fx_open','ck_biz_fx_diff');
    IF n <> 4 THEN
        RAISE EXCEPTION 'V0315 自检失败：CHECK 只有 %/4', n;
    END IF;

    -- 1.6 3 个索引在位
    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_fx_revaluation'
       AND indexname IN ('uk_biz_fx_period_ar','idx_biz_fx_receivable','idx_biz_fx_period');
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0315 自检失败：索引只有 %/3', n;
    END IF;

    -- 2.1 正常行可写（差额恒等成立）
    INSERT INTO biz_fx_revaluation
        (id, period_code, receivable_id, ar_no, currency_code, book_rate, close_rate,
         open_amount, book_base, revalued_base, diff_base)
    VALUES (probe_ok, '2099-12', ar_probe, 'PROBE-AR-0001', 'USD', 7.1, 7.9,
            1000, 7100, 7900, 800);

    -- 2.2 小写币种码必须被拒
    BEGIN
        INSERT INTO biz_fx_revaluation
            (period_code, receivable_id, ar_no, currency_code, book_rate, close_rate,
             open_amount, book_base, revalued_base, diff_base)
        VALUES ('2099-12', gen_random_uuid(), 'PROBE-AR-0002', 'usd', 7.1, 7.9,
                1, 1, 1, 0);
        RAISE EXCEPTION 'V0315 自检失败：小写币种码应被 ck_biz_fx_currency 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 2.3 汇率 0 必须被拒
    BEGIN
        INSERT INTO biz_fx_revaluation
            (period_code, receivable_id, ar_no, currency_code, book_rate, close_rate,
             open_amount, book_base, revalued_base, diff_base)
        VALUES ('2099-12', gen_random_uuid(), 'PROBE-AR-0003', 'USD', 0, 7.9,
                1, 1, 1, 0);
        RAISE EXCEPTION 'V0315 自检失败：close_rate=0 应被 ck_biz_fx_rate 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 2.4 未核销原币 0 必须被拒
    BEGIN
        INSERT INTO biz_fx_revaluation
            (period_code, receivable_id, ar_no, currency_code, book_rate, close_rate,
             open_amount, book_base, revalued_base, diff_base)
        VALUES ('2099-12', gen_random_uuid(), 'PROBE-AR-0004', 'USD', 7.1, 7.9,
                0, 0, 0, 0);
        RAISE EXCEPTION 'V0315 自检失败：open_amount=0 应被 ck_biz_fx_open 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 2.5 差额恒等被破坏必须被拒
    BEGIN
        INSERT INTO biz_fx_revaluation
            (period_code, receivable_id, ar_no, currency_code, book_rate, close_rate,
             open_amount, book_base, revalued_base, diff_base)
        VALUES ('2099-12', gen_random_uuid(), 'PROBE-AR-0005', 'USD', 7.1, 7.9,
                1000, 7100, 7900, 801);
        RAISE EXCEPTION 'V0315 自检失败：diff_base 与两列不符应被 ck_biz_fx_diff 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 2.6 幂等键：同期同应收再插必须撞唯一索引
    BEGIN
        INSERT INTO biz_fx_revaluation
            (period_code, receivable_id, ar_no, currency_code, book_rate, close_rate,
             open_amount, book_base, revalued_base, diff_base)
        VALUES ('2099-12', ar_probe, 'PROBE-AR-0001', 'USD', 7.1, 7.9,
                1000, 7100, 7900, 800);
        RAISE EXCEPTION 'V0315 自检失败：同期同应收重复行应撞 uk_biz_fx_period_ar';
    EXCEPTION WHEN unique_violation THEN NULL;
    END;

    -- 3. 探针清场
    DELETE FROM biz_fx_revaluation WHERE period_code = '2099-12';

    RAISE NOTICE 'V0315 自检通过：14 列 / 4 CHECK / 3 索引 / 5 组行为探针';
END $$;
