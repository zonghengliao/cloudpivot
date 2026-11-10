-- ============================================================================
-- V0376：完工入库 —— `in_type` 加 `PRODUCTION` + 两列（ARCH-9 §3.3 ②）+ 自检
--
-- 卡：t_b2c9aa3d（COST-B1-5【成本】迁移 V0373/V0374）—— 拆卡清单 §8.1 的 B1-5
--     「入库单加 PRODUCTION 类型 + 两列」
-- 依据：docs/architecture/ARCH-9-成本核算升级设计.md §3.3 ②、§1 D-11、§4.7（结算：完工入库）
--       （§3 DDL 已在真库跑通并 ROLLBACK，见 docs/evidence/2026-11-10-arch9-3-cost-ddl/）
--
-- # 号段：卡面写 V0373/V0374，实际落 V0375（出库加列）+ V0376（本迁移）
--   V0372/V0373 = B1-3，V0374 = B1-4，见 V0375 头注释。
--
-- # 本迁移做三件事（**只改结构，不写业务代码**）
--   ① biz_stock_in 加两列：cost_object_id（哪个成本对象）、cost_period_id（哪个结算批次）；
--   ② ck_biz_si_type 值域从 4 值放宽到 5 值（+ PRODUCTION，设计 D-11：
--      词表 TxnBizType::ProductionIn='PRODUCTION_IN' 早就存在，只差这个值域）；
--   ③ 新增 ck_biz_si_production：PRODUCTION 单**必须**挂对象与批次、且没有供应商、且不是无源单。
--
-- # 存量数据一个不改
--   · 两列都可空、无默认值（不重写表）；
--   · 约束只**放宽**（4 值 ⊂ 5 值），另加的一条只约束 PRODUCTION 行 ⇒ 存量 4 类单据全不受影响；
--   · 迁移本身不 INSERT/UPDATE 任何业务行（自检探针行在同一迁移内删净）。
--
-- # ★ 已知开放问题（本迁移按§3.3 逐字落地，不改设计；需后续卡决策）
--   §3.3 的 ck_biz_si_production 要求 `is_sourceless = FALSE`，而 V0199 的
--   `ck_biz_si_sourceless_pair` 钉死 `is_sourceless = (source_order_id IS NULL)`。
--   两条叠起来 ⇒ 一 PRODUCTION 单**必须**带 source_order_id（采购单）。但 §4.7 的结算
--   根本没有采购单（它由结算批次在同一事务内直接生成）⇒ 现状下「无采购单的完工入库」
--   会被 ck_biz_si_sourceless_pair 拦（自检行为断言 P1/形态断言会把这个事实打进部署日志）。
--   最小修法（留给 B3-4 结算卡或独立迁移，不改 §3.3 的 ck_biz_si_production）：
--     ck_biz_si_sourceless_pair 放宽为
--       CASE WHEN in_type = 'PRODUCTION' THEN is_sourceless = FALSE AND source_order_id IS NULL
--            ELSE (is_sourceless = (source_order_id IS NULL)) END
--   本卡**不自作主张**改 V0199 的约束语义（卡面：设计已定稿，发现设计有误要说明并停下问）。
--
-- # 幂等
--   `ADD COLUMN IF NOT EXISTS` + `CREATE INDEX IF NOT EXISTS` +
--   `DROP CONSTRAINT IF EXISTS` + `ADD CONSTRAINT`（PG 没有 ADD CONSTRAINT IF NOT EXISTS）。
--   重复执行结果不变：值域/约束定义每次都落到同一份文本。
--
-- # 回滚（down）
--   先断言「没有 PRODUCTION 单据」（有就拒绝执行并点名数量）→ 再删新约束 → 回滚值域到 4 值 →
--   删索引 → 删两列。顺序不能反：存量 PRODUCTION 行一存在，旧 4 值 CHECK 就加不回去。
-- ============================================================================

-- ---------- ① 两列（可空、无默认值 ⇒ 不重写表、存量全 NULL） ----------
ALTER TABLE biz_stock_in ADD COLUMN IF NOT EXISTS cost_object_id UUID;
ALTER TABLE biz_stock_in ADD COLUMN IF NOT EXISTS cost_period_id UUID;

-- ---------- ② in_type 值域：4 值 → 5 值（+ PRODUCTION） ----------
ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_type;
ALTER TABLE biz_stock_in ADD CONSTRAINT ck_biz_si_type
    CHECK (in_type IN ('PURCHASE','RETURN','OTHER','SOURCELESS','PRODUCTION'));

-- ---------- ③ 完工入库的自洽约束（设计 §3.3 逐字） ----------
ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_production;
ALTER TABLE biz_stock_in ADD CONSTRAINT ck_biz_si_production
    CHECK (in_type <> 'PRODUCTION'
           OR (cost_object_id IS NOT NULL AND cost_period_id IS NOT NULL
               AND supplier_id IS NULL AND is_sourceless = FALSE));

-- ---------- ④ 局部索引（只索引非空行） ----------
CREATE INDEX IF NOT EXISTS idx_biz_si_cost_obj
    ON biz_stock_in (cost_object_id) WHERE cost_object_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_si_cost_period
    ON biz_stock_in (cost_period_id) WHERE cost_period_id IS NOT NULL;

-- ---------- ⑤ 注释 ----------
COMMENT ON COLUMN biz_stock_in.cost_object_id IS
    '成本对象（弱引用 biz_cost_object.id，不建物理外键）。PRODUCTION 单必填，其余类型应为 NULL';
COMMENT ON COLUMN biz_stock_in.cost_period_id IS
    '结算批次（弱引用 biz_cost_period.id，不建物理外键）。PRODUCTION 单必填；'
    '红冲时按它反查该批次的完工入库单（§4.7 反向场景 ④）';
COMMENT ON COLUMN biz_stock_in.in_type IS
    '入库类型（词表 DocSubtype::IN_TYPES）：PURCHASE 采购入库（有采购单）/ RETURN 退货入库 / '
    'OTHER 其他入库（无采购单、无供应商）/ SOURCELESS 无源入库（无采购单、有供应商，is_sourceless=true）/ '
    'PRODUCTION 完工入库（结算批次生成，V0376 起；必挂 cost_object_id + cost_period_id、无供应商，见 ck_biz_si_production）';

-- ============================================================================
-- 自检 A：形态断言（列 / 值域 / 约束 / 索引 / 存量零改动）
-- ============================================================================
DO $$
DECLARE
    ck_type   TEXT;
    ck_prod   TEXT;
    ck_scope  TEXT;
    col_obj   TEXT;
    col_per   TEXT;
    def_obj   TEXT;
    def_per   TEXT;
    n_prod    BIGINT;
    n_old     BIGINT;
    idx_cnt   BIGINT;
BEGIN
    -- ① 两列存在、类型 UUID、可空、无默认值
    SELECT data_type, coalesce(column_default, '')
      INTO col_obj, def_obj
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_stock_in' AND column_name = 'cost_object_id';
    SELECT data_type, coalesce(column_default, '')
      INTO col_per, def_per
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_stock_in' AND column_name = 'cost_period_id';

    IF col_obj IS NULL OR col_per IS NULL THEN
        RAISE EXCEPTION 'V0376 自检失败：cost_object_id / cost_period_id 没建全（% / %）',
            coalesce(col_obj, '<缺失>'), coalesce(col_per, '<缺失>');
    END IF;
    IF lower(col_obj) <> 'uuid' OR lower(col_per) <> 'uuid' THEN
        RAISE EXCEPTION 'V0376 自检失败：两列类型应为 uuid（实际 % / %）', col_obj, col_per;
    END IF;
    IF def_obj <> '' OR def_per <> '' THEN
        RAISE EXCEPTION 'V0376 自检失败：新列带了默认值（% / %）—— 会触发全表重写', def_obj, def_per;
    END IF;

    -- ② ck_biz_si_type 必须是 5 值、且含 PRODUCTION（放宽，不是替换）
    SELECT pg_get_constraintdef(oid) INTO ck_type
      FROM pg_constraint
     WHERE conname = 'ck_biz_si_type' AND conrelid = 'public.biz_stock_in'::regclass;
    IF ck_type IS NULL THEN
        RAISE EXCEPTION 'V0376 自检失败：ck_biz_si_type 不存在';
    END IF;
    IF ck_type NOT LIKE '%PRODUCTION%' THEN
        RAISE EXCEPTION 'V0376 自检失败：ck_biz_si_type 没包含 PRODUCTION（实际 %）', ck_type;
    END IF;
    IF ck_type NOT LIKE '%PURCHASE%' OR ck_type NOT LIKE '%RETURN%'
       OR ck_type NOT LIKE '%OTHER%' OR ck_type NOT LIKE '%SOURCELESS%' THEN
        RAISE EXCEPTION 'V0376 自检失败：ck_biz_si_type 丢掉了历史值域（实际 %）', ck_type;
    END IF;

    -- ③ ck_biz_si_production 必须覆盖「挂对象 + 挂批次 + 无供应商 + 非无源」四要素
    SELECT pg_get_constraintdef(oid) INTO ck_prod
      FROM pg_constraint
     WHERE conname = 'ck_biz_si_production' AND conrelid = 'public.biz_stock_in'::regclass;
    IF ck_prod IS NULL THEN
        RAISE EXCEPTION 'V0376 自检失败：ck_biz_si_production 没建出来';
    END IF;
    IF ck_prod NOT LIKE '%cost_object_id IS NOT NULL%'
       OR ck_prod NOT LIKE '%cost_period_id IS NOT NULL%'
       OR ck_prod NOT LIKE '%supplier_id IS NULL%'
       OR ck_prod NOT LIKE '%is_sourceless%' THEN
        RAISE EXCEPTION 'V0376 自检失败：ck_biz_si_production 缺要素（实际 %）', ck_prod;
    END IF;

    -- ④ 供应商两向约束必须还在（本卡不动它，但它与 in_type 是同一对象；
    --    真被误删，PRODUCTION 就能带供应商进来，界面上会出现「生产入库有供应商」）
    SELECT pg_get_constraintdef(oid) INTO ck_scope
      FROM pg_constraint
     WHERE conname = 'ck_biz_si_supplier_scope' AND conrelid = 'public.biz_stock_in'::regclass;
    IF ck_scope IS NULL THEN
        RAISE EXCEPTION 'V0376 自检失败：ck_biz_si_supplier_scope 不见了（与 in_type 值域是同一约束族，AGENTS §12 / 设计 §1 注）';
    END IF;
    IF ck_scope NOT LIKE '%supplier_id IS NULL%' THEN
        RAISE EXCEPTION 'V0376 自检失败：ck_biz_si_supplier_scope 没有「非采购/非无源必须无供应商」分支（实际 %）', ck_scope;
    END IF;

    -- ⑤ 两条局部索引
    SELECT count(*) INTO idx_cnt
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('idx_biz_si_cost_obj', 'idx_biz_si_cost_period')
       AND indexdef LIKE '%IS NOT NULL%';
    IF idx_cnt <> 2 THEN
        RAISE EXCEPTION 'V0376 自检失败：两条局部索引只找到 % 条', idx_cnt;
    END IF;

    -- ⑥ 存量数据零改动：迁移自己不许造 PRODUCTION 行，老 4 类单据一个值都不许变
    SELECT count(*) INTO n_prod FROM biz_stock_in WHERE in_type = 'PRODUCTION';
    IF n_prod > 0 THEN
        RAISE EXCEPTION 'V0376 自检失败：迁移后已有 % 张 PRODUCTION 单（本迁移不写业务行）', n_prod;
    END IF;
    SELECT count(*) INTO n_old
      FROM biz_stock_in
     WHERE in_type NOT IN ('PURCHASE','RETURN','OTHER','SOURCELESS','PRODUCTION');
    IF n_old > 0 THEN
        RAISE EXCEPTION 'V0376 自检失败：% 张入库单的 in_type 不在值域内', n_old;
    END IF;

    RAISE NOTICE 'V0376 形态断言通过：两列(UUID 可空无默认) + ck_biz_si_type 5 值 + ck_biz_si_production 四要素 + supplier_scope 未破 + 2 条局部索引 + 存量零改动';
END
$$;

-- ============================================================================
-- 自检 B：行为断言（★ 卡面验收「biz_stock_in.type 允许 PRODUCTION」活体验证）
--
-- 每个探针都在 BEGIN…EXCEPTION 子块里跑，`inserted` 旗标区分「真写进去了」与「被约束拦」，
-- 探针行一律删净；最后一律断言 in_no LIKE 'V0376-PROBE-%' 零残留。
-- ============================================================================
DO $$
DECLARE
    inserted BOOLEAN;
    err      TEXT;
    leftover BIGINT;
BEGIN
    DELETE FROM biz_stock_in WHERE in_no LIKE 'V0376-PROBE-%';

    -- P1：带 source_order_id 的完工入库 —— 必须能写（证明 ck_biz_si_type / _production 接受 PRODUCTION）
    inserted := FALSE; err := NULL;
    BEGIN
        INSERT INTO biz_stock_in (in_no, in_type, warehouse_id, in_date, status,
                                  cost_object_id, cost_period_id, source_order_id, is_sourceless)
        VALUES ('V0376-PROBE-P1', 'PRODUCTION', gen_random_uuid(), DATE '2026-11-10', 'POSTED',
                gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), FALSE);
        inserted := TRUE;
    EXCEPTION WHEN OTHERS THEN err := SQLERRM;
    END;
    IF NOT inserted THEN
        RAISE EXCEPTION 'V0376 自检失败：PRODUCTION 入库单写不进去（%）—— ck_biz_si_type 没真正加进 PRODUCTION', err;
    END IF;
    RAISE NOTICE 'V0376 行为断言：PRODUCTION 完工入库单写入成功 ⇒ in_type 值域已放宽（卡面验收）';

    -- P2：无采购单的完工入库 —— 现状被 V0199 的 ck_biz_si_sourceless_pair 拦（见迁移头「已知开放问题」）
    inserted := FALSE; err := NULL;
    BEGIN
        INSERT INTO biz_stock_in (in_no, in_type, warehouse_id, in_date, status,
                                  cost_object_id, cost_period_id, is_sourceless)
        VALUES ('V0376-PROBE-P2', 'PRODUCTION', gen_random_uuid(), DATE '2026-11-10', 'POSTED',
                gen_random_uuid(), gen_random_uuid(), FALSE);
        inserted := TRUE;
    EXCEPTION WHEN OTHERS THEN err := SQLERRM;
    END;
    IF inserted THEN
        RAISE NOTICE 'V0376 行为断言：无采购单的完工入库写入成功（ck_biz_si_sourceless_pair 已放宽，开放问题已解决）';
    ELSE
        RAISE NOTICE 'V0376 行为断言：无采购单的完工入库被拦（%）。已知开放问题：ck_biz_si_sourceless_pair(V0199) 与 §3.3 ck_biz_si_production 冲突，结算卡（B3-4）前必须用新迁移放宽', err;
    END IF;

    -- P3：PRODUCTION 带供应商 —— 必须被拒（ck_biz_si_supplier_scope / ck_biz_si_production 双保险）
    inserted := FALSE; err := NULL;
    BEGIN
        INSERT INTO biz_stock_in (in_no, in_type, warehouse_id, in_date, status,
                                  cost_object_id, cost_period_id, supplier_id,
                                  source_order_id, is_sourceless)
        VALUES ('V0376-PROBE-P3', 'PRODUCTION', gen_random_uuid(), DATE '2026-11-10', 'POSTED',
                gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), FALSE);
        inserted := TRUE;
    EXCEPTION WHEN OTHERS THEN err := SQLERRM;
    END;
    IF inserted THEN
        DELETE FROM biz_stock_in WHERE in_no = 'V0376-PROBE-P3';
        RAISE EXCEPTION 'V0376 自检失败：PRODUCTION 带供应商写入成功（ck_biz_si_supplier_scope 对 PRODUCTION 失效）';
    END IF;

    -- P4：PRODUCTION 当无源单（is_sourceless=TRUE）—— 必须被拒（ck_biz_si_production）
    inserted := FALSE; err := NULL;
    BEGIN
        INSERT INTO biz_stock_in (in_no, in_type, warehouse_id, in_date, status,
                                  cost_object_id, cost_period_id, is_sourceless)
        VALUES ('V0376-PROBE-P4', 'PRODUCTION', gen_random_uuid(), DATE '2026-11-10', 'POSTED',
                gen_random_uuid(), gen_random_uuid(), TRUE);
        inserted := TRUE;
    EXCEPTION WHEN OTHERS THEN err := SQLERRM;
    END;
    IF inserted THEN
        DELETE FROM biz_stock_in WHERE in_no = 'V0376-PROBE-P4';
        RAISE EXCEPTION 'V0376 自检失败：PRODUCTION 且 is_sourceless=TRUE 写入成功（ck_biz_si_production 失效）';
    END IF;

    -- P5：PRODUCTION 缺 cost_period_id —— 必须被拒（ck_biz_si_production：红冲要按批次反查）
    inserted := FALSE; err := NULL;
    BEGIN
        INSERT INTO biz_stock_in (in_no, in_type, warehouse_id, in_date, status,
                                  cost_object_id, source_order_id, is_sourceless)
        VALUES ('V0376-PROBE-P5', 'PRODUCTION', gen_random_uuid(), DATE '2026-11-10', 'POSTED',
                gen_random_uuid(), gen_random_uuid(), FALSE);
        inserted := TRUE;
    EXCEPTION WHEN OTHERS THEN err := SQLERRM;
    END;
    IF inserted THEN
        DELETE FROM biz_stock_in WHERE in_no = 'V0376-PROBE-P5';
        RAISE EXCEPTION 'V0376 自检失败：PRODUCTION 缺 cost_period_id 写入成功（ck_biz_si_production 失效）';
    END IF;

    -- P6：老口径（PURCHASE + 供应商 + 采购单）必须照旧能写 —— 存量路径零影响
    inserted := FALSE; err := NULL;
    BEGIN
        INSERT INTO biz_stock_in (in_no, in_type, warehouse_id, in_date, status,
                                  supplier_id, source_order_id, is_sourceless)
        VALUES ('V0376-PROBE-P6', 'PURCHASE', gen_random_uuid(), DATE '2026-11-10', 'DRAFT',
                gen_random_uuid(), gen_random_uuid(), FALSE);
        inserted := TRUE;
    EXCEPTION WHEN OTHERS THEN err := SQLERRM;
    END;
    IF NOT inserted THEN
        RAISE EXCEPTION 'V0376 自检失败：采购入库（存量口径）写不进去了（%）—— 新约束误伤了历史路径', err;
    END IF;

    -- 探针行必须删净：迁移重复执行不能往真库里攒垃圾行
    DELETE FROM biz_stock_in WHERE in_no LIKE 'V0376-PROBE-%';
    SELECT count(*) INTO leftover FROM biz_stock_in WHERE in_no LIKE 'V0376-PROBE-%';
    IF leftover > 0 THEN
        RAISE EXCEPTION 'V0376 自检失败：探针行未清理干净（剩 % 行）', leftover;
    END IF;

    RAISE NOTICE 'V0376 行为断言通过：PRODUCTION 可写 / 带供应商被拒 / 无源被拒 / 缺批次被拒 / 采购入库照旧';
END
$$;
