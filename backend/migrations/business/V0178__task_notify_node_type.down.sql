-- ======================================================
-- V0178 回滚：撤掉 TASK_NODE / NOTIFY_NODE 与三条 config 分区 CHECK
--
-- 前置：必须先删这两类节点行，否则 ① 的旧 CHECK 加不回去（旧 CHECK 不认这两个值）。
-- 顺序与 up 严格相反，全程可重复执行（IF EXISTS）。
-- ======================================================

-- ---------- ① 先删数据：边 → 节点（nodes 是 edges 的父，顺序不能反）----------
DELETE FROM biz_flow_edge
 WHERE from_node_id IN (SELECT id FROM biz_flow_node WHERE node_type::text IN ('TASK_NODE','NOTIFY_NODE'))
    OR to_node_id   IN (SELECT id FROM biz_flow_node WHERE node_type::text IN ('TASK_NODE','NOTIFY_NODE'));
-- （edges 有 ON DELETE CASCADE 指向 node，这里显式删一次让 down 脚本自解释，不依赖 CASCADE 的隐式行为）

DELETE FROM biz_flow_node WHERE node_type::text IN ('TASK_NODE','NOTIFY_NODE');

-- ---------- ② 删三条分区 CHECK ----------
ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS ck_biz_flow_node_config_partition;
ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS ck_biz_flow_node_notify_config;
ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS ck_biz_flow_node_task_config;

-- ---------- ③ node_type 收回到 7 值 ----------
ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS biz_flow_node_node_type_check;
ALTER TABLE biz_flow_node ADD CONSTRAINT biz_flow_node_node_type_check CHECK (
    node_type::text = ANY (ARRAY[
      'START','APPROVAL','COUNTERSIGN','CONDITION','PARALLEL','CC','END']::text[])
);

COMMENT ON COLUMN biz_flow_node.config IS
'节点扩展配置（JSONB）。现有用途：assignee_type=EXPRESSION → {"expression":{version,rules[],otherwise}}。
发布后随 graph_snapshot 冻结，改配置 = 发新版本。';

-- ---------- ④ 自检 ----------
DO $$
DECLARE
    left_rows int;
    def text;
BEGIN
    SELECT count(*) INTO left_rows
      FROM biz_flow_node
     WHERE node_type::text IN ('TASK_NODE','NOTIFY_NODE');
    IF left_rows <> 0 THEN
        RAISE EXCEPTION 'V0178 down 自检失败：还剩 % 行 TASK_NODE / NOTIFY_NODE', left_rows;
    END IF;

    SELECT pg_get_constraintdef(oid) INTO def
      FROM pg_constraint
     WHERE conrelid = 'biz_flow_node'::regclass
       AND conname = 'biz_flow_node_node_type_check';
    IF def LIKE '%TASK_NODE%' THEN
        RAISE EXCEPTION 'V0178 down 自检失败：node_type CHECK 仍认 TASK_NODE';
    END IF;

    RAISE NOTICE 'V0178 down 自检通过：节点类型收回 7 值 + 3 条分区 CHECK 撤除';
END
$$;
