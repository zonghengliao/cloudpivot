-- ============================================================================
-- V0463：标准成本版本表（物料 × 期间）· 版本护栏（M5-08【M5·成本】）
--
-- 卡：t_bda3bb6b（M5-08「标准成本：版本表（物料 × 期间）」含生效/失效）
-- 拆卡依据：docs/planning/20261113-新卡清单.md §M5 的 M5-08 行；业务口径见
--           docs/planning/20261113-全量开发计划.md「2-1 标准成本版本表（物料 × 期间）」。
--
-- ── 本卡为什么**不新建表**（最重要的一条决定）─────────────────────────────
--   标准成本的两张表 V0374（COST-B1-4）就已经建好了，且**一个字节的业务代码都没写**：
--     · biz_cost_standard       卡头（单位标准料/工/费 + 生效区间 + 版本号 + 状态）
--     · biz_cost_standard_item  明细（标准 BOM：单位标准用量 × 标准价，量差的依据）
--   ARCH-9 §3.2 ⑪⑫ 就是为这一步准备的；`biz_cost_variance.standard_id` 还挂着
--   `ON DELETE RESTRICT` 指向 biz_cost_standard —— 再开一张「标准成本版本表」会让
--   差异卡（M5-11…M5-16）面对两张表，且 V0374 那两张变成永久死表。所以本卡是
--   **给既有表补代码 + 补护栏**，不是再画一张表。
--
--   「物料 × 期间」这个说法怎么落到既有结构上：
--     卡头已经有 (material_id, effective_from, effective_to) —— 「哪天起用哪版」。
--     期间（yyyy-MM）是**取数视图**，不是新维度：一个版本可以连月有效，不需要每月
--     复制一张卡。代码层提供 `period_code → [期初, 期末]` 的解析与「某期间末生效版本」
--     查询（`cost_standard_dto::period_window`），期间内切换过版本会被如实提示
--     （`switch_in_period`），而不是悄悄取其一。
--
-- ── 本迁移落的四件护栏（每件都写明为什么必须补）─────────────────────────
--   ① `ex_biz_cost_std_active_window`（EXCLUDE，需 btree_gist）
--        同一物料的**启用**版本生效区间不许重叠：
--        `EXCLUDE USING gist (material_id WITH =, daterange(effective_from, effective_to, '[]') WITH &&)
--         WHERE (deleted_at IS NULL AND status = 1)`
--      为什么：V0374 只锁了「同物料同生效日唯一」（uk_biz_cost_std），那只能防「同一天
--      起两版」；`[01-01, 03-31]` 与 `[02-01, 12-31]` 同时启用它一句话都不说 ⇒
--      「2 月 15 日用哪版的单价算差异」没有确定答案（差异金额可以差出任意多）。
--      区间不重叠才是「任一日期一个真相」，与 V0407（汇率区间）同一套打法。
--      `status = 1` 进谓词是关键：**失效**的版本不占窗口，所以「先失效旧版、再启用新版」
--      这条正路不会被约束挡住（这正是卡面「含生效/失效」要的行为）。
--   ② `uk_biz_cost_std_active_start`（UNIQUE (material_id, effective_from)
--                                     WHERE deleted_at IS NULL AND status = 1）
--      **替代** V0374 的 `uk_biz_cost_std`（同物料同生效日唯一，不带 status 谓词）。
--      为什么要换（本卡踩到的真问题）：旧索引连**失效**版本一起锁，于是
--      「标准过时 → 失效 v1 → 同一天按新口径重建 v2」这条**唯一**的正路走不通
--      （新 v2 与失效的 v1 同生效日 ⇒ 撞 uk_biz_cost_std），用户只能改库或把新版生效日
--      挪到月中（期间对齐就破了）。而它想防的「同一天两个启用版本」已经由 ① 的区间
--      约束覆盖（同起点必然重叠）—— 所以把它收窄成「仅启用行唯一」既不丢防线，又放开了
--      失效→重建。btree_gist 缺失（降级环境）时，本索引就是「同一天不许两个启用版本」
--      的唯一库层防线，因此**必须**用带谓词的版本替代，而不是直接删掉。
--   ③ `uk_biz_cost_std_ver`（UNIQUE (material_id, version_no) WHERE deleted_at IS NULL）
--      版本号是给人看的口径（「11 月用的是第 3 版」），同物料重号等于没有版本号。
--      代码侧自动取 max+1，本索引是并发下的兜底。
--   ④ `idx_biz_cost_std_window`（BTREE (material_id, effective_from, effective_to)）
--      期间/日期定位（按物料取全部版本再判区间、期间末生效版本）走它；
--      gist 排除索引只服务「重叠判定」，不替代这个点查路径。
--
-- ── 降级口径（与 V0407 / V0336 一致，不因扩展缺失让服务起不来）───────────
--   `btree_gist` 是 `uuid` 上 `=` 走 gist 的前提（PG13+ 为 trusted extension，库属主可建）。
--   建不出来时：本迁移只落 ②③④ + 应用层预检（handler 在事务内按物料取 advisory lock
--   后重查重叠 ⇒ 单实例并发仍被挡住），并 RAISE WARNING 说清「库层没有区间护栏」。
--   自检里对应的反向探针会跳过并留 NOTICE，不会假绿。
--
-- ── 幂等 ─────────────────────────────────────────────────────────────────
--   索引 `IF NOT EXISTS`；约束先查 `pg_constraint`；探针用一次性随机物料 id（本表
--   `material_id` 是弱引用、**没有外键**，不需要先造 biz_material 行），
--   末尾把探针行按该 id 删干净并断言残留 0 ⇒ 重复执行零结构变更、零数据变更。
--
-- ── down ────────────────────────────────────────────────────────────────
--   V0463.down.sql：先拆约束、再拆本卡两个索引、**并把 V0374 的 uk_biz_cost_std 建回来**；
--   `btree_gist` 扩展不删（V0336/V0407 同口径：别的对象可能也在用）。版本数据一行不动。
-- ============================================================================

-- ----------------------------------------------------------------------------
-- 0. 前置护栏：V0374 的两张表必须在（缺了说明没跑到 B1-4，本卡不是第一个该跑的人）
-- ----------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.biz_cost_standard') IS NULL
       OR to_regclass('public.biz_cost_standard_item') IS NULL THEN
        RAISE EXCEPTION 'V0463 拒绝执行：biz_cost_standard / biz_cost_standard_item 不存在，请先执行 V0374（COST-B1-4）';
    END IF;
END $$;

-- ----------------------------------------------------------------------------
-- 1. 区间排除约束需要的扩展（PG13+ trusted，库属主可建；失败降级不阻塞启动）
-- ----------------------------------------------------------------------------
DO $$
BEGIN
    BEGIN
        CREATE EXTENSION IF NOT EXISTS btree_gist;
    EXCEPTION WHEN OTHERS THEN
        RAISE WARNING 'V0463: btree_gist 建不出来（%）→ 未建区间排除约束，启用版本重叠只靠应用层（并发窗口见头注释「降级口径」）', SQLERRM;
    END;
END $$;

-- ----------------------------------------------------------------------------
-- 2. 「同物料同生效日」从「所有行唯一」收窄成「仅启用行唯一」（理由见头注释 ②）
--    顺序：先建新索引（部分唯一）→ 再删旧索引。中间那一段两把锁同时存在，
--    不会出现「没锁的空档」；旧索引删掉后 ② 立刻接管。
-- ----------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.uk_biz_cost_std') IS NOT NULL
       AND to_regclass('public.uk_biz_cost_std_active_start') IS NULL THEN
        BEGIN
            CREATE UNIQUE INDEX uk_biz_cost_std_active_start
                ON biz_cost_standard (material_id, effective_from)
                WHERE deleted_at IS NULL AND status = 1;
        EXCEPTION WHEN unique_violation THEN
            RAISE EXCEPTION 'V0463 拒绝执行：biz_cost_standard 里已有「同物料同生效日的多个启用版本」，'
                '先把多余的置失效（status = 0）再重跑：'
                'SELECT material_id, effective_from, count(*) FROM biz_cost_standard '
                'WHERE deleted_at IS NULL AND status = 1 '
                'GROUP BY 1, 2 HAVING count(*) > 1;';
        END;
    ELSIF to_regclass('public.uk_biz_cost_std_active_start') IS NULL THEN
        CREATE UNIQUE INDEX uk_biz_cost_std_active_start
            ON biz_cost_standard (material_id, effective_from)
            WHERE deleted_at IS NULL AND status = 1;
    END IF;
END $$;

COMMENT ON INDEX uk_biz_cost_std_active_start IS
    '标准成本卡：同物料同生效日**只有一个启用版本**（M5-08 替代 V0374 的 uk_biz_cost_std；'
    '失效版本不占名额，所以「失效后同一天重建新版」这条正路走得通）';

-- 旧索引：连失效行一起锁，堵死「失效 → 同日重建」（本卡踩到过），由上面那条替代
DROP INDEX IF EXISTS uk_biz_cost_std;

-- ----------------------------------------------------------------------------
-- 3. 版本身份：同物料版本号唯一（软删后可复用号码 —— 录错卡删掉重来）
-- ----------------------------------------------------------------------------
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_std_ver
    ON biz_cost_standard (material_id, version_no) WHERE deleted_at IS NULL;

COMMENT ON INDEX uk_biz_cost_std_ver IS
    '标准成本卡版本号唯一（同物料不许重号）：版本号是给人看的口径，代码取 max+1，本索引兜并发';

-- ----------------------------------------------------------------------------
-- 4. 定位索引：按物料点查版本区间（期间取版本 / 期间末生效版本 / 同期切换提示）
-- ----------------------------------------------------------------------------
CREATE INDEX IF NOT EXISTS idx_biz_cost_std_window
    ON biz_cost_standard (material_id, effective_from, effective_to) WHERE deleted_at IS NULL;

COMMENT ON INDEX idx_biz_cost_std_window IS
    '标准成本卡按物料定位生效区间（M5-08 期间取数：物料 × 期间 → 版本）';

-- ----------------------------------------------------------------------------
-- 5. 库级防重叠：同一物料「任一日期只有一个启用版本」
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    v_has_gist boolean;
BEGIN
    SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'btree_gist') INTO v_has_gist;
    IF NOT v_has_gist THEN
        RAISE WARNING 'V0463: btree_gist 不可用 ⇒ 未建 ex_biz_cost_std_active_window，启用版本区间重叠只由应用层 advisory lock + 预检 + uk_biz_cost_std_active_start 维持';
    ELSIF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = 'public.biz_cost_standard'::regclass
           AND conname  = 'ex_biz_cost_std_active_window'
    ) THEN
        BEGIN
            ALTER TABLE biz_cost_standard
                ADD CONSTRAINT ex_biz_cost_std_active_window
                EXCLUDE USING gist (
                    material_id WITH =,
                    daterange(effective_from, effective_to, '[]') WITH &&
                ) WHERE (deleted_at IS NULL AND status = 1);
            RAISE NOTICE 'V0463: 已建 ex_biz_cost_std_active_window（同物料启用版本生效区间不许重叠）';
        EXCEPTION WHEN exclusion_violation THEN
            RAISE EXCEPTION 'V0463 拒绝执行：biz_cost_standard 里已存在「同物料启用版本生效期重叠」的行。'
                '先按下述查询定位并处理（失效其一或收口旧版停止日），再重跑本迁移：'
                'SELECT a.id, b.id FROM biz_cost_standard a JOIN biz_cost_standard b '
                'ON a.id < b.id AND a.material_id = b.material_id '
                'AND a.deleted_at IS NULL AND b.deleted_at IS NULL AND a.status = 1 AND b.status = 1 '
                'AND daterange(a.effective_from, a.effective_to, ''[]'') '
                '&& daterange(b.effective_from, b.effective_to, ''[]'');';
        END;
    END IF;
END $$;

-- ----------------------------------------------------------------------------
-- 6. 自检（结构 + 回归护栏 + 行为反证；探针自清场，重放安全）
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    v_has_gist  boolean;
    v_probe_mat uuid := gen_random_uuid();   -- 弱引用列：不给 material_id 建 FK，探针不需要物料行
    v_probe_two uuid := gen_random_uuid();
    n           integer;
    txt         text;
BEGIN
    -- ===== 结构 1：V0374 的算数防线还在（本卡只换唯一索引，不许把 CHECK 顶掉）=====
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_cost_standard'::regclass
       AND conname IN ('ck_biz_cost_std_sum', 'ck_biz_cost_std_labor', 'ck_biz_cost_std_dates', 'ck_biz_cost_std_ver');
    IF n <> 4 THEN
        RAISE EXCEPTION 'V0463: V0374 的四条卡头 CHECK 只剩 % 条（应为 4）', n;
    END IF;
    IF to_regclass('public.uk_biz_cost_std_item') IS NULL THEN
        RAISE EXCEPTION 'V0463: 明细表 uk_biz_cost_std_item（同卡行号唯一）丢了，不许拆';
    END IF;

    -- ===== 结构 2：本卡的三个索引在位；旧的 uk_biz_cost_std 已让位 =====
    FOR txt IN SELECT unnest(ARRAY['uk_biz_cost_std_active_start', 'uk_biz_cost_std_ver', 'idx_biz_cost_std_window']) LOOP
        IF to_regclass('public.' || txt) IS NULL THEN
            RAISE EXCEPTION 'V0463: 缺索引 %', txt;
        END IF;
    END LOOP;
    SELECT indexdef INTO txt FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_cost_std_active_start';
    IF txt NOT LIKE 'CREATE UNIQUE INDEX%'
       OR txt NOT LIKE '%deleted_at IS NULL%' OR txt NOT LIKE '%status = 1%' THEN
        RAISE EXCEPTION 'V0463: uk_biz_cost_std_active_start 不是「仅启用行」的部分唯一索引：%', txt;
    END IF;
    SELECT indexdef INTO txt FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_cost_std_ver';
    IF txt NOT LIKE 'CREATE UNIQUE INDEX%' OR txt NOT LIKE '%deleted_at IS NULL%' THEN
        RAISE EXCEPTION 'V0463: uk_biz_cost_std_ver 不是「软删后可复用」的部分唯一索引：%', txt;
    END IF;
    IF to_regclass('public.uk_biz_cost_std') IS NOT NULL THEN
        RAISE EXCEPTION 'V0463: 旧的 uk_biz_cost_std 还在（它会连失效行一起锁，堵死「失效后同日重建」）';
    END IF;

    -- ===== 结构 3：排除约束（btree_gist 可用时必须在位，且谓词真的带 status = 1）=====
    SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'btree_gist') INTO v_has_gist;
    IF v_has_gist THEN
        SELECT count(*) INTO n FROM pg_constraint
         WHERE conrelid = 'public.biz_cost_standard'::regclass
           AND conname  = 'ex_biz_cost_std_active_window' AND contype = 'x';
        IF n <> 1 THEN
            RAISE EXCEPTION 'V0463: btree_gist 在，但缺 ex_biz_cost_std_active_window 排除约束';
        END IF;
        SELECT pg_get_constraintdef(oid) INTO txt FROM pg_constraint
         WHERE conrelid = 'public.biz_cost_standard'::regclass
           AND conname = 'ex_biz_cost_std_active_window';
        IF txt NOT LIKE '%daterange%' OR txt NOT LIKE '%&&%'
           OR txt NOT LIKE '%deleted_at IS NULL%' OR txt NOT LIKE '%status = 1%' THEN
            RAISE EXCEPTION 'V0463: ex_biz_cost_std_active_window 定义不对（必须 daterange && 且只锁启用未删行）：%', txt;
        END IF;
    ELSE
        RAISE NOTICE 'V0463: 无 btree_gist ⇒ 跳过排除约束的结构断言与 P1/P2/P4 反向探针';
    END IF;

    -- ===== 行为探针 A：基础行可插（区间 [01-01, 01-31]，启用）=====
    INSERT INTO biz_cost_standard (material_id, version_no, effective_from, effective_to, status, remark)
    VALUES (v_probe_mat, 1, DATE '2026-01-01', DATE '2026-01-31', 1, '__M508_PROBE__');

    IF v_has_gist THEN
        -- ===== 行为探针 B：同物料启用版本插进既有区间 ⇒ 必须被拒 =====
        BEGIN
            INSERT INTO biz_cost_standard (material_id, version_no, effective_from, effective_to, status, remark)
            VALUES (v_probe_mat, 2, DATE '2026-01-15', DATE '2026-02-10', 1, '__M508_PROBE__');
            RAISE EXCEPTION 'V0463: P1 —— 启用版本区间重叠竟然成功（排除约束没生效）';
        EXCEPTION WHEN exclusion_violation THEN
            NULL;   -- 预期路径
        END;
    ELSE
        RAISE NOTICE 'V0463: 跳过 P1 区间重叠探针（无排除约束）';
    END IF;

    -- ===== 行为探针 C：同一区间但 status = 0（失效）⇒ 必须放行 =====
    -- 「含失效」的库层证明①：失效版本不占区间窗口。
    INSERT INTO biz_cost_standard (material_id, version_no, effective_from, effective_to, status, remark)
    VALUES (v_probe_mat, 2, DATE '2026-01-15', DATE '2026-02-10', 0, '__M508_PROBE__');

    -- ===== 行为探针 D：**同物料同生效日**、一条启用一条失效 ⇒ 必须放行 =====
    -- 「含失效」的库层证明②（本卡踩到的真问题）：旧 uk_biz_cost_std 会在这里报
    -- unique_violation，把「失效 v1 → 同一天按新口径重建 v2」堵死；收窄后应放行。
    INSERT INTO biz_cost_standard (material_id, version_no, effective_from, effective_to, status, remark)
    VALUES (v_probe_mat, 3, DATE '2026-01-01', DATE '2026-01-31', 0, '__M508_PROBE__');

    -- ===== 行为探针 E：同物料同生效日的**两个启用**版本 ⇒ 必须被拒（新索引的牙）=====
    BEGIN
        INSERT INTO biz_cost_standard (material_id, version_no, effective_from, effective_to, status, remark)
        VALUES (v_probe_mat, 4, DATE '2026-01-01', DATE '2026-01-05', 1, '__M508_PROBE__');
        RAISE EXCEPTION 'V0463: P4 —— 同商品同生效日两个启用版本竟然成功（uk_biz_cost_std_active_start 没生效）';
    EXCEPTION WHEN unique_violation OR exclusion_violation THEN
        NULL;   -- 预期路径（可能新索引先响、也可能区间约束先响，两者都对）
    END;

    -- ===== 行为探针 F：端点相接不算重叠 ⇒ 必须放行 =====
    INSERT INTO biz_cost_standard (material_id, version_no, effective_from, effective_to, status, remark)
    VALUES (v_probe_mat, 5, DATE '2026-02-01', NULL, 1, '__M508_PROBE__');

    -- ===== 行为探针 G：同物料版本号重复 ⇒ unique_violation（uk_biz_cost_std_ver）=====
    BEGIN
        INSERT INTO biz_cost_standard (material_id, version_no, effective_from, effective_to, status, remark)
        VALUES (v_probe_mat, 1, DATE '2026-05-01', NULL, 0, '__M508_PROBE__');
        RAISE EXCEPTION 'V0463: P5 —— 同物料版本号重复竟然成功（uk_biz_cost_std_ver 没生效）';
    EXCEPTION WHEN unique_violation THEN
        NULL;   -- 预期路径
    END;

    -- ===== 行为探针 H：V0374 的三条算数防线仍在（换索引不许松掉它们）=====
    BEGIN
        INSERT INTO biz_cost_standard
            (material_id, version_no, effective_from, std_material_cost, std_total_cost, status, remark)
        VALUES (v_probe_two, 1, DATE '2026-01-01', 100.000000, 101.000000, 0, '__M508_PROBE__');
        RAISE EXCEPTION 'V0463: P6 —— 标准总成本 ≠ 料+工+费 竟然成功（ck_biz_cost_std_sum 失效）';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 预期路径
    END;

    BEGIN
        INSERT INTO biz_cost_standard
            (material_id, version_no, effective_from, std_labor_hours, std_labor_rate, std_labor_cost, status, remark)
        VALUES (v_probe_two, 1, DATE '2026-01-01', 2.0000, 3.000000, 5.000000, 0, '__M508_PROBE__');
        RAISE EXCEPTION 'V0463: P7 —— 标准人工成本 ≠ round(工时×费率,6) 竟然成功（ck_biz_cost_std_labor 失效）';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 预期路径
    END;

    BEGIN
        INSERT INTO biz_cost_standard (material_id, version_no, effective_from, status, remark)
        VALUES (v_probe_two, 0, DATE '2026-01-01', 0, '__M508_PROBE__');
        RAISE EXCEPTION 'V0463: P8 —— version_no = 0 竟然成功（ck_biz_cost_std_ver 失效）';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 预期路径
    END;

    -- ===== 清场：探针行必须一行不剩（重放零残留）=====
    DELETE FROM biz_cost_standard WHERE material_id IN (v_probe_mat, v_probe_two);
    SELECT count(*) INTO n FROM biz_cost_standard WHERE material_id IN (v_probe_mat, v_probe_two);
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0463: 探针清场失败，仍残留 % 行', n;
    END IF;

    RAISE NOTICE 'V0463 完成：生效行同日唯一（收窄版）+ 版本号唯一 + 区间定位索引 + 启用版本区间排除约束（btree_gist=%），探针 8 条已清场', v_has_gist;
END $$;
