-- ============================================================================
-- V0470：在制品「完工率录入（按工序）」—— biz_cost_object_operation（M5-21）
--
-- 卡：t_46c1745d（M5-21【M5·成本】在制品：完工率录入 · 按工序）
-- 依据：ARCH-9 §4.6（在制与完工：约当产量法的加工约当 = 完工 + 在制 × 完工率）、
--       M5-20 模块头（`cost_wip_equiv_dto`）里写死的交接：
--       「工序级完工率落地后按加权汇总写回对象级即可，本表不需要改」。
--
-- # 这张表存什么
--   一个成本对象（生产批）在某一个时点的**工序级完工进度**：
--     seq（工序顺序）+ operation_name（工序名）+ weight（工作量权重）+ completion_pct（该工序完工率）。
--   对象级完工率 `biz_cost_object.completion_pct` 不再是手填的孤立数字，而是本表
--   Σ(weight × completion_pct) / Σweight 的**派生物**（写入口在
--   `PUT /business/erp/cost-object/{id}/operations`，同一事务回写对象级）。
--
-- # 为什么单独一张表（不塞进 biz_cost_object 的 JSON 列 / 不塞进计算单）
--   * `biz_cost_object` 是对象抬头，一行一个批；工序是 1:N。JSON 列没法做
--     「同批同一工序只能一行」的部分唯一索引，并发下会写出两道 3 号工序；
--   * `biz_cost_period_item`（计算单）是**期间结算**的产物：它记的是「算这一期时
--     用的完工率」，属于历史快照；本表是「现在车间报的工序进度」，是录入面。
--     两者混用会让「改完工率」和「改历史账」变成同一个动作。
--
-- # 为什么没有 period_code（与对象级同一口径，是决定不是疏漏）
--   `biz_cost_object.completion_pct` 本身就是**一个当前快照**（V0370 列注释：
--   「期末在制完工进度%，约当产量法用」），没有期间维度；M5-20 的约当产量表拿它
--   与当期计算单对拍，漂移了标 `drift`。工序表若自带期间，就会与对象级成为两本账：
--   到底听 2026-11 的工序行还是听对象抬头？所以本表与对象级同生共死 ——
--   一次保存 = 覆盖当前工序进度 + 回写对象级。历史版本的完工率在计算单
--   （`biz_cost_period_item.completion_pct`）里，那里才是按期间留档的地方。
--
-- # 权重语义（为什么 weight > 0 是硬约束）
--   综合完工率 = Σ(weight × pct) / Σweight。`Σweight = 0` 时公式没有定义
--   （0/0），库层直接拒；前端不填权重时 DTO 默认 1 = 算术平均。
--   权重一般填**标准工时 / 定额工时**（工作量大的工序说话更重）。
--
-- # 幂等约定
--   CREATE TABLE / INDEX 一律 IF NOT EXISTS；COMMENT ON 重复执行结果不变；
--   自检探针行在断言后立即删除，重复执行零残留。
-- ============================================================================

-- ----------------------------------------------------------------------------
-- 0. 前置护栏：地基表不在就直说「先跑哪张卡」
-- ----------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.biz_cost_object') IS NULL THEN
        RAISE EXCEPTION 'V0470 拒绝执行：biz_cost_object 不存在，请先执行 V0370（COST-B1-1）';
    END IF;
END $$;

-- ----------------------------------------------------------------------------
-- ① 工序完工率
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_object_operation (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cost_object_id  UUID          NOT NULL,                    -- 成本对象（物理 FK，RESTRICT）
    seq             INT           NOT NULL,                    -- 工序顺序号（1..999，同批唯一）
    operation_name  VARCHAR(64)   NOT NULL,                    -- 工序名称（下料 / 焊接 / 装配 …）
    weight          NUMERIC(12,4) NOT NULL DEFAULT 1,          -- 工作量权重（工时定额；缺省 1 = 算术平均）
    completion_pct  NUMERIC(5,2)  NOT NULL DEFAULT 0,          -- 该工序完工进度 0–100
    remark          VARCHAR(255),                              -- 备注（为什么 60% 而不是 100%）
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by       UUID,
    update_by       UUID,
    deleted_at      TIMESTAMPTZ,
    CONSTRAINT fk_biz_cost_object_operation_obj FOREIGN KEY (cost_object_id)
        REFERENCES biz_cost_object (id) ON DELETE RESTRICT,
    CONSTRAINT ck_biz_cost_object_operation_seq
        CHECK (seq >= 1 AND seq <= 999),
    CONSTRAINT ck_biz_cost_object_operation_name
        CHECK (btrim(operation_name) <> '' AND char_length(operation_name) <= 64),
    -- 权重必须 > 0：综合完工率的分母是 Σweight，0 权重会让公式在「全是 0」时变成 0/0
    CONSTRAINT ck_biz_cost_object_operation_weight
        CHECK (weight > 0 AND weight <= 99999999.9999),
    CONSTRAINT ck_biz_cost_object_operation_pct
        CHECK (completion_pct >= 0 AND completion_pct <= 100),
    CONSTRAINT ck_biz_cost_object_operation_remark
        CHECK (remark IS NULL OR char_length(remark) <= 255)
);

COMMENT ON TABLE  biz_cost_object_operation IS
    '在制品工序完工率：一个成本对象的逐工序完工进度；对象级完工率 = Σ(weight × completion_pct) / Σweight（M5-21）';
COMMENT ON COLUMN biz_cost_object_operation.cost_object_id IS
    '成本对象（生产批）；软删批的行由应用层清理，物理删除受 FK RESTRICT 保护';
COMMENT ON COLUMN biz_cost_object_operation.seq IS
    '工序顺序号 1..999：同批同一序号只能有一行活记录（部分唯一索引），前台按它排序';
COMMENT ON COLUMN biz_cost_object_operation.operation_name IS
    '工序名称：无 MES/工艺路线时由车间手工维护，2–64 字符';
COMMENT ON COLUMN biz_cost_object_operation.weight IS
    '工作量权重（一般填标准工时/定额工时）：综合完工率 = Σ(weight × pct) / Σweight；缺省 1 即算术平均';
COMMENT ON COLUMN biz_cost_object_operation.completion_pct IS
    '本工序完工进度 0–100（2 位小数）；与对象级 completion_pct 同一量纲（NUMERIC(5,2)）';
COMMENT ON COLUMN biz_cost_object_operation.remark IS
    '备注：记录「这道工序为什么是 60%」这类现场事实，不超过 255 字符';

-- ----------------------------------------------------------------------------
-- ② 索引
--    · 同批同一 seq 只许一行活记录（录入整表保存 + 并发最后一道闸）
--    · 按批取工序（保存后的回查 / 详情）
-- ----------------------------------------------------------------------------
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_object_operation_seq
    ON biz_cost_object_operation (cost_object_id, seq) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_cost_object_operation_obj
    ON biz_cost_object_operation (cost_object_id, seq) WHERE deleted_at IS NULL;

-- ============================================================================
-- ③ 自检（写进迁移，部署即校验；失败即整笔回滚 ⇒ 不存在「半建好的表」）
--
--    A 形态：表 / 列数 / CHECK 名单 / 外键 / 部分唯一索引的谓词
--    B 行为：完工率 101 必须被拦；权重 0 必须被拦；同批同 seq 第二行必须撞唯一键；
--            软删行不占键（删掉 3 号工序后可以重新建一行 3 号）
--    探针行断言后立即删除，重复执行零残留。
-- ============================================================================
DO $$
DECLARE
    v_probe_obj UUID := '00000000-0000-0000-0000-0000000c0470';
    v_probe_row UUID := '00000000-0000-0000-0000-0000000c0471';
    v_n         INT;
    v_idx       TEXT;
BEGIN
    -- ===== A. 形态 =====
    IF to_regclass('public.biz_cost_object_operation') IS NULL THEN
        RAISE EXCEPTION 'V0470 自检失败：biz_cost_object_operation 未建出来';
    END IF;

    SELECT count(*) INTO v_n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_cost_object_operation';
    IF v_n <> 12 THEN
        RAISE EXCEPTION 'V0470 自检失败：biz_cost_object_operation 有 % 列（应为 12）', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE conrelid = 'biz_cost_object_operation'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_cost_object_operation_seq','ck_biz_cost_object_operation_name',
                       'ck_biz_cost_object_operation_weight','ck_biz_cost_object_operation_pct',
                       'ck_biz_cost_object_operation_remark');
    IF v_n <> 5 THEN
        RAISE EXCEPTION 'V0470 自检失败：5 条 CHECK 只有 % 条落库', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE conrelid = 'biz_cost_object_operation'::regclass AND contype = 'f'
       AND conname = 'fk_biz_cost_object_operation_obj';
    IF v_n <> 1 THEN
        RAISE EXCEPTION 'V0470 自检失败：cost_object_id 的外键没建出来';
    END IF;

    SELECT indexdef INTO v_idx FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_cost_object_operation_seq';
    IF v_idx IS NULL THEN
        RAISE EXCEPTION 'V0470 自检失败：uk_biz_cost_object_operation_seq 不存在';
    END IF;
    IF v_idx NOT LIKE 'CREATE UNIQUE%' THEN
        RAISE EXCEPTION 'V0470 自检失败：uk_biz_cost_object_operation_seq 不是 UNIQUE 索引：%', v_idx;
    END IF;
    IF replace(v_idx, ' ', '') NOT LIKE '%WHERE(deleted_atISNULL)%' THEN
        RAISE EXCEPTION 'V0470 自检失败：uk_biz_cost_object_operation_seq 缺软删条件（删掉工序后无法重建同号）：%', v_idx;
    END IF;

    -- ===== B. 行为（探针行，断言后必删） =====
    -- 探针成本对象（biz_cost_object 的 product/warehouse/dept 都是弱引用，无外键 ⇒ 可直接造）
    DELETE FROM biz_cost_object_operation WHERE cost_object_id = v_probe_obj;
    DELETE FROM biz_cost_object WHERE id = v_probe_obj;
    INSERT INTO biz_cost_object (id, object_no, object_type, product_id, target_warehouse_id, plan_qty,
                                 workshop_dept_id, workshop_dept_name, start_date, status)
    VALUES (v_probe_obj, 'SELFCHECK-M521-OBJ', 'BATCH',
            '00000000-0000-0000-0000-0000000c0472', '00000000-0000-0000-0000-0000000c0473', 1,
            '00000000-0000-0000-0000-0000000c0474', 'V0470 自检车间', DATE '2026-01-01', 'OPEN');

    -- B1 完工率 101 必须被拦
    BEGIN
        INSERT INTO biz_cost_object_operation (cost_object_id, seq, operation_name, completion_pct)
        VALUES (v_probe_obj, 1, '下料', 101);
        RAISE EXCEPTION 'V0470 自检失败：完工率 101 被放行了';
    EXCEPTION WHEN check_violation THEN
        NULL;  -- 预期
    END;

    -- B2 权重 0 必须被拦（公式分母不能是 0）
    BEGIN
        INSERT INTO biz_cost_object_operation (cost_object_id, seq, operation_name, weight, completion_pct)
        VALUES (v_probe_obj, 1, '下料', 0, 50);
        RAISE EXCEPTION 'V0470 自检失败：权重 0 被放行了';
    EXCEPTION WHEN check_violation THEN
        NULL;  -- 预期
    END;

    -- B3 合法行可插入
    INSERT INTO biz_cost_object_operation (id, cost_object_id, seq, operation_name, weight, completion_pct)
    VALUES (v_probe_row, v_probe_obj, 1, '下料', 2, 100);

    -- B4 同批同 seq 的第二行必须撞唯一键
    BEGIN
        INSERT INTO biz_cost_object_operation (cost_object_id, seq, operation_name, completion_pct)
        VALUES (v_probe_obj, 1, '下料（重复）', 50);
        RAISE EXCEPTION 'V0470 自检失败：同批同 seq 的第二行插进去了（工序号闸失效）';
    EXCEPTION WHEN unique_violation THEN
        NULL;  -- 预期
    END;

    -- B5 软删行不占键：删掉 1 号工序后可以重新建一行 1 号
    UPDATE biz_cost_object_operation SET deleted_at = now() WHERE id = v_probe_row;
    INSERT INTO biz_cost_object_operation (cost_object_id, seq, operation_name, completion_pct)
    VALUES (v_probe_obj, 1, '下料（重建）', 60);

    -- B6 工序名空白必须被拦
    BEGIN
        INSERT INTO biz_cost_object_operation (cost_object_id, seq, operation_name, completion_pct)
        VALUES (v_probe_obj, 2, '   ', 10);
        RAISE EXCEPTION 'V0470 自检失败：空白工序名被放行了';
    EXCEPTION WHEN check_violation THEN
        NULL;  -- 预期
    END;

    -- 清探针（工序先删：外键 RESTRICT 会挡住删对象）
    DELETE FROM biz_cost_object_operation WHERE cost_object_id = v_probe_obj;
    DELETE FROM biz_cost_object WHERE id = v_probe_obj;

    SELECT count(*) INTO v_n FROM biz_cost_object_operation WHERE cost_object_id = v_probe_obj;
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0470 自检失败：探针工序残留 % 行', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM biz_cost_object WHERE id = v_probe_obj;
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0470 自检失败：探针成本对象没删干净（% 行）', v_n;
    END IF;

    RAISE NOTICE 'V0470 自检通过：形态 5 条 CHECK + 外键 + 部分唯一索引；行为 5 条（完工率值域 / 权重 0 / 重复 seq / 软删不占键 / 空工序名）';
END $$;
