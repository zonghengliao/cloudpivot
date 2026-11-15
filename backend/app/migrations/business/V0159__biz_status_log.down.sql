-- ============================================================
-- V0159 down：撤销单据状态变更日志表（批次 2 · P10）
--
-- 安全阀：日志是审计证据链，**有数据时默认中止**（DROP 会把出事时唯一能查的东西抹掉）。
-- 真要回滚到无日志版本，先显式清空（`TRUNCATE biz_status_log`）再重放本 down，
-- 或者由运维带 `--i-know-its-prod` 的维护脚本处理 —— 迁移本身不做静默丢弃。
-- ============================================================

DO $$
DECLARE
    v_rows bigint;
BEGIN
    IF EXISTS (SELECT 1
                 FROM information_schema.tables
                WHERE table_schema = 'public' AND table_name = 'biz_status_log') THEN
        EXECUTE 'SELECT count(*) FROM biz_status_log' INTO v_rows;
        IF v_rows > 0 THEN
            RAISE EXCEPTION 'V0159 down 中止：biz_status_log 还有 % 行状态日志（审计证据链），先 TRUNCATE 再回滚', v_rows;
        END IF;
    END IF;
END $$;

DROP INDEX IF EXISTS idx_status_log_type_time;
DROP INDEX IF EXISTS idx_status_log_no;
DROP INDEX IF EXISTS idx_status_log_entity;
DROP TABLE IF EXISTS biz_status_log;
