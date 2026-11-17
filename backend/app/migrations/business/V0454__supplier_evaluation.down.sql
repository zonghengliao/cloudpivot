-- ============================================================================
-- V0454（down）：撤掉供应商评估表 + 取号规则 SEV
--
-- 数据护栏：**有真数据就拒绝回滚**。
--   新建的这张表里没有任何"演示数据"可丢 —— 每一行都是一份人工评估结论
--   （四维打分 + 权重 + 等级），回滚等于把「这家供应商当时评了多少分」抹掉，
--   而 M4-14 的准入/黑名单决策正是按它判的。所以先数行，非空就 RAISE EXCEPTION，
--   让调用方先决定「归档 / 导出 / 备份」再回滚。
--   自检探针在 up 的 ③.9 已经清干净，正常库这里一定是 0 行。
--
-- 顺序：表（索引随表 DROP 自动消失）→ 取号行。
-- ============================================================================

DO $$
DECLARE
    v_head integer;
BEGIN
    SELECT count(*) INTO v_head FROM biz_supplier_evaluation;
    IF v_head > 0 THEN
        RAISE EXCEPTION 'V0454 回滚被拒绝：供应商评估表还有 % 行评估结论 —— 请先导出或归档，再把表清空后重试',
            v_head;
    END IF;
END $$;

DROP TABLE IF EXISTS biz_supplier_evaluation;

DELETE FROM biz_seq WHERE seq_type = 'SEV';
