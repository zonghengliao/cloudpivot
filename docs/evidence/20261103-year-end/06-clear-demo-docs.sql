-- ======================================================
-- YEAR-END 真机验证 · phase 2 前置：把三张挡路单据「处理掉」
--
-- 真机脚本的 phase 2 断言的是「挡路单据清空后，年结检查清单回到 ✅、年度结转可点」，
-- 所以这里直接把三张演示单据删掉（等价于「在业务页把单走到终态」——
-- 采购单审批通过并收货完成 / 收货单过账 / 调拨单审批完成）。
--
-- 为什么删而不是走状态机：走状态机会生成凭证与库存流水，把「年结」的预检
-- 变成「先要保证账平」的另一件事；本卡要验的是**清单与催办**，不是过账链路。
--
-- 复原成「三张都挡路」：重跑 `05-demo-data.sql`（它按前缀先删后插，幂等）。
-- ======================================================

BEGIN;

DELETE FROM biz_purchase_order WHERE order_no LIKE 'PC-YE-PO-%';
DELETE FROM biz_stock_in WHERE in_no LIKE 'PC-YE-IN-%';
DELETE FROM biz_stock_transfer WHERE transfer_no LIKE 'PC-YE-TR-%';

COMMIT;

DO $$
DECLARE
    n_docs int;
BEGIN
    SELECT (SELECT count(*) FROM biz_purchase_order WHERE order_no LIKE 'PC-YE-PO-%')
         + (SELECT count(*) FROM biz_stock_in WHERE in_no LIKE 'PC-YE-IN-%')
         + (SELECT count(*) FROM biz_stock_transfer WHERE transfer_no LIKE 'PC-YE-TR-%')
      INTO n_docs;
    IF n_docs <> 0 THEN
        RAISE EXCEPTION '演示单据没清干净（还剩 % 张）', n_docs;
    END IF;
    RAISE NOTICE 'YEAR-END 演示单据已清空：挡路项应为 0';
END $$;
