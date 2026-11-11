-- COST-B4-1 页面/导出对照用的小样本（浏览器实测用，不是性能用例）
--
-- 与 01_seed.sql 同口径（§4.7.5 算例 A），但只造 2 个对象：
--   对象 A：料 19000 + 工 4000 + 费 4000 = 27000 → 完工 22311.1104 + 在制 4688.8896
--   对象 B：料 19000 + 工 4000 + 费 4000 = 27000 → 完工 22311.1104 + 在制 4688.8896
-- 分摊：每个对象摊 3.0000（待摊 6.0000，基数 10 / 20）
--
-- 期间用 2026-08，避免与 01_seed.sql 的 2026-09 混在一起。
\set ON_ERROR_STOP on

BEGIN;

DELETE FROM biz_cost_alloc_result WHERE period_code = '2026-08';
DELETE FROM biz_cost_period WHERE period_code = '2026-08';
DELETE FROM biz_cost_object WHERE workshop_dept_name = '成本B4页面车间';
DELETE FROM biz_cost_alloc_rule WHERE rule_code = 'AR-B41-UI';

INSERT INTO biz_cost_period (id, period_code, batch_no, status, labor_source, remark)
VALUES ('b4100000-0000-4000-8000-000000000011', '2026-08', 'CS-202608-0001', 'DRAFT', 'MANUAL',
        'COST-B4-1 页面/导出对照（2 个对象）')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_cost_object (id, object_no, object_type, product_id, target_warehouse_id, plan_qty,
                             completed_qty, wip_qty, completion_pct, material_input_mode,
                             workshop_dept_id, workshop_dept_name, start_date, status)
VALUES ('b4100000-0000-4000-8000-000000000021', 'CB-202608-000001', 'BATCH',
        'd1370003-0000-4000-8000-000000000004'::uuid, 'd1370001-0000-4000-8000-000000000002'::uuid,
        1000, 800, 200, 50.00, 'ONE_TIME', gen_random_uuid(), '成本B4页面车间',
        DATE '2026-08-01', 'OPEN'),
       ('b4100000-0000-4000-8000-000000000022', 'CB-202608-000002', 'BATCH',
        'd1370003-0000-4000-8000-000000000004'::uuid, 'd1370001-0000-4000-8000-000000000002'::uuid,
        1000, 800, 200, 50.00, 'ONE_TIME', gen_random_uuid(), '成本B4页面车间',
        DATE '2026-08-01', 'OPEN')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_cost_period_item (id, period_id, cost_object_id, product_id,
                                  wip_beg_qty, wip_beg_cost, completed_qty, wip_end_qty, completion_pct,
                                  equiv_material, equiv_conversion,
                                  mat_cur, labor_cur, oh_cur,
                                  unit_material, unit_labor, unit_overhead,
                                  finished_cost, wip_end_cost, unit_finished_cost, status)
VALUES ('b4100000-0000-4000-8000-000000000031', 'b4100000-0000-4000-8000-000000000011',
        'b4100000-0000-4000-8000-000000000021', 'd1370003-0000-4000-8000-000000000004'::uuid,
        0, 0, 800, 200, 50.00, 1000, 900, 19000, 4000, 4000,
        19, 4.444444, 4.444444, 22311.1104, 4688.8896, 27.888888, 'CALC'),
       ('b4100000-0000-4000-8000-000000000032', 'b4100000-0000-4000-8000-000000000011',
        'b4100000-0000-4000-8000-000000000022', 'd1370003-0000-4000-8000-000000000004'::uuid,
        0, 0, 800, 200, 50.00, 1000, 900, 19000, 4000, 4000,
        19, 4.444444, 4.444444, 22311.1104, 4688.8896, 27.888888, 'CALC')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_cost_alloc_rule (id, rule_code, rule_name, biz_type, source_dept_id, source_dept_name,
                                 account_code, basis, priority, effective_from, status)
VALUES ('b4100000-0000-4000-8000-000000000041', 'AR-B41-UI', '成本B4页面车间制造费用按工时', 'OVERHEAD',
        gen_random_uuid(), '成本B4页面车间', '4101.01', 'LABOR_HOURS', 10, DATE '2026-01-01', 1)
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_cost_alloc_result (id, period_code, rule_id, cost_object_id, dept_id, dept_name,
                                   account_code, basis_value, basis_total, pool_amount, alloc_amount, rounding_adj)
VALUES ('b4100000-0000-4000-8000-000000000051', '2026-08', 'b4100000-0000-4000-8000-000000000041'::uuid,
        'b4100000-0000-4000-8000-000000000021', gen_random_uuid(), '成本B4页面车间',
        '4101.01', 10, 20, 6.0000, 3.0000, 0),
       ('b4100000-0000-4000-8000-000000000052', '2026-08', 'b4100000-0000-4000-8000-000000000041'::uuid,
        'b4100000-0000-4000-8000-000000000022', gen_random_uuid(), '成本B4页面车间',
        '4101.01', 10, 20, 6.0000, 3.0000, 0)
ON CONFLICT (id) DO NOTHING;

COMMIT;

\echo '--- 小样本造数结果 ---'
SELECT (SELECT count(*) FROM biz_cost_object WHERE workshop_dept_name = '成本B4页面车间') AS objects,
       (SELECT count(*) FROM biz_cost_period_item WHERE period_id = 'b4100000-0000-4000-8000-000000000011') AS items,
       (SELECT count(*) FROM biz_cost_alloc_result WHERE period_code = '2026-08') AS allocs;
SELECT id, period_code, batch_no FROM biz_cost_period WHERE period_code = '2026-08';
