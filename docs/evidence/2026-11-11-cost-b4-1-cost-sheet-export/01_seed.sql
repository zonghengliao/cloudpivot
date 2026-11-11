-- COST-B4-1 导出实测数据：1 个批次 + 1000 个成本对象 + 1000 行计算单 + 1 条分摊规则 + 1000 行分摊结果
--
-- 口径全部照 ARCH-9 §4.7.5 算例 A：料 19000 + 工 4000 + 费 4000 = 27000
--   完工转出 22311.1104 + 期末在制 4688.8896 = 27000   （满足 ck_biz_cost_item_cons）
--   约当产量 材料 1000 / 转换 900，完工 800，期末在制 200（进度 50%）
--
-- 幂等：先按期间把本脚本造的数据删干净（分摊结果无软删 → 整删；计算单/对象/批次按 id 级联）。
\set ON_ERROR_STOP on

BEGIN;

-- ---------- 0. 清场（只清 2026-09 的实测数据） ----------
DELETE FROM biz_cost_alloc_result WHERE period_code = '2026-09';
DELETE FROM biz_cost_period WHERE period_code = '2026-09';
DELETE FROM biz_cost_object WHERE workshop_dept_name = '成本B4实测车间';
DELETE FROM biz_cost_alloc_rule WHERE rule_code = 'AR-B41';

-- ---------- 1. 期间批次（DRAFT：单头 total_* 恒 0，符合 ck_..._sum / ck_..._cons） ----------
INSERT INTO biz_cost_period (id, period_code, batch_no, status, labor_source, remark)
VALUES ('b4100000-0000-4000-8000-000000000001', '2026-09', 'CS-202609-0001', 'DRAFT', 'MANUAL',
        'COST-B4-1 导出实测：1000 个成本对象')
ON CONFLICT (id) DO NOTHING;

-- ---------- 2. 1000 个成本对象（主数据弱引用一律 gen_random_uuid，不牵连别的表） ----------
INSERT INTO biz_cost_object (id, object_no, object_type, product_id, target_warehouse_id, plan_qty,
                             completed_qty, wip_qty, completion_pct, material_input_mode,
                             workshop_dept_id, workshop_dept_name, start_date, status)
SELECT gen_random_uuid(),
       'CB-202609-' || lpad(g::text, 6, '0'),
       'BATCH',
       'd1370003-0000-4000-8000-000000000004'::uuid,   -- 种子：成品D
       'd1370001-0000-4000-8000-000000000002'::uuid,   -- 种子：分仓
       1000 + g,
       800, 200, 50.00, 'ONE_TIME',
       gen_random_uuid(), '成本B4实测车间', DATE '2026-09-01', 'OPEN'
  FROM generate_series(1, 1000) AS g;

-- ---------- 3. 1000 行计算单（对象 × 期间，唯一键 uk_biz_cost_item 一对象一张） ----------
INSERT INTO biz_cost_period_item (id, period_id, cost_object_id, product_id,
                                  wip_beg_qty, wip_beg_cost, completed_qty, wip_end_qty, completion_pct,
                                  equiv_material, equiv_conversion,
                                  mat_cur, labor_cur, oh_cur,
                                  unit_material, unit_labor, unit_overhead,
                                  finished_cost, wip_end_cost, unit_finished_cost, status)
SELECT gen_random_uuid(),
       'b4100000-0000-4000-8000-000000000001'::uuid,
       o.id, o.product_id,
       0, 0, 800, 200, 50.00,
       1000, 900,
       19000, 4000, 4000,
       19.000000, 4.444444, 4.444444,
       22311.1104, 4688.8896, 27.888888,
       'CALC'
  FROM biz_cost_object o
 WHERE o.workshop_dept_name = '成本B4实测车间';

-- ---------- 4. 分摊规则 + 1000 行分摊结果（对象数 × 1 条规则 × 1 个科目） ----------
INSERT INTO biz_cost_alloc_rule (id, rule_code, rule_name, biz_type, source_dept_id, source_dept_name,
                                 account_code, basis, priority, effective_from, status)
VALUES ('b4100000-0000-4000-8000-000000000002', 'AR-B41', '成本B4实测车间制造费用按工时', 'OVERHEAD',
        gen_random_uuid(), '成本B4实测车间', '4101.01', 'LABOR_HOURS', 10, DATE '2026-01-01', 1)
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_cost_alloc_result (id, period_code, rule_id, cost_object_id, dept_id, dept_name,
                                   account_code, basis_value, basis_total, pool_amount, alloc_amount, rounding_adj)
SELECT gen_random_uuid(), '2026-09', 'b4100000-0000-4000-8000-000000000002'::uuid,
       o.id, 'b4100000-0000-4000-8000-000000000002'::uuid, '成本B4实测车间',
       '4101.01', 10, 10000, 3000.0000, 3.0000, 0
  FROM biz_cost_object o
 WHERE o.workshop_dept_name = '成本B4实测车间';

COMMIT;

-- ---------- 5. 自检（打印给证据留档） ----------
\echo '--- 造数结果 ---'
SELECT (SELECT count(*) FROM biz_cost_period WHERE period_code = '2026-09')            AS periods,
       (SELECT count(*) FROM biz_cost_object WHERE workshop_dept_name = '成本B4实测车间') AS objects,
       (SELECT count(*) FROM biz_cost_period_item i
          JOIN biz_cost_period p ON p.id = i.period_id
         WHERE p.period_code = '2026-09')                                              AS items,
       (SELECT count(*) FROM biz_cost_alloc_result WHERE period_code = '2026-09')      AS allocs;
SELECT p.id AS period_id, p.batch_no
  FROM biz_cost_period p WHERE p.period_code = '2026-09';
