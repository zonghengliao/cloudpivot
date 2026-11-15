-- ============================================================================
-- V0221__erp_seed_accounting_period_year.up.sql
--
-- 【P0-FIX-5】新库不自动开账：把**当年 12 个会计期间**建齐（幂等）。
--
-- # 病（评审实测，不是推测）
--
-- 空库把迁移跑完 163 条之后，`biz_accounting_period` 只有 **1 行**（V0137 §5.4 的
-- `2026-10`），而 `voucher::period_of` 对「日期区间里查不到期间」是 **fail-open** 的
-- （退化成 `yyyy-MM` 字符串，见 `voucher.rs` 的 `period_of`）。后果：
--
--   · 首笔业务（今天 2026-11）落地的凭证 `period = '2026-11'`，但期间表里没有 2026-11；
--   · 巡检 `scripts/verify/erp_invariants.py` 的 F2「凭证期间必有期间记录」立刻 WARN
--     —— 隔离库 cloudpivot_audit 实测：`39/40 通过 · WARN 1 · 5 张凭证的期间无记录`；
--   · 部署新环境必须先有人在「会计期间」页手工开账，否则账做出来对不上期间。
--
-- # 做什么
--
-- 按 `CURRENT_DATE` 所属年度补齐 1–12 月各一行，区间取自然月、状态一律 `OPEN`。
--
-- # 状态为什么是 `OPEN`，不是 `NOT_OPENED`（也没有 FUTURE）
--
-- 1. 库层 `ck_biz_period_status` 只有 `NOT_OPENED | OPEN | CLOSED` 三值
--    （V0137:196），本仓**没有 FUTURE** 这个概念，别自创第四种状态；
-- 2. 既有语义里 `NOT_OPENED` =「行在、账没开」，把 `NOT_OPENED` 变 `OPEN` 的唯一入口是
--    单期开账 `POST /business/erp/period`（`open_period`）；`batch_open`（批量开账）
--    对**已存在的行一律 skipped**（period.rs `batch_open` 的 `Some(row) => skipped`），
--    不会顺手把它打开。种子若写 `NOT_OPENED`，用户点「批量开账」会看到 12 条 skipped、
--    一期都没开成 —— 比「没有行」更难收拾；
-- 3. `OPEN` 是唯一「建出来就能用」的状态：写凭证只拦 `CLOSED`（`voucher::post_voucher`），
--    录期初要求 `OPEN`（`opening::lock_period`）；
-- 4. 与 V0137 §5.4 的既有种子同形（那一期就是 `OPEN`）。
--
-- # 幂等（up / down / up 都能重复跑）
--
-- · up 只看 `(period_year, period_month)` 唯一索引 `uk_biz_period_ym`：已存在的行
--   **一律不动**（不覆盖状态、不改日期区间），V0137 那条字面 id 的 2026-10 也保持原样；
-- · 新建行的 id 由 `md5('biz_accounting_period:<year>-<mm>')::uuid` 确定性生成，
--   down 才能**只删自己种下的行**：V0137 的字面 id 行、别人手工建的行、被业务改过的行
--   都不会被误删。
--
-- # down 的边界（回滚不是无损的，写清楚）
--
-- down 只删「本迁移种的、且还没被业务用起来」的行：`status <> 'CLOSED'` 且该期间
-- **没有任何未删除的凭证**。被结账过、或已经有凭证落进去的期间一律保留 ——
-- 删掉它们等于把 F2 的坑重新挖开（凭证期间无记录），回滚不该制造新的账务问题。
-- ============================================================================

DO $$
DECLARE
    cur_year  smallint := EXTRACT(YEAR  FROM CURRENT_DATE)::smallint;
    cur_month smallint := EXTRACT(MONTH FROM CURRENT_DATE)::smallint;
    m         smallint;
    created   int := 0;
    kept      int := 0;
BEGIN
    FOR m IN 1..12 LOOP
        INSERT INTO biz_accounting_period
            (id, period_year, period_month, period_code, start_date, end_date, status)
        VALUES (
            md5('biz_accounting_period:' || cur_year::text || '-' || lpad(m::text, 2, '0'))::uuid,
            cur_year,
            m,
            cur_year::text || '-' || lpad(m::text, 2, '0'),
            make_date(cur_year, m, 1),
            (make_date(cur_year, m, 1) + INTERVAL '1 month' - INTERVAL '1 day')::date,
            'OPEN'
        )
        ON CONFLICT (period_year, period_month) DO NOTHING;

        IF FOUND THEN
            created := created + 1;
        ELSE
            kept := kept + 1;
        END IF;
    END LOOP;

    RAISE NOTICE 'V0221：当年 % 年会计期间 —— 新建 % 期，已存在 % 期（共 12 期）',
        cur_year, created, kept;
    -- 当年月份边界不该被误当普通年份：显式说明一次，便于排障
    RAISE NOTICE 'V0221：按 CURRENT_DATE = % 判定，当月 = %',
        CURRENT_DATE, cur_month;
END $$;

-- 自检：当年 12 个月**每一个**都要有行、编码与自然月区间一致。
-- 只对「本迁移负责的那件事」下硬约束，避免在生产库上把别人的历史口径判红
-- （若某期区间被人为拉长/缩短，只 NOTICE 提示，不拦迁移）。
DO $$
DECLARE
    cur_year smallint := EXTRACT(YEAR FROM CURRENT_DATE)::smallint;
    total    int;
    missing  text;
    drifted  int;
BEGIN
    SELECT count(*) INTO total
      FROM biz_accounting_period
     WHERE period_year = cur_year;
    IF total <> 12 THEN
        RAISE EXCEPTION 'V0221 自检失败：当年 % 的期间行 % 条（应为 12 条）', cur_year, total;
    END IF;

    SELECT string_agg(to_char(make_date(cur_year, m, 1), 'YYYY-MM'), '、' ORDER BY m)
      INTO missing
      FROM generate_series(1, 12) AS m
     WHERE NOT EXISTS (
           SELECT 1 FROM biz_accounting_period p
            WHERE p.period_year  = cur_year
              AND p.period_month = m
              AND p.period_code  = cur_year::text || '-' || lpad(m::text, 2, '0'));
    IF missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0221 自检失败：当年 % 缺期间 / 编码不符：%', cur_year, missing;
    END IF;

    SELECT count(*) INTO drifted
      FROM biz_accounting_period p
     WHERE p.period_year = cur_year
       AND (p.start_date <> make_date(cur_year, p.period_month, 1)
            OR p.end_date <> (make_date(cur_year, p.period_month, 1)
                              + INTERVAL '1 month' - INTERVAL '1 day')::date);
    IF drifted <> 0 THEN
        RAISE NOTICE 'V0221 提示：当年 % 有 % 期的日期区间不是自然月（保留原值，未改动）',
            cur_year, drifted;
    END IF;

    RAISE NOTICE 'V0221 自检通过：当年 % 的 12 期齐备', cur_year;
END $$;
