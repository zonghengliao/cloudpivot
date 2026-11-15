-- ======================================================
-- V0177：TASK_NODE 的地基 —— `biz_task` 加 3 列 + `source` 扩 FLOW + 幂等索引
--
-- 依据：`docs/architecture/流程全配置化-设计.md` §2.2（决策 D5，commit 09b87fe）
-- 回滚：`V0177__task_node_biz_task_link.down.sql`
--
-- ── 为什么不用 `biz_task.parent_id`（决策 D5）────────────
-- `parent_id` 是 `REFERENCES biz_task(id)` 的**自引用外键**，语义是「子任务」。
-- 拿它指向流程实例（`biz_flow_instance.id`）：
--   ① 都是 uuid，类型能过，但 FK 校验当场失败；
--   ② 即使去掉 FK，任务系统的三处逻辑立刻误判 ——
--      `count_open_children`（父任务 DONE 前所有子任务必须完成）、
--      `progress_locked`（有子任务时进度由子任务汇总，不许手改）、
--      `blocks_child_advance`（父任务已取消 → 子任务一律不许推进）；
--   ③ 「一个 TASK_NODE 生成的任务」不是「另一个任务的子任务」，它是「流程实例的产物」。
--      两种关系必须分开表达，否则「查这个实例下所有任务」要扫 parent_id 树。
--
-- 于是改用 3 个专用列：`flow_instance_id` / `flow_node_key` / `flow_round` + `source='FLOW'`。
--
-- ── 为什么唯一索引要带 `deleted_at IS NULL` ──────────────
-- 任务软删后允许重建（人为修复不被唯一索引挡住），与 `uk_biz_flow_def_code` /
-- `uk_biz_notice_template_code` 的写法一致（都是 `WHERE … IS NULL`）。
-- ======================================================

-- ---------- ① 三个新列：把「这张任务从哪个流程的哪个节点第几轮生成」变成可直查的事实 ----------
ALTER TABLE biz_task
    ADD COLUMN flow_instance_id uuid,
    ADD COLUMN flow_node_key   varchar(64),
    ADD COLUMN flow_round      integer;

COMMENT ON COLUMN biz_task.flow_instance_id IS
    '生成这张任务的流程实例（biz_flow_instance.id）；非流程任务为 NULL。不设级联删除：实例永不物理删除，而任务可以软删';
COMMENT ON COLUMN biz_task.flow_node_key IS
    '生成它的 TASK_NODE 的 node_key（biz_flow_node.node_key），与实例快照里的 key 一致';
COMMENT ON COLUMN biz_task.flow_round IS
    '生成时该节点的轮次（biz_flow_instance_node.round）。退回重进同一节点时 round+1，于是「同一节点同一轮只有一个任务」';

-- ---------- ② 外键：ON DELETE SET NULL ----------
-- 任务不该因为实例被清理而消失（它可能有工时 / 附件 / 评论）。实例侧实际是 RESTRICT 保护
-- （biz_flow_instance.def_id ON DELETE RESTRICT），物理删除几乎不会发生；这里用 SET NULL 是
-- 「就算发生了，任务也留着，source='FLOW' 会把它标成孤儿待人工处理」。
ALTER TABLE biz_task
    ADD CONSTRAINT biz_task_flow_instance_id_fkey
    FOREIGN KEY (flow_instance_id) REFERENCES biz_flow_instance(id) ON DELETE SET NULL;

-- ---------- ③ 三列要么全空（普通任务），要么全非空（流程任务）—— 半截数据不可排障 ----------
ALTER TABLE biz_task
    ADD CONSTRAINT ck_biz_task_flow_triplet CHECK (
        (flow_instance_id IS NULL AND flow_node_key IS NULL AND flow_round IS NULL)
     OR (flow_instance_id IS NOT NULL AND flow_node_key IS NOT NULL
         AND flow_round IS NOT NULL AND flow_round >= 1)
    );

-- ---------- ④ source 扩第 5 个值 'FLOW' ----------
-- PostgreSQL 没有 `ALTER CHECK`，就地改必须 DROP + ADD。
-- `biz_task` 当前 0 行（§6.4 SQL ② 的体检口径），扫描成本可忽略；
-- ADD 时会顺带校验存量 —— 存量若真有 source='FLOW' 的脏行，迁移当场失败（这是好事）。
ALTER TABLE biz_task DROP CONSTRAINT biz_task_source_check;
ALTER TABLE biz_task ADD CONSTRAINT biz_task_source_check CHECK (
    source::text = ANY (ARRAY['SELF','ASSIGNED','TRANSFERRED','RECURRING','FLOW']::text[])
);

-- ---------- ⑤ source='FLOW' ⇔ 三个流程列非空（双向绑定，防「手写 source='FLOW' 却没流程」）----------
ALTER TABLE biz_task
    ADD CONSTRAINT ck_biz_task_source_flow CHECK (
        (source::text = 'FLOW') = (flow_instance_id IS NOT NULL)
    );

-- ---------- ⑥ 幂等键：同一实例 + 同一节点 + 同一轮 → 最多一张任务 ----------
-- 这条唯一索引是「生成幂等」与「循环 / 退回不生成重复任务」的**唯一依据**；
-- 引擎侧不需要额外加锁判断，23505 直接按幂等成功处理（与 engine.rs 的 is_pending_duplicate 同款）。
CREATE UNIQUE INDEX uk_biz_task_flow_node
    ON biz_task (flow_instance_id, flow_node_key, flow_round)
    WHERE flow_instance_id IS NOT NULL AND deleted_at IS NULL;

-- ---------- ⑦ 流程详情页「本实例下的任务」与任务详情页「所属流程」都要按实例查 ----------
CREATE INDEX idx_biz_task_flow_instance
    ON biz_task (flow_instance_id, flow_node_key)
    WHERE flow_instance_id IS NOT NULL;

-- ---------- ⑧ 自检（失败即迁移失败 → 启动失败）----------
DO $$
DECLARE
    missing int;
BEGIN
    SELECT count(*) INTO missing
      FROM information_schema.columns
     WHERE table_name = 'biz_task'
       AND column_name IN ('flow_instance_id', 'flow_node_key', 'flow_round')
       AND is_nullable = 'YES';
    IF missing <> 3 THEN
        RAISE EXCEPTION 'V0177 自检失败：biz_task 的三个流程列只存在 % 个（应为 3 且可空）', missing;
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_indexes WHERE indexname = 'uk_biz_task_flow_node'
    ) THEN
        RAISE EXCEPTION 'V0177 自检失败：幂等唯一索引 uk_biz_task_flow_node 不存在';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_task_source_flow'
    ) THEN
        RAISE EXCEPTION 'V0177 自检失败：source ⇔ 流程列的双向约束不存在';
    END IF;

    RAISE NOTICE 'V0177 自检通过：3 列 + source 扩 FLOW + 幂等索引 + 2 条 CHECK';
END
$$;
