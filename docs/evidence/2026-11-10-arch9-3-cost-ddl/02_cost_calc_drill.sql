-- =============================================================================
-- ARCH9-3 成本核算 —— 算例真库演算（约当产量法 / 分摊守恒 / 差异恒等式）
--
-- 目的：设计文档 §4.7（约当产量与完工结转）与 §4.9（差异公式）里的每一个数字
--       都由真实 PG 算出来，不是手算填表。文档引用本脚本输出的数字。
--
-- 说明：本脚本用 tmp_drill_* 最小表（列与正式 DDL 同构，去掉与算例无关的列），
--       这样演算不依赖 01 脚本的表是否已建（01 是整段 ROLLBACK 的）。
--       tmp 表同样整段 ROLLBACK，零残留。
-- =============================================================================
\set ON_ERROR_STOP on
BEGIN;

CREATE TABLE tmp_drill_object (
    id            UUID PRIMARY KEY,
    object_no     VARCHAR(32)   NOT NULL,
    completed_qty NUMERIC(18,4) NOT NULL,
    wip_qty       NUMERIC(18,4) NOT NULL,
    completion_pct NUMERIC(5,2) NOT NULL,
    material_input_mode VARCHAR(16) NOT NULL
);
CREATE TABLE tmp_drill_collect (
    id             BIGSERIAL PRIMARY KEY,
    cost_object_id UUID          NOT NULL,
    element        VARCHAR(12)   NOT NULL,
    amount         NUMERIC(18,4) NOT NULL
);
CREATE TABLE tmp_drill_alloc (
    id             BIGSERIAL PRIMARY KEY,
    rule_code      VARCHAR(32)   NOT NULL,
    cost_object_id UUID          NOT NULL,
    basis_value    NUMERIC(18,4) NOT NULL,
    basis_total    NUMERIC(18,4) NOT NULL,
    pool_amount    NUMERIC(18,4) NOT NULL,
    alloc_amount   NUMERIC(18,4) NOT NULL,
    rounding_adj   NUMERIC(18,4) NOT NULL DEFAULT 0
);
CREATE TABLE tmp_drill_item (
    id               BIGSERIAL PRIMARY KEY,
    cost_object_id   UUID NOT NULL,
    wip_beg_qty      NUMERIC(18,4) NOT NULL DEFAULT 0,
    wip_beg_cost     NUMERIC(18,4) NOT NULL DEFAULT 0,
    completed_qty    NUMERIC(18,4) NOT NULL DEFAULT 0,
    wip_end_qty      NUMERIC(18,4) NOT NULL DEFAULT 0,
    completion_pct   NUMERIC(5,2)  NOT NULL DEFAULT 0,
    equiv_material   NUMERIC(18,4) NOT NULL DEFAULT 0,
    equiv_conversion NUMERIC(18,4) NOT NULL DEFAULT 0,
    mat_cur          NUMERIC(18,4) NOT NULL DEFAULT 0,
    labor_cur        NUMERIC(18,4) NOT NULL DEFAULT 0,
    oh_cur           NUMERIC(18,4) NOT NULL DEFAULT 0,
    unit_material    NUMERIC(18,6) NOT NULL DEFAULT 0,
    unit_labor       NUMERIC(18,6) NOT NULL DEFAULT 0,
    unit_overhead    NUMERIC(18,6) NOT NULL DEFAULT 0,
    finished_cost    NUMERIC(18,4) NOT NULL DEFAULT 0,
    wip_end_cost     NUMERIC(18,4) NOT NULL DEFAULT 0,
    unit_finished_cost NUMERIC(18,6) NOT NULL DEFAULT 0
);

-- =============================================================================
-- 算例 A：单成本对象，约当产量法（材料一次投入 / 加工按进度）
--
-- 业务：CB-20261110-000001 生产 P-100
--   本期投料：A 1200 件 × 12.50 = 15000.00
--             B  500 件 ×  8.00 =  4000.00
--   本期工时：80 小时 × 50.00 元/小时 = 4000.00
--   制造费用：经分摊得 4000.00（见算例 B）
--   完工 800 件；期末在制 200 件，完工进度 50%；材料开工一次投入
-- =============================================================================
INSERT INTO tmp_drill_object VALUES
  ('aaaaaaaa-0000-4000-8000-000000000001', 'CB-20261110-000001', 800, 200, 50.00, 'ONE_TIME');

INSERT INTO tmp_drill_collect (cost_object_id, element, amount) VALUES
  ('aaaaaaaa-0000-4000-8000-000000000001', 'MATERIAL', 1200 * 12.50),
  ('aaaaaaaa-0000-4000-8000-000000000001', 'MATERIAL',  500 *  8.00),
  ('aaaaaaaa-0000-4000-8000-000000000001', 'LABOR',      80 * 50.00),
  ('aaaaaaaa-0000-4000-8000-000000000001', 'OVERHEAD', 4000.00);

-- ---- 成本计算（设计 §4.7 的公式逐条落 SQL）----
INSERT INTO tmp_drill_item (
    cost_object_id, completed_qty, wip_end_qty, completion_pct,
    equiv_material, equiv_conversion,
    mat_cur, labor_cur, oh_cur,
    unit_material, unit_labor, unit_overhead,
    finished_cost, wip_end_cost, unit_finished_cost)
SELECT
    o.id,
    o.completed_qty,
    o.wip_qty,
    o.completion_pct,
    -- 材料约当：一次投料 ⇒ 期初在制（此处 0）+ 期末在制按 100%
    o.completed_qty + o.wip_qty,
    -- 加工约当：完工 + 期末在制 × 完工进度
    o.completed_qty + o.wip_qty * o.completion_pct / 100,
    c.mat, c.lab, c.oh,
    -- 单位成本（6 位）：期初在制为 0 时 = 本期投入 / 约当产量
    round(c.mat / (o.completed_qty + o.wip_qty), 6),
    round(c.lab / (o.completed_qty + o.wip_qty * o.completion_pct / 100), 6),
    round(c.oh  / (o.completed_qty + o.wip_qty * o.completion_pct / 100), 6),
    -- 完工转出 = 完工数量 × 各要素单位成本（4 位）
    round(o.completed_qty * round(c.mat / (o.completed_qty + o.wip_qty), 6), 4)
  + round(o.completed_qty * round(c.lab / (o.completed_qty + o.wip_qty * o.completion_pct / 100), 6), 4)
  + round(o.completed_qty * round(c.oh  / (o.completed_qty + o.wip_qty * o.completion_pct / 100), 6), 4),
    -- 期末在制 = 投入 - 完工转出（差额倒挤，保证守恒；不另算一遍免得 4 位尾差打架）
    (c.mat + c.lab + c.oh)
  - (round(o.completed_qty * round(c.mat / (o.completed_qty + o.wip_qty), 6), 4)
   + round(o.completed_qty * round(c.lab / (o.completed_qty + o.wip_qty * o.completion_pct / 100), 6), 4)
   + round(o.completed_qty * round(c.oh  / (o.completed_qty + o.wip_qty * o.completion_pct / 100), 6), 4)),
    -- 完工单位成本 = 完工转出 / 完工数量（6 位，写 stock_in.cost_price）
    round(((round(o.completed_qty * round(c.mat / (o.completed_qty + o.wip_qty), 6), 4)
          + round(o.completed_qty * round(c.lab / (o.completed_qty + o.wip_qty * o.completion_pct / 100), 6), 4)
          + round(o.completed_qty * round(c.oh  / (o.completed_qty + o.wip_qty * o.completion_pct / 100), 6), 4))
         / o.completed_qty), 6)
  FROM tmp_drill_object o
  JOIN (SELECT cost_object_id,
               sum(amount) FILTER (WHERE element = 'MATERIAL') AS mat,
               sum(amount) FILTER (WHERE element = 'LABOR')    AS lab,
               sum(amount) FILTER (WHERE element = 'OVERHEAD') AS oh
          FROM tmp_drill_collect GROUP BY cost_object_id) c ON c.cost_object_id = o.id;

\echo '--- 算例 A：成本计算单 ---'
\x on
SELECT cost_object_id, completed_qty, wip_end_qty, completion_pct,
       equiv_material, equiv_conversion,
       mat_cur, labor_cur, oh_cur,
       unit_material, unit_labor, unit_overhead,
       finished_cost, wip_end_cost, unit_finished_cost
  FROM tmp_drill_item;
\x off

-- 断言：数字必须与公式一致（写死期望值 = 把「文档里的数字」钉死在这里）
DO $$
DECLARE r tmp_drill_item%ROWTYPE;
BEGIN
    SELECT * INTO r FROM tmp_drill_item LIMIT 1;

    IF r.equiv_material <> 1000 OR r.equiv_conversion <> 900 THEN
        RAISE EXCEPTION '约当产量错：材料 % / 加工 %（应 1000 / 900）', r.equiv_material, r.equiv_conversion;
    END IF;
    IF r.mat_cur <> 19000 OR r.labor_cur <> 4000 OR r.oh_cur <> 4000 THEN
        RAISE EXCEPTION '本期投入错：%/%/%（应 19000/4000/4000）', r.mat_cur, r.labor_cur, r.oh_cur;
    END IF;
    IF r.unit_material <> 19.000000
       OR r.unit_labor <> 4.444444 OR r.unit_overhead <> 4.444444 THEN
        RAISE EXCEPTION '单位成本错：%/%/%（应 19.000000/4.444444/4.444444）',
            r.unit_material, r.unit_labor, r.unit_overhead;
    END IF;
    IF r.finished_cost <> 22311.1104 THEN
        RAISE EXCEPTION '完工转出错：%（应 22311.1104）', r.finished_cost;
    END IF;
    IF r.wip_end_cost <> 4688.8896 THEN
        RAISE EXCEPTION '期末在制错：%（应 4688.8896）', r.wip_end_cost;
    END IF;
    IF r.unit_finished_cost <> 27.888888 THEN
        RAISE EXCEPTION '完工单位成本错：%（应 27.888888）', r.unit_finished_cost;
    END IF;
    -- 守恒（正式 DDL 里是 ck_biz_cost_item_cons 库层约束）
    IF r.finished_cost + r.wip_end_cost <> r.wip_beg_cost + r.mat_cur + r.labor_cur + r.oh_cur THEN
        RAISE EXCEPTION '成本不守恒：完工 % + 在制 % <> 期初 % + 投入 %',
            r.finished_cost, r.wip_end_cost, r.wip_beg_cost, r.mat_cur + r.labor_cur + r.oh_cur;
    END IF;
    RAISE NOTICE '✅ 算例 A 通过：完工 800 件 22311.1104（单位 27.888888）/ 在制 200 件 4688.8896 / 合计 27000.0000 守恒';
END $$;

-- =============================================================================
-- 算例 B：多成本对象分摊（按工时分摊制造费用）+ 尾差守恒
--
-- 车间 D-WS1 本期制造费用 4000.00（4101.01 折旧 3000 + 4101.02 水电 1000）
-- 三个成本对象本期人工工时：CB1 60h / CB2 20h / CB3 20h（合计 100h）
-- 分摊 = 4000 × 60/100 = 2400 / 800 / 800（恰好整除，无尾差）
-- 另设一个「除不尽」场景：pool 1000.00，三对象基数各 1 ⇒ 333.3333×3 = 999.9999，
--   尾差 0.0001 归「余数最大者」，保证 Σ分摊 = pool（守恒）
-- =============================================================================

-- 场景 B1：正常分摊
INSERT INTO tmp_drill_alloc (rule_code, cost_object_id, basis_value, basis_total, pool_amount, alloc_amount)
SELECT 'RULE-WS1-LABOR', v.obj, v.basis, sum(v.basis) OVER (), 4000.00,
       round(4000.00 * v.basis / sum(v.basis) OVER (), 4)
  FROM (VALUES
        ('aaaaaaaa-0000-4000-8000-000000000001'::uuid, 60::numeric),
        ('aaaaaaaa-0000-4000-8000-000000000002'::uuid, 20::numeric),
        ('aaaaaaaa-0000-4000-8000-000000000003'::uuid, 20::numeric)
       ) AS v(obj, basis);

-- 场景 B2：除不尽的尾差（最大余数法）
WITH base AS (
    SELECT v.obj, v.basis,
           sum(v.basis) OVER () AS total,
           1000.00::numeric     AS pool
      FROM (VALUES
            ('bbbbbbbb-0000-4000-8000-00000000000a'::uuid, 1::numeric),
            ('bbbbbbbb-0000-4000-8000-00000000000b'::uuid, 1::numeric),
            ('bbbbbbbb-0000-4000-8000-00000000000c'::uuid, 1::numeric)
           ) AS v(obj, basis)
), calc AS (
    SELECT obj, basis, total, pool,
           trunc(pool * basis / total, 4)                      AS alloc,
           (pool * basis / total) - trunc(pool * basis / total, 4) AS rem
      FROM base
), adjusted AS (
    SELECT obj, basis, total, pool, alloc,
           pool - sum(alloc) OVER ()                          AS shortfall,
           row_number() OVER (ORDER BY rem DESC, basis DESC, obj) AS rn
      FROM calc
)
INSERT INTO tmp_drill_alloc (rule_code, cost_object_id, basis_value, basis_total, pool_amount, alloc_amount, rounding_adj)
SELECT 'RULE-WS1-DIV3', obj, basis, total, pool,
       alloc + CASE WHEN rn = 1 THEN shortfall ELSE 0 END,
       CASE WHEN rn = 1 THEN shortfall ELSE 0 END
  FROM adjusted;

\echo '--- 算例 B：分摊结果（两场景）---'
SELECT rule_code, cost_object_id, basis_value, basis_total, pool_amount, alloc_amount, rounding_adj
  FROM tmp_drill_alloc ORDER BY rule_code, cost_object_id;

DO $$
DECLARE
    n_b1      INT;
    s_b1      NUMERIC;
    n_b2      INT;
    s_b2      NUMERIC;
BEGIN
    SELECT count(*), sum(alloc_amount) INTO n_b1, s_b1
      FROM tmp_drill_alloc WHERE rule_code = 'RULE-WS1-LABOR';
    IF n_b1 <> 3 OR s_b1 <> 4000.00 THEN
        RAISE EXCEPTION '场景 B1 分摊不守恒：行数 % 合计 %（应 3 / 4000.00）', n_b1, s_b1;
    END IF;

    SELECT count(*), sum(alloc_amount) INTO n_b2, s_b2
      FROM tmp_drill_alloc WHERE rule_code = 'RULE-WS1-DIV3';
    IF n_b2 <> 3 OR s_b2 <> 1000.00 THEN
        RAISE EXCEPTION '场景 B2 尾差未收口：行数 % 合计 %（应 3 / 1000.00）', n_b2, s_b2;
    END IF;

    IF (SELECT count(*) FROM tmp_drill_alloc
         WHERE rule_code = 'RULE-WS1-DIV3' AND rounding_adj <> 0) <> 1 THEN
        RAISE EXCEPTION '场景 B2 应只有 1 行承担尾差';
    END IF;

    RAISE NOTICE '✅ 算例 B 通过：B1 合计 4000.00（2400/800/800）；B2 合计 1000.00（333.3333×2 + 333.3334，尾差 0.0001 单行收口）';
END $$;

-- =============================================================================
-- 算例 C：二期差异公式的两因素恒等式（价差/量差、工资率差/效率差）
--
-- 材料 A：标准 1.1 件/单位 × 标准单价 12.00；实际用量 1200 件 @ 12.50（完工 800 件）
--   标准用量 = 800 × 1.1 = 880
--   价差 = (12.50 - 12.00) × 1200 = +600（不利）
--   量差 = (1200 - 880) × 12.00 = +3840（不利）
--   合计 +4440 = 实际 15000 - 标准 10560 ✅
-- 人工：标准 0.10 小时/单位 × 标准工资率 50.00；实际 90 小时 @ 48.00
--   标准工时 = 800 × 0.10 = 80；标准成本 = 80 × 50 = 4000；实际 = 90 × 48 = 4320
--   工资率差 = (48 - 50) × 90  = -180（有利）
--   效率差   = (90 - 80) × 50  = +500（不利）
--   合计 +320 = 4320 - 4000 ✅
-- =============================================================================
WITH params AS (
    SELECT 800::numeric AS out_qty,
           1200::numeric AS mat_act_qty, 12.50::numeric AS mat_act_price,
           1.1::numeric  AS mat_std_qty_per, 12.00::numeric AS mat_std_price,
           90::numeric   AS lab_act_hours, 48.00::numeric AS lab_act_rate,
           0.10::numeric AS lab_std_qty_per, 50.00::numeric AS lab_std_rate
), calc AS (
    SELECT p.*,
           p.out_qty * p.mat_std_qty_per AS mat_std_qty,
           p.out_qty * p.lab_std_qty_per AS lab_std_hours
      FROM params p
)
SELECT 'MAT_PRICE' AS variance_type,
       round((mat_act_price - mat_std_price) * mat_act_qty, 4) AS amount,
       CASE WHEN (mat_act_price - mat_std_price) * mat_act_qty >= 0 THEN 'UNFAVORABLE' ELSE 'FAVORABLE' END AS direction,
       round(mat_act_qty * mat_act_price - mat_std_qty * mat_std_price, 4)     AS actual_minus_std
  FROM calc
UNION ALL
SELECT 'MAT_QTY',
       round((mat_act_qty - mat_std_qty) * mat_std_price, 4),
       CASE WHEN (mat_act_qty - mat_std_qty) * mat_std_price >= 0 THEN 'UNFAVORABLE' ELSE 'FAVORABLE' END,
       round(mat_act_qty * mat_act_price - mat_std_qty * mat_std_price, 4)
  FROM calc
UNION ALL
SELECT 'LABOR_RATE',
       round((lab_act_rate - lab_std_rate) * lab_act_hours, 4),
       CASE WHEN (lab_act_rate - lab_std_rate) * lab_act_hours >= 0 THEN 'UNFAVORABLE' ELSE 'FAVORABLE' END,
       round(lab_act_hours * lab_act_rate - lab_std_hours * lab_std_rate, 4)
  FROM calc
UNION ALL
SELECT 'LABOR_EFF',
       round((lab_act_hours - lab_std_hours) * lab_std_rate, 4),
       CASE WHEN (lab_act_hours - lab_std_hours) * lab_std_rate >= 0 THEN 'UNFAVORABLE' ELSE 'FAVORABLE' END,
       round(lab_act_hours * lab_act_rate - lab_std_hours * lab_std_rate, 4)
  FROM calc;

\echo '--- 算例 C：差异两因素恒等式 ---'
DO $$
DECLARE
    v_mat_price NUMERIC; v_mat_qty NUMERIC; v_act_minus_std_mat NUMERIC;
    v_lab_rate  NUMERIC; v_lab_eff NUMERIC; v_act_minus_std_lab NUMERIC;
    p RECORD;
BEGIN
    SELECT * INTO p FROM (
        SELECT 800::numeric AS out_qty, 1200::numeric AS mat_act_qty, 12.50::numeric AS mat_act_price,
               1.1::numeric AS mat_std_qty_per, 12.00::numeric AS mat_std_price,
               90::numeric AS lab_act_hours, 48.00::numeric AS lab_act_rate,
               0.10::numeric AS lab_std_qty_per, 50.00::numeric AS lab_std_rate) x;

    v_mat_price := (p.mat_act_price - p.mat_std_price) * p.mat_act_qty;
    v_mat_qty   := (p.mat_act_qty - p.out_qty * p.mat_std_qty_per) * p.mat_std_price;
    v_act_minus_std_mat := p.mat_act_qty * p.mat_act_price - (p.out_qty * p.mat_std_qty_per) * p.mat_std_price;
    IF v_mat_price + v_mat_qty <> v_act_minus_std_mat THEN
        RAISE EXCEPTION '材料差异恒等式不成立：% + % <> %', v_mat_price, v_mat_qty, v_act_minus_std_mat;
    END IF;
    IF v_mat_price <> 600 OR v_mat_qty <> 3840 OR v_act_minus_std_mat <> 4440 THEN
        RAISE EXCEPTION '材料差异数值错：%/%/%（应 600/3840/4440）', v_mat_price, v_mat_qty, v_act_minus_std_mat;
    END IF;

    v_lab_rate := (p.lab_act_rate - p.lab_std_rate) * p.lab_act_hours;
    v_lab_eff  := (p.lab_act_hours - p.out_qty * p.lab_std_qty_per) * p.lab_std_rate;
    v_act_minus_std_lab := p.lab_act_hours * p.lab_act_rate - (p.out_qty * p.lab_std_qty_per) * p.lab_std_rate;
    IF v_lab_rate + v_lab_eff <> v_act_minus_std_lab THEN
        RAISE EXCEPTION '人工差异恒等式不成立：% + % <> %', v_lab_rate, v_lab_eff, v_act_minus_std_lab;
    END IF;
    IF v_lab_rate <> -180 OR v_lab_eff <> 500 OR v_act_minus_std_lab <> 320 THEN
        RAISE EXCEPTION '人工差异数值错：%/%/%（应 -180/500/320）', v_lab_rate, v_lab_eff, v_act_minus_std_lab;
    END IF;

    RAISE NOTICE '✅ 算例 C 通过：材料 600+3840=4440（实际15000-标准10560）；人工 -180+500=320（实际4320-标准4000）';
END $$;

ROLLBACK;

DO $$
DECLARE n INT;
BEGIN
    SELECT count(*) INTO n FROM information_schema.tables
     WHERE table_schema = 'public' AND table_name LIKE 'tmp_drill_%';
    IF n <> 0 THEN RAISE EXCEPTION 'ROLLBACK 后仍残留 % 张 tmp_drill_* 表', n; END IF;
    RAISE NOTICE '✅ 零残留：算例脚本 ROLLBACK 干净';
END $$;
