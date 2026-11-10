-- ============================================================================
-- V0375 down：撤销「领料出库挂成本对象」
--
-- 只回滚本迁移动过的东西：删局部索引 + 删列。**不删行、不动其他列、不动金额**。
--
-- # 数据护栏（卡面验收「回滚脚本能删列且不丢其他数据」的另一半）
--   删列本身会让非空的 cost_object_id 静默消失 —— 那是真实业务信息（哪个对象领的料），
--   所以**先断言「没有单据挂着成本对象」**，有就拒绝执行并点名数量，让人先把单据处理掉
--   （打回草稿 / 清空该字段 / 删单），而不是由工程师改库补救（AGENTS 约定 11 的同一条精神）。
--
-- # 幂等
--   DROP INDEX IF EXISTS + DROP COLUMN IF EXISTS，第二遍零改动；
--   护栏读的 in_type / cost_object_id 列在第二遍已不存在，所以护栏本身也要能跳过。
-- ============================================================================

-- ---------- ① 数据护栏 ----------
DO $$
DECLARE
    has_col BOOLEAN;
    filled  BIGINT;
BEGIN
    SELECT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'biz_stock_out'
           AND column_name = 'cost_object_id') INTO has_col;
    IF NOT has_col THEN
        RAISE NOTICE 'V0375 down：cost_object_id 已不存在（重复回滚），跳过数据护栏';
        RETURN;
    END IF;

    EXECUTE 'SELECT count(*) FROM biz_stock_out WHERE cost_object_id IS NOT NULL' INTO filled;
    IF filled > 0 THEN
        RAISE EXCEPTION
            'V0375 down 拒绝执行：还有 % 张出库单挂着成本对象（cost_object_id 非空）。'
            '删列会丢掉「哪个成本对象领的料」，请先把这些单据打回草稿/清空该字段后再回滚', filled;
    END IF;
END
$$;

-- ---------- ② 删索引、删列 ----------
DROP INDEX IF EXISTS idx_biz_so_cost_obj;
ALTER TABLE biz_stock_out DROP COLUMN IF EXISTS cost_object_id;

-- ---------- ③ 反向自检 ----------
DO $$
DECLARE
    col_left BIGINT;
    idx_left BIGINT;
BEGIN
    SELECT count(*) INTO col_left
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_stock_out'
       AND column_name = 'cost_object_id';
    IF col_left > 0 THEN
        RAISE EXCEPTION 'V0375 down 自检失败：cost_object_id 仍在';
    END IF;

    SELECT count(*) INTO idx_left
      FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'idx_biz_so_cost_obj';
    IF idx_left > 0 THEN
        RAISE EXCEPTION 'V0375 down 自检失败：idx_biz_so_cost_obj 仍在';
    END IF;

    RAISE NOTICE 'V0375 down 完成：cost_object_id 与 idx_biz_so_cost_obj 已删除，其余列与数据未动';
END
$$;
