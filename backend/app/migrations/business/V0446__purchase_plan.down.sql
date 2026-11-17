-- ============================================================================
-- V0446__purchase_plan.down.sql —— 回滚采购计划表结构（M4-01）
--
--   ① 数据护栏：**两张新表里只要有行就拒绝回滚** —— 采购计划是人工编制/审批过的
--      业务数据，不是演示数据，没有「删了再灌一遍」这回事。要真回滚请先自己备份并清表
--      （明确写在这里，免得有人以为回滚脚本会顺手删数据）。
--   ② 删索引 / 明细表 / 单头表（明细先删，避免外键依赖顺序问题）。
--   ③ 删 biz_seq(seq_type='PPL') 取号行。
--
-- 幂等：全部 IF EXISTS / WHERE 条件，重跑不报错。
-- ============================================================================

DO $$
DECLARE
    v_plan integer := 0;
    v_item integer := 0;
BEGIN
    IF to_regclass('public.biz_purchase_plan') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_purchase_plan' INTO v_plan;
    END IF;
    IF to_regclass('public.biz_purchase_plan_item') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_purchase_plan_item' INTO v_item;
    END IF;
    IF v_plan > 0 OR v_item > 0 THEN
        RAISE EXCEPTION 'V0446 回滚被拒绝：biz_purchase_plan 有 % 行、biz_purchase_plan_item 有 % 行真实数据（先备份并清表再回滚）',
            v_plan, v_item;
    END IF;
END $$;

DROP INDEX IF EXISTS idx_biz_pp_item_material;
DROP INDEX IF EXISTS uk_biz_pp_item_line;
DROP INDEX IF EXISTS uk_biz_pp_item_cell;
DROP TABLE IF EXISTS biz_purchase_plan_item;

DROP INDEX IF EXISTS idx_biz_purchase_plan_parent;
DROP INDEX IF EXISTS idx_biz_purchase_plan_status;
DROP INDEX IF EXISTS idx_biz_purchase_plan_period;
DROP INDEX IF EXISTS uk_biz_purchase_plan_period;
DROP INDEX IF EXISTS uk_biz_purchase_plan_no;
DROP TABLE IF EXISTS biz_purchase_plan;

DELETE FROM biz_seq WHERE seq_type = 'PPL';

DO $$
BEGIN
    IF to_regclass('public.biz_purchase_plan') IS NOT NULL
       OR to_regclass('public.biz_purchase_plan_item') IS NOT NULL THEN
        RAISE EXCEPTION 'V0446 回滚自检失败：表还在';
    END IF;
    IF EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'PPL') THEN
        RAISE EXCEPTION 'V0446 回滚自检失败：biz_seq(PPL) 行还在';
    END IF;
    RAISE NOTICE 'M4-01/V0446 回滚完成：两张计划表与取号行已删除';
END $$;
