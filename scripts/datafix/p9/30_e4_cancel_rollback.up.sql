-- P9 / E4：已作废收货单的库存没退 —— 补退库存 + 退库位账 + 写反向流水
--
-- 症状（erp_invariants.py E4）：2 组「物料×仓库」库存 > 有效入库。
--   证据（收货 000006 / 000007，作废于 09:27:15 / 09:27:32）：
--     凭证侧做了红冲（PZ-...-000004 / 000006），但**库存和流水一个都没退**：
--     biz_inventory_txn 里这两张单只有 1 条 IN，没有对应的 OUT。
--   根因：P9 之前 stockin_handler::cancel_in_tx 只做「红冲凭证 + 作废应付」，
--         库存/库位账/流水全靠人工红字调整单（代码里那条 WARN 当时就写着这个）。
--         代码侧已在 P9 修好（stockin_model::reverse_cost 等），这里补历史数据。
--
-- 口径（与修复后的代码逐字一致，不自己发明）：
--   数量   ← biz_stock_in_item.qty
--   金额   ← biz_inventory_txn 里该行过账时写入的 amount（**不是**当前 avg_cost × qty）
--   均价   ← 剩余金额 / 剩余数量（6 位）；数量归零则金额与均价一起归零
--   流水   ← txn_type='OUT' / biz_type='PURCHASE_IN_CANCEL'，before/after 六字段全部真填
--
-- 幂等：某单据行已经有 PURCHASE_IN_CANCEL 流水 → 跳过；第二遍跑 0 行。
-- 安全：库存不够就 RAISE EXCEPTION（不静默少退、更不扣成负数）。
DO $$
DECLARE
    r RECORD;
    inv RECORD;
    new_qty    numeric;
    new_amount numeric;
    new_cost   numeric;
    loc_qty    numeric;
    /* 期间码（不要叫 period_code：biz_inventory_txn 有同名列） */
    period_cd   text := to_char(now() AT TIME ZONE 'Asia/Shanghai', 'YYYY-MM');
    zero_uuid  constant uuid := '00000000-0000-0000-0000-000000000000';
BEGIN
    FOR r IN
        SELECT s.id AS stock_in_id, s.in_no, s.warehouse_id,
               i.line_no, i.material_id, i.location_id,
               t.qty AS txn_qty, t.amount AS txn_amount, t.unit_cost
          FROM biz_stock_in s
          JOIN biz_stock_in_item i ON i.stock_in_id = s.id
          JOIN biz_inventory_txn t
            ON t.doc_id = s.id AND t.doc_type = 'STOCK_IN' AND t.txn_type = 'IN'
           AND t.biz_type = 'PURCHASE_IN' AND t.line_no = i.line_no
         WHERE s.deleted_at IS NULL
           AND s.status = 'CANCELLED'
           AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn c
                            WHERE c.doc_id = s.id AND c.line_no = i.line_no
                              AND c.biz_type = 'PURCHASE_IN_CANCEL')
         ORDER BY i.material_id, i.line_no
    LOOP
        -- ① 锁成本账行
        SELECT inv2.qty, inv2.amount, inv2.avg_cost
          INTO inv
          FROM biz_inventory inv2
         WHERE inv2.material_id = r.material_id
           AND inv2.warehouse_id = r.warehouse_id
           AND coalesce(inv2.location_id, zero_uuid) = coalesce(r.location_id, zero_uuid)
           FOR UPDATE;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'P9-E4: % 第 % 行没有库存行（物料 % 仓库 %），不能自动退',
                r.in_no, r.line_no, r.material_id, r.warehouse_id;
        END IF;
        IF inv.qty - r.txn_qty < -0.0001 THEN
            RAISE EXCEPTION 'P9-E4: % 第 % 行退不回来：库存 % < 本单 %（货已出库，需人工处理）',
                r.in_no, r.line_no, inv.qty, r.txn_qty;
        END IF;

        new_qty    := inv.qty - r.txn_qty;
        new_amount := CASE WHEN new_qty <= 0 THEN 0 ELSE inv.amount - r.txn_amount END;
        new_cost   := CASE WHEN new_qty <= 0 THEN 0 ELSE round(new_amount / new_qty, 6) END;

        UPDATE biz_inventory
           SET qty = new_qty, amount = new_amount, avg_cost = new_cost,
               last_txn_at = now(), updated_at = now()
         WHERE material_id = r.material_id
           AND warehouse_id = r.warehouse_id
           AND coalesce(location_id, zero_uuid) = coalesce(r.location_id, zero_uuid);

        -- ② 库位账同步退（P6 之前的历史单没有库位行 → 由 10_b1 补的 NULL 桶承担）
        IF r.location_id IS NULL THEN
            SELECT qty INTO loc_qty FROM biz_inventory_location
             WHERE material_id = r.material_id AND warehouse_id = r.warehouse_id
               AND location_id IS NULL
               FOR UPDATE;
        ELSE
            SELECT qty INTO loc_qty FROM biz_inventory_location
             WHERE id = r.location_id
               FOR UPDATE;
        END IF;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'P9-E4: % 第 % 行没有库位账行，不能自动退（先跑 10_b1）',
                r.in_no, r.line_no;
        END IF;
        IF loc_qty - r.txn_qty < -0.0001 THEN
            RAISE EXCEPTION 'P9-E4: % 第 % 行库位账退不回来：库位账 % < 本单 %',
                r.in_no, r.line_no, loc_qty, r.txn_qty;
        END IF;
        IF r.location_id IS NULL THEN
            UPDATE biz_inventory_location
               SET qty = qty - r.txn_qty, last_txn_at = now(), updated_at = now()
             WHERE material_id = r.material_id AND warehouse_id = r.warehouse_id
               AND location_id IS NULL;
        ELSE
            UPDATE biz_inventory_location
               SET qty = qty - r.txn_qty, last_txn_at = now(), updated_at = now()
             WHERE id = r.location_id;
        END IF;

        -- ③ 反向流水（before 由 after + 本次退量精确反推：加减可逆）
        INSERT INTO biz_inventory_txn
               (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no,
                qty, unit_cost, amount, before_qty, before_amount, before_cost,
                after_qty, after_amount, after_cost, period_code, operator_id, created_at, remark)
        VALUES (gen_random_uuid(), r.material_id, r.warehouse_id, 'OUT', 'PURCHASE_IN_CANCEL',
                'STOCK_IN', r.stock_in_id, r.in_no, r.line_no,
                r.txn_qty, r.unit_cost, r.txn_amount,
                inv.qty, inv.amount, inv.avg_cost,
                new_qty, new_amount, new_cost,
                period_cd, NULL, now(),
                'P9 数据修复：补做 P9 之前的作废冲销流水');

        RAISE NOTICE 'P9-E4: % 第 % 行已退 数量 % / 金额 %（库存 % → %，均价 % → %）',
            r.in_no, r.line_no, r.txn_qty, r.txn_amount, inv.qty, new_qty, inv.avg_cost, new_cost;
    END LOOP;
END $$;
