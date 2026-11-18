\set ON_ERROR_STOP off
BEGIN;
-- 造一个最小形态：对象 + 已过账领料单（一行明细） + 正确的归集行（source_id = 明细 id）
INSERT INTO biz_cost_object (id, object_no, object_type, product_id, target_warehouse_id, plan_qty, workshop_dept_id, workshop_dept_name, start_date, status, created_at, updated_at)
VALUES ('11111111-1111-1111-1111-111111111111', 'PROBE-M506-1', 'BATCH',
        (SELECT id FROM biz_material WHERE deleted_at IS NULL LIMIT 1),
        (SELECT id FROM biz_warehouse LIMIT 1), 100,
        (SELECT id FROM sys_dept LIMIT 1), '探针车间', '2600-01-01', 'OPEN', now(), now());
INSERT INTO biz_stock_out (id, out_no, out_type, warehouse_id, out_date, status, total_qty, total_amount, cost_object_id, created_at, updated_at)
VALUES ('22222222-2222-2222-2222-222222222222', 'PROBE-M506-OUT-1', 'ISSUE',
        (SELECT id FROM biz_warehouse LIMIT 1), '2600-01-05', 'POSTED', 10, 100,
        '11111111-1111-1111-1111-111111111111', now(), now());
INSERT INTO biz_stock_out_item (id, stock_out_id, line_no, material_id, unit_id, qty, unit_cost, amount, created_at, updated_at)
VALUES ('33333333-3333-3333-3333-333333333333', '22222222-2222-2222-2222-222222222222', 1,
        (SELECT id FROM biz_material WHERE deleted_at IS NULL LIMIT 1),
        (SELECT id FROM biz_unit LIMIT 1), 10, 10, 100, now(), now());
INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, source_id, source_no, line_no,
                              occur_date, material_id, qty, unit_price, amount, created_at, updated_at)
VALUES ('44444444-4444-4444-4444-444444444444', '11111111-1111-1111-1111-111111111111', '2600-01', 'MATERIAL',
        'STOCK_OUT', '33333333-3333-3333-3333-333333333333', 'PROBE-M506-OUT-1', 1, '2600-01-05',
        (SELECT id FROM biz_material WHERE deleted_at IS NULL LIMIT 1), 10, 10, 100, now(), now());

-- 归集行确实存在（source_id = 明细 id）
SELECT 'collect_rows' AS probe, count(*) FROM biz_cost_collect cc
 WHERE cc.source_type = 'STOCK_OUT' AND cc.source_id = '33333333-3333-3333-3333-333333333333' AND cc.deleted_at IS NULL;

-- close_facts 的现状 SQL（cc.source_id = so.id）
SELECT 'by_header_id' AS probe, count(*) FROM biz_stock_out so
 WHERE so.cost_object_id = '11111111-1111-1111-1111-111111111111' AND so.deleted_at IS NULL AND so.status = 'POSTED'
   AND NOT EXISTS (SELECT 1 FROM biz_cost_collect cc
                    WHERE cc.source_type = 'STOCK_OUT' AND cc.source_id = so.id
                      AND cc.element = 'MATERIAL' AND cc.deleted_at IS NULL);

-- 修正口径（cc.source_id = 明细行 id）
SELECT 'by_item_id' AS probe, count(*) FROM biz_stock_out so
 WHERE so.cost_object_id = '11111111-1111-1111-1111-111111111111' AND so.deleted_at IS NULL AND so.status = 'POSTED'
   AND NOT EXISTS (SELECT 1 FROM biz_cost_collect cc
                    JOIN biz_stock_out_item i ON i.id = cc.source_id
                    WHERE cc.source_type = 'STOCK_OUT' AND i.stock_out_id = so.id
                      AND cc.element = 'MATERIAL' AND cc.deleted_at IS NULL);
ROLLBACK;
