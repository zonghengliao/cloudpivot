-- ============================================================================
-- V0382：完工入库（PRODUCTION）解除「必须挂采购单」的库层矛盾 + 自检
--
-- 卡：t_5f3f2da5（COST-B3-4【成本】结算：完工入库）—— 拆卡清单 §8.1 的 B3-4
-- 依据：docs/architecture/ARCH-9-成本核算升级设计.md §3.3 ② / §4.7.4 / §4.7.6 / §4.8 ③
--
-- # 本迁移解决的是**两条既有约束互相打架**（V0376 头注释「已知开放问题」留着的那条）
--
--   V0376 的 §3.3 逐字约束：
--     ck_biz_si_production      PRODUCTION ⇒ cost_object_id / cost_period_id 非空、
--                                             supplier_id IS NULL、is_sourceless = FALSE
--   V0199 的存量约束：
--     ck_biz_si_sourceless_pair (is_sourceless = (source_order_id IS NULL)) IS TRUE
--
--   两条叠加 ⇒ 「PRODUCTION 且 is_sourceless = FALSE」**必然**要求 source_order_id 非空，
--   即一张完工入库单必须挂一张采购单。而 §4.7.4 / §4.8 的完工入库是**结算批次在同一
--   事务内直接生成**的，根本没有采购单 —— 于是「无采购单的完工入库」被库层拦死：
--     ERROR: new row for relation "biz_stock_in" violates check constraint
--            "ck_biz_si_sourceless_pair"
--   （B3-4 开工时在演练库 costb31_drill 上实测复现，见卡内证据 01_psql_probe.out）
--
-- # 修法（V0376 头注释给出的最小修法，**不改** §3.3 的 ck_biz_si_production）
--
--   把 ck_biz_si_sourceless_pair 的语义按 in_type 分叉：
--     PRODUCTION → 完工入库永远不是「无源单」（is_sourceless = FALSE），
--                  且**不带**采购单（source_order_id IS NULL）；
--     其余类型   → 语义一字不改（is_sourceless = (source_order_id IS NULL)）。
--
--   为什么 PRODUCTION 要**强制** source_order_id IS NULL（而不是「随便」）：
--   §4.8 的结算路径没有采购单，来源是 cost_period_id（V0376 已给该列 + 局部索引）；
--   允许 PRODUCTION 挂采购单只会让「这批完工货是哪张采购单收的」这种假追溯重新长出来。
--
-- # 存量数据一个不改
--   · CHECK 只**放宽**（原语义 ⊂ 新语义：「source_order_id IS NULL AND is_sourceless」
--     在 PRODUCTION 外仍然被 ELSE 分支原样钉死）；
--   · V0376 的自检断言「迁移后 PRODUCTION 行数 = 0」当时通过 ⇒ 生产库上没有任何一行
--     需要被重新校验（新增的 PRODUCTION 行由 B3-4 的结算代码写入，天然满足）；
--   · 迁移本身不 INSERT/UPDATE 任何业务行（探针行在同一迁移内删净）。
--
-- # 幂等
--   `DROP CONSTRAINT IF EXISTS` + `ADD CONSTRAINT`（PG 没有 ADD CONSTRAINT IF NOT EXISTS）。
--   重复执行结果不变：约束定义每次都落到同一份文本（`pg_get_constraintdef` 逐字相同）。
--
-- # 回滚（down）
--   先断言「库里没有 PRODUCTION 单据」→ 再删新约束 → 装回 V0199 的原定义。
--   顺序不能反：存量 PRODUCTION 行（无采购单）一存在，原定义就加不回去。
-- ============================================================================

-- ---------- ① 换约束：PRODUCTION 走「非无源 + 无采购单」分支，其余类型语义不变 ----------
ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_sourceless_pair;
ALTER TABLE biz_stock_in ADD CONSTRAINT ck_biz_si_sourceless_pair
    CHECK ((CASE
                WHEN in_type = 'PRODUCTION'
                    THEN (source_order_id IS NULL AND is_sourceless = FALSE)
                ELSE (is_sourceless = (source_order_id IS NULL))
            END) IS TRUE);

COMMENT ON COLUMN biz_stock_in.is_sourceless IS
    '无源单据标记（A4）：true = 不绑采购单（期初/复制/其他，E4 情形），由建单时显式声明；'
    '与 source_order_id 由 ck_biz_si_sourceless_pair 钉死。V0382 起 PRODUCTION（完工入库）'
    '另有分支：恒为 FALSE 且 source_order_id 恒为 NULL —— 它的来源是 cost_period_id，不是采购单';

-- ============================================================================
-- 自检 A：形态断言（约束存在 + 双分支 + PRODUCTION 外的原语义没丢）
-- ============================================================================
DO $$
DECLARE
    def    TEXT;
    n_prod BIGINT;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO def
      FROM pg_constraint
     WHERE conname = 'ck_biz_si_sourceless_pair' AND conrelid = 'public.biz_stock_in'::regclass;
    IF def IS NULL THEN
        RAISE EXCEPTION 'V0382 自检失败：ck_biz_si_sourceless_pair 不存在（V0199 未落或已被回滚）';
    END IF;

    -- ① 必须按 in_type 分叉（PRODUCTION 分支存在）
    IF def NOT LIKE '%PRODUCTION%' THEN
        RAISE EXCEPTION 'V0382 自检失败：约束没有 PRODUCTION 分支（实际 %）', def;
    END IF;
    -- ② PRODUCTION 分支的两个要素：不带采购单 + 非无源
    IF def NOT LIKE '%source_order_id IS NULL%' THEN
        RAISE EXCEPTION 'V0382 自检失败：PRODUCTION 分支没有「不带采购单」（实际 %）', def;
    END IF;
    IF def NOT LIKE '%is_sourceless%' THEN
        RAISE EXCEPTION 'V0382 自检失败：约束没有引用 is_sourceless（实际 %）', def;
    END IF;
    -- ③ ELSE 分支必须还在（其余类型的原语义不许被这次放宽带走）
    IF def NOT LIKE '%ELSE%' THEN
        RAISE EXCEPTION 'V0382 自检失败：约束丢了 ELSE 分支（存量四类单据的口径会被放宽，实际 %）', def;
    END IF;

    -- ④ 存量零改动：迁移自己不许造 PRODUCTION 行
    SELECT count(*) INTO n_prod FROM biz_stock_in WHERE in_type = 'PRODUCTION';
    RAISE NOTICE 'V0382 形态断言通过：双分支约束 + ELSE 原样；当前 PRODUCTION 行 = %（本迁移不写业务行）', n_prod;
END
$$;

-- ============================================================================
-- 自检 B：行为断言（★ 卡面判据「入库单类型 PRODUCTION」的库层活体验证）
--
-- 每个探针在 BEGIN…EXCEPTION 子块里跑，`inserted` 区分「真写进去了」与「被约束拦」，
-- 探针行一律删净（重复执行不攒垃圾行）。
-- ============================================================================
DO $$
DECLARE
    inserted BOOLEAN;
    err      TEXT;
    leftover BIGINT;
BEGIN
    DELETE FROM biz_stock_in WHERE in_no LIKE 'V0382-PROBE-%';

    -- P1：★ 完工入库（PRODUCTION + 挂批次与对象 + 无供应商 + 无采购单）—— 必须能写
    --     （B3-4 的结算路径就是这个形状；V0382 之前这里必被 ck_biz_si_sourceless_pair 拦）
    inserted := FALSE; err := NULL;
    BEGIN
        INSERT INTO biz_stock_in (in_no, in_type, warehouse_id, in_date, status,
                                  cost_object_id, cost_period_id, is_sourceless)
        VALUES ('V0382-PROBE-P1', 'PRODUCTION', gen_random_uuid(), DATE '2026-11-11', 'POSTED',
                gen_random_uuid(), gen_random_uuid(), FALSE);
        inserted := TRUE;
    EXCEPTION WHEN OTHERS THEN err := SQLERRM;
    END;
    IF NOT inserted THEN
        RAISE EXCEPTION 'V0382 自检失败：无采购单的完工入库仍写不进去（%）—— 结算路径被库层堵死', err;
    END IF;

    -- P2：PRODUCTION 且 is_sourceless = TRUE —— 必须被拒（ck_biz_si_production + 本约束双保险）
    inserted := FALSE; err := NULL;
    BEGIN
        INSERT INTO biz_stock_in (in_no, in_type, warehouse_id, in_date, status,
                                  cost_object_id, cost_period_id, is_sourceless)
        VALUES ('V0382-PROBE-P2', 'PRODUCTION', gen_random_uuid(), DATE '2026-11-11', 'POSTED',
                gen_random_uuid(), gen_random_uuid(), TRUE);
        inserted := TRUE;
    EXCEPTION WHEN OTHERS THEN err := SQLERRM;
    END;
    IF inserted THEN
        RAISE EXCEPTION 'V0382 自检失败：PRODUCTION 且 is_sourceless = TRUE 写入成功（本约束的 PRODUCTION 分支失效）';
    END IF;

    -- P3：PRODUCTION 挂采购单 —— 必须被拒（§4.7.4 的完工入库没有采购单；挂上就是假追溯）
    inserted := FALSE; err := NULL;
    BEGIN
        INSERT INTO biz_stock_in (in_no, in_type, warehouse_id, in_date, status,
                                  cost_object_id, cost_period_id, source_order_id, is_sourceless)
        VALUES ('V0382-PROBE-P3', 'PRODUCTION', gen_random_uuid(), DATE '2026-11-11', 'POSTED',
                gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), FALSE);
        inserted := TRUE;
    EXCEPTION WHEN OTHERS THEN err := SQLERRM;
    END;
    IF inserted THEN
        RAISE EXCEPTION 'V0382 自检失败：PRODUCTION 带 source_order_id 写入成功（应被 PRODUCTION 分支拦下）';
    END IF;

    -- P4：存量口径 —— OTHER + 无采购单 + is_sourceless = TRUE 必须照旧能写
    inserted := FALSE; err := NULL;
    BEGIN
        INSERT INTO biz_stock_in (in_no, in_type, warehouse_id, in_date, status, is_sourceless)
        VALUES ('V0382-PROBE-P4', 'OTHER', gen_random_uuid(), DATE '2026-11-11', 'DRAFT', TRUE);
        inserted := TRUE;
    EXCEPTION WHEN OTHERS THEN err := SQLERRM;
    END;
    IF NOT inserted THEN
        RAISE EXCEPTION 'V0382 自检失败：无源其他入库（存量口径）写不进去了（%）—— 新约束误伤历史路径', err;
    END IF;

    -- P5：存量口径 —— OTHER + 有 source_order_id + is_sourceless = FALSE 必须照旧能写
    inserted := FALSE; err := NULL;
    BEGIN
        INSERT INTO biz_stock_in (in_no, in_type, warehouse_id, in_date, status,
                                  source_order_id, is_sourceless)
        VALUES ('V0382-PROBE-P5', 'OTHER', gen_random_uuid(), DATE '2026-11-11', 'DRAFT',
                gen_random_uuid(), FALSE);
        inserted := TRUE;
    EXCEPTION WHEN OTHERS THEN err := SQLERRM;
    END;
    IF NOT inserted THEN
        RAISE EXCEPTION 'V0382 自检失败：有来源的其他入库（存量口径）写不进去了（%）', err;
    END IF;

    -- P6：存量口径 —— OTHER + 无采购单 + is_sourceless = FALSE 必须仍被拒（恒等式没被放宽）
    inserted := FALSE; err := NULL;
    BEGIN
        INSERT INTO biz_stock_in (in_no, in_type, warehouse_id, in_date, status, is_sourceless)
        VALUES ('V0382-PROBE-P6', 'OTHER', gen_random_uuid(), DATE '2026-11-11', 'DRAFT', FALSE);
        inserted := TRUE;
    EXCEPTION WHEN OTHERS THEN err := SQLERRM;
    END;
    IF inserted THEN
        RAISE EXCEPTION 'V0382 自检失败：OTHER + 无采购单 + is_sourceless=FALSE 写入成功（ELSE 分支被放宽了）';
    END IF;

    -- 探针行必须删净
    DELETE FROM biz_stock_in WHERE in_no LIKE 'V0382-PROBE-%';
    SELECT count(*) INTO leftover FROM biz_stock_in WHERE in_no LIKE 'V0382-PROBE-%';
    IF leftover > 0 THEN
        RAISE EXCEPTION 'V0382 自检失败：探针行未清理干净（剩 % 行）', leftover;
    END IF;

    RAISE NOTICE 'V0382 行为断言通过：完工入库(无采购单)可写 / 无源被拒 / 挂采购单被拒 / 存量三向口径不变 / 探针零残留';
END
$$;
