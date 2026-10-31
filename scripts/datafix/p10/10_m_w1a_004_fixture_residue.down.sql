-- ============================================================================
-- P10 / C3 回滚：把 M-W1A-004 的夹具行原样写回
-- ----------------------------------------------------------------------------
-- 快照来源：prod `cloudpivot` 2026-10-31 10:45 逐列导出（同 `00_backup.sh` 的
--           `biz_inventory.sql` / `biz_inventory_location.sql` / `biz_inventory_txn.sql` /
--           `biz_material_serial.sql`），id 与时间戳一律用快照原值，不做「现在的 now()」——
--           回滚要回到修复前的**同一个状态**，不是回到「大概齐一样」。
--
-- 幂等：INSERT 全部 `ON CONFLICT DO NOTHING`（主键就是快照 id）：
--       UPDATE 全部带 `deleted_at IS NOT NULL`（第二遍命中 0 行）。
-- 顺序：先写回成本账 / 库位账 / 流水，再恢复序列号（序列号的 location_id 为 NULL，
--       与库位账无外键关系，顺序其实无关，但从「先父后子」排更稳）。
-- 不变量回归：down 之后 B1 / B4 / E4 应重新「等式成立但带着这批夹具」，
--       C3 / C5 重新变成 WARN（这正是修复前的真实状态）。
-- ============================================================================
DO $$
DECLARE
    v_material uuid;
    v_wh01     uuid;
    v_wh02     uuid;
    v_inv      integer := 0;
    v_loc      integer := 0;
    v_txn      integer := 0;
    v_serial   integer := 0;
    v_n        integer := 0;
BEGIN
    SELECT id INTO v_material FROM biz_material
     WHERE material_code = 'M-W1A-004' AND deleted_at IS NULL ORDER BY created_at LIMIT 1;
    SELECT id INTO v_wh01 FROM biz_warehouse WHERE warehouse_code = 'WH01' ORDER BY created_at LIMIT 1;
    SELECT id INTO v_wh02 FROM biz_warehouse WHERE warehouse_code = 'WH02' ORDER BY created_at LIMIT 1;

    IF v_material IS NULL OR v_wh01 IS NULL OR v_wh02 IS NULL THEN
        RAISE NOTICE 'P10-down: 物料或种子仓库缺失，跳过（本脚本只在还原修复前状态时用）';
        RETURN;
    END IF;

    -- ---------- ① 成本账（2 行）----------
    INSERT INTO biz_inventory
        (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount, last_txn_at, created_at, updated_at)
    VALUES
        ('01a18cb6-73b9-72f7-92b4-bab63d104093', v_material, v_wh02, NULL,
         0.0000, 0.0000, 10.000000, 0.0000,
         '2026-10-30 23:39:25.226602+08', '2026-10-30 20:08:49.847153+08', '2026-10-30 23:39:25.226602+08'),
        ('01a18d77-40b5-74b8-988f-4169cadc9468', v_material, v_wh01, NULL,
         2.0000, 0.0000, 0.000000, 0.0000,
         '2026-10-30 23:39:25.226602+08', '2026-10-30 23:39:25.226602+08', '2026-10-31 10:13:02.458332+08')
    ON CONFLICT DO NOTHING;
    GET DIAGNOSTICS v_inv = ROW_COUNT;

    -- ---------- ② 库位账（WH01 未分配库位桶 2 件）----------
    INSERT INTO biz_inventory_location
        (id, material_id, warehouse_id, location_id, qty, last_txn_at, created_at)
    VALUES
        ('da2492a8-217e-4428-8a75-9d850f367c59', v_material, v_wh01, NULL,
         2.0000, '2026-10-31 10:13:02.334375+08', '2026-10-31 10:13:02.334375+08')
    ON CONFLICT DO NOTHING;
    GET DIAGNOSTICS v_loc = ROW_COUNT;

    -- ---------- ③ 流水（SERIAL_IN / SERIAL_OUT / 夹具调拨的 TRANSFER_IN、OUT）----------
    INSERT INTO biz_inventory_txn
        (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no,
         qty, unit_cost, amount, before_qty, before_amount, before_cost,
         after_qty, after_amount, after_cost, period_code, operator_id, remark, created_at)
    VALUES
        ('01a18cb6-73ba-77c6-89a1-6e253b9d2b33', v_material, v_wh02, 'IN', 'SERIAL_IN', 'SERIAL_BATCH',
         '01a18cb6-73b8-76ac-9077-e058523eae16', 'SN-20261030-000009', 1,
         3.0000, 0.000000, 0.0000, 0.0000, 0.0000, 0.000000,
         3.0000, 0.0000, 0.000000, '2026-10', '01a18bc8-bd0c-7792-9118-90fccc9ef0a2',
         '批量生成 3 件（SN-20261030-000009 起）', '2026-10-30 20:08:49.849701+08'),
        ('01a18cb6-73c2-746a-8eb0-ccb5ce701a4d', v_material, v_wh02, 'OUT', 'SERIAL_OUT', 'MATERIAL_SERIAL',
         '01a18cb6-73b8-76ac-9077-e058523eae16', 'SN-20261030-000009', 1,
         1.0000, 0.000000, 0.0000, 3.0000, 0.0000, 0.000000,
         2.0000, 0.0000, 0.000000, '2026-10', '01a18bc8-bd0c-7792-9118-90fccc9ef0a2',
         '序列号出库：SN-20261030-000009', '2026-10-30 20:08:49.858556+08'),
        ('01a18d77-40b7-7627-a198-adf9d19cca1d', v_material, v_wh01, 'IN', 'TRANSFER_IN', 'STOCK_TRANSFER',
         '01a18d77-34c6-766e-ae4c-7bed0d31e135', 'TK-20261030-000001', 1,
         2.0000, 10.000000, 20.0000, 0.0000, 0.0000, 10.000000,
         2.0000, 20.0000, 10.000000, '2026-10', '01a18bc8-bd0c-7792-9118-90fccc9ef0a2',
         '', '2026-10-30 23:39:25.226602+08'),
        ('01a18d77-40b3-7725-b169-33526ccd5fd3', v_material, v_wh02, 'OUT', 'TRANSFER_OUT', 'STOCK_TRANSFER',
         '01a18d77-34c6-766e-ae4c-7bed0d31e135', 'TK-20261030-000001', 1,
         2.0000, 10.000000, 20.0000, 2.0000, 20.0000, 10.000000,
         0.0000, 0.0000, 10.000000, '2026-10', '01a18bc8-bd0c-7792-9118-90fccc9ef0a2',
         '', '2026-10-30 23:39:25.226602+08')
    ON CONFLICT DO NOTHING;
    GET DIAGNOSTICS v_txn = ROW_COUNT;

    -- ---------- ④ 序列号：把 3 条存活号从软删态还原（`deleted_at` 与 `updated_at` 用快照值）----------
    UPDATE biz_material_serial SET deleted_at = NULL, updated_at = '2026-10-30 20:08:49.858036+08'
     WHERE material_id = v_material AND serial_no = 'SN-20261030-000009' AND deleted_at IS NOT NULL;
    GET DIAGNOSTICS v_serial = ROW_COUNT;
    UPDATE biz_material_serial SET deleted_at = NULL, updated_at = '2026-10-30 23:39:25.226602+08'
     WHERE material_id = v_material AND serial_no IN ('SN-20261030-000010', 'SN-20261030-000011')
       AND deleted_at IS NOT NULL;
    GET DIAGNOSTICS v_n = ROW_COUNT;
    v_serial := v_serial + v_n;

    RAISE NOTICE 'P10-down: 已还原 —— 成本账 % 行 / 库位账 % 行 / 流水 % 条 / 序列号 % 条',
        v_inv, v_loc, v_txn, v_serial;
END
$$;
