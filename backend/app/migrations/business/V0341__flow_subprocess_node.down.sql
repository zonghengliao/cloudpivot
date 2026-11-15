-- ======================================================
-- V0341 down：撤销 SUBPROCESS 子流程节点 + 父子实例关联（P3-08）
--
-- # 顺序（卡面明写：先删子实例引用列，再删节点表列）
--   ① 删部分索引 → ② 删实例侧 CHECK / FK / 两列 → ③ 删 config.sub 形状 CHECK
--   → ④ 分区 CHECK 退回两条 → ⑤ 删 subprocess_def_id 的 FK 与列
--   → ⑥ node_type CHECK 退回 9 值 → ⑦ 零残留断言
--
-- # 回滚的业务后果（写清楚，避免有人以为「回滚无害」）
--   `biz_flow_node.subprocess_def_id` 与 `biz_flow_instance.parent_*` 一旦删掉，
--   **已经发布出去的含 SUBPROCESS 节点的版本无法再执行**：引擎解析快照时
--   `NodeType::parse("SUBPROCESS")` 会返回 None（引擎按 APPROVAL 兜底），
--   子实例上的父指针也会永久丢失（父流程会停在 SUBPROCESS 节点 ACTIVE）。
--   回滚前请确认：① 没有任何 PUBLISHED 版本含 SUBPROCESS 节点；
--   ② 没有任何在跑的子实例（`SELECT count(*) FROM biz_flow_instance
--   WHERE parent_instance_id IS NOT NULL AND status='PENDING'` = 0）。
--   本脚本**不删行**，只回结构；被引用的 `biz_flow_def` 行同样原样保留。
-- ======================================================

BEGIN;

-- ---------- ① 索引 ----------
DROP INDEX IF EXISTS idx_biz_flow_instance_parent;

-- ---------- ② 实例侧（先约束后列）----------
ALTER TABLE biz_flow_instance DROP CONSTRAINT IF EXISTS fk_biz_flow_instance_parent;
ALTER TABLE biz_flow_instance DROP CONSTRAINT IF EXISTS ck_biz_flow_instance_parent;
ALTER TABLE biz_flow_instance DROP COLUMN IF EXISTS parent_node_key;
ALTER TABLE biz_flow_instance DROP COLUMN IF EXISTS parent_instance_id;

-- ---------- ③ config.sub 形状 + 讲回分区的老注释 ----------
ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS ck_biz_flow_node_subprocess_config;

-- ---------- ④ 分区 CHECK 退回 V0178 的两条 ----------
-- 注意：如果此刻库里还有 `node_type='SUBPROCESS'` 的行（或别的类型挂了 config.sub），
-- 底下这两步会**成功**（分区 CHECK 只管 sub 键的归属），但紧接着 ⑥ 的 node_type
-- 老取值域会失败 —— 那正是我们想要的：宁可在迁移期炸，也不要在运行期静默跑错图。
ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS ck_biz_flow_node_config_partition;
ALTER TABLE biz_flow_node ADD CONSTRAINT ck_biz_flow_node_config_partition CHECK (
    (node_type::text = 'TASK_NODE'   OR NOT (config ? 'task'))
AND (node_type::text = 'NOTIFY_NODE' OR NOT (config ? 'notify'))
);

COMMENT ON COLUMN biz_flow_node.config IS
'节点扩展配置（JSONB）。按节点类型分区，互斥：
 · assignee_type=EXPRESSION → {"expression":{version,rules[],otherwise}}（现有）
 · node_type=TASK_NODE       → {"task":{title_template,on_complete,due,…}}
 · node_type=NOTIFY_NODE     → {"notify":{when,targets[],template_code|inline,…}}
形状由 ck_biz_flow_node_* CHECK 兜关键字段，跨字段约束由 flow::graph::validate (R17–R21) 兜。
发布后随 graph_snapshot 冻结，改配置 = 发新版本。';

-- ---------- ⑤ 子流程定义指针 ----------
ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS fk_biz_flow_node_subprocess_def;
ALTER TABLE biz_flow_node DROP COLUMN IF EXISTS subprocess_def_id;

-- ---------- ⑥ node_type 退回 9 值 ----------
ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS biz_flow_node_node_type_check;
ALTER TABLE biz_flow_node ADD CONSTRAINT biz_flow_node_node_type_check CHECK (
    node_type::text = ANY (ARRAY[
      'START','APPROVAL','COUNTERSIGN','CONDITION','PARALLEL','CC','END',
      'TASK_NODE','NOTIFY_NODE'
    ]::text[])
);

-- ---------- ⑦ 零残留断言 ----------
DO $$
DECLARE
    left_over int;
BEGIN
    SELECT count(*) INTO left_over
      FROM information_schema.columns
     WHERE table_name = 'biz_flow_instance'
       AND column_name IN ('parent_instance_id', 'parent_node_key');
    IF left_over <> 0 THEN
        RAISE EXCEPTION 'V0341 down 失败：biz_flow_instance 仍有 % 列父子关联列', left_over;
    END IF;

    SELECT count(*) INTO left_over
      FROM information_schema.columns
     WHERE table_name = 'biz_flow_node' AND column_name = 'subprocess_def_id';
    IF left_over <> 0 THEN
        RAISE EXCEPTION 'V0341 down 失败：biz_flow_node.subprocess_def_id 仍存在';
    END IF;

    SELECT count(*) INTO left_over
      FROM pg_indexes
     WHERE tablename = 'biz_flow_instance' AND indexname = 'idx_biz_flow_instance_parent';
    IF left_over <> 0 THEN
        RAISE EXCEPTION 'V0341 down 失败：部分索引 idx_biz_flow_instance_parent 仍存在';
    END IF;

    SELECT count(*) INTO left_over
      FROM pg_constraint
     WHERE conrelid = 'biz_flow_node'::regclass
       AND conname IN ('ck_biz_flow_node_subprocess_config', 'fk_biz_flow_node_subprocess_def');
    IF left_over <> 0 THEN
        RAISE EXCEPTION 'V0341 down 失败：子流程相关约束仍残留 % 条', left_over;
    END IF;

    RAISE NOTICE 'V0341 down 自检通过：父子实例两列 / 部分索引 / 子流程约束全部清除';
END
$$;

COMMIT;
