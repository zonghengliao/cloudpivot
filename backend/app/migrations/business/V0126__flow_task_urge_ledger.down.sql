-- 批次 D · D4c 回滚：删催办台账两列（remind_count / last_remind_at 是 V0122 的，不在这里动）
DROP INDEX IF EXISTS idx_biz_flow_task_urge;

ALTER TABLE biz_flow_task DROP COLUMN IF EXISTS last_urge_at;
ALTER TABLE biz_flow_task DROP COLUMN IF EXISTS urge_count;

-- 自检：零残留
DO $$
DECLARE
    leftover int;
BEGIN
    SELECT count(*) INTO leftover
      FROM information_schema.columns
     WHERE table_name = 'biz_flow_task'
       AND column_name IN ('urge_count', 'last_urge_at');
    IF leftover <> 0 THEN
        RAISE EXCEPTION 'V0126 down 自检失败：仍残留 % 个催办列', leftover;
    END IF;
    IF EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'idx_biz_flow_task_urge') THEN
        RAISE EXCEPTION 'V0126 down 自检失败：索引 idx_biz_flow_task_urge 仍存在';
    END IF;
END $$;
