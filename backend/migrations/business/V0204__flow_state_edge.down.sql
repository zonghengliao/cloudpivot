-- ============================================================
-- V0204 down：撤掉状态机边配置表
--
-- 顺序：
--   ① 安全检查：表里还有存活配置就中止（回滚迁移 = 退回「全部使用 BASE_EDGES」，
--      那是一次**配置丢失**，必须先有人确认这些配置不要了）
--   ② 删索引 / 表
--   ③ 残留自检：表不存在、索引不存在
--
-- ⚠ 本 down 不动 `biz_status_log`：那是审计日志（只增不改），
--   `entity_type = 'FLOW_EDGE_CFG'` 的历史配置变更记录必须留着——
--   「谁在什么时候放宽过流程规则」的证据不能因为回滚代码就消失。
-- ============================================================

-- ---------- ① 安全检查 ----------
DO $$
DECLARE
    n integer;
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.tables
                WHERE table_schema = 'public' AND table_name = 'biz_flow_state_edge') THEN
        EXECUTE 'SELECT count(*) FROM biz_flow_state_edge WHERE deleted_at IS NULL' INTO n;
        IF n > 0 THEN
            RAISE EXCEPTION 'V0204 down 中止：还有 % 条存活的状态机边配置。'
                            ' 回滚本迁移会让这些配置全部失效（退回 BASE_EDGES），'
                            ' 请先确认为什么要丢配置（必要时把 biz_flow_state_edge 整表导出留档）', n;
        END IF;
    END IF;
END $$;

-- ---------- ② 表（索引随表一起删） ----------
DROP TABLE IF EXISTS biz_flow_state_edge;

-- ---------- ③ 残留自检 ----------
DO $$
DECLARE
    n_tbl integer;
    n_idx integer;
BEGIN
    SELECT count(*) INTO n_tbl FROM information_schema.tables
     WHERE table_schema = 'public' AND table_name = 'biz_flow_state_edge';
    SELECT count(*) INTO n_idx FROM pg_indexes
     WHERE schemaname = 'public' AND indexname IN ('idx_flow_state_edge_entity', 'uk_flow_state_edge');
    IF n_tbl <> 0 OR n_idx <> 0 THEN
        RAISE EXCEPTION 'V0204 down 残留：表 % / 索引 %（应全为 0）', n_tbl, n_idx;
    END IF;
    RAISE NOTICE 'V0204 down 完成：biz_flow_state_edge 与索引全部撤销，零残留（biz_status_log 审计保留）';
END $$;
