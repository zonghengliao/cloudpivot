-- ============================================================================
-- V0461：分批法「完工批次结转（完工转出）」—— biz_cost_batch_finish（M5-04）
--
-- 卡：t_75ca9f0d（M5-04【M5·成本】分批法：完工批次结转 / 完工转出）
-- 依据：docs/architecture/ARCH-9-成本核算升级设计.md
--       §4.6（在制与完工：完工转出 = 投入 − 期末在制，分批法下在制为 0 即全额转出）、
--       §4.7.4（完工结转借贷科目）、§4.11（C-COST-2 / C-COST-3 两条勾稽）
--
-- # 这张表存什么
--   一张「批次完工结转单」= 某一批货在某一次结转里**转出了多少钱**（料 / 工 / 费三分量），
--   外加「结转时点的累计归集」与「完工数量 / 单位成本」两个凭据。
--   它是**成本工作底稿**（ARCH-9 D-1：成本对象层不建第二本库存账）：
--   不出凭证、不建完工入库单 —— 账务出口仍是期间结算（B3-4/B3-5）。
--
-- # 为什么要单独一张表（不塞进 biz_cost_collect / biz_cost_period_item）
--   * biz_cost_collect 是「钱从哪儿进来」的归集明细（只能增、按来源幂等）；
--     结转是「钱从哪儿出去」的另一侧，幂等键、生命周期、审计口径全不同；
--   * biz_cost_period_item 是「对象 × 期间」的计算单，属于期间结算这条船；
--     分批法结转是**批次级**动作（跨期间累计），期间可能还没建批次。
--   两者混用会让「这批还剩多少在制」（M5-05）只能靠猜。
--
-- # 为什么允许同一批多行（transfer_seq）
--   结转只写**差额**（累计归集 − 已结转）。批完工后再过账的迟到领料 / 迟确认工时是真成本，
--   必须还能转出去；所以同批第 n 次结转各留一行，`transfer_seq` 从 1 递增。
--   「同批同一个 seq 只能有一行」由部分唯一索引兜底（并发下绕开行锁直插会撞键）。
--
-- # 为什么没有 status 列
--   结转单是不可变凭据：要作废就软删（`deleted_at`），要冲回就由账务侧（期间结算红冲）
--   处理。加一个永远只写 'FINISHED' 的状态列 = 死结构。
--
-- # 幂等约定
--   CREATE TABLE / INDEX 一律 IF NOT EXISTS；COMMENT ON 重复执行结果不变；
--   自检探针行（`SELFCHECK-M504-*`）断言后立即删除，重复执行零残留。
-- ============================================================================

-- ----------------------------------------------------------------------------
-- 0. 前置护栏：地基表不在就直说「先跑哪张卡」
-- ----------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.biz_cost_object') IS NULL THEN
        RAISE EXCEPTION 'V0461 拒绝执行：biz_cost_object 不存在，请先执行 V0370（COST-B1-1）';
    END IF;
    IF to_regclass('public.biz_cost_collect') IS NULL THEN
        RAISE EXCEPTION 'V0461 拒绝执行：biz_cost_collect 不存在，请先执行 V0371（COST-B1-2）';
    END IF;
END $$;

-- ----------------------------------------------------------------------------
-- ① 结转单
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_batch_finish (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    object_id           UUID          NOT NULL,                       -- 成本对象（物理 FK，RESTRICT）
    object_no           VARCHAR(32)   NOT NULL,                       -- 对象号快照（审计口径，不 join）
    period_code         VARCHAR(7)    NOT NULL,                       -- 结转期间 yyyy-MM（这笔结转落在哪期）
    transfer_seq        INT           NOT NULL,                       -- 同批第几次结转（从 1 起）
    product_id          UUID          NOT NULL,                       -- 产出品（弱引用 biz_material.id）
    lot_no              VARCHAR(64),                                  -- 产出批号快照
    finished_qty        NUMERIC(18,4) NOT NULL,                       -- 结转口径的完工数量（批次累计完工）
    unit_cost           NUMERIC(18,6) NOT NULL DEFAULT 0,             -- 本次单位成本 = 本次合计 / 完工数
    collected_material  NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 结转时点累计归集：料
    collected_labor     NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 结转时点累计归集：工
    collected_overhead  NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 结转时点累计归集：费
    collected_total     NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 结转时点累计归集：合计
    material_amount     NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 本次结转：料
    labor_amount        NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 本次结转：工
    overhead_amount     NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 本次结转：费
    total_amount        NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 本次结转：合计
    remark              TEXT,
    created_at          TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by           UUID,
    update_by           UUID,
    deleted_at          TIMESTAMPTZ,
    CONSTRAINT fk_biz_cost_batch_finish_obj FOREIGN KEY (object_id)
        REFERENCES biz_cost_object (id) ON DELETE RESTRICT,
    CONSTRAINT ck_biz_cost_batch_finish_seq
        CHECK (transfer_seq >= 1),
    CONSTRAINT ck_biz_cost_batch_finish_qty
        CHECK (finished_qty >= 0 AND unit_cost >= 0),
    CONSTRAINT ck_biz_cost_batch_finish_parts
        CHECK (material_amount >= 0 AND labor_amount >= 0 AND overhead_amount >= 0),
    CONSTRAINT ck_biz_cost_batch_finish_collected_parts
        CHECK (collected_material >= 0 AND collected_labor >= 0 AND collected_overhead >= 0),
    -- 合计必须等于三分量之和（库层守恒：任何一条「合计和分量不一致」的结转单都别想落库）
    CONSTRAINT ck_biz_cost_batch_finish_total
        CHECK (total_amount = material_amount + labor_amount + overhead_amount),
    CONSTRAINT ck_biz_cost_batch_finish_collected_total
        CHECK (collected_total = collected_material + collected_labor + collected_overhead),
    CONSTRAINT ck_biz_cost_batch_finish_period
        CHECK (period_code ~ '^[0-9]{4}-(0[1-9]|1[0-2])$')
);

COMMENT ON TABLE  biz_cost_batch_finish IS
    '分批法完工批次结转单（工作底稿）：一批货一次结转转出多少料/工/费；不出凭证、不建入库单（账务出口=期间结算，ARCH-9 D-1）';
COMMENT ON COLUMN biz_cost_batch_finish.object_no IS '对象号快照：审计/历史口径，按 AGENTS 约定 13 存快照而不 join';
COMMENT ON COLUMN biz_cost_batch_finish.period_code IS '结转期间 yyyy-MM：这笔结转记在哪个会计期间';
COMMENT ON COLUMN biz_cost_batch_finish.transfer_seq IS '同批第几次结转：本次 = 累计归集 − 已结转，差额结转允许多次';
COMMENT ON COLUMN biz_cost_batch_finish.lot_no IS '产出批号快照：这批货是哪一批（完工入库时用同一个批号）';
COMMENT ON COLUMN biz_cost_batch_finish.finished_qty IS '结转口径的完工数量 = biz_cost_object.completed_qty';
COMMENT ON COLUMN biz_cost_batch_finish.unit_cost IS '本次单位成本 = 本次合计 / 完工数量，round 6（与设计 §4.6 ⑤ 同标度）';
COMMENT ON COLUMN biz_cost_batch_finish.collected_material IS '结转时点的累计归集快照（料）：事后能解释「为什么转这么多」';
COMMENT ON COLUMN biz_cost_batch_finish.collected_labor IS '结转时点的累计归集快照（工）';
COMMENT ON COLUMN biz_cost_batch_finish.collected_overhead IS '结转时点的累计归集快照（费）';
COMMENT ON COLUMN biz_cost_batch_finish.collected_total IS '结转时点的累计归集合计 = 料+工+费（库层 CHECK 守恒）';
COMMENT ON COLUMN biz_cost_batch_finish.total_amount IS
    '本次结转合计 = 料+工+费（库层 CHECK 守恒）；M5-05 在制余额 = 累计归集 − Σ 本表活行';

-- ----------------------------------------------------------------------------
-- ② 索引
--    · 同批同一 seq 只许一行活记录（幂等 + 并发最后一道闸）
--    · 按批取历史 / 按期间汇总
-- ----------------------------------------------------------------------------
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_batch_finish_seq
    ON biz_cost_batch_finish (object_id, transfer_seq) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_cost_batch_finish_obj
    ON biz_cost_batch_finish (object_id, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_cost_batch_finish_period
    ON biz_cost_batch_finish (period_code);

-- ============================================================================
-- ③ 自检（写进迁移，部署即校验；失败即整笔回滚 ⇒ 不存在「半建好的表」）
--
--    A 形态：表 / 列 / CHECK 名单 / 部分唯一索引的谓词
--    B 行为：合计与分量不等必须被拦；同批同 seq 第二行必须撞唯一键；
--            软删行不占键（作废后可以重新结一次）
--    探针行断言后立即删除，重复执行零残留。
-- ============================================================================
DO $$
DECLARE
    v_probe_obj  UUID := '00000000-0000-0000-0000-0000000c0461';
    v_probe_row  UUID := '00000000-0000-0000-0000-0000000c0462';
    v_n          INT;
    v_idx        TEXT;
BEGIN
    -- ===== A. 形态 =====
    IF to_regclass('public.biz_cost_batch_finish') IS NULL THEN
        RAISE EXCEPTION 'V0461 自检失败：biz_cost_batch_finish 未建出来';
    END IF;

    SELECT count(*) INTO v_n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_cost_batch_finish';
    IF v_n <> 23 THEN
        RAISE EXCEPTION 'V0461 自检失败：biz_cost_batch_finish 有 % 列（应为 23）', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE conrelid = 'biz_cost_batch_finish'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_cost_batch_finish_seq','ck_biz_cost_batch_finish_qty',
                       'ck_biz_cost_batch_finish_parts','ck_biz_cost_batch_finish_collected_parts',
                       'ck_biz_cost_batch_finish_total','ck_biz_cost_batch_finish_collected_total',
                       'ck_biz_cost_batch_finish_period');
    IF v_n <> 7 THEN
        RAISE EXCEPTION 'V0461 自检失败：7 条 CHECK 只有 % 条落库', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE conrelid = 'biz_cost_batch_finish'::regclass AND contype = 'f'
       AND conname = 'fk_biz_cost_batch_finish_obj';
    IF v_n <> 1 THEN
        RAISE EXCEPTION 'V0461 自检失败：object_id 的外键没建出来';
    END IF;

    SELECT indexdef INTO v_idx FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_cost_batch_finish_seq';
    IF v_idx IS NULL THEN
        RAISE EXCEPTION 'V0461 自检失败：uk_biz_cost_batch_finish_seq 不存在';
    END IF;
    IF v_idx NOT LIKE 'CREATE UNIQUE%' THEN
        RAISE EXCEPTION 'V0461 自检失败：uk_biz_cost_batch_finish_seq 不是 UNIQUE 索引：%', v_idx;
    END IF;
    IF replace(v_idx, ' ', '') NOT LIKE '%WHERE(deleted_atISNULL)%' THEN
        RAISE EXCEPTION 'V0461 自检失败：uk_biz_cost_batch_finish_seq 缺软删条件（作废后无法重结）：%', v_idx;
    END IF;

    -- ===== B. 行为（探针行，断言后必删） =====
    -- 探针成本对象（biz_cost_object 的 product/warehouse/dept 都是弱引用，无外键 ⇒ 可直接造）
    DELETE FROM biz_cost_batch_finish WHERE object_id = v_probe_obj;
    DELETE FROM biz_cost_object WHERE id = v_probe_obj;
    INSERT INTO biz_cost_object (id, object_no, object_type, product_id, target_warehouse_id, plan_qty,
                                 workshop_dept_id, workshop_dept_name, start_date, status)
    VALUES (v_probe_obj, 'SELFCHECK-M504-OBJ', 'BATCH',
            '00000000-0000-0000-0000-0000000c0463', '00000000-0000-0000-0000-0000000c0464', 1,
            '00000000-0000-0000-0000-0000000c0465', 'V0461 自检车间', DATE '2026-01-01', 'OPEN');

    -- B1 合计 ≠ 分量之和 ⇒ 必须被库层拦住
    BEGIN
        INSERT INTO biz_cost_batch_finish (id, object_id, object_no, period_code, transfer_seq,
                                           product_id, finished_qty, unit_cost,
                                           material_amount, labor_amount, overhead_amount, total_amount)
        VALUES (v_probe_row, v_probe_obj, 'SELFCHECK-M504-OBJ', '2026-01', 1,
                '00000000-0000-0000-0000-0000000c0463', 1, 1, 10, 20, 30, 999);
        RAISE EXCEPTION 'V0461 自检失败：合计与分量不一致的结转单被放行了';
    EXCEPTION WHEN check_violation THEN
        NULL;  -- 预期
    END;

    -- B2 合法行可插入
    INSERT INTO biz_cost_batch_finish (id, object_id, object_no, period_code, transfer_seq,
                                       product_id, finished_qty, unit_cost,
                                       collected_material, collected_labor, collected_overhead, collected_total,
                                       material_amount, labor_amount, overhead_amount, total_amount)
    VALUES (v_probe_row, v_probe_obj, 'SELFCHECK-M504-OBJ', '2026-01', 1,
            '00000000-0000-0000-0000-0000000c0463', 10, 6,
            60, 0, 0, 60, 60, 0, 0, 60);

    -- B3 同批同 seq 的第二行必须撞唯一键
    BEGIN
        INSERT INTO biz_cost_batch_finish (object_id, object_no, period_code, transfer_seq,
                                           product_id, finished_qty, unit_cost, total_amount)
        VALUES (v_probe_obj, 'SELFCHECK-M504-OBJ', '2026-01', 1,
                '00000000-0000-0000-0000-0000000c0463', 10, 6, 0);
        RAISE EXCEPTION 'V0461 自检失败：同批同 seq 的第二行插进去了（幂等闸失效）';
    EXCEPTION WHEN unique_violation THEN
        NULL;  -- 预期
    END;

    -- B4 软删行不占键：作废这一张后，同 seq 可以重新结一次
    UPDATE biz_cost_batch_finish SET deleted_at = now() WHERE id = v_probe_row;
    INSERT INTO biz_cost_batch_finish (object_id, object_no, period_code, transfer_seq,
                                       product_id, finished_qty, unit_cost, total_amount)
    VALUES (v_probe_obj, 'SELFCHECK-M504-OBJ', '2026-01', 1,
            '00000000-0000-0000-0000-0000000c0463', 10, 6, 0);

    -- B5 期间形状 CHECK（'2026-13' 必须被拦）
    BEGIN
        INSERT INTO biz_cost_batch_finish (object_id, object_no, period_code, transfer_seq,
                                           product_id, finished_qty, unit_cost, total_amount)
        VALUES (v_probe_obj, 'SELFCHECK-M504-OBJ', '2026-13', 2,
                '00000000-0000-0000-0000-0000000c0463', 10, 6, 0);
        RAISE EXCEPTION 'V0461 自检失败：非法期间 2026-13 被放行了';
    EXCEPTION WHEN check_violation THEN
        NULL;  -- 预期
    END;

    -- 清探针（结转单先删：外键 RESTRICT 会挡住删对象）
    DELETE FROM biz_cost_batch_finish WHERE object_id = v_probe_obj;
    DELETE FROM biz_cost_object WHERE id = v_probe_obj;

    SELECT count(*) INTO v_n FROM biz_cost_batch_finish WHERE object_no LIKE 'SELFCHECK-M504-%';
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0461 自检失败：探针残留 % 行', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM biz_cost_object WHERE id = v_probe_obj;
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0461 自检失败：探针成本对象没删干净（% 行）', v_n;
    END IF;

    RAISE NOTICE 'V0461 自检通过：形态 7 条 CHECK + 外键 + 部分唯一索引；行为 5 条（合计守恒 / 重复 seq / 软删不占键 / 期间形状）';
END $$;
