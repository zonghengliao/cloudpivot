-- ======================================================
-- V0177 回滚：撤掉流程任务的 3 列 / source=FLOW / 幂等索引
--
-- 顺序与 up **严格相反**，并且**先清数据再删列**：
-- 不显式处理「已经生成了流程任务」的情况，DROP COLUMN 后这些行会变成
-- source='FLOW' 却没有流程关联的僵尸行（还会撞 ④ 的 CHECK 加不回去）。
-- 全程可重复执行（IF EXISTS），零残留。
-- ======================================================

-- ---------- ① 先清数据：只删流程生成的（人工任务一行不动）----------
DELETE FROM biz_task WHERE source::text = 'FLOW';

-- ---------- ② 删索引 ----------
DROP INDEX IF EXISTS idx_biz_task_flow_instance;
DROP INDEX IF EXISTS uk_biz_task_flow_node;

-- ---------- ③ 删约束（顺序：先依赖 source 的，再动 source 的取值域）----------
ALTER TABLE biz_task DROP CONSTRAINT IF EXISTS ck_biz_task_source_flow;
ALTER TABLE biz_task DROP CONSTRAINT IF EXISTS biz_task_source_check;
ALTER TABLE biz_task ADD CONSTRAINT biz_task_source_check CHECK (
    source::text = ANY (ARRAY['SELF','ASSIGNED','TRANSFERRED','RECURRING']::text[])
);
ALTER TABLE biz_task DROP CONSTRAINT IF EXISTS ck_biz_task_flow_triplet;
ALTER TABLE biz_task DROP CONSTRAINT IF EXISTS biz_task_flow_instance_id_fkey;

-- ---------- ④ 删列 ----------
ALTER TABLE biz_task DROP COLUMN IF EXISTS flow_round;
ALTER TABLE biz_task DROP COLUMN IF EXISTS flow_node_key;
ALTER TABLE biz_task DROP COLUMN IF EXISTS flow_instance_id;

-- ---------- ⑤ 自检 ----------
DO $$
DECLARE
    left_cols int;
BEGIN
    SELECT count(*) INTO left_cols
      FROM information_schema.columns
     WHERE table_name = 'biz_task'
       AND column_name IN ('flow_instance_id', 'flow_node_key', 'flow_round');
    IF left_cols <> 0 THEN
        RAISE EXCEPTION 'V0177 down 自检失败：还有 % 个流程列没删掉', left_cols;
    END IF;

    IF EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'uk_biz_task_flow_node') THEN
        RAISE EXCEPTION 'V0177 down 自检失败：幂等唯一索引仍在';
    END IF;

    RAISE NOTICE 'V0177 down 自检通过：3 列 / 幂等索引 / 2 条 CHECK 全部撤除';
END
$$;
