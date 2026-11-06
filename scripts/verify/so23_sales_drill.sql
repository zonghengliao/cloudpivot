-- =============================================================================
-- SO-23 负向演练：对正向夹具逐条注入 8 处违约，S1–S8 必须全部变红（BLOCKER）
-- =============================================================================
-- 目的：证明这 8 条**不是恒绿的装饰**——每一条都真的能抓到对应的账错。
-- 只跑在演练库 cloudpivot_so23neg（从 cloudpivot_so23pos 复制并注入），生产不用。
--
-- ⚠️ 关于两处 `ALTER TABLE … DROP CONSTRAINT`
--   S2（delivered_qty ≤ qty）与 S3（发票价税合计）、S4（已收 ≤ 应收）、
--   S5a（已核销 ≤ 收款）在库层都有 CHECK，**正常路径根本写不进坏值**。
--   这正是 erp_invariants.py 文件头列的第 ② 种情况（约束被摘掉的老库 /
--   有人 ALTER TABLE 之后写进去的行）——演练就先摘约束再写坏值，
--   否则演练本身会被库层挡住（那是「约束有效」的证明，不是「巡检有效」的证明）。
--   演练结束不恢复约束：库是一次性的，下次 pos 运行会重新复制。
--
-- 预期：S1..S8 全红（8/8 失败）。同时 A1/A2/B5 之类**全账**判据也会跟着红，
--       那是应有的连带反应（同一笔账坏了，全账判据当然看得见）。
-- =============================================================================
\set ON_ERROR_STOP on

BEGIN;

-- ---------- S1 订单：明细金额改小，头不变 → 明细 Σ ≠ 头 ----------
UPDATE biz_sales_order_item
   SET amount = 900.0000
 WHERE id = 'd0231100-0000-4000-8000-000000000001'::uuid;

-- ---------- S2 发货：摘掉超发防线，再把已发量写超 ----------
ALTER TABLE biz_sales_order_item DROP CONSTRAINT ck_sales_order_item_delivered;
UPDATE biz_sales_order_item
   SET delivered_qty = 6.0000
 WHERE id = 'd0231100-0000-4000-8000-000000000002'::uuid;   -- qty = 5

-- ---------- S3 发票：价税合计 +10（恒等式破） ----------
ALTER TABLE biz_invoice DROP CONSTRAINT ck_biz_invoice_amount;
UPDATE biz_invoice
   SET total_with_tax = 1140.0000
 WHERE invoice_no = 'SO23-IV-0001';

-- ---------- S4 应收：已收款 > 应收金额 ----------
ALTER TABLE biz_receivable DROP CONSTRAINT ck_biz_ar_amount;
UPDATE biz_receivable
   SET received_amount  = 2000.0000,
       unsettled_amount = -870.0000      -- 保持 ck_biz_ar_unsettled 自洽，专测 S4 的边界判据
 WHERE ar_no = 'SO23-AR-0001';   -- ⚠ 不能打 AR-0002：那条被 S6 软删后就退出 S4 的判定范围了

-- ---------- S5 收款：已核销写超收款金额，且 Σ核销 ≠ 已核销 ----------
ALTER TABLE biz_receipt DROP CONSTRAINT ck_biz_receipt_amount;
UPDATE biz_receipt
   SET allocated_amount = 700.0000        -- 收款金额只有 565
 WHERE receipt_no = 'SO23-RC-0001';
UPDATE biz_receipt_allocation
   SET amount = 500.0000                  -- Σ核销 = 500 ≠ 已核销 700
 WHERE receipt_id = 'd0235000-0000-4000-8000-000000000001'::uuid;

-- ---------- S6 出库 ↔ 应收：把第 2 张出库的应收软删（出库已过账却无应收） ----------
UPDATE biz_receivable
   SET deleted_at = now()
 WHERE ar_no = 'SO23-AR-0002';

-- ---------- S7 凭证：收入凭证借方 1130 → 1200（借贷不平 & 头与分录不一致） ----------
UPDATE biz_voucher_entry
   SET debit_amount = 1200.0000
 WHERE voucher_id = 'd0237000-0000-4000-8000-000000000004'::uuid
   AND line_no = 1;

-- ---------- S8 三账：库位账 -1，且第 2 张出库的流水少记 1 ----------
UPDATE biz_inventory_location
   SET qty = 9.0000
 WHERE id = 'd0239000-0000-4000-8000-000000000001'::uuid;

UPDATE biz_inventory_txn
   SET qty = 4.0000
 WHERE doc_type = 'STOCK_OUT' AND doc_id = 'd0233000-0000-4000-8000-000000000002'::uuid;

COMMIT;

DO $$
DECLARE n bigint;
BEGIN
    SELECT count(*) INTO n FROM biz_sales_order_item
     WHERE id = 'd0231100-0000-4000-8000-000000000001'::uuid AND amount = 900;
    IF n <> 1 THEN RAISE EXCEPTION 'S1 注入失败'; END IF;
    SELECT count(*) INTO n FROM biz_sales_order_item
     WHERE id = 'd0231100-0000-4000-8000-000000000002'::uuid AND delivered_qty > qty;
    IF n <> 1 THEN RAISE EXCEPTION 'S2 注入失败'; END IF;
    SELECT count(*) INTO n FROM biz_invoice
     WHERE invoice_no = 'SO23-IV-0001' AND total_with_tax <> amount + tax_amount;
    IF n <> 1 THEN RAISE EXCEPTION 'S3 注入失败'; END IF;
    SELECT count(*) INTO n FROM biz_receivable
     WHERE ar_no = 'SO23-AR-0001' AND received_amount > amount;
    IF n <> 1 THEN RAISE EXCEPTION 'S4 注入失败'; END IF;
    SELECT count(*) INTO n FROM biz_receipt
     WHERE receipt_no = 'SO23-RC-0001' AND allocated_amount > amount;
    IF n <> 1 THEN RAISE EXCEPTION 'S5 注入失败'; END IF;
    SELECT count(*) INTO n FROM biz_receivable
     WHERE ar_no = 'SO23-AR-0002' AND deleted_at IS NOT NULL;
    IF n <> 1 THEN RAISE EXCEPTION 'S6 注入失败'; END IF;
    SELECT count(*) INTO n FROM biz_voucher_entry
     WHERE voucher_id = 'd0237000-0000-4000-8000-000000000004'::uuid AND debit_amount = 1200;
    IF n <> 1 THEN RAISE EXCEPTION 'S7 注入失败'; END IF;
    SELECT count(*) INTO n FROM biz_inventory_location
     WHERE id = 'd0239000-0000-4000-8000-000000000001'::uuid AND qty = 9;
    IF n <> 1 THEN RAISE EXCEPTION 'S8 注入失败'; END IF;
    RAISE NOTICE 'SO-23 负向演练注入完成：S1..S8 各 1 处违约';
END $$;
