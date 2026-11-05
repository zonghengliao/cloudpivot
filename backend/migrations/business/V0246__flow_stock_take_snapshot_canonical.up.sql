-- ============================================================================
-- V0246__flow_stock_take_snapshot_canonical.up.sql
--
-- 修 V0211 种子的「快照数组顺序」缺陷（数据修正，不改任何业务逻辑）：
--
--   * 症状：`stock_take_post` 的 v1 版本 `graph_snapshot` 里 nodes / edges **数组**
--     是按「画布 sort_order」写的，而 canonical 序列化规则是
--       · nodes 按 `key` 升序
--       · edges 按 (`from`, `to`, `key`) 升序
--     两者对这张图**恰好不同**（sort_order 序：s, c_amt, n_wh, n_fin, n_gm, e；
--     key 序：c_amt, e, n_fin, n_gm, n_wh, s），于是种子里写死的 hash 只对
--     「非 canonical 顺序」成立：
--       cfc610…（V0211 写死）  ≠  331f25…（引擎从行重建算出的值）
--
--   * 影响：① 任何一次「另存草稿 → 重新发布」都会把 hash 改掉（发布回归用例
--     `pflow1a::publish_regression_keeps_graph_hash` 因此红）；
--           ② D4d 每日自检（`flow::maintain`）会把这条种子报成 SnapshotEdited 漂移。
--     内容（节点/边字段、审批人、坐标）**一个字节都没错**，错的只有数组顺序。
--
--   * 做法：把 arrays 重排成 canonical 顺序，并把 `graph_hash` 改成引擎对同一张图
--     算出的值（331f25…，与「重新发布一次」产出的 hash 逐字节相同；已在干净库上
--     用独立 canonical 序列化器（Python，递归键排序 + 紧凑无空白 + sha256）双向验过：
--     canonical(重排后的快照) == canonical(重新发布后的快照) == 331f25…）。
--
--   * 守卫（任一不满足就整段跳过，绝不误改别人的数据）：
--       ① 版本行仍是种子里那一行（id = …c2111）且 hash 仍是 cfc610…；
--       ② 6 个节点 / 6 条边的**内容集合**与种子一致（只允许顺序不同）。
--     用户已经重新发布过（hash 已不是种子值）的库：跳过（它已经是 canonical 的）。
--
--   * 回滚：V0246__flow_stock_take_snapshot_canonical.down.sql（按 sort_order 重排 +
--     写回 cfc610…，即 V0211 的原始形态）。
-- ============================================================================

DO $$
DECLARE
    c_def   CONSTANT uuid := '20000000-0000-4000-8000-0000000c2110';
    c_ver   CONSTANT uuid := '20000000-0000-4000-8000-0000000c2111';
    c_old   CONSTANT text := 'cfc6101455d799fb4377353ad57c85dfae8f30956a94ae642c88e1644a6bfb4a';
    c_new   CONSTANT text := '331f253a63ed68bb88fbcc7d4cc1d9273f3a49efde9d8d76955d71d36ac95ae8';
    v_snap     jsonb;
    v_rebuilt  jsonb;
    n          integer;
BEGIN
    SELECT v.graph_snapshot
      INTO v_snap
      FROM biz_flow_def_version v
     WHERE v.id = c_ver;

    IF v_snap IS NULL THEN
        RAISE NOTICE 'V0246：种子版本 % 不存在（非本库口径），跳过', c_ver;
        RETURN;
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM biz_flow_def_version v
         WHERE v.id = c_ver AND v.def_id = c_def AND btrim(v.graph_hash) = c_old
    ) THEN
        RAISE NOTICE 'V0246：stock_take_post 已不是种子 hash（用户已重新发布过），跳过';
        RETURN;
    END IF;

    -- ① 节点内容集合必须是种子那 6 个 key（只允许顺序不同）
    SELECT count(*) INTO n
      FROM jsonb_array_elements(v_snap -> 'nodes') AS node
     WHERE node ->> 'key' IN ('s', 'c_amt', 'n_wh', 'n_fin', 'n_gm', 'e');
    IF n <> 6 OR jsonb_array_length(v_snap -> 'nodes') <> 6 THEN
        RAISE NOTICE 'V0246：节点集合与种子不一致（% / 6），怀疑被手工改过，跳过', n;
        RETURN;
    END IF;

    -- ② 边内容集合必须是种子那 6 条 (from, to, key)
    SELECT count(*) INTO n
      FROM jsonb_array_elements(v_snap -> 'edges') AS edge
     WHERE (edge ->> 'from', edge ->> 'to', edge ->> 'key') IN (
              ('s', 'c_amt', 'e_start'),
              ('c_amt', 'n_wh', 'e_low'),
              ('c_amt', 'n_fin', 'e_low_else'),
              ('n_fin', 'n_gm', 'e_fin_gm'),
              ('n_gm', 'e', 'e_gm_end'),
              ('n_wh', 'e', 'e_wh_end')
           );
    IF n <> 6 OR jsonb_array_length(v_snap -> 'edges') <> 6 THEN
        RAISE NOTICE 'V0246：边集合与种子不一致（% / 6），怀疑被手工改过，跳过', n;
        RETURN;
    END IF;

    -- ③ 按 canonical 规则重排（nodes: key；edges: from,to,key）
    SELECT jsonb_build_object(
               'schema_version', (v_snap ->> 'schema_version')::int,
               'nodes', (
                   SELECT jsonb_agg(node ORDER BY node ->> 'key')
                     FROM jsonb_array_elements(v_snap -> 'nodes') AS node
               ),
               'edges', (
                   SELECT jsonb_agg(edge ORDER BY edge ->> 'from', edge ->> 'to', edge ->> 'key')
                     FROM jsonb_array_elements(v_snap -> 'edges') AS edge
               )
           )
      INTO v_rebuilt;

    IF v_rebuilt IS NULL THEN
        RAISE NOTICE 'V0246：重排失败（快照结构不完整），跳过';
        RETURN;
    END IF;

    UPDATE biz_flow_def_version
       SET graph_snapshot = v_rebuilt,
           graph_hash = c_new,
           updated_at = now()
     WHERE id = c_ver
       AND btrim(graph_hash) = c_old;

    RAISE NOTICE 'V0246：stock_take_post 快照已按 canonical 顺序重排（% → %）', c_old, c_new;
END $$;

-- 自检：改完之后 hash 必须是引擎口径（不满足直接抛错，让迁移失败而不是留半条）
DO $$
DECLARE
    v_ver CONSTANT uuid := '20000000-0000-4000-8000-0000000c2111';
    v_hash text;
    v_had  boolean;
BEGIN
    SELECT btrim(graph_hash) INTO v_hash FROM biz_flow_def_version WHERE id = v_ver;
    IF v_hash IS NULL THEN
        RETURN;  -- 库里没有这条种子（上面的 DO 已经 NOTICE 过）
    END IF;
    IF v_hash = 'cfc6101455d799fb4377353ad57c85dfae8f30956a94ae642c88e1644a6bfb4a' THEN
        -- 未改动：说明守卫跳过（允许，但必须仍然结构完整）
        RAISE NOTICE 'V0246 自检：仍是种子 hash（本次跳过）';
        RETURN;
    END IF;
    IF v_hash <> '331f253a63ed68bb88fbcc7d4cc1d9273f3a49efde9d8d76955d71d36ac95ae8' THEN
        RAISE EXCEPTION 'V0246 自检失败：stock_take_post v1 的 hash 既不是种子值也不是 canonical 值（% ）', v_hash;
    END IF;
    SELECT true INTO v_had FROM biz_flow_def_version
     WHERE id = v_ver AND graph_snapshot -> 'nodes' -> 0 ->> 'key' = 'c_amt';
    IF v_had IS NOT TRUE THEN
        RAISE EXCEPTION 'V0246 自检失败：节点数组首项不是 c_amt（未按 key 升序重排）';
    END IF;
    RAISE NOTICE 'V0246 自检通过：快照顺序 + hash 均为 canonical 口径';
END $$;
