-- ======================================================
-- V0407：汇率表加「生效区间」+ 区间唯一（M1-02 · 多币种第二步）
--
-- 卡：t_438a4c0d（M1-02【M1·财务】多币种：迁移 biz_exchange_rate 加生效区间 + 唯一键）
-- 拆卡依据：docs/planning/20261113-新卡清单.md §4 M1 财务 的 M1-02 行。
--
-- ── 动手前的现状实测（决定本卡到底补什么）──────────────
--   `biz_exchange_rate` 由 V0272（SO-39）建出，11 列，**已经**有：
--     · `effective_date DATE NOT NULL`（生效日，含当日）
--     · `uk_biz_exchange_rate (from_currency, to_currency, rate_type, effective_date)`
--   所以卡面「加唯一键」的字面一半**已经在了** —— 同币对 / 同类型 / 同一生效日只能一行。
--   真正缺的是**区间**：只有起点没有终点，「这条汇率到哪天失效」表达不出来；两条起点不同的
--   汇率可以同时覆盖同一张单据日期，取数靠 `ORDER BY effective_date DESC LIMIT 1` 兜住 ——
--   只要行序/索引变了就换了一个真相，且没有任何一层能证明「这一天只该用这条」。
--
-- ── 本卡落的四件东西 ─────────────────────────────────
--   ① `effective_to DATE`（失效日，含；NULL = 沿用至下一条）—— 生效区间的右端点；
--   ② `ck_biz_exchange_rate_dates`：`effective_to IS NULL OR effective_to >= effective_date`
--      —— 反向区间（止早于起）会被库层直接拒；
--   ③ `ex_biz_exchange_rate_period`：
--      `EXCLUDE USING gist (from_currency WITH =, to_currency WITH =, rate_type WITH =,
--                           daterange(effective_date, effective_to, '[]') WITH &&)`
--      —— 同一 (from, to, rate_type) 的区间**不许重叠**。这才是卡面「按币种+日期唯一」的真正
--      含义：**任何一天，一个币对最多只有一条汇率生效**（不是「起点不重复」那么弱）。
--   ④ 存量行回填：按 (from, to, rate_type) 分区、`effective_date` 升序，
--      `effective_to = 下一行生效日 - 1`；每组最后一行保持 NULL（长期有效）。
--      ⇒ 升级后时间轴被切成互不相交、首尾相接的段，历史汇率与当前汇率各归各位。
--
-- ── 四个口径决定（写清楚，下游 M1-04/M1-05/M1-09 照这个来）──────
--   1) **`effective_to` 是派生列**，不是人工填的业务字段：它恒等于「同组下一条的生效日 - 1」，
--      最后一行为 NULL。写入口（`currency_admin_model` 的 insert / upsert / update / delete）
--      在同一事务里调 `rederive_group` 重算整组区间，故正常操作永远连续、不留缝。
--      代价：没有「这条汇率作废但保留行」的表达 —— 汇率是时间序列，要停用就补一条新的，
--      或者删掉这一条（删掉后前后两段自动接上）。这与 V0272「取 <= 目标日的最新一条」的
--      语义完全兼容：连续分区下，「最新起点」与「区间命中」是同一个答案。
--   2) 约束取 **DEFERRABLE INITIALLY DEFERRED**：写入口是「先写行、后重算整组区间」，新行落地
--      那一刻与旧邻居必然重叠，重算之后才连续；延迟到 commit 校验，正好放过中间态。
--      ⚠ 别的迁移/脚本不要在没重算的情况下依赖它的即时性；DB 侧想立刻看到冲突要
--      `SET CONSTRAINTS ALL IMMEDIATE`（本迁移的探针就是这么做的）。
--   3) **相邻不算重叠**：`[2026-01-01, 2026-01-31]` 与 `[2026-02-01, NULL]` 合法
--      （`daterange(...,'[]')` 端点相接不 `&&`）。「月末 31 号失效、下月 1 号生效」正是常态。
--   4) `btree_gist` 是 `varchar` 上 `=` 走 gist 的前提（PG13+ 为 trusted extension，库属主可建）。
--      建不出来时本迁移**退化成只有 ① ② + 应用层重算**（重叠拦不住并发窗口），并 RAISE WARNING；
--      与 V0336 的降级口径一致，不因此让整个服务起不来。
--
-- ── 为什么不动 uk_biz_exchange_rate ──────────────────
--   它的四列是 upsert 的 `ON CONFLICT` 目标（V0272 头注释 ②.5 / `rate_upsert`），改列序等于改
--   一遍写入口 + 现有汇率维护页的 409 判重文案；而「起点不重复」本身没错（同币对同类型同一天
--   两条汇率仍是脏数据）。两张网各管一件事，都留着：
--     · `uk_biz_exchange_rate` → 起点唯一（同一生效日不许两行，upsert 靠它）
--     · `ex_biz_exchange_rate_period` → 区间不重叠（任一日期只有一个真相）
--
-- ── 幂等 ──────────────────────────────────────────
--   `ADD COLUMN IF NOT EXISTS` / 约束前判 `pg_constraint` / 回填只动 `effective_to IS NULL` 的行 /
--   末尾 DO 块探针自清场（探针币种与探针汇率都删干净，残留必须为 0）。
--   重复执行 = 零结构变更 + 零数据变更 + 探针重新跑一遍通过。
--
-- ── down ─────────────────────────────────────────
--   纯派生列：重放 up 可完全重建 ⇒ **无数据护栏**（对比 V0406 拒绝回滚外币数据，那里三列是
--   业务信息不可重算）。拆的顺序反过来：先拆约束、再拆列；`btree_gist` 扩展**不删**
--   （V0336 down 同口径：别的对象可能也在用）。
-- ======================================================

-- ---------- 0) 区间排除约束需要的扩展（PG13+ 为 trusted，库属主可建） ----------
DO $$
BEGIN
    BEGIN
        CREATE EXTENSION IF NOT EXISTS btree_gist;
    EXCEPTION WHEN OTHERS THEN
        RAISE NOTICE 'V0407: btree_gist 建不出来（%）→ 区间不重叠只靠应用层 rederive（并发窗口见头注释决定 4）', SQLERRM;
    END;
END $$;

-- ---------- 1) 生效区间右端点 ----------
ALTER TABLE biz_exchange_rate ADD COLUMN IF NOT EXISTS effective_to DATE;

COMMENT ON COLUMN biz_exchange_rate.effective_to IS
    '失效日（含）；NULL = 沿用至下一条（本组最后一行）。派生列：恒等于「同组下一条 effective_date - 1」，由写入口 rederive_group 维护，不人工填';

-- ---------- 2) 存量回填：把时间轴切成首尾相接的段 ----------
-- 每组（币对 + 汇率类型）按生效日升序，每一行的失效日 = 下一行的生效日 - 1；最后一行留 NULL。
-- 只动 effective_to IS NULL 的行 ⇒ 重放不覆盖任何已派生的值（幂等）。
WITH derived AS (
    SELECT id,
           (lead(effective_date) OVER (
                PARTITION BY from_currency, to_currency, rate_type
                ORDER BY effective_date)) - 1 AS derived_to
      FROM biz_exchange_rate
)
UPDATE biz_exchange_rate r
   SET effective_to = d.derived_to
  FROM derived d
 WHERE r.id = d.id
   AND d.derived_to IS NOT NULL
   AND r.effective_to IS NULL;

-- ---------- 3) 反向区间护栏（止早于起一律拒） ----------
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = 'public.biz_exchange_rate'::regclass
           AND conname  = 'ck_biz_exchange_rate_dates'
    ) THEN
        ALTER TABLE biz_exchange_rate
            ADD CONSTRAINT ck_biz_exchange_rate_dates
            CHECK (effective_to IS NULL OR effective_to >= effective_date);
    END IF;
END $$;

-- ---------- 4) 库级防重叠：任一日期一个币对只有一个真相 ----------
DO $$
DECLARE
    v_has_gist boolean;
BEGIN
    SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'btree_gist') INTO v_has_gist;
    IF NOT v_has_gist THEN
        RAISE WARNING 'V0407: btree_gist 不可用 ⇒ 未建 ex_biz_exchange_rate_period，区间重叠只由应用层 rederive 维持（存在毫秒级并发窗口）';
    ELSIF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = 'public.biz_exchange_rate'::regclass
           AND conname  = 'ex_biz_exchange_rate_period'
    ) THEN
        ALTER TABLE biz_exchange_rate
            ADD CONSTRAINT ex_biz_exchange_rate_period
            EXCLUDE USING gist (
                from_currency WITH =,
                to_currency   WITH =,
                rate_type     WITH =,
                daterange(effective_date, effective_to, '[]') WITH &&
            ) DEFERRABLE INITIALLY DEFERRED;
        RAISE NOTICE 'V0407: 已建 ex_biz_exchange_rate_period（同币对同类型区间不许重叠）';
    END IF;
END $$;

-- ---------- 5) 迁移自检（结构 + 回填 + 行为探针，可反复执行） ----------
DO $$
DECLARE
    n           integer;
    txt         text;
    v_has_ex    boolean;
    d_start     date;
BEGIN
    -- ===== 结构 1：effective_to 必须存在、DATE、可空 =====
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_exchange_rate'
       AND column_name = 'effective_to' AND data_type = 'date' AND is_nullable = 'YES';
    IF n <> 1 THEN
        RAISE EXCEPTION 'M1-02: biz_exchange_rate.effective_to 不是可空 DATE（命中 % 列）', n;
    END IF;

    -- ===== 结构 2：反向区间 CHECK 在位且定义正确 =====
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_exchange_rate'::regclass
       AND conname  = 'ck_biz_exchange_rate_dates' AND contype = 'c';
    IF n <> 1 THEN
        RAISE EXCEPTION 'M1-02: 缺 ck_biz_exchange_rate_dates';
    END IF;
    SELECT pg_get_constraintdef(oid) INTO txt FROM pg_constraint
     WHERE conrelid = 'public.biz_exchange_rate'::regclass AND conname = 'ck_biz_exchange_rate_dates';
    IF txt NOT LIKE '%effective_to%' OR txt NOT LIKE '%effective_date%' THEN
        RAISE EXCEPTION 'M1-02: ck_biz_exchange_rate_dates 定义不对：%', txt;
    END IF;

    -- ===== 结构 3：回归护栏 —— V0272 的起点唯一索引必须还在（upsert 的 ON CONFLICT 目标）=====
    IF to_regclass('public.uk_biz_exchange_rate') IS NULL THEN
        RAISE EXCEPTION 'M1-02: uk_biz_exchange_rate 丢了（V0272 的 upsert 目标，不许拆）';
    END IF;
    SELECT indexdef INTO txt FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_exchange_rate';
    IF txt NOT LIKE 'CREATE UNIQUE INDEX%'
       OR txt NOT LIKE '%from_currency%' OR txt NOT LIKE '%to_currency%'
       OR txt NOT LIKE '%rate_type%'   OR txt NOT LIKE '%effective_date%' THEN
        RAISE EXCEPTION 'M1-02: uk_biz_exchange_rate 定义被改了：%', txt;
    END IF;

    -- ===== 结构 4：区间排除约束（btree_gist 可用时必须在位）=====
    SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'btree_gist') INTO v_has_ex;
    IF v_has_ex THEN
        SELECT count(*) INTO n FROM pg_constraint
         WHERE conrelid = 'public.biz_exchange_rate'::regclass
           AND conname  = 'ex_biz_exchange_rate_period' AND contype = 'x';
        IF n <> 1 THEN
            RAISE EXCEPTION 'M1-02: btree_gist 在，但缺 ex_biz_exchange_rate_period 排除约束';
        END IF;
        SELECT pg_get_constraintdef(oid) INTO txt FROM pg_constraint
         WHERE conrelid = 'public.biz_exchange_rate'::regclass
           AND conname = 'ex_biz_exchange_rate_period';
        IF txt NOT LIKE '%daterange%' OR txt NOT LIKE '%&%' THEN
            RAISE EXCEPTION 'M1-02: ex_biz_exchange_rate_period 定义不含 daterange &&：%', txt;
        END IF;
    ELSE
        RAISE WARNING 'M1-02: btree_gist 缺失 ⇒ 跳过排除约束断言（重叠仅靠应用层）';
    END IF;

    -- ===== 回填 1：全表任意两行不许区间重叠（升级后立刻成立）=====
    SELECT count(*) INTO n
      FROM biz_exchange_rate a
      JOIN biz_exchange_rate b
        ON a.id <> b.id
       AND a.from_currency = b.from_currency
       AND a.to_currency   = b.to_currency
       AND a.rate_type     = b.rate_type
       AND daterange(a.effective_date, a.effective_to, '[]')
        && daterange(b.effective_date, b.effective_to, '[]');
    IF n <> 0 THEN
        RAISE EXCEPTION 'M1-02: 回填后仍有 % 对区间重叠', n / 2;
    END IF;

    -- ===== 回填 2：非末日行必须都有关闭日（否则时间轴有洞/有悬空开放区间）=====
    SELECT count(*) INTO n FROM (
        SELECT id, effective_to, effective_date,
               max(effective_date) OVER (
                   PARTITION BY from_currency, to_currency, rate_type) AS group_max
          FROM biz_exchange_rate
    ) t
     WHERE t.effective_date < t.group_max AND t.effective_to IS NULL;
    IF n <> 0 THEN
        RAISE EXCEPTION 'M1-02: 回填不完整 —— % 行不是本组最后一行却仍 effective_to IS NULL', n;
    END IF;

    -- ===== 行为探针：用专用币种（ZZM → ZZN），不与任何真实汇率撞键 =====
    BEGIN
        INSERT INTO biz_currency (code, name, symbol, decimal_places, is_base, status, remark)
        VALUES ('ZZM', '__M102_PROBE_FROM__', 'Z', 2, false, 'ACTIVE', '__M102_PROBE__'),
               ('ZZN', '__M102_PROBE_TO__',   'Z', 2, false, 'ACTIVE', '__M102_PROBE__')
        ON CONFLICT (code) DO NOTHING;

        -- 基准行：[2025-01-01, NULL) 开放区间
        INSERT INTO biz_exchange_rate
            (from_currency, to_currency, rate, rate_type, effective_date, remark)
        VALUES ('ZZM', 'ZZN', 7.80000000, 'SPOT', DATE '2025-01-01', '__M102_PROBE__');

        IF v_has_ex THEN
            -- 约束默认延迟到 commit；探针要当场看到冲突 ⇒ 切成立即校验
            EXECUTE 'SET CONSTRAINTS ex_biz_exchange_rate_period IMMEDIATE';

            -- P1：新起点落在开放区间内部 ⇒ 必须被拒
            BEGIN
                INSERT INTO biz_exchange_rate
                    (from_currency, to_currency, rate, rate_type, effective_date, remark)
                VALUES ('ZZM', 'ZZN', 7.90000000, 'SPOT', DATE '2025-06-01', '__M102_PROBE__');
                RAISE EXCEPTION 'M1-02: P1 —— 起点插进开放区间竟然成功（排除约束没生效）';
            EXCEPTION WHEN exclusion_violation THEN
                NULL;   -- 预期路径
            END;

            -- P2：整段新区间夹在开放区间里 ⇒ 同样必须被拒
            BEGIN
                INSERT INTO biz_exchange_rate
                    (from_currency, to_currency, rate, rate_type, effective_date, effective_to, remark)
                VALUES ('ZZM', 'ZZN', 7.95000000, 'SPOT', DATE '2025-03-01', DATE '2025-04-30', '__M102_PROBE__');
                RAISE EXCEPTION 'M1-02: P2 —— 完全嵌进开放区间的区间竟然成功';
            EXCEPTION WHEN exclusion_violation THEN
                NULL;
            END;

            -- P3：把基准区间收口到 2025-05-31 后，2025-06-01 起的新区间必须放行（相邻不算重叠）
            UPDATE biz_exchange_rate SET effective_to = DATE '2025-05-31'
             WHERE remark = '__M102_PROBE__' AND from_currency = 'ZZM'
               AND effective_date = DATE '2025-01-01';
            INSERT INTO biz_exchange_rate
                (from_currency, to_currency, rate, rate_type, effective_date, remark)
            VALUES ('ZZM', 'ZZN', 7.90000000, 'SPOT', DATE '2025-06-01', '__M102_PROBE__');

            -- P4：再往已收口的 [01-01, 05-31] 里塞一段 ⇒ 必须被拒（闭合区间同样受管）
            BEGIN
                INSERT INTO biz_exchange_rate
                    (from_currency, to_currency, rate, rate_type, effective_date, effective_to, remark)
                VALUES ('ZZM', 'ZZN', 8.00000000, 'SPOT', DATE '2025-05-01', DATE '2025-05-15', '__M102_PROBE__');
                RAISE EXCEPTION 'M1-02: P4 —— 重叠已闭合区间竟然成功';
            EXCEPTION WHEN exclusion_violation THEN
                NULL;
            END;

            -- 交还延迟模式（后续迁移若插入相邻汇率，仍是「先写后重算」的节奏）
            EXECUTE 'SET CONSTRAINTS ALL DEFERRED';
        ELSE
            RAISE WARNING 'M1-02: 无排除约束 ⇒ 跳过 P1/P2/P4 区间重叠探针';
            UPDATE biz_exchange_rate SET effective_to = DATE '2025-05-31'
             WHERE remark = '__M102_PROBE__' AND from_currency = 'ZZM'
               AND effective_date = DATE '2025-01-01';
            INSERT INTO biz_exchange_rate
                (from_currency, to_currency, rate, rate_type, effective_date, remark)
            VALUES ('ZZM', 'ZZN', 7.90000000, 'SPOT', DATE '2025-06-01', '__M102_PROBE__');
        END IF;

        -- P5：止早于起 ⇒ check_violation
        BEGIN
            INSERT INTO biz_exchange_rate
                (from_currency, to_currency, rate, rate_type, effective_date, effective_to, remark)
            VALUES ('ZZM', 'ZZN', 8.10000000, 'SPOT', DATE '2025-08-01', DATE '2025-07-01', '__M102_PROBE__');
            RAISE EXCEPTION 'M1-02: P5 —— effective_to < effective_date 竟然成功';
        EXCEPTION WHEN check_violation THEN
            NULL;
        END;

        -- P6：同起点重复 ⇒ unique_violation（V0272 的起点唯一仍在，且它是 upsert 的前提）
        BEGIN
            INSERT INTO biz_exchange_rate
                (from_currency, to_currency, rate, rate_type, effective_date, remark)
            VALUES ('ZZM', 'ZZN', 8.20000000, 'SPOT', DATE '2025-01-01', '__M102_PROBE__');
            RAISE EXCEPTION 'M1-02: P6 —— 同起点重复插入竟然成功（uk 丢了）';
        EXCEPTION WHEN unique_violation THEN
            NULL;
        END;

        -- P7：区间命中 —— 2025-03-15 只该命中基准行（7.80），2025-12-31 只该命中后一行（7.90）
        SELECT count(*) INTO n FROM biz_exchange_rate
         WHERE from_currency = 'ZZM' AND to_currency = 'ZZN' AND rate_type = 'SPOT'
           AND effective_date <= DATE '2025-03-15'
           AND (effective_to IS NULL OR effective_to >= DATE '2025-03-15');
        IF n <> 1 THEN
            RAISE EXCEPTION 'M1-02: P7 —— 2025-03-15 命中 % 条（期望 1）', n;
        END IF;
        SELECT rate::text INTO txt FROM biz_exchange_rate
         WHERE from_currency = 'ZZM' AND to_currency = 'ZZN' AND rate_type = 'SPOT'
           AND effective_date <= DATE '2025-03-15'
           AND (effective_to IS NULL OR effective_to >= DATE '2025-03-15');
        IF txt <> '7.80000000' THEN
            RAISE EXCEPTION 'M1-02: P7 —— 2025-03-15 取到 %（期望基准行的 7.80000000）', txt;
        END IF;
        SELECT rate::text INTO txt FROM biz_exchange_rate
         WHERE from_currency = 'ZZM' AND to_currency = 'ZZN' AND rate_type = 'SPOT'
           AND effective_date <= DATE '2025-12-31'
           AND (effective_to IS NULL OR effective_to >= DATE '2025-12-31');
        IF txt <> '7.90000000' THEN
            RAISE EXCEPTION 'M1-02: P7 —— 2025-12-31 取到 %（期望后一行的 7.90000000；说明区间没接上）', txt;
        END IF;

        -- P8：派生规则自检 —— 再把基准行放开，rederive 的等价 UPDATE 必须得到「下一行 - 1」
        --     （这里直接验证 lead 的算法与本迁移回填一致，防止回填 SQL 与写入口口径分叉）
        SELECT (lead(effective_date) OVER (ORDER BY effective_date)) - 1 INTO d_start
          FROM biz_exchange_rate
         WHERE from_currency = 'ZZM' AND to_currency = 'ZZN' AND rate_type = 'SPOT'
         ORDER BY effective_date LIMIT 1;
        IF d_start IS DISTINCT FROM DATE '2025-05-31' THEN
            RAISE EXCEPTION 'M1-02: P8 —— 派生口径算得 %（期望 2025-05-31，即 2025-06-01 前一天）', d_start;
        END IF;

        -- ===== 探针清场：先删汇率、再删币种（FK RESTRICT，顺序不能反）=====
        DELETE FROM biz_exchange_rate WHERE remark = '__M102_PROBE__';
        DELETE FROM biz_currency WHERE code IN ('ZZM', 'ZZN') OR remark = '__M102_PROBE__';
        SELECT count(*) INTO n FROM biz_exchange_rate WHERE remark = '__M102_PROBE__';
        IF n <> 0 THEN
            RAISE EXCEPTION 'M1-02: 汇率探针清场残留 % 行', n;
        END IF;
        SELECT count(*) INTO n FROM biz_currency WHERE code IN ('ZZM', 'ZZN');
        IF n <> 0 THEN
            RAISE EXCEPTION 'M1-02: 币种探针清场残留 % 行', n;
        END IF;
    END;

    RAISE NOTICE 'M1-02 自检通过：effective_to 区间 + 反向护栏 + 区间排除约束 + 存量回填连续 + 7 组行为探针';
END $$;
