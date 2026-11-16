-- ============================================================================
-- m218_warehouse_gap_fixture_phase2.sql —— M2-18 E2E 阶段 2：**签收确认后的落库形态**
--
-- 卡：t_659b64de（M2-18【M2·仓库】仓库缺口收口：在途 + 库龄 + 安全库存 E2E）
--
-- 为什么这一段用 SQL 模拟而不是调 `POST /business/erp/shipment/{id}/receive`：
--   签收确认的**过账内核**（负库存闸 → 三本账负向 → 成本结转凭证 → 挂应收 → 在途清零）
--   是 M2-03 的被测对象，已由 `scripts/verify/m203_shipment_receipt_drill.sh` 用 5 条真库
--   用例钉住（含重复签收 409、失败回滚可重试）。本卡要验的是**三个查询面在签收之后是否
--   一致地跟着变**，所以这里只复刻那一笔的**落库结果**（m203 已证明引擎就是产生这个形态）：
--
--     · 结存      100 → 70        （出库 30）
--     · 在途(本单) 30 → 0         （received_qty = shipped_qty + status = RECEIVED）
--     · 在途(合计) 60 → 30        （另一张单 50 发 / 20 签，仍在途 30）
--     · 库龄层     FIFO 吃掉最老两层（120 天的 10 + 75 天的 20）→ 四档变 40/30/0/0
--     · 预计可用   100 − 60 + 100 = 140 → 70 − 30 + 100 = 140  **守恒**
--
-- 关键恒等式（E2E 阶段 B 逐条断言）：
--   Σ库龄层剩余 == biz_inventory.qty                       （账龄与结存同源）
--   结存 − 在途 + 未清订单 == 签收前后不变                    （签收只是把「在路上」挪进「账上」）
--   呆滞层随最老层被吃掉而消失                                （库龄不是标签，是 FIFO 摊出来的）
-- ============================================================================

\set ON_ERROR_STOP on

BEGIN;

-- 前置：阶段 1 的夹具必须在（否则这是跑错库了）
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM biz_inventory
                    WHERE material_id = 'a2180000-0000-4000-8000-000000000101'
                      AND warehouse_id = 'a2180000-0000-4000-8000-000000000010'
                      AND qty = 100) THEN
        RAISE EXCEPTION '阶段 2 拒绝执行：找不到阶段 1 的结存 100（请先跑 m218_warehouse_gap_fixture.sql）';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM biz_inventory_in_transit
                    WHERE shipment_id = 'a2180000-0000-4000-8000-000000000202'
                      AND status = 'IN_TRANSIT' AND qty_in_transit = 30) THEN
        RAISE EXCEPTION '阶段 2 拒绝执行：M218-SH-0001 不是在途 30（阶段 1 未就位或已跑过阶段 2）';
    END IF;
END $$;

-- ---------- ① 出库单（签收确认时由过账内核生成的那张）----------
INSERT INTO biz_stock_out (id, out_no, out_type, warehouse_id, out_date, status, total_qty,
                           total_amount, posted_at, source_type, source_id, reason)
VALUES ('a2180000-0000-4000-8000-000000000801', 'M218-OUT-0001', 'ISSUE',
        'a2180000-0000-4000-8000-000000000010', CURRENT_DATE, 'POSTED', 30, 300, now(),
        'SALES', 'a2180000-0000-4000-8000-000000000202', 'M218 E2E：签收确认（模拟）');

INSERT INTO biz_stock_out_item (stock_out_id, line_no, material_id, unit_id, qty, unit_cost, amount)
VALUES ('a2180000-0000-4000-8000-000000000801', 1, 'a2180000-0000-4000-8000-000000000101',
        'a2180000-0000-4000-8000-000000000001', 30, 10, 300);

UPDATE biz_shipment SET stock_out_id = 'a2180000-0000-4000-8000-000000000801'
 WHERE id = 'a2180000-0000-4000-8000-000000000202';

-- ---------- ② 出库流水（FIFO 吃掉最老两层的依据）----------
INSERT INTO biz_inventory_txn (material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id,
                               doc_no, line_no, qty, unit_cost, amount, before_qty, after_qty,
                               period_code, remark)
VALUES ('a2180000-0000-4000-8000-000000000101', 'a2180000-0000-4000-8000-000000000010',
        'OUT', 'SALES_OUT', 'STOCK_OUT', 'a2180000-0000-4000-8000-000000000801', 'M218-OUT-0001',
        1, 30, 10, 300, 100, 70, to_char(CURRENT_DATE, 'YYYY-MM'), 'M218 E2E：签收确认出库');

-- ---------- ③ 结存 100 → 70 ----------
UPDATE biz_inventory SET qty = 70, amount = 700, last_txn_at = now()
 WHERE id = 'a2180000-0000-4000-8000-000000000601';

-- ---------- ④ 在途清零（本单）：一条 UPDATE 同时推 received_qty + status ----------
-- 只改 received_qty 不动 status 会被 ck_biz_inv_transit_state 拦下（M2-03 的口径）
UPDATE biz_inventory_in_transit
   SET received_qty = shipped_qty, status = 'RECEIVED', received_at = now(), updated_at = now()
 WHERE shipment_id = 'a2180000-0000-4000-8000-000000000202'
   AND status = 'IN_TRANSIT';

COMMIT;

-- ---------- ⑤ 阶段 2 自检 ----------
DO $$
DECLARE
    v_qty     numeric;
    v_tt      numeric;
    v_layer   integer;
    v_left    numeric;
BEGIN
    SELECT qty INTO v_qty FROM biz_inventory WHERE id = 'a2180000-0000-4000-8000-000000000601';
    SELECT coalesce(sum(qty_in_transit), 0) INTO v_tt FROM biz_inventory_in_transit
     WHERE material_id = 'a2180000-0000-4000-8000-000000000101' AND status = 'IN_TRANSIT';
    SELECT count(*), coalesce(sum(qty_remaining), 0) INTO v_layer, v_left
      FROM biz_inventory_aging_layers(CURRENT_DATE, 'a2180000-0000-4000-8000-000000000101',
                                      'a2180000-0000-4000-8000-000000000010')
     WHERE qty_remaining > 0;
    IF v_qty <> 70 OR v_tt <> 30 OR v_layer <> 2 OR v_left <> 70 THEN
        RAISE EXCEPTION 'M218 阶段 2 自检失败：结存 %/70、在途 %/30、库龄层 %/2、层剩余 %/70',
            v_qty, v_tt, v_layer, v_left;
    END IF;
    RAISE NOTICE 'M218 阶段 2 就绪（签收后）：结存 70 / 在途 30 / 库龄层 2（−45 的 30 + −5 的 40）';
END $$;
