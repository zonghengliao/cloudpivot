-- ============================================================
-- V0199 down：撤销 A4 的「无源单据」显式标记
--
-- 回滚分档（与 V0198 down 同口径）：
--   档 A（本文件实际执行）：删约束 + 删索引 + 删列 —— **不丢业务数据**。
--     `is_sourceless` 是「`source_order_id` 为空」的显式投影（由 ck_biz_si_sourceless_pair 钉死），
--     删列后这条信息仍可从 `source_order_id IS NULL` 完整还原，因此本档**无损**。
--   档 B（无需单列）：无。本迁移只有这一列，删列即到底。
--
-- 执行顺序：索引 → 约束 → 列（列上的注释随列一起消失，无需单独 COMMENT NULL）。
-- 与 A3 的 V0198 down（档 A）叠加使用即回到 A3 之前的结构。
-- ============================================================

DROP INDEX IF EXISTS idx_biz_si_sourceless;

ALTER TABLE biz_stock_in
    DROP CONSTRAINT IF EXISTS ck_biz_si_sourceless_pair;

ALTER TABLE biz_stock_in
    DROP COLUMN IF EXISTS is_sourceless;

-- 注释回退：A4 之前的 supplier_id / source_order_id 没有列注释（V0145 未写），
-- 这里如实清空，避免 down 之后库里留着「指向已不存在语义」的注释。
COMMENT ON COLUMN biz_stock_in.supplier_id IS NULL;
COMMENT ON COLUMN biz_stock_in.source_order_id IS NULL;

-- ---------- 反向自检（down 必须自己证明自己干净） ----------
DO $$
DECLARE
    col_count bigint;
    con_count bigint;
    idx_count bigint;
BEGIN
    SELECT count(*) INTO col_count
      FROM information_schema.columns
     WHERE table_name = 'biz_stock_in' AND column_name = 'is_sourceless';
    SELECT count(*) INTO con_count
      FROM pg_constraint WHERE conname = 'ck_biz_si_sourceless_pair';
    SELECT count(*) INTO idx_count
      FROM pg_indexes WHERE indexname = 'idx_biz_si_sourceless';

    IF col_count <> 0 OR con_count <> 0 OR idx_count <> 0 THEN
        RAISE EXCEPTION 'down 自检失败：列=% / 约束=% / 索引=%（应全为 0）', col_count, con_count, idx_count;
    END IF;

    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_si_supplier_scope') THEN
        RAISE EXCEPTION 'down 自检失败：A3 的 ck_biz_si_supplier_scope 被连带删掉了（本文件不该动它）';
    END IF;

    RAISE NOTICE '回滚完成（V0199）：is_sourceless 列 / 约束 / 索引已删，业务数据一行未动；A3 的供应商方向约束保持原样';
END $$;
