-- ======================================================
-- V0122 down：审批流 · 运行侧 4 张表（批次 D · D2）
--
-- 逆序 DROP（抄送 / 待办 / 节点 / 实例），末尾自检「零残留」：
-- 表 / 索引 / 约束三项都必须清零，否则 RAISE EXCEPTION 让整条回滚失败。
-- 注意：**不能**用 `indexname LIKE 'idx_biz_flow%'` 做残留判定 —— 那会把
-- V0120（定义侧 5 表）的索引一起算进来，导致 D2 单独回滚时永远报残留。
-- ======================================================

-- ---------- ① 抄送 ----------
DROP TABLE IF EXISTS biz_flow_cc;

-- ---------- ② 待办 / 审批记录 ----------
DROP TABLE IF EXISTS biz_flow_task;

-- ---------- ③ 实例节点 ----------
DROP TABLE IF EXISTS biz_flow_instance_node;

-- ---------- ④ 实例 ----------
DROP TABLE IF EXISTS biz_flow_instance;

-- ---------- ⑤ 自检：回滚零残留 ----------
DO $$
DECLARE
    left_tbl int;
    left_idx int;
    left_ck  int;
BEGIN
    SELECT count(*) INTO left_tbl
      FROM information_schema.tables
     WHERE table_schema = current_schema()
       AND table_name IN ('biz_flow_instance','biz_flow_instance_node',
                          'biz_flow_task','biz_flow_cc');

    SELECT count(*) INTO left_idx
      FROM pg_indexes
     WHERE schemaname = current_schema()
       AND tablename IN ('biz_flow_instance','biz_flow_instance_node',
                         'biz_flow_task','biz_flow_cc');

    SELECT count(*) INTO left_ck
      FROM pg_constraint
     WHERE connamespace = current_schema()::regnamespace
       AND conrelid::regclass::text IN ('biz_flow_instance','biz_flow_instance_node',
                                        'biz_flow_task','biz_flow_cc');

    IF left_tbl > 0 OR left_idx > 0 OR left_ck > 0 THEN
        RAISE EXCEPTION
            'V0122 回滚不干净：表残留 % / 索引残留 % / 约束残留 %',
            left_tbl, left_idx, left_ck;
    END IF;

    RAISE NOTICE 'V0122 回滚自检通过：4 张运行侧表 / 索引 / 约束全清零';
END
$$;
