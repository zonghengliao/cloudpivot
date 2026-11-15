-- ======================================================
-- V0186：补 12 个外键缺失的索引（ERP-R6）
--
-- 依据：kanban 卡 t_4b18bf5f（评审发现「12 个外键没有索引 → 数据量上来后 JOIN 全表扫」）
-- 清单来源：**先在生产库跑卡面 §先确认清单 的查询**取真值，不照抄卡面示例。
--   生产库（cloudpivot@127.0.0.1:5432）当时返回 exactly 12 行，与卡面 10 行 + 「另有 2 个」吻合：
--     多出的 2 个是 biz_task_template.project_id 与 biz_task_worklog.user_id。
--
--   | # | 表.列                                            | 可空？ | 索引                                  |
--   |---|--------------------------------------------------|--------|---------------------------------------|
--   | 1 | biz_flow_instance.prev_instance_id              | 可空   | 部分索引（IS NOT NULL）               |
--   | 2 | biz_flow_instance.version_id                    | NOT NULL | 全表索引                            |
--   | 3 | biz_flow_instance_node.node_id                  | 可空   | 部分索引（IS NOT NULL）               |
--   | 4 | biz_flow_task.transferred_from                  | 可空   | 部分索引（IS NOT NULL）               |
--   | 5 | biz_stock_transfer.transit_warehouse_id         | 可空   | 部分索引（IS NOT NULL）               |
--   | 6 | biz_stock_transfer_item.from_location_id        | 可空   | 部分索引（IS NOT NULL）               |
--   | 7 | biz_stock_transfer_item.to_location_id          | 可空   | 部分索引（IS NOT NULL）               |
--   | 8 | biz_task_comment.parent_comment_id              | 可空   | 部分索引（IS NOT NULL）               |
--   | 9 | biz_task_comment.user_id                        | NOT NULL | 全表索引                            |
--   | 10| biz_task_template.owner_id                      | 可空   | 部分索引（IS NOT NULL）               |
--   | 11| biz_task_template.project_id                    | 可空   | 部分索引（IS NOT NULL）               |
--   | 12| biz_task_worklog.user_id                        | NOT NULL | 全表索引                            |
--
-- ── 为什么可空列用部分索引（`WHERE col IS NOT NULL`）──────────────────────
--   外键索引服务的查询几乎都带一个等值/反连接条件（`child.fk = parent.id`、
--   `NOT EXISTS (SELECT 1 FROM child WHERE child.fk = parent.id)`）。PG 的
--   `predicate_implied_by` 对**严格算子**（uuid 的 `=` 是 strict）能证明
--   「col = 某值 ⇒ col IS NOT NULL」，所以这类查询仍然命中部分索引。
--   实测（PG 18.6，1000/2000 行 + `enable_seqscan=off`）：
--     · 常量等值   → Index Scan using idx_child_fk_partial
--     · join 等值  → Index Only Scan using idx_child_fk_partial
--     · var=var 等值 / 范围 / 反连接 → 均使用该索引
--   空值行（例如「首次发起的流程没有前驱实例」「非在途调拨没有中转仓」）本来就
--   不参与这些查询，不进索引 → 索引更小、更热。
--
-- ── 为什么不用 CONCURRENTLY ────────────────────────────────────────────
--   迁移器把整个 up.sql 放在**一个事务**里跑，`CREATE INDEX CONCURRENTLY`
--   不能在事务块内执行（PG 报 25001）。本库这些表当前最大 947 行，普通
--   CREATE INDEX 的锁窗口是毫秒级，可接受。
--
-- ── 幂等 ──────────────────────────────────────────────────────────────
--   每条都带 `IF NOT EXISTS`：重复执行（含 down 后重放）不报错、不重建。
--
-- ── 只加索引，不动数据 ────────────────────────────────────────────────
--   本迁移不建表、不改列、不写业务行；回滚见 `V0186__fk_indexes.down.sql`（DROP INDEX IF EXISTS）。
-- ======================================================

-- ---------- 审批流（biz_flow_*） ----------
-- 流程「重新发起 / 版本追踪 / 节点追溯」三个反查入口
CREATE INDEX IF NOT EXISTS idx_biz_flow_instance_prev_instance
    ON biz_flow_instance (prev_instance_id)
    WHERE prev_instance_id IS NOT NULL;

-- version_id 是 NOT NULL 列，全表索引（流程按版本统计/回放）
CREATE INDEX IF NOT EXISTS idx_biz_flow_instance_version_id
    ON biz_flow_instance (version_id);

-- 节点定义 → 运行实例节点（改节点配置前查影响面）
CREATE INDEX IF NOT EXISTS idx_biz_flow_instance_node_node_id
    ON biz_flow_instance_node (node_id)
    WHERE node_id IS NOT NULL;

-- 待办转派链（A 转给 B 后反查「从谁转来的」）
CREATE INDEX IF NOT EXISTS idx_biz_flow_task_transferred_from
    ON biz_flow_task (transferred_from)
    WHERE transferred_from IS NOT NULL;

-- ---------- 库存调拨（biz_stock_transfer*） ----------
-- 在途仓反查在途单（在途库存对账）
CREATE INDEX IF NOT EXISTS idx_biz_stock_transfer_transit_warehouse_id
    ON biz_stock_transfer (transit_warehouse_id)
    WHERE transit_warehouse_id IS NOT NULL;

-- 库位维度反查调拨明细（库位余额 / 库位流水核对）
CREATE INDEX IF NOT EXISTS idx_biz_stock_transfer_item_from_location_id
    ON biz_stock_transfer_item (from_location_id)
    WHERE from_location_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_biz_stock_transfer_item_to_location_id
    ON biz_stock_transfer_item (to_location_id)
    WHERE to_location_id IS NOT NULL;

-- ---------- 任务（biz_task_*） ----------
-- 评论盖楼：按父评论取回复（parent 为空的是顶层评论，不进索引）
CREATE INDEX IF NOT EXISTS idx_biz_task_comment_parent_comment_id
    ON biz_task_comment (parent_comment_id)
    WHERE parent_comment_id IS NOT NULL;

-- user_id 是 NOT NULL 列，「我评论过的任务」全表索引
CREATE INDEX IF NOT EXISTS idx_biz_task_comment_user_id
    ON biz_task_comment (user_id);

-- 「我负责的模板 / 某项目下的模板」
CREATE INDEX IF NOT EXISTS idx_biz_task_template_owner_id
    ON biz_task_template (owner_id)
    WHERE owner_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_biz_task_template_project_id
    ON biz_task_template (project_id)
    WHERE project_id IS NOT NULL;

-- user_id 是 NOT NULL 列，「我填报的工时」全表索引
CREATE INDEX IF NOT EXISTS idx_biz_task_worklog_user_id
    ON biz_task_worklog (user_id);

-- ---------- 自检（结果进迁移日志） ----------
DO $$
DECLARE
    n_created integer;
    n_left    integer;
    left_list text;
BEGIN
    -- 本迁移的 12 个索引是否都在（按名核对，防止「表名/列名写错但语句没报错」）
    SELECT count(*) INTO n_created
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN (
            'idx_biz_flow_instance_prev_instance',
            'idx_biz_flow_instance_version_id',
            'idx_biz_flow_instance_node_node_id',
            'idx_biz_flow_task_transferred_from',
            'idx_biz_stock_transfer_transit_warehouse_id',
            'idx_biz_stock_transfer_item_from_location_id',
            'idx_biz_stock_transfer_item_to_location_id',
            'idx_biz_task_comment_parent_comment_id',
            'idx_biz_task_comment_user_id',
            'idx_biz_task_template_owner_id',
            'idx_biz_task_template_project_id',
            'idx_biz_task_worklog_user_id');
    IF n_created <> 12 THEN
        RAISE EXCEPTION 'V0186 自检失败：预期 12 个外键索引，实际 % 个', n_created;
    END IF;

    -- 复查：biz_% 里还剩几个「外键列上没有索引」（验收口径，期望 0）
    SELECT count(*), string_agg(tbl || '.' || col, ', ' ORDER BY tbl, col)
      INTO n_left, left_list
      FROM (SELECT c.conrelid::regclass::text AS tbl, a.attname AS col
              FROM pg_constraint c
              JOIN pg_attribute a
                ON a.attrelid = c.conrelid AND a.attnum = c.conkey[1]
             WHERE c.contype = 'f'
               AND NOT EXISTS (SELECT 1 FROM pg_index i
                                WHERE i.indrelid = c.conrelid
                                  AND a.attnum = ANY (i.indkey))
               AND c.conrelid::regclass::text LIKE 'biz_%') t;

    IF n_left > 0 THEN
        RAISE WARNING 'V0186：仍有 % 个业务外键无索引 → %', n_left, left_list;
    END IF;

    RAISE NOTICE 'V0186 自检通过：新增外键索引 % 个，剩余无索引外键 % 个', n_created, n_left;
END $$;
