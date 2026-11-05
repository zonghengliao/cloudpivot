-- ============================================================================
-- V0246__flow_stock_take_snapshot_canonical.down.sql
--
-- 回滚：把 stock_take_post 的 v1 快照还原成 V0211 的原始形态
-- （nodes 按画布 sort_order、edges 按 (from 节点 sort_order, priority, key)，
--   hash 写回种子值 cfc610…）。
--
-- 只在「当前仍是 V0246 写下的 canonical hash」时回滚；用户已经重新发布过
-- （hash 是引擎算出的别的值）就不动它。
-- ============================================================================

DO $$
DECLARE
    c_ver CONSTANT uuid := '20000000-0000-4000-8000-0000000c2111';
    c_new CONSTANT text := '331f253a63ed68bb88fbcc7d4cc1d9273f3a49efde9d8d76955d71d36ac95ae8';
    c_old CONSTANT text := 'cfc6101455d799fb4377353ad57c85dfae8f30956a94ae642c88e1644a6bfb4a';
    v_snap    jsonb;
    v_back    jsonb;
BEGIN
    SELECT graph_snapshot INTO v_snap
      FROM biz_flow_def_version
     WHERE id = c_ver AND btrim(graph_hash) = c_new;

    IF v_snap IS NULL THEN
        RAISE NOTICE 'V0246.down：不是本迁移写下的状态，跳过';
        RETURN;
    END IF;

    SELECT jsonb_build_object(
               'schema_version', (v_snap ->> 'schema_version')::int,
               'nodes', (
                   SELECT jsonb_agg(node ORDER BY (node ->> 'sort_order')::int, node ->> 'key')
                     FROM jsonb_array_elements(v_snap -> 'nodes') AS node
               ),
               'edges', (
                   SELECT jsonb_agg(edge ORDER BY (src ->> 'sort_order')::int, (edge ->> 'priority')::int, edge ->> 'key')
                     FROM jsonb_array_elements(v_snap -> 'edges') AS edge
                     JOIN jsonb_array_elements(v_snap -> 'nodes') AS src
                       ON src ->> 'key' = edge ->> 'from'
               )
           )
      INTO v_back;

    UPDATE biz_flow_def_version
       SET graph_snapshot = v_back,
           graph_hash = c_old,
           updated_at = now()
     WHERE id = c_ver AND btrim(graph_hash) = c_new;

    RAISE NOTICE 'V0246.down：stock_take_post v1 快照已还原为 V0211 形态';
END $$;
