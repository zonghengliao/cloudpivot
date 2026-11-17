-- ============================================================================
-- V0450（down）：撤掉框架协议两表 + 取号规则 FWA
--
-- 数据护栏：**有真数据就拒绝回滚**。
--   新建的这两张表里没有任何"演示数据"可丢 —— 每一行都是人工录入的协议
--   （含协议价与数量上限），回滚等于把合同条款抹掉。所以先数行，非空就
--   RAISE EXCEPTION，让调用方先决定「归档 / 导出 / 备份」再回滚。
--   自检探针在 up 的 ④.7 已经清干净，正常库这里一定是 0 行。
--
-- 顺序：明细表（外键子表）→ 单头表 → 取号行。
-- 索引随表 DROP 自动消失，不必单独 DROP INDEX。
-- ============================================================================

DO $$
DECLARE
    v_head integer;
    v_item integer;
BEGIN
    SELECT count(*) INTO v_head FROM biz_framework_agreement;
    SELECT count(*) INTO v_item FROM biz_framework_agreement_item;
    IF v_head > 0 OR v_item > 0 THEN
        RAISE EXCEPTION 'V0450 回滚被拒绝：框架协议还有 % 张协议 / % 行物料范围 —— 请先导出或归档，再把表清空后重试',
            v_head, v_item;
    END IF;
END $$;

DROP TABLE IF EXISTS biz_framework_agreement_item;
DROP TABLE IF EXISTS biz_framework_agreement;

DELETE FROM biz_seq WHERE seq_type = 'FWA';
