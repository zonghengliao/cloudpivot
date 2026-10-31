-- P9 / E4 回滚：把 30_e4 退掉的库存加回去，并删掉它写的反向流水
--
-- 只处理 remark 带「P9 数据修复」标记的水，避免误删正常业务产生的 PURCHASE_IN_CANCEL。
-- 幂等：第二遍没有可处理的流水 → 0 行。
DO $$
DECLARE
    r RECORD;
    inv RECORD;
    new_qty    numeric;
    new_amount numeric;
    new_cost   numeric;
    zero_uuid  constant uuid := '00000000-0000-0000-0000-000000000000';
BEGIN
    FOR r IN
        SELECT t.id AS txn_id, t.material_id, t.warehouse_id, t.qty, t.amount
          FROM biz_inventory_txn t
         WHERE t.biz_type = 'PURCHASE_IN_CANCEL'
           AND t.remark LIKE 'P9 数据修复%'
         ORDER BY t.material_id, t.line_no
           FOR UPDATE
    LOOP
        SELECT inv2.qty, inv2.amount INTO inv
          FROM biz_inventory inv2
         WHERE inv2.material_id = r.material_id
           AND inv2.warehouse_id = r.warehouse_id
           AND inv2.location_id IS NULL
           FOR UPDATE;
        IF FOUND THEN
            new_qty    := inv.qty + r.qty;
            new_amount := inv.amount + r.amount;
            new_cost   := CASE WHEN new_qty <= 0 THEN 0 ELSE round(new_amount / new_qty, 6) END;
            UPDATE biz_inventory
               SET qty = new_qty, amount = new_amount, avg_cost = new_cost,
                   last_txn_at = now(), updated_at = now()
             WHERE material_id = r.material_id AND warehouse_id = r.warehouse_id
               AND location_id IS NULL;
        END IF;

        UPDATE biz_inventory_location
           SET qty = qty + r.qty, last_txn_at = now(), updated_at = now()
         WHERE material_id = r.material_id AND warehouse_id = r.warehouse_id
           AND location_id IS NULL;

        DELETE FROM biz_inventory_txn WHERE id = r.txn_id;
        RAISE NOTICE 'P9-E4 down: 物料 % 仓库 % 加回 数量 % / 金额 %',
            r.material_id, r.warehouse_id, r.qty, r.amount;
    END LOOP;
END $$;
