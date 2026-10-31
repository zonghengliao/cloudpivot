-- P9 / B1：补写库位账（成本账有数量、库位账没有行）
--
-- 症状（erp_invariants.py B1）：3 组「物料×仓库」两账不符。
-- 根因：P6（t_493a6088）之前过账只写成本账 biz_inventory，**一行库位账都没写**
--       （biz_inventory_location 全表 0 行），所以 SUM(库位账) = 0 ≠ 成本账 qty。
--
-- 修法：按 (物料, 仓库) 求「成本账合计 − 已有库位账合计」，差额补进
--       **未分配库位桶**（location_id IS NULL，唯一键 uk_biz_inv_loc 带 NULLS NOT DISTINCT
--       保证最多一行）。历史单据没有库位信息，落 NULL 桶是唯一诚实的口径。
--
-- 幂等：第二遍 delta = 0 → 不产生任何行。
-- 反例保护：库位账 **大于** 成本账（delta < 0）→ 直接 RAISE EXCEPTION，
--           那是「调拨/序列号流程重复写了库位账」，必须人查，脚本不自动抹。
DO $$
DECLARE
    r RECORD;
    delta numeric;
BEGIN
    FOR r IN
        SELECT inv.material_id, inv.warehouse_id, sum(inv.qty) AS cost_qty
          FROM biz_inventory inv
         GROUP BY inv.material_id, inv.warehouse_id
    LOOP
        SELECT r.cost_qty
             - coalesce((SELECT sum(l.qty) FROM biz_inventory_location l
                          WHERE l.material_id = r.material_id
                            AND l.warehouse_id = r.warehouse_id), 0)
          INTO delta;

        IF delta > 0.0001 THEN
            INSERT INTO biz_inventory_location AS il
                   (id, material_id, warehouse_id, location_id, qty, locked_qty,
                    last_txn_at, created_at, updated_at)
            VALUES (gen_random_uuid(), r.material_id, r.warehouse_id, NULL, delta, 0,
                    now(), now(), now())
            ON CONFLICT (material_id, warehouse_id, location_id) DO UPDATE
                SET qty = il.qty + EXCLUDED.qty,
                    last_txn_at = EXCLUDED.last_txn_at,
                    updated_at = now();
            RAISE NOTICE 'P9-B1: 物料 % 仓库 % 补未分配库位桶 +%',
                r.material_id, r.warehouse_id, delta;
        ELSIF delta < -0.0001 THEN
            RAISE EXCEPTION
                'P9-B1: 物料 % 仓库 % 的库位账比成本账多 % —— 不是缺行，是重复写，需人工排查（脚本不自动抹）',
                r.material_id, r.warehouse_id, -delta;
        END IF;
    END LOOP;
END $$;
