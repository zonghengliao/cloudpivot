-- ============================================================================
-- V0370：成本核算升级 · 地基两张主数据表（ARCH-9 §3.1 ① ②）
--
-- 卡：t_f2b6d80a（COST-B1-1【成本】迁移 V0368）—— 拆卡清单 §8.1 的 B1-1
-- 依据：docs/architecture/ARCH-9-成本核算升级设计.md §3.1
--       （该节 DDL 已在真库跑通并 ROLLBACK，见
--         docs/evidence/2026-11-10-arch9-3-cost-ddl/01_cost_ddl_check.sql 与 .out）
--
-- 本卡范围：**只有** biz_lot（批次主数据）+ biz_cost_object（成本对象）两张表、
--           它们的索引 / CHECK / 注释，以及本迁移末尾的自检断言。
--           归集四表（collect / labor / overhead / rate）属 B1-2（V0371），
--           分摊两表与期间两表属 B1-3，二期三表属 B1-4 —— 都不在本文件里。
--
-- # 号段为什么是 V0370，而不是设计稿 §8.1 写的 V0368
--   设计稿写 B1-1=V0368 / B1-2=V0369，但这两个号**已经被别的卡用掉了**：
--     V0368__receipt_refund_page_and_price_page（收款退款页 + 价格页）
--     V0369__perf_list_order_indexes（N-8 性能基线排序索引）
--   nik 的迁移加载器对版本号做**跨目录全局唯一**校验
--   （backend/vendor/nik-db/src/migrate/loader.rs::load_from_sources "版本号跨目录全局唯一"），
--   重号会让服务启动直接报错；AGENTS 约定 6 也要求「动手前先 ls 核对号段、已应用迁移不可改」。
--   故本卡顺延到下一个空号 V0370，后续成本卡从 V0371 起排（偏差已写进卡内汇报）。
--
-- # 为什么必须有 biz_lot（现状实测，不是「感觉缺」）
--   库里原先**没有**批次主数据，批号只是三处自由文本列：
--     biz_stock_in_item.lot_no      VARCHAR(64)   (V0145:225)
--     biz_stock_out_item.lot_no     VARCHAR(64)   (V0203:141)
--     biz_material_serial.batch_no  VARCHAR(64)   (V0138:80)
--   同一批货写法不同即断链（空串 / 大小写 / 全半角），分批法（成本对象=生产批）
--   就答不出「这批料进了哪个筐」。
--   **刻意不给上面三处加物理外键**：存量 lot_no 质量不可控，强 FK 会让历史单据
--   连 UPDATE 都做不了；校验交给 handler + 巡检报告（设计 §3.1 ① 已定稿）。
--
-- # biz_cost_object 的三种 object_type
--   BATCH 一期实现（按生产批，可携带 source_doc_no = 客户订单号 ⇒ 覆盖「按订单生产」）
--   ORDER / PRODUCT 值域先留着（三期接 MES 工单 / 品种法），**一期只写 BATCH**。
--   约束 ck_biz_cost_object_type 现在就把三个值都放行，避免三期再加约束要动存量行。
--
-- # 幂等约定
--   全部对象用 CREATE TABLE / INDEX **IF NOT EXISTS**，注释用 CREATE OR REPLACE 语义的
--   COMMENT ON（重复执行结果不变）；自检探针行在断言后立即删除，重复执行零残留。
-- ============================================================================

-- ----------------------------------------------------------------------------
-- ① biz_lot —— 批次主数据（采购批 / 生产批共用一张表，靠 source_type 区分）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_lot (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    lot_no          VARCHAR(64)   NOT NULL,                          -- 批号（业务键，物料级唯一）
    material_id     UUID          NOT NULL,                          -- 物料（弱引用 biz_material.id，不建 FK）
    source_type     VARCHAR(16)   NOT NULL DEFAULT 'MANUAL',         -- PURCHASE 采购批 / PRODUCTION 生产批 / OPENING 期初 / MANUAL 手工建
    source_doc_type VARCHAR(24),                                     -- 来源单据类型：STOCK_IN / COST_PERIOD / 空
    source_doc_id   UUID,                                            -- 来源单据 id
    source_doc_no   VARCHAR(32),                                     -- 来源单号快照（审计口径，不 join）
    supplier_id     UUID,                                            -- 采购批的供应商（弱引用 biz_customer/supplier，不建 FK）
    prod_date       DATE,                                            -- 生产日期 / 到货日期
    expire_date     DATE,                                            -- 失效日期（食品医药用）
    qc_status       VARCHAR(12)   NOT NULL DEFAULT 'NA',             -- NA 不适用 / PENDING 待检 / PASSED 合格 / REJECTED 不合格
    status          SMALLINT      NOT NULL DEFAULT 1,                -- 1 启用 / 0 停用
    remark          TEXT,
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by       UUID,
    update_by       UUID,
    deleted_at      TIMESTAMPTZ,
    CONSTRAINT ck_biz_lot_source CHECK (source_type IN ('PURCHASE','PRODUCTION','OPENING','MANUAL')),
    CONSTRAINT ck_biz_lot_qc     CHECK (qc_status IN ('NA','PENDING','PASSED','REJECTED')),
    CONSTRAINT ck_biz_lot_status CHECK (status IN (0,1)),
    CONSTRAINT ck_biz_lot_dates  CHECK (expire_date IS NULL OR prod_date IS NULL OR expire_date >= prod_date)
);

COMMENT ON TABLE  biz_lot IS '批次主数据（物料级批号）：分批法的批次锚点；采购批 / 生产批共用一张表，靠 source_type 区分';
COMMENT ON COLUMN biz_lot.lot_no IS '批号，与 biz_stock_in_item.lot_no / biz_stock_out_item.lot_no / biz_material_serial.batch_no 同字面值；物料内唯一';
COMMENT ON COLUMN biz_lot.material_id IS '物料 id（弱引用 biz_material.id，刻意不建 FK：存量批号质量不可控）';
COMMENT ON COLUMN biz_lot.source_type IS '来源类型：PURCHASE 采购批 / PRODUCTION 生产批 / OPENING 期初 / MANUAL 手工建';
COMMENT ON COLUMN biz_lot.source_doc_no IS '来源单号快照：历史/审计类字段，按 AGENTS 约定 13 存快照而非 join';
COMMENT ON COLUMN biz_lot.qc_status IS '质检状态：NA / PENDING / PASSED / REJECTED';

-- 唯一键必须带软删条件：同一物料同一批号允许「删除后重建」，但活行不许重复
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_lot_material_no
    ON biz_lot (material_id, lot_no) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_lot_no     ON biz_lot (lot_no);
CREATE INDEX IF NOT EXISTS idx_biz_lot_source ON biz_lot (source_doc_type, source_doc_id);

-- ----------------------------------------------------------------------------
-- ② biz_cost_object —— 成本对象（一期：生产批）
--    成本对象 = 「料工费往哪儿归集」的那个筐。分批法里这个筐就是一批货。
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_object (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    object_no           VARCHAR(32)   NOT NULL,                      -- 成本对象号（生产批号）：CB-yyyyMMdd-000001
    object_type         VARCHAR(12)   NOT NULL DEFAULT 'BATCH',      -- BATCH / ORDER / PRODUCT（一期只写 BATCH）
    product_id          UUID          NOT NULL,                      -- 产出品物料（弱引用 biz_material.id）
    lot_no              VARCHAR(64),                                 -- 产出批号（完工入库时写 biz_lot / 入库明细 lot_no）
    target_warehouse_id UUID          NOT NULL,                      -- 完工入库仓（弱引用 biz_warehouse.id）
    target_location_id  UUID,                                        -- 完工入库库位（可空，弱引用 biz_inventory_location）
    plan_qty            NUMERIC(18,4) NOT NULL,                      -- 计划产量
    completed_qty       NUMERIC(18,4) NOT NULL DEFAULT 0,            -- 累计完工数量（结算时回写）
    wip_qty             NUMERIC(18,4) NOT NULL DEFAULT 0,            -- 期末在制数量（结算时手填）
    completion_pct      NUMERIC(5,2)  NOT NULL DEFAULT 0,            -- 期末在制完工进度%（约当产量用）
    material_input_mode VARCHAR(16)   NOT NULL DEFAULT 'ONE_TIME',   -- ONE_TIME 开工一次投入（一期唯一实现）/ AS_PROGRESS 随加工投入
    source_doc_no       VARCHAR(32),                                 -- 上游订单号（无 MES 时手填客户订单/生产通知单号）
    customer_id         UUID,                                        -- 按订单生产时的客户（弱引用 biz_customer.id）
    workshop_dept_id    UUID          NOT NULL,                      -- 生产车间（弱引用 sys_dept.id）
    workshop_dept_name  VARCHAR(120)  NOT NULL,                      -- 车间名快照（审计/历史口径，AGENTS 约定 13）
    start_date          DATE          NOT NULL,                      -- 开工日期
    due_date            DATE,                                        -- 计划完工日期
    status              VARCHAR(12)   NOT NULL DEFAULT 'OPEN',       -- OPEN 在产 / CLOSED 关批 / CANCELLED 作废
    closed_at           TIMESTAMPTZ,
    closed_by           UUID,
    remark              TEXT,
    created_at          TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by           UUID,
    update_by           UUID,
    deleted_at          TIMESTAMPTZ,
    CONSTRAINT ck_biz_cost_object_type   CHECK (object_type IN ('BATCH','ORDER','PRODUCT')),
    CONSTRAINT ck_biz_cost_object_status CHECK (status IN ('OPEN','CLOSED','CANCELLED')),
    CONSTRAINT ck_biz_cost_object_input  CHECK (material_input_mode IN ('ONE_TIME','AS_PROGRESS')),
    CONSTRAINT ck_biz_cost_object_qty    CHECK (plan_qty > 0 AND completed_qty >= 0 AND wip_qty >= 0),
    CONSTRAINT ck_biz_cost_object_pct    CHECK (completion_pct >= 0 AND completion_pct <= 100),
    CONSTRAINT ck_biz_cost_object_dates  CHECK (due_date IS NULL OR due_date >= start_date),
    -- 关批必须有时间戳：否则「已关批」的对象 statle 在 OPEN/CLOSED 之间说不清谁改的
    CONSTRAINT ck_biz_cost_object_closed CHECK (status <> 'CLOSED' OR closed_at IS NOT NULL)
);

COMMENT ON TABLE  biz_cost_object IS '成本对象：料工费归集主体。一期=BATCH（生产批），ORDER/PRODUCT 值域预留给三期';
COMMENT ON COLUMN biz_cost_object.object_no IS '成本对象号（生产批号）：CB-yyyyMMdd-000001，取 biz_serial_no 号段';
COMMENT ON COLUMN biz_cost_object.object_type IS '对象类型：BATCH 一期 / ORDER、PRODUCT 三期预留';
COMMENT ON COLUMN biz_cost_object.wip_qty IS '期末在制数量，结算时必填（无 MES 时无法自动采集，见设计 §4.5 数据源）';
COMMENT ON COLUMN biz_cost_object.completion_pct IS '期末在制完工进度%，约当产量法用；材料一次投入时材料约当按 100% 另算';
COMMENT ON COLUMN biz_cost_object.workshop_dept_name IS '车间名快照：成本记录属审计/历史口径';
COMMENT ON COLUMN biz_cost_object.lot_no IS '产出批号：结算生成完工入库时作为 lot_no 写入入库明细，并自动建 biz_lot 行（source_type=PRODUCTION）';

-- 唯一键带软删条件（同 biz_lot）：作废的成本对象号可以再建一次，活行不许重号
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_object_no
    ON biz_cost_object (object_no) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_cost_object_product  ON biz_cost_object (product_id, status);
CREATE INDEX IF NOT EXISTS idx_biz_cost_object_workshop ON biz_cost_object (workshop_dept_id, start_date DESC);
CREATE INDEX IF NOT EXISTS idx_biz_cost_object_lot      ON biz_cost_object (lot_no) WHERE lot_no IS NOT NULL;

-- ----------------------------------------------------------------------------
-- ③ 自检（写进迁移，部署即校验；失败即整笔回滚 ⇒ 不存在「半建好的表」）
--
--    分两类断言：
--      A. 形态：表 / 索引 / CHECK 约束按名字和谓词核对（防「表建了但唯一键写成
--         全局唯一」「漏掉软删条件」这类静默错误 —— 它们不会报错，只会以后算错账）。
--      B. 行为：真的插两条同键活行必须被拦；软删行不占键；关批无时间戳必须被拦；
--         plan_qty=0 必须被拦。探针行断言后立即删除，重复执行零残留。
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    v_mat  UUID := '00000000-0000-0000-0000-0000000c0570';
    v_wh   UUID := '00000000-0000-0000-0000-0000000c0571';
    v_dept UUID := '00000000-0000-0000-0000-0000000c0572';
    v_idx  TEXT;
    v_def  TEXT;
    v_n    INT;
    v_hit  BOOLEAN;
BEGIN
    -- ===== A. 形态 =====
    IF to_regclass('public.biz_lot') IS NULL THEN
        RAISE EXCEPTION 'V0370 自检失败：biz_lot 未建出来';
    END IF;
    IF to_regclass('public.biz_cost_object') IS NULL THEN
        RAISE EXCEPTION 'V0370 自检失败：biz_cost_object 未建出来';
    END IF;

    -- 唯一键必须是「唯一 + 带 deleted_at IS NULL 的部分索引」
    SELECT indexdef INTO v_def FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_lot_material_no';
    IF v_def IS NULL THEN
        RAISE EXCEPTION 'V0370 自检失败：uk_biz_lot_material_no 不存在';
    END IF;
    IF v_def NOT LIKE 'CREATE UNIQUE%' THEN
        RAISE EXCEPTION 'V0370 自检失败：uk_biz_lot_material_no 不是 UNIQUE 索引：%', v_def;
    END IF;
    IF replace(v_def, ' ', '') NOT LIKE '%WHERE(deleted_atISNULL)%' THEN
        RAISE EXCEPTION 'V0370 自检失败：uk_biz_lot_material_no 缺软删条件（软删后同批号无法重建）：%', v_def;
    END IF;

    SELECT indexdef INTO v_def FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_cost_object_no';
    IF v_def IS NULL THEN
        RAISE EXCEPTION 'V0370 自检失败：uk_biz_cost_object_no 不存在';
    END IF;
    IF v_def NOT LIKE 'CREATE UNIQUE%' THEN
        RAISE EXCEPTION 'V0370 自检失败：uk_biz_cost_object_no 不是 UNIQUE 索引：%', v_def;
    END IF;
    IF replace(v_def, ' ', '') NOT LIKE '%WHERE(deleted_atISNULL)%' THEN
        RAISE EXCEPTION 'V0370 自检失败：uk_biz_cost_object_no 缺软删条件：%', v_def;
    END IF;

    -- CHECK 约束按名单核对（漏一个 = 一条脏数据入口）
    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE conrelid = 'biz_lot'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_lot_source','ck_biz_lot_qc','ck_biz_lot_status','ck_biz_lot_dates');
    IF v_n <> 4 THEN
        RAISE EXCEPTION 'V0370 自检失败：biz_lot 的 4 条 CHECK 约束只有 % 条落库', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE conrelid = 'biz_cost_object'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_cost_object_type','ck_biz_cost_object_status','ck_biz_cost_object_input',
                       'ck_biz_cost_object_qty','ck_biz_cost_object_pct','ck_biz_cost_object_dates',
                       'ck_biz_cost_object_closed');
    IF v_n <> 7 THEN
        RAISE EXCEPTION 'V0370 自检失败：biz_cost_object 的 7 条 CHECK 约束只有 % 条落库', v_n;
    END IF;

    -- 主键在，且没有残留的同名旧索引
    SELECT count(*) INTO v_n FROM pg_indexes
     WHERE schemaname = 'public' AND tablename IN ('biz_lot','biz_cost_object')
       AND indexname IN ('uk_biz_lot_material_no','uk_biz_cost_object_no');
    IF v_n <> 2 THEN
        RAISE EXCEPTION 'V0370 自检失败：两条唯一索引应有 2 个，实际 %', v_n;
    END IF;

    -- ===== B. 行为（探针行，断言后必删） =====

    -- B1 软删行不占唯一键：同 (material_id, lot_no) 允许「一条活行 + 若干软删行」
    INSERT INTO biz_lot (lot_no, material_id, deleted_at) VALUES ('SELFCHECK-LOT', v_mat, NULL);
    INSERT INTO biz_lot (lot_no, material_id, deleted_at) VALUES ('SELFCHECK-LOT', v_mat, now());
    SELECT count(*) INTO v_n FROM biz_lot WHERE material_id = v_mat;
    IF v_n <> 2 THEN
        RAISE EXCEPTION 'V0370 自检失败：软删行不该占用唯一键（期望 2 行，实际 %）', v_n;
    END IF;

    -- B2 第二条**活行**必须被唯一键拦下
    v_hit := false;
    BEGIN
        INSERT INTO biz_lot (lot_no, material_id) VALUES ('SELFCHECK-LOT', v_mat);
        v_hit := true;
    EXCEPTION WHEN unique_violation THEN
        NULL;   -- 期望路径
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0370 自检失败：同物料同批号写入第二条活行竟然成功（唯一键未生效）';
    END IF;

    -- B3 CHECK 真的在拦：期初/手工批的 qc_status 值域、失效日期不得早于生产日期
    v_hit := false;
    BEGIN
        INSERT INTO biz_lot (lot_no, material_id, qc_status) VALUES ('SELFCHECK-LOT2', v_mat, 'GOOD');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0370 自检失败：qc_status 值域外取值写入成功（ck_biz_lot_qc 未生效）';
    END IF;

    v_hit := false;
    BEGIN
        INSERT INTO biz_lot (lot_no, material_id, prod_date, expire_date)
        VALUES ('SELFCHECK-LOT3', v_mat, DATE '2026-01-10', DATE '2026-01-01');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0370 自检失败：失效日期早于生产日期写入成功（ck_biz_lot_dates 未生效）';
    END IF;

    -- B4 成本对象：同 object_no 活行被拦、软删后可重建
    INSERT INTO biz_cost_object (object_no, product_id, target_warehouse_id, plan_qty,
                                 workshop_dept_id, workshop_dept_name, start_date, deleted_at)
    VALUES ('SELFCHECK-CB-1', v_mat, v_wh, 100, v_dept, '自检车间', DATE '2026-01-01', NULL);
    INSERT INTO biz_cost_object (object_no, product_id, target_warehouse_id, plan_qty,
                                 workshop_dept_id, workshop_dept_name, start_date, deleted_at)
    VALUES ('SELFCHECK-CB-1', v_mat, v_wh, 100, v_dept, '自检车间', DATE '2026-01-01', now());

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_object (object_no, product_id, target_warehouse_id, plan_qty,
                                     workshop_dept_id, workshop_dept_name, start_date)
        VALUES ('SELFCHECK-CB-1', v_mat, v_wh, 100, v_dept, '自检车间', DATE '2026-01-01');
        v_hit := true;
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0370 自检失败：同 object_no 写入第二条活行竟然成功（uk_biz_cost_object_no 未生效）';
    END IF;

    -- B5 关批必须有 closed_at（死单防线：CLOSED 无时间戳 = 谁关的说不清）
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_object (object_no, product_id, target_warehouse_id, plan_qty,
                                     workshop_dept_id, workshop_dept_name, start_date, status)
        VALUES ('SELFCHECK-CB-2', v_mat, v_wh, 100, v_dept, '自检车间', DATE '2026-01-01', 'CLOSED');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0370 自检失败：status=CLOSED 且 closed_at 为空竟然写入成功（ck_biz_cost_object_closed 未生效）';
    END IF;

    -- B6 计划产量必须 > 0（0 或负数会让约当产量除零）
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_object (object_no, product_id, target_warehouse_id, plan_qty,
                                     workshop_dept_id, workshop_dept_name, start_date)
        VALUES ('SELFCHECK-CB-3', v_mat, v_wh, 0, v_dept, '自检车间', DATE '2026-01-01');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0370 自检失败：plan_qty=0 写入成功（ck_biz_cost_object_qty 未生效）';
    END IF;

    -- ===== 清理探针（重复执行零残留） =====
    DELETE FROM biz_cost_object WHERE product_id = v_mat;
    DELETE FROM biz_lot         WHERE material_id = v_mat;

    SELECT count(*) INTO v_n FROM biz_lot WHERE material_id = v_mat;
    SELECT v_n + count(*) INTO v_n FROM biz_cost_object WHERE product_id = v_mat;
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0370 自检失败：探针行未清理干净（残留 % 行）', v_n;
    END IF;

    RAISE NOTICE 'V0370 完成：biz_lot + biz_cost_object 建成，8 条形态断言 + 6 条行为断言全过，探针已清理';
END $$;
