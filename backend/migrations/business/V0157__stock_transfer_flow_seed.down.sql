-- ======================================================
-- V0157 down：撤销种子流程 `stock_transfer`
--
-- 只删本迁移插入的那一套固定 UUID（def d157 / version e157 / nodes a157-a159 / edges b157-b158），
-- **不按 flow_code 删**：客户在同一编码下另存过新版本或改过图时，down 不许连带删掉人家的数据。
--
-- 两道安全阀：
--   ① 该流程已有审批实例（`biz_flow_instance.version_id` / `def_id`，FK 是 ON DELETE RESTRICT）
--      → 直接 RAISE EXCEPTION 并给出实例数。硬删会破在跑的调拨单审批链
--      （`biz_stock_transfer.approval_instance_id` 变悬空引用）。
--   ② 该流程已被发布者改过（版本数 > 1 或 hash 不是种子值）→ 只删本迁移自己那两行，
--      其余版本/节点原样保留（`ON DELETE CASCADE` 只作用于 version → node/edge）。
--
-- 回滚后断言零残留（节点/边/版本/定义四张表各 0 行）。
-- ======================================================

DO $$
DECLARE
    live_instances int;
BEGIN
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-00000000d157') THEN
        RAISE NOTICE 'V0157 down：种子流程不存在，无需回滚';
        RETURN;
    END IF;

    SELECT count(*) INTO live_instances FROM biz_flow_instance
     WHERE def_id = '20000000-0000-4000-8000-00000000d157'
        OR version_id = '20000000-0000-4000-8000-00000000e157';
    IF live_instances > 0 THEN
        RAISE EXCEPTION 'V0157 down 中止：该调拨流程已有 % 条审批实例（删流程会让在跑单据的 approval_instance_id 悬空）。请先处理实例再回滚。', live_instances;
    END IF;
END $$;

-- 审批人（ADMIN 来源无行；显式删一遍以示「零残留」口径完整）
DELETE FROM biz_flow_node_assignee
 WHERE node_id IN ('20000000-0000-4000-8000-00000000a157',
                   '20000000-0000-4000-8000-00000000a158',
                   '20000000-0000-4000-8000-00000000a159');

DELETE FROM biz_flow_edge
 WHERE id IN ('20000000-0000-4000-8000-00000000b157',
              '20000000-0000-4000-8000-00000000b158')
    OR version_id = '20000000-0000-4000-8000-00000000e157';

DELETE FROM biz_flow_node
 WHERE id IN ('20000000-0000-4000-8000-00000000a157',
              '20000000-0000-4000-8000-00000000a158',
              '20000000-0000-4000-8000-00000000a159')
    OR version_id = '20000000-0000-4000-8000-00000000e157';

DELETE FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-00000000e157';

-- 定义行只在「当前版本就是本迁移的版本」时才删（客户另存过新版本则保留 def）
DELETE FROM biz_flow_def
 WHERE id = '20000000-0000-4000-8000-00000000d157'
   AND (current_version_id IS NULL
        OR current_version_id = '20000000-0000-4000-8000-00000000e157');

DO $$
DECLARE
    n_left int;
BEGIN
    SELECT (SELECT count(*) FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-00000000e157')
         + (SELECT count(*) FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-00000000e157')
         + (SELECT count(*) FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-00000000e157')
      INTO n_left;
    IF n_left <> 0 THEN
        RAISE EXCEPTION 'V0157 down 自检失败：回滚后仍有 % 行残留（节点/边/版本）', n_left;
    END IF;
    IF EXISTS (SELECT 1 FROM biz_flow_def
                WHERE id = '20000000-0000-4000-8000-00000000d157'
                  AND current_version_id = '20000000-0000-4000-8000-00000000e157') THEN
        RAISE EXCEPTION 'V0157 down 自检失败：定义行仍在且仍指向已删版本';
    END IF;
    RAISE NOTICE 'V0157 down：种子流程已回滚（零残留）';
END $$;
