-- ============================================================================
-- V0375：领料出库挂成本对象（`biz_stock_out.cost_object_id`）（ARCH-9 §3.3 ①）+ 自检
--
-- 卡：t_b2c9aa3d（COST-B1-5【成本】迁移 V0373/V0374）—— 拆卡清单 §8.1 的 B1-5
--     「出库单加列（成本对象）」
-- 依据：docs/architecture/ARCH-9-成本核算升级设计.md §3.3 ①、§2「表改造清单」
--       （§3 DDL 已在真库跑通并 ROLLBACK，见 docs/evidence/2026-11-10-arch9-3-cost-ddl/）
--
-- # 号段：卡面写 V0373/V0374，实际落 V0375/V0376
--   V0372/V0373 = B1-3（分摊两表 / 期间两表），V0374 = B1-4（二期三表），
--   本卡顺延为 V0375（本迁移：出库加列）+ V0376（入库加 PRODUCTION + 两列）。
--   nik 迁移加载器对版本号做跨目录全局唯一校验，重号会让服务启动直接失败（AGENTS 约定 6）。
--
-- # 本迁移只做一件事：给 biz_stock_out 加一列「成本对象」，**只加列，不动任何存量值**
--   · 存量行 cost_object_id 全 NULL ⇒ 领料过账借方仍走 6401（§4.3 存量口径），行为零变化；
--   · 只有显式挂了对象的领料单才进成本核算（借 4001.01），这条判断在 handler 里做
--     （B2-1 起的代码卡），迁移只负责把列摆好。
--
-- # 三个刻意的「不做」
--   ① **不设 DEFAULT**：默认值会触发全表重写（设计 §7.2「加可空列，不设默认值」），
--      而且 `DEFAULT gen_random_uuid()` 之流等于给每张历史单据凭空编一个成本对象。
--   ② **不建物理外键**：成本对象是主数据，全库统一弱引用（设计 §3.4；
--      AGENTS 约定 13「主数据关联 join 实时取名」同理），强 FK 会让软删锁死历史单据。
--   ③ **不改 biz_stock_out 的 out_type 值域**：领料口径不变（ISSUE/SCRAP/LOSS/OTHER），
--      本卡只加「挂哪个成本对象」，不新增出库类型。
--
-- # 幂等
--   `ADD COLUMN IF NOT EXISTS` + `CREATE INDEX IF NOT EXISTS`，重复执行结果不变
--   （第二遍零改动，自检仍然通过）。
--
-- # 回滚（down）
--   DROP INDEX + DROP COLUMN，**不删行、不动其他列**；
--   若已有单据挂了成本对象则**拒绝执行并点名数量** —— 否则删列等于静默丢业务数据。
-- ============================================================================

-- ---------- ① 加列 ----------
ALTER TABLE biz_stock_out ADD COLUMN IF NOT EXISTS cost_object_id UUID;

-- ---------- ② 局部索引（只索引非空行：存量行全 NULL，索引极小） ----------
CREATE INDEX IF NOT EXISTS idx_biz_so_cost_obj
    ON biz_stock_out (cost_object_id) WHERE cost_object_id IS NOT NULL;

-- ---------- ③ 列注释（列要能自解释） ----------
COMMENT ON COLUMN biz_stock_out.cost_object_id IS
    '成本对象（弱引用 biz_cost_object.id，不建物理外键）。'
    'NULL = 不进成本核算的存量口径（领料过账借方 6401）；'
    '非 NULL = 领料挂到了成本对象，过账借方走 4001.01（ARCH-9 §4.3）';

-- ---------- ④ 自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    col_type     TEXT;
    col_nullable TEXT;
    col_default  TEXT;
    idx_def      TEXT;
    filled       BIGINT;
    fk_cnt       BIGINT;
BEGIN
    -- ① 列必须存在、类型 UUID、可空、无默认值
    SELECT data_type, is_nullable, coalesce(column_default, '')
      INTO col_type, col_nullable, col_default
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_stock_out'
       AND column_name = 'cost_object_id';

    IF col_type IS NULL THEN
        RAISE EXCEPTION 'V0375 自检失败：biz_stock_out.cost_object_id 没建出来';
    END IF;
    IF lower(col_type) <> 'uuid' THEN
        RAISE EXCEPTION 'V0375 自检失败：cost_object_id 类型是 %（应为 uuid）', col_type;
    END IF;
    IF col_nullable <> 'YES' THEN
        RAISE EXCEPTION 'V0375 自检失败：cost_object_id 不可空 —— 存量行没有值，加 NOT NULL 会直接失败';
    END IF;
    IF col_default <> '' THEN
        RAISE EXCEPTION 'V0375 自检失败：cost_object_id 带了默认值（%）—— 会给历史单据凭空编成本对象，且触发全表重写', col_default;
    END IF;

    -- ② 局部索引必须在，且带「非空」谓词
    SELECT indexdef INTO idx_def
      FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'idx_biz_so_cost_obj';
    IF idx_def IS NULL THEN
        RAISE EXCEPTION 'V0375 自检失败：idx_biz_so_cost_obj 没建出来';
    END IF;
    IF idx_def NOT LIKE '%cost_object_id IS NOT NULL%' THEN
        RAISE EXCEPTION 'V0375 自检失败：idx_biz_so_cost_obj 不是局部索引（实际 %）', idx_def;
    END IF;

    -- ③ 存量数据不受影响：加列后不该有任何一行被填值
    SELECT count(*) INTO filled FROM biz_stock_out WHERE cost_object_id IS NOT NULL;
    IF filled > 0 THEN
        RAISE EXCEPTION 'V0375 自检失败：加列后有 % 张出库单的 cost_object_id 非空（存量数据被污染）', filled;
    END IF;

    -- ④ 弱引用：不许有指向 biz_cost_object 的外键（设计 §3.4）
    SELECT count(*) INTO fk_cnt
      FROM pg_constraint
     WHERE contype = 'f' AND conrelid = 'public.biz_stock_out'::regclass
       AND confrelid = 'public.biz_cost_object'::regclass;
    IF fk_cnt > 0 THEN
        RAISE EXCEPTION 'V0375 自检失败：biz_stock_out → biz_cost_object 建了 % 条物理外键（主数据必须弱引用）', fk_cnt;
    END IF;

    RAISE NOTICE 'V0375 形态断言通过：cost_object_id(UUID, 可空, 无默认) + idx_biz_so_cost_obj(局部) + 存量零填充 + 零外键';
END
$$;
