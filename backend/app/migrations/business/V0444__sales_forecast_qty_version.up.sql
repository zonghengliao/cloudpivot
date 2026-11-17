-- ============================================================================
-- V0444：销售预测表结构补全 —— 预测数量 + 版本链 + 物料维度索引 + 「当前版本」视图
--        （M3-20 · 表结构：月度 / 客户 / 物料维度 + 预测值 + 版本）
--
-- 卡：t_0be1f0b6（M3-20【M3·销售】销售预测：表结构（月度/客户/物料维度））
--
-- ── 动手前的现状实测（决定这一刀到底补什么，别重复造表）────────────────
--   `biz_sales_forecast` 由 V0318（SO-44 / ARCH-7 §15.8）建出，14 列，**已经**有：
--     · `customer_id` / `material_id` 两维（都可空，双空 = 全局滚动预测）
--     · `period_code`（`yyyy-MM` 或 `yyyy-QN`，CHECK `ck_biz_sf_period`）——「月度」维度已在
--     · `forecast_amount`（NUMERIC(18,2)）—— 预测值的**金额**侧
--     · `version_no`（INT，>= 1）+ 唯一索引 `uk_biz_sales_forecast_dim`
--       （维度 + 期间 + 版本，存活行唯一）
--     · `confidence` / `remark` / 软删 / 审计四列
--     · 取号 `SFC-yyyyMMdd-NNNNNN`（biz_seq）、8 个权限码、菜单页、model/dto/handler 全套
--   所以卡面「月度 / 客户 / 物料 + 预测值 + 版本」的**骨架在 V0318 就已经落地**，
--   本迁移不重建表、不改已有列语义，只补四处真正缺的结构（下面 ①~④）。
--
-- ── 本迁移落的四件东西 ─────────────────────────────────────────────
--   ① `forecast_qty NUMERIC(18,4) NOT NULL DEFAULT 0` —— 预测值的**数量**侧。
--      为什么非补不可：物料维度的预测天然按数量谈（"下月 A 料 5000 件"），而本表原来只有金额，
--      与同模块的 `biz_sales_target.target_qty`（同精度、同 `check_qty` 入口校验）不对称；
--      没有数量列，「物料维度预测」只能拿金额凑，M3-21 的「预测 vs 实际」对比也就只能比金额。
--      口径与 target 表**逐字对齐**：精度 (18,4)、非负、默认 0（0 = 只预测金额不预测数量）。
--
--   ② `revised_from_id UUID` —— 版本链（这一版取代了哪一版）。
--      `version_no` 只回答"第几版"，回答不了"从哪一版改来的"：同维度同期间有 v1/v2/v3 时，
--      光看数字无法判断 v3 是 v2 的滚动修订，还是有人手工跳号另起一条。加一列自引用弱关联
--      （指向被取代的上一条预测），版本演进才可追溯；M3-21 做「版本间偏差」、M3-23 做版本
--      历史抽屉都要用它。
--      · `ck_biz_sf_revised_from` 挡住自指（`revised_from_id <> id`）；
--      · 「必须同维度同期间、且 `version_no = 上一版 + 1`」是多行约束，库层表达不了，
--        由 `sales_target_handler::validate_version_chain` 在写入口校验（见该函数文档）。
--
--   ③ 物料维度索引 `idx_biz_sales_forecast_material` —— V0318 只给客户维度建了
--      `(customer_id, period_code DESC)`；「物料维度」取数与 M3-21 的按物料对比没有索引，
--      `material_id` 过滤只能顺扫。与客户维度对称补齐（同为部分索引，`deleted_at IS NULL`）。
--      另加 `idx_biz_sales_forecast_revised_from`（版本链反查"谁修订了我"）。
--
--   ④ 视图 `v_biz_sales_forecast_current` —— 「当前版本」的唯一口径。
--      表里同维度同期间有多个版本（滚动预测的本意），列表直接查表会把 v1/v2/v3 一起返回；
--      下游若 SUM 就会把历史版本和当前版本相加（预测值翻三倍，还看不出来）。
--      视图钉死口径：**同（客户, 物料, 期间）取存活行里 `version_no` 最大的一条**，
--      其余版本只作为历史（要看历史就查表）。M3-21 的对比取数、M3-23 的当前预测口径都用它。
--      用 `NOT EXISTS(更大版本)` 而非 `max()` 子查询：命中 `uk_biz_sales_forecast_dim`
--      的首列前缀，单维度取数是一次索引探针，不用把维度组扫两遍。
--
-- ── 不变的东西（刻意不动，写清楚防误改）──────────────────────────────
--   · `period_code` 仍同时收 `yyyy-MM` 与 `yyyy-QN`：季度预测是既有能力（V0318 决定 4 号），
--     「月度维度」是新增不得的另一种粒度，不是唯一粒度 —— 本卡不改 CHECK。
--   · `uk_biz_sales_forecast_dim` 不动：它是「维度 + 期间 + 版本」的唯一性来源，改它等于
--     改一遍写入口和 409 文案；新版本 = 新行，不是原地改（既有语义）。
--   · `forecast_amount` 精度 (18,2) 不动（与销售金额口径一致）。
--
-- ── 幂等 ────────────────────────────────────────────────────────────
--   `ADD COLUMN IF NOT EXISTS` / 约束前判 `pg_constraint` / `CREATE INDEX IF NOT EXISTS` /
--   `CREATE OR REPLACE VIEW`；探针行用 `period_code = '1900-01'` 这个不会与真实业务撞的
--   期间（正则允许任意 yyyy-MM，真实数据不会有 1900 年的销售预测），并以
--   `remark = '__M320_PROBE__'` 双保险清场。末尾 DO 块做结构 + 行为自检，**残留必须为 0**。
--   重放 up = 零结构变更 + 零数据变更 + 探针重跑通过。
--
-- ── down ────────────────────────────────────────────────────────────
-- 见 `V0444__sales_forecast_qty_version.down.sql`：有真实预测数量 / 版本链数据时**拒绝回滚**
-- （业务信息不可重算），空表或全默认值时正常拆除。
-- ============================================================================

-- ---------- ① 预测数量（与 biz_sales_target.target_qty 同精度同口径） ----------
ALTER TABLE biz_sales_forecast ADD COLUMN IF NOT EXISTS forecast_qty NUMERIC(18,4) NOT NULL DEFAULT 0;

COMMENT ON COLUMN biz_sales_forecast.forecast_qty IS
    '预测数量（>= 0，NUMERIC(18,4)，默认 0 = 只预测金额不预测数量）；与 biz_sales_target.target_qty 同口径，物料维度预测主要按它谈';

-- ---------- ② 版本链：这一版取代了哪一版 ----------
ALTER TABLE biz_sales_forecast ADD COLUMN IF NOT EXISTS revised_from_id UUID;

COMMENT ON COLUMN biz_sales_forecast.revised_from_id IS
    '版本链：本行取代的上一条预测（弱引用 biz_sales_forecast.id，可空 = 该维度的首版）。写入时由 handler 校验「同客户同物料同期间 + version_no = 上一版 + 1」';

-- ---------- ③ 两条 CHECK（自指 / 非负） ----------
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = 'public.biz_sales_forecast'::regclass
           AND conname  = 'ck_biz_sf_qty'
    ) THEN
        ALTER TABLE biz_sales_forecast
            ADD CONSTRAINT ck_biz_sf_qty CHECK (forecast_qty >= 0);
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = 'public.biz_sales_forecast'::regclass
           AND conname  = 'ck_biz_sf_revised_from'
    ) THEN
        -- 自指会让「版本链」变成环，任何沿链上溯的代码都会死循环 —— 库层先挡
        ALTER TABLE biz_sales_forecast
            ADD CONSTRAINT ck_biz_sf_revised_from
            CHECK (revised_from_id IS NULL OR revised_from_id <> id);
    END IF;
END $$;

-- ---------- ④ 物料维度索引 + 版本链反查索引 ----------
CREATE INDEX IF NOT EXISTS idx_biz_sales_forecast_material
    ON biz_sales_forecast (material_id, period_code DESC) WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_biz_sales_forecast_revised_from
    ON biz_sales_forecast (revised_from_id) WHERE revised_from_id IS NOT NULL;

-- ---------- ⑤ 当前版本视图（"同维度同期间取最大 version_no 的存活行"的唯一口径） ----------
-- 列清单显式列出（不用 f.*）：视图列在创建时冻结，后续再加列的迁移要 CREATE OR REPLACE 本视图。
CREATE OR REPLACE VIEW v_biz_sales_forecast_current AS
SELECT f.id,
       f.forecast_no,
       f.customer_id,
       f.material_id,
       f.period_code,
       f.forecast_amount,
       f.forecast_qty,
       f.confidence,
       f.version_no,
       f.revised_from_id,
       f.remark,
       f.created_at,
       f.updated_at,
       f.create_by,
       f.update_by
  FROM biz_sales_forecast f
 WHERE f.deleted_at IS NULL
   AND NOT EXISTS (
        SELECT 1
          FROM biz_sales_forecast g
         WHERE g.deleted_at IS NULL
           AND g.period_code  = f.period_code
           AND g.customer_id IS NOT DISTINCT FROM f.customer_id
           AND g.material_id IS NOT DISTINCT FROM f.material_id
           AND g.version_no   > f.version_no
   );

COMMENT ON VIEW v_biz_sales_forecast_current IS
    '销售预测的当前版本（M3-20）：同客户同物料同期间取存活行里 version_no 最大的一条；其余版本只作历史，查表。下游 SUM 预测值必须用它，否则历史版本会被重复累加';

-- ---------- ⑥ 自检（结构 + 行为探针，可反复执行） ----------
DO $$
DECLARE
    n          integer;
    txt        text;
    v_qty_type text;
    v_default  text;
    v_nullable text;
    v_v1       uuid := gen_random_uuid();
    v_v2       uuid := gen_random_uuid();
    v_src      uuid;
    v_ver      integer;
    v_qty      numeric;
    v_cnt      integer;
BEGIN
    -- ===== 结构 1：两列在、类型 / 可空 / 默认值对 =====
    SELECT data_type, COALESCE(column_default, ''), is_nullable
      INTO v_qty_type, v_default, v_nullable
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_forecast'
       AND column_name = 'forecast_qty';
    IF v_qty_type IS NULL THEN
        RAISE EXCEPTION 'V0444 自检失败：biz_sales_forecast.forecast_qty 未建出';
    END IF;
    IF v_qty_type <> 'numeric' THEN
        RAISE EXCEPTION 'V0444 自检失败：forecast_qty 类型是 %（应为 numeric）', v_qty_type;
    END IF;
    IF v_nullable <> 'NO' OR v_default NOT LIKE '%0%' THEN
        RAISE EXCEPTION 'V0444 自检失败：forecast_qty 应为 NOT NULL DEFAULT 0（实际 nullable=%, default=%）',
            v_nullable, v_default;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_forecast'
       AND column_name = 'forecast_qty' AND numeric_scale = 4 AND numeric_precision = 18;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0444 自检失败：forecast_qty 不是 NUMERIC(18,4)';
    END IF;

    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_forecast'
       AND column_name = 'revised_from_id' AND data_type = 'uuid' AND is_nullable = 'YES';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0444 自检失败：revised_from_id 不是可空 uuid';
    END IF;

    -- ===== 结构 2：两条 CHECK 在位且定义正确 =====
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_sales_forecast'::regclass
       AND conname IN ('ck_biz_sf_qty', 'ck_biz_sf_revised_from') AND contype = 'c';
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0444 自检失败：两条 CHECK 只建出 % 条', n;
    END IF;
    SELECT pg_get_constraintdef(oid) INTO txt FROM pg_constraint
     WHERE conrelid = 'public.biz_sales_forecast'::regclass AND conname = 'ck_biz_sf_revised_from';
    IF txt NOT LIKE '%revised_from_id%' OR txt NOT LIKE '%<> id%' THEN
        RAISE EXCEPTION 'V0444 自检失败：ck_biz_sf_revised_from 定义不对：%', txt;
    END IF;

    -- ===== 结构 3：两条索引 + 视图 =====
    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('idx_biz_sales_forecast_material', 'idx_biz_sales_forecast_revised_from');
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0444 自检失败：新增索引只建出 % 条', n;
    END IF;
    IF to_regclass('public.v_biz_sales_forecast_current') IS NULL THEN
        RAISE EXCEPTION 'V0444 自检失败：视图 v_biz_sales_forecast_current 未建出';
    END IF;

    -- ===== 结构 4：回归护栏 —— V0318 的唯一索引 / 月度期间 CHECK 必须还在 =====
    IF to_regclass('public.uk_biz_sales_forecast_dim') IS NULL THEN
        RAISE EXCEPTION 'V0444 自检失败：uk_biz_sales_forecast_dim 丢了（V0318 的「维度+期间+版本」唯一）';
    END IF;
    IF to_regclass('public.uk_biz_sales_forecast_no') IS NULL THEN
        RAISE EXCEPTION 'V0444 自检失败：uk_biz_sales_forecast_no 丢了';
    END IF;
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_sales_forecast'::regclass AND conname = 'ck_biz_sf_period';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0444 自检失败：ck_biz_sf_period（月度/季度期间码）不见了';
    END IF;

    -- ===== 行为探针（全程 period_code = '1900-01' 隔离，与真实业务不撞）=====
    DELETE FROM biz_sales_forecast WHERE period_code = '1900-01';

    -- P1：数量默认 0（只写金额的行，qty 必须自动是 0）
    INSERT INTO biz_sales_forecast (id, forecast_no, period_code, forecast_amount, remark)
    VALUES (v_v1, '__M320-P1__', '1900-01', 1000.00, '__M320_PROBE__');
    SELECT forecast_qty INTO v_qty FROM biz_sales_forecast WHERE id = v_v1;
    IF v_qty <> 0 THEN
        RAISE EXCEPTION 'V0444 自检失败：P1 —— forecast_qty 默认值不是 0（是 %）', v_qty;
    END IF;

    -- P2：负数量必须被 CHECK 拒
    BEGIN
        UPDATE biz_sales_forecast SET forecast_qty = -1 WHERE id = v_v1;
        RAISE EXCEPTION 'V0444 自检失败：P2 —— 负预测数量被接受（ck_biz_sf_qty 没生效）';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- P3：数量 4 位小数可存、原样读回（NUMERIC(18,4)）
    UPDATE biz_sales_forecast SET forecast_qty = 1234.5678 WHERE id = v_v1;
    SELECT forecast_qty INTO v_qty FROM biz_sales_forecast WHERE id = v_v1;
    IF v_qty <> 1234.5678 THEN
        RAISE EXCEPTION 'V0444 自检失败：P3 —— 4 位小数没原样存回（读到 %）', v_qty;
    END IF;

    -- P4：版本链自指必须被拒
    BEGIN
        UPDATE biz_sales_forecast SET revised_from_id = v_v1 WHERE id = v_v1;
        RAISE EXCEPTION 'V0444 自检失败：P4 —— revised_from_id 指向自己竟然成功';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- P5：v1 → v2 版本链可写；视图只给 v2（当前版本 = 最大 version_no）
    INSERT INTO biz_sales_forecast
        (id, forecast_no, period_code, forecast_amount, forecast_qty, version_no, revised_from_id, remark)
    VALUES (v_v2, '__M320-P5__', '1900-01', 1200.00, 1300.0000, 2, v_v1, '__M320_PROBE__');
    SELECT count(*) INTO v_cnt FROM v_biz_sales_forecast_current WHERE period_code = '1900-01';
    IF v_cnt <> 1 THEN
        RAISE EXCEPTION 'V0444 自检失败：P5 —— 当前版本视图返回 % 行（期望 1，两个版本只该出一条）', v_cnt;
    END IF;
    SELECT id, version_no INTO v_src, v_ver
      FROM v_biz_sales_forecast_current WHERE period_code = '1900-01';
    IF v_src <> v_v2 OR v_ver <> 2 THEN
        RAISE EXCEPTION 'V0444 自检失败：P5 —— 视图给的当前版本是 id=%(v_ver=%)，期望 v2 / 2', v_src, v_ver;
    END IF;

    -- P6：同维度同期间同版本第二条必须被 uk_biz_sales_forecast_dim 拒（V0318 约束仍在）
    BEGIN
        INSERT INTO biz_sales_forecast (forecast_no, period_code, forecast_amount, version_no, remark)
        VALUES ('__M320-P6__', '1900-01', 1300.00, 2, '__M320_PROBE__');
        RAISE EXCEPTION 'V0444 自检失败：P6 —— 同维度同期间同版本的第二条被接受';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    -- P7：软删当前版本后，视图回落上一版（版本链语义：删最新的不算把历史也删了）
    UPDATE biz_sales_forecast SET deleted_at = now() WHERE id = v_v2;
    SELECT count(*) INTO v_cnt FROM v_biz_sales_forecast_current WHERE period_code = '1900-01';
    IF v_cnt <> 1 THEN
        RAISE EXCEPTION 'V0444 自检失败：P7 —— 软删 v2 后视图返回 % 行（期望回落到 v1 的 1 行）', v_cnt;
    END IF;
    SELECT id INTO v_src FROM v_biz_sales_forecast_current WHERE period_code = '1900-01';
    IF v_src <> v_v1 THEN
        RAISE EXCEPTION 'V0444 自检失败：P7 —— 软删 v2 后视图给的 id=%，期望回落到 v1', v_src;
    END IF;

    -- ===== 清场：探针行必须 0 残留 =====
    DELETE FROM biz_sales_forecast WHERE period_code = '1900-01';
    SELECT count(*) INTO n FROM biz_sales_forecast WHERE period_code = '1900-01';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0444 自检失败：探针清场残留 % 行', n;
    END IF;

    RAISE NOTICE 'M3-20 自检通过：forecast_qty(NUMERIC(18,4) 非负) + 版本链 revised_from_id(禁自指) + 物料维度索引 + 当前版本视图（7 组行为探针）';
END $$;
