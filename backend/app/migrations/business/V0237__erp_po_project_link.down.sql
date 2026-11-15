-- ============================================================
-- V0237 回滚：撤掉采购单的 project_id 与按项目索引。
--
-- 顺序：先删索引再删列（列一删索引会跟着走，显式删是为了回执里能看到两步都做了）。
-- 回滚不碰任何业务数据：V0237 up 只加列/索引、没有回填，所以 down 之后
-- 采购单回到「与项目无关」的原样，其它表一行不动。
-- ============================================================

DROP INDEX IF EXISTS idx_biz_po_project;

ALTER TABLE biz_purchase_order
    DROP COLUMN IF EXISTS project_id;

DO $$
DECLARE
    n_col   integer;
    n_index integer;
BEGIN
    SELECT count(*) INTO n_col
      FROM information_schema.columns
     WHERE table_schema = current_schema()
       AND table_name = 'biz_purchase_order'
       AND column_name = 'project_id';
    IF n_col <> 0 THEN
        RAISE EXCEPTION 'V0237 down 自检失败：project_id 仍在（% 列）', n_col;
    END IF;

    SELECT count(*) INTO n_index
      FROM pg_indexes
     WHERE schemaname = current_schema()
       AND indexname = 'idx_biz_po_project';
    IF n_index <> 0 THEN
        RAISE EXCEPTION 'V0237 down 自检失败：idx_biz_po_project 仍在（% 个）', n_index;
    END IF;

    RAISE NOTICE 'V0237 down：project_id 与 idx_biz_po_project 已删除，其余表未动';
END $$;
