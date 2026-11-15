-- ======================================================
-- V0120 回滚：审批流 · 定义侧 5 张表（批次 D · D1）
--
-- 顺序：先删引用方、再删被引用方（与架构 §6.8 一致）：
--   biz_flow_edge → biz_flow_node_assignee → biz_flow_node → biz_flow_def_version → biz_flow_def
--
-- ⚠️ 运行侧（`biz_flow_instance.version_id`）在本卡范围内**还不存在**（D2 才建）。
--    D2 落地后必须**先**跑 V0116 的 down（否则 `biz_flow_def_version` 上的
--    ON DELETE RESTRICT / 实例引用会让本脚本的 DROP 被外键挡住 —— 这在语义上是正确的：
--    有在跑的审批单时不允许把定义侧拆掉）。
--
-- 零残留自检放在末尾：表 / 索引 / 约束 / 注释任一残留 → RAISE EXCEPTION（回滚失败）。
-- ======================================================

-- ---------- ① 连线（引用 version + node 各一条外键） ----------
DROP TABLE IF EXISTS biz_flow_edge;

-- ---------- ② 审批人引用（引用 node） ----------
DROP TABLE IF EXISTS biz_flow_node_assignee;

-- ---------- ③ 节点（引用 version） ----------
DROP TABLE IF EXISTS biz_flow_node;

-- ---------- ④ 版本（引用 def） ----------
DROP TABLE IF EXISTS biz_flow_def_version;

-- ---------- ⑤ 定义 ----------
DROP TABLE IF EXISTS biz_flow_def;

-- ---------- ⑥ 自检：回滚零残留 ----------
DO $$
DECLARE
    left_tbl int;
    left_idx int;
    left_ck  int;
BEGIN
    SELECT count(*) INTO left_tbl
      FROM information_schema.tables
     WHERE table_schema = current_schema()
       AND table_name IN ('biz_flow_def','biz_flow_def_version','biz_flow_node',
                          'biz_flow_node_assignee','biz_flow_edge');

    SELECT count(*) INTO left_idx
      FROM pg_indexes
     WHERE schemaname = current_schema()
       AND (indexname LIKE 'uk_biz_flow%' OR indexname LIKE 'idx_biz_flow%');

    SELECT count(*) INTO left_ck
      FROM pg_constraint
     WHERE connamespace = current_schema()::regnamespace
       AND (conname LIKE '%biz_flow%');

    IF left_tbl > 0 OR left_idx > 0 OR left_ck > 0 THEN
        RAISE EXCEPTION
            'V0120 回滚不干净：表残留 % / 索引残留 % / 约束残留 %',
            left_tbl, left_idx, left_ck;
    END IF;

    RAISE NOTICE 'V0120 回滚自检通过：5 张表 / 索引 / 约束全清零';
END
$$;
