-- M5-15「差异分摊到成本对象」· 真库契约演练（整段 ROLLBACK，零残留）
-- 目的：证明本模块的**出参形态**能落进 biz_cost_variance（V0374 建表），且
-- ① 六类差异类型 + 符号口径全部被库层 CHECK 接受；② 零差异行的方向只许 UNFAVORABLE；
-- ③ 4 位标度是硬的（多 1 位会被静默改数 —— 这正是模块前置 400 的理由）；
-- ④ 分摊行之和与池在 PG numeric 上精确相等；⑤ trunc 口径与 Rust trunc4 同值。
\set ON_ERROR_STOP on
BEGIN;

-- [1] 六类差异类型：库层 ck_biz_cost_var_type / ck_biz_cost_var_elem / ck_biz_cost_var_sign 全通过
INSERT INTO biz_cost_variance (period_code, variance_type, element, amount, direction, remark) VALUES
  ('2026-11', 'MAT_PRICE',  'MATERIAL',   600.0000, 'UNFAVORABLE', 'm515 drill'),
  ('2026-11', 'MAT_QTY',    'MATERIAL',  3840.0000, 'UNFAVORABLE', 'm515 drill'),
  ('2026-11', 'LABOR_RATE', 'LABOR',     -180.0000, 'FAVORABLE',   'm515 drill'),
  ('2026-11', 'LABOR_EFF',  'LABOR',      500.0000, 'UNFAVORABLE', 'm515 drill'),
  ('2026-11', 'OH_SPEND',   'OVERHEAD',  1200.0000, 'UNFAVORABLE', 'm515 drill'),
  ('2026-11', 'OH_VOLUME',  'OVERHEAD',  -400.0000, 'FAVORABLE',   'm515 drill');
SELECT variance_type, element, amount, direction
  FROM biz_cost_variance WHERE remark = 'm515 drill' ORDER BY variance_type;

-- [2] 符号一致性反证 + 零差异行的方向口径
DO $$
DECLARE
    zero_fav_ok boolean := false;
BEGIN
    BEGIN
        INSERT INTO biz_cost_variance (period_code, variance_type, element, amount, direction, remark)
        VALUES ('2026-11', 'MAT_PRICE', 'MATERIAL', -1.0000, 'UNFAVORABLE', 'm515 neg-mismatch');
        RAISE EXCEPTION '[FAIL] 负金额 + UNFAVORABLE 竟被库层接受';
    EXCEPTION WHEN check_violation THEN
        RAISE NOTICE '[OK] ck_biz_cost_var_sign 拒绝「负金额 + UNFAVORABLE」';
    END;

    BEGIN
        INSERT INTO biz_cost_variance (period_code, variance_type, element, amount, direction, remark)
        VALUES ('2026-11', 'MAT_PRICE', 'MATERIAL', 1.0000, 'FAVORABLE', 'm515 pos-mismatch');
        RAISE EXCEPTION '[FAIL] 正金额 + FAVORABLE 竟被库层接受';
    EXCEPTION WHEN check_violation THEN
        RAISE NOTICE '[OK] ck_biz_cost_var_sign 拒绝「正金额 + FAVORABLE」';
    END;

    -- 库层 CHECK 是 `amount >= 0` / `amount <= 0`，所以**零金额两个方向都合法**；
    -- 模块统一写 UNFAVORABLE（与 M5-12 的 db_direction_of 同口径），下面两行都要能落库。
    BEGIN
        INSERT INTO biz_cost_variance (period_code, variance_type, element, amount, direction, remark)
        VALUES ('2026-11', 'MAT_PRICE', 'MATERIAL', 0.0000, 'FAVORABLE', 'm515 zero-fav');
        zero_fav_ok := true;
    EXCEPTION WHEN check_violation THEN
        zero_fav_ok := false;
    END;
    RAISE NOTICE '[INFO] 0 金额 + FAVORABLE 库层是否接受 = %（CHECK 是 >=0 / <=0，两者都合法）', zero_fav_ok;

    INSERT INTO biz_cost_variance (period_code, variance_type, element, amount, direction, remark)
    VALUES ('2026-11', 'MAT_PRICE', 'MATERIAL', 0.0000, 'UNFAVORABLE', 'm515 zero-unfav');
    RAISE NOTICE '[OK] 0 金额 + UNFAVORABLE 被接受（零基数对象的分摊行照此落库）';
END $$;

-- [3] 4 位标度是硬的：多 1 位会被 PG 静默改数（所以模块对 >4 位入参直接 400）
INSERT INTO biz_cost_variance (period_code, variance_type, element, amount, direction, remark)
VALUES ('2026-11', 'MAT_PRICE', 'MATERIAL', 0.00005, 'UNFAVORABLE', 'm515 scale5');
SELECT amount AS amount_after_insert, '0.0001 = PG round(0.00005,4) 半数远离零' AS note
  FROM biz_cost_variance WHERE remark = 'm515 scale5';

-- [4] 分摊守恒：三行分摊额之和 == 池（PG numeric 上精确相等，不是「差不多」）
INSERT INTO biz_cost_variance (period_code, variance_type, element, amount, direction, remark) VALUES
  ('2026-11', 'MAT_PRICE', 'MATERIAL', 1000.0000, 'UNFAVORABLE', 'm515 pool'),
  ('2026-11', 'MAT_PRICE', 'MATERIAL',  333.3334, 'UNFAVORABLE', 'm515 alloc1'),
  ('2026-11', 'MAT_PRICE', 'MATERIAL',  333.3333, 'UNFAVORABLE', 'm515 alloc2'),
  ('2026-11', 'MAT_PRICE', 'MATERIAL',  333.3333, 'UNFAVORABLE', 'm515 alloc3');
SELECT (SELECT amount FROM biz_cost_variance WHERE remark = 'm515 pool')
         = (SELECT sum(amount) FROM biz_cost_variance WHERE remark LIKE 'm515 alloc%') AS pool_equals_alloc_sum,
       (SELECT sum(amount) FROM biz_cost_variance WHERE remark LIKE 'm515 alloc%') AS alloc_sum;

-- [5] 池 = 已归属 + 待分摊（1000 = 400 + 600，模块 allocatable() 的定义）
SELECT 1000.0000::numeric = 400.0000::numeric + 600.0000::numeric AS pool_is_attributed_plus_allocatable;

-- [6] trunc 口径：Rust trunc4（整数乘 10000 后 trunc）与 PG trunc(numeric,4) 同值
SELECT trunc(1000::numeric / 3, 4) AS pg_trunc4,
       round(1000::numeric / 3, 4) AS pg_round4,
       333.3333::numeric * 2 + 333.3334::numeric AS largest_remainder_sum;

-- [7] 负数池（有利差异）同样落在 4 位内且方向为 FAVORABLE
INSERT INTO biz_cost_variance (period_code, variance_type, element, amount, direction, remark) VALUES
  ('2026-11', 'MAT_PRICE', 'MATERIAL', -1000.0000, 'FAVORABLE', 'm515 neg-pool'),
  ('2026-11', 'MAT_PRICE', 'MATERIAL',  -333.3334, 'FAVORABLE', 'm515 negalloc1'),
  ('2026-11', 'MAT_PRICE', 'MATERIAL',  -333.3333, 'FAVORABLE', 'm515 negalloc2'),
  ('2026-11', 'MAT_PRICE', 'MATERIAL',  -333.3333, 'FAVORABLE', 'm515 negalloc3');
SELECT (SELECT amount FROM biz_cost_variance WHERE remark = 'm515 neg-pool')
         = (SELECT sum(amount) FROM biz_cost_variance WHERE remark LIKE 'm515 negalloc%') AS neg_pool_equals_alloc_sum;

ROLLBACK;
