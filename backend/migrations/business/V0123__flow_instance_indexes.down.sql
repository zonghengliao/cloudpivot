-- ======================================================
-- V0123 down：审批流 · 运行侧索引补齐（批次 D · D2）
--
-- 只 DROP 本文件建的 3 条索引，再断言这 3 条确实不存在。
-- `idx_biz_flow_task_node` 在 V0122 里**没有**内联建（刻意分离），所以 DROP 是安全的；
-- 另两条与 V0122 的同名语句互为 `IF NOT EXISTS`，DROP 后由 V0122 重建也不会丢（重放 up 时补回）。
-- ======================================================

DROP INDEX IF EXISTS idx_biz_flow_cc_unread;
DROP INDEX IF EXISTS idx_biz_flow_instance_node_active;
DROP INDEX IF EXISTS idx_biz_flow_task_node;

DO $$
DECLARE
    left_idx int;
BEGIN
    SELECT count(*) INTO left_idx
      FROM pg_indexes
     WHERE schemaname = current_schema()
       AND indexname IN ('idx_biz_flow_task_node',
                         'idx_biz_flow_instance_node_active',
                         'idx_biz_flow_cc_unread');

    IF left_idx > 0 THEN
        RAISE EXCEPTION 'V0123 回滚不干净：索引残留 %', left_idx;
    END IF;

    RAISE NOTICE 'V0123 回滚自检通过：3 条索引全清零';
END
$$;
