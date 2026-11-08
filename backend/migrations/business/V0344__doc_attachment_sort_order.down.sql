-- ============================================================
-- V0344 回滚：删 `biz_doc_attachment.sort_order`
--
-- # 为什么先拒绝
--
-- 有非 0 的手工序号说明**用户真的排过序**。直接 DROP COLUMN 会静默丢掉这份人工结果，
-- 且没有任何地方留痕（排序不进 operation_logs 的 detail）。
-- 因此 down 先断言「没有手工排序」，让人显式决定：
--   · 真要回滚：先 `UPDATE biz_doc_attachment SET sort_order = 0;` 再重跑 down；
--   · 或者不退这条迁移（代码回退到没有 sort_order 的版本也能跑：默认 0 时列仍在，只是没人读）。
-- ============================================================

DO $$
DECLARE
    n_sorted integer;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_schema = 'public'
           AND table_name = 'biz_doc_attachment'
           AND column_name = 'sort_order'
    ) THEN
        RAISE NOTICE 'V0344 down：sort_order 列不存在，无需回滚';
        RETURN;
    END IF;

    SELECT count(*) INTO n_sorted FROM biz_doc_attachment WHERE sort_order <> 0;
    IF n_sorted > 0 THEN
        RAISE EXCEPTION
            'V0344 down 拒绝执行：% 行附件有手工排序（sort_order <> 0）。先确认放弃人工排序，再 SET sort_order = 0 后重试',
            n_sorted;
    END IF;
END $$;

DROP INDEX IF EXISTS idx_biz_doc_attachment_sort;

ALTER TABLE biz_doc_attachment DROP COLUMN IF EXISTS sort_order;
