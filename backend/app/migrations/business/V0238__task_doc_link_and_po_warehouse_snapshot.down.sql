-- ============================================================
-- V0238 回滚：撤掉「任务挂单据」两列 + 采购单仓库名快照。
--
-- 顺序：先删索引与 CHECK，再删列（列一删索引/约束会跟着走，显式删是为了
-- 回执里能看到每一步都做了）。
--
-- ⚠ 回滚**会丢数据**：已挂单据的任务会回到「自由任务」，已落快照的采购单
-- 会失去建单当时的仓库名（只剩 warehouse_id）。这是 DDL 回滚的固有代价，
-- 与 V0237 的 down（只删空列、无回填）不同 —— 所以只在开发/演练环境回滚，
-- 生产回滚前先 `pg_dump` 这两列。
-- ============================================================

DROP INDEX IF EXISTS idx_biz_task_doc;

ALTER TABLE biz_task DROP CONSTRAINT IF EXISTS ck_biz_task_doc_pair;

ALTER TABLE biz_task
    DROP COLUMN IF EXISTS doc_id,
    DROP COLUMN IF EXISTS doc_type;

ALTER TABLE biz_purchase_order
    DROP COLUMN IF EXISTS warehouse_name_snapshot;

DO $$
DECLARE
    n_col   integer;
    n_index integer;
    n_ck    integer;
BEGIN
    SELECT count(*) INTO n_col
      FROM information_schema.columns
     WHERE table_schema = current_schema()
       AND table_name = 'biz_task'
       AND column_name IN ('doc_type', 'doc_id');
    IF n_col <> 0 THEN
        RAISE EXCEPTION 'V0238 down 自检失败：biz_task.doc_type / doc_id 仍在（% 列）', n_col;
    END IF;

    SELECT count(*) INTO n_index
      FROM pg_indexes
     WHERE schemaname = current_schema()
       AND indexname = 'idx_biz_task_doc';
    IF n_index <> 0 THEN
        RAISE EXCEPTION 'V0238 down 自检失败：idx_biz_task_doc 仍在（% 个）', n_index;
    END IF;

    SELECT count(*) INTO n_ck
      FROM pg_constraint
     WHERE conname = 'ck_biz_task_doc_pair';
    IF n_ck <> 0 THEN
        RAISE EXCEPTION 'V0238 down 自检失败：ck_biz_task_doc_pair 仍在（% 个）', n_ck;
    END IF;

    SELECT count(*) INTO n_col
      FROM information_schema.columns
     WHERE table_schema = current_schema()
       AND table_name = 'biz_purchase_order'
       AND column_name = 'warehouse_name_snapshot';
    IF n_col <> 0 THEN
        RAISE EXCEPTION 'V0238 down 自检失败：biz_purchase_order.warehouse_name_snapshot 仍在（% 列）', n_col;
    END IF;

    RAISE NOTICE 'V0238 down：任务挂单据两列 + 采购单仓库名快照已删除，其余列未动';
END $$;
