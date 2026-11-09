-- ============================================================================
-- P16 · ④ 补录差额流水（class C：真实/可推导数据，绝不改账面）
-- ----------------------------------------------------------------------------
-- 唯一一条 class C：
--   物料 DEMO-INV01 / 仓库 WH-DVA（145 dev 库 11-07 库存域演练 P0-03 的演示物料）
--   现状：成本账 92 / 1020.00，流水累计 90 / 1000.00 —— 差 2 件 / 20.00
--
--   归因（有据可查）：
--     docs/evidence/20261107-p003-inventory-chain/14_final_reconcile_output.txt 里
--     该组当日 收尾时是 92 = 92，共 7 条流水，其中：
--        IN  STOCK_TAKE_GAIN    ST-20261107-000001   5 件  60→65
--        OUT STOCK_TAKE_LOSS    ST-20261107-000002   3 件  65→62
--     今天库里这两条流水与两张盘点单都不在了（盘点单 deleted，流水被后续夹具清理连带删掉），
--     但成本账仍保留着 +5−3 = +2 的结果 ⇒ 账面 92 / 流水 90。
--     ⚠ 这是「删了单据没回滚账」的残留，不是账错了。
--
--   处置：按差额补一条**盘盈流水**（不动 biz_inventory 的任何数字）：
--        IN  STOCK_TAKE_GAIN    ADJ-20261109-FIX7-01   +2 件 @ 10.00 = 20.00
--        before 90 / 1000.00  →  after 92 / 1020.00
--     补完：Σ流水 = 92 = 账面；金额 1020 = 1020；且该行自身 after-before = ±qty 成立。
--
--   幂等：固定 id；已补过就 NOTICE 跳过（不重复加）。
--   护栏：补之前断言「账面 − 流水 = 恰好 2 件 / 20.00」，否则整单回滚 ——
--         换个库/换个数不该被这条脚本悄悄改。
-- ============================================================================
\set ON_ERROR_STOP on
BEGIN;

DO $$
DECLARE
    v_inv_qty   numeric; v_inv_amt numeric;
    v_txn_qty   numeric; v_txn_amt numeric;
    v_mat uuid; v_wh uuid;
    v_id  uuid := 'f17c0001-0000-4000-8000-000000000001';
    v_doc uuid := 'f17c0002-0000-4000-8000-000000000001';
BEGIN
    SELECT m.id, w.id INTO v_mat, v_wh
      FROM biz_material m, biz_warehouse w
     WHERE m.material_code = 'DEMO-INV01' AND w.warehouse_code = 'WH-DVA';

    IF v_mat IS NULL THEN
        RAISE NOTICE 'P16/30 跳过：本库没有 DEMO-INV01 / WH-DVA（不是 145 dev 库的形态）';
        RETURN;
    END IF;

    SELECT coalesce(sum(qty),0), coalesce(sum(amount),0) INTO v_inv_qty, v_inv_amt
      FROM biz_inventory WHERE material_id = v_mat AND warehouse_id = v_wh;
    SELECT coalesce(sum(CASE WHEN txn_type='IN' THEN qty ELSE -qty END),0),
           coalesce(sum(CASE WHEN txn_type='IN' THEN amount ELSE -amount END),0)
      INTO v_txn_qty, v_txn_amt
      FROM biz_inventory_txn WHERE material_id = v_mat AND warehouse_id = v_wh;

    IF EXISTS (SELECT 1 FROM biz_inventory_txn WHERE id = v_id) THEN
        RAISE NOTICE 'P16/30 跳过：补录流水 % 已存在（幂等）', v_id;
        RETURN;
    END IF;

    IF v_inv_qty = v_txn_qty AND v_inv_amt = v_txn_amt THEN
        RAISE NOTICE 'P16/30 跳过：DEMO-INV01/WH-DVA 账实已平（% / %），无需补录', v_inv_qty, v_inv_amt;
        RETURN;
    END IF;

    IF v_inv_qty - v_txn_qty <> 2 OR v_inv_amt - v_txn_amt <> 20 THEN
        RAISE EXCEPTION 'P16/30 中止：DEMO-INV01/WH-DVA 差额不是预期的 2 件/20.00（实际 件 % / 额 %）——人工确认后再补',
            v_inv_qty - v_txn_qty, v_inv_amt - v_txn_amt;
    END IF;

    INSERT INTO biz_inventory_txn
        (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no,
         qty, unit_cost, amount,
         before_qty, before_amount, before_cost, after_qty, after_amount, after_cost,
         period_code, remark, created_at)
    VALUES
        (v_id, v_mat, v_wh, 'IN', 'STOCK_TAKE_GAIN', 'STOCK_TAKE', v_doc,
         'ADJ-20261109-FIX7-01', 1,
         2, 10.00, 20.00,
         v_txn_qty, v_txn_amt, 10.00, v_inv_qty, v_inv_amt, 11.086957,
         '2026-11',
         'E2E-FIX-7 补录：11-07 演练的两张盘点单（ST-20261107-000001 +5 / -000002 -3）流水在后续夹具清理中被删，账面仍保留 +2，按差额补一条盘盈流水；不改账面数字',
         now());

    RAISE NOTICE 'P16/30 已补录：DEMO-INV01/WH-DVA 盘盈 +2 件 / 20.00（% → % 件；% → % 元）',
        v_txn_qty, v_inv_qty, v_txn_amt, v_inv_amt;
END $$;

-- 补完立刻复核（不平就回滚整个事务）
DO $$
DECLARE v_inv numeric; v_txn numeric; v_ia numeric; v_ta numeric;
BEGIN
    SELECT coalesce(sum(i.qty),0), coalesce(sum(i.amount),0) INTO v_inv, v_ia
      FROM biz_inventory i JOIN biz_material m ON m.id=i.material_id
      JOIN biz_warehouse w ON w.id=i.warehouse_id
     WHERE m.material_code='DEMO-INV01' AND w.warehouse_code='WH-DVA';
    SELECT coalesce(sum(CASE WHEN txn_type='IN' THEN qty ELSE -qty END),0),
           coalesce(sum(CASE WHEN txn_type='IN' THEN amount ELSE -amount END),0)
      INTO v_txn, v_ta
      FROM biz_inventory_txn t JOIN biz_material m ON m.id=t.material_id
      JOIN biz_warehouse w ON w.id=t.warehouse_id
     WHERE m.material_code='DEMO-INV01' AND w.warehouse_code='WH-DVA';
    IF v_inv <> v_txn OR v_ia <> v_ta THEN
        RAISE EXCEPTION 'P16/30 自检失败：补录后仍不平（账面 % / % vs 流水 % / %）', v_inv, v_ia, v_txn, v_ta;
    END IF;
    RAISE NOTICE 'P16/30 自检通过：DEMO-INV01/WH-DVA 账面 % 件 / % 元 = 流水合计', v_inv, v_ia;
END $$;

COMMIT;
