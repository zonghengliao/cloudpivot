-- ============================================================
-- V0205 down：删除跨单据联动动作白名单表
--
-- 安全阀：**还有存活配置时拒绝 down**。
--   down 一跑，整套联动配置就没了（表都没了，改过的业务规则无法回滚）。
--   真要回滚请先把规则逐条软删（`UPDATE … SET deleted_at = now()`）并导出备份
--   （`COPY biz_flow_linkage_rule TO STDOUT WITH CSV HEADER`），再跑 down。
--
-- 不动的东西：
--   · `biz_status_log` 里 `entity_type = 'FLOW_LINKAGE_CFG'` 的审计行**保留**
--     —— 「谁在什么时候改过哪条联动」是审计事实，删表不该抹掉它。
--   · 表上的索引 / 约束随表一起被 DROP TABLE 带走，无需单独 DROP。
-- ============================================================

DO $$
DECLARE
    n_alive integer;
BEGIN
    IF to_regclass('public.biz_flow_linkage_rule') IS NULL THEN
        RAISE NOTICE 'V0205 down：biz_flow_linkage_rule 不存在，跳过';
        RETURN;
    END IF;

    SELECT count(*) INTO n_alive FROM biz_flow_linkage_rule WHERE deleted_at IS NULL;
    IF n_alive > 0 THEN
        RAISE EXCEPTION
            'V0205 down 中止：还有 % 条存活的联动规则。down 会连表一起删（配置无法回滚），'
            '请先备份并软删（UPDATE biz_flow_linkage_rule SET deleted_at = now()）后再执行',
            n_alive;
    END IF;

    RAISE NOTICE 'V0205 down：存活规则 0 条，允许删表（biz_status_log 的 FLOW_LINKAGE_CFG 审计行保留）';
END $$;

DROP INDEX IF EXISTS idx_flow_linkage_rule_action;
DROP INDEX IF EXISTS idx_flow_linkage_rule_trigger;
DROP TABLE IF EXISTS biz_flow_linkage_rule;
