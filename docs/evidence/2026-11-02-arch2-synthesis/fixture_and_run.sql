-- 收敛后 schema + 收敛后巡检编号的端到端演练（探针库 arch2_synth_poc，可随时 dropdb 重来）
-- 探针库由 cloudpivot_t06d 的 schema-only 副本重建，初始无业务数据，故本脚本不做清理（幂等靠固定 UUID 插入一次）
\set ON_ERROR_STOP on

INSERT INTO biz_supplier (id, supplier_code, supplier_name, status) VALUES
 ('11111111-1111-1111-1111-111111111111','FIXSUP-A','甲方供应商',1),
 ('22222222-2222-2222-2222-222222222222','FIXSUP-B','乙方供应商',1);

INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path) VALUES
 ('33333333-3333-3333-3333-333333333333','FIXWH-1','主仓','1');

-- 合同：乙方 = A（真相），状态已终止
INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name,
                          amount, sign_date, start_date, end_date, status)
VALUES ('44444444-4444-4444-4444-444444444444','FIXCT-1','夹具采购合同','PURCHASE','SUPPLIER',
        '11111111-1111-1111-1111-111111111111','甲方供应商', 100000, '2026-01-01','2026-01-01','2026-12-31','TERMINATED');

-- 采购单 P1：快照写了 B（≠合同乙方 A），合同已终止 → DI-01③(P0) + DI-15(P0)
INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status,
                                contract_id, purchase_type, contract_no_snapshot, supplier_name_snapshot,
                                total_amount, total_tax, total_with_tax)
VALUES ('55555555-5555-5555-5555-555555555555','FIXPO-1','22222222-2222-2222-2222-222222222222','2026-06-01',
        '33333333-3333-3333-3333-333333333333','APPROVED','44444444-4444-4444-4444-444444444444','CONTRACT',
        'FIXCT-1','乙方供应商', 1000, 0, 1000);

-- 采购单 P2：同一合同下另一个供应商 → DI-13（同合同快照互斥）
INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status,
                                contract_id, purchase_type, contract_no_snapshot, supplier_name_snapshot,
                                total_amount, total_tax, total_with_tax)
VALUES ('66666666-6666-6666-6666-666666666666','FIXPO-2','11111111-1111-1111-1111-111111111111','2026-06-02',
        '33333333-3333-3333-3333-333333333333','DRAFT','44444444-4444-4444-4444-444444444444','CONTRACT',
        'FIXCT-1','甲方供应商', 500, 0, 500);

-- 入库单：来源 P1（供应商 B），单头供应商写成 A → DI-06 DEFECT
INSERT INTO biz_stock_in (id, in_no, in_type, source_order_id, supplier_id, warehouse_id, in_date, status,
                          total_qty, total_amount, total_tax, total_with_tax)
VALUES ('77777777-7777-7777-7777-777777777777','FIXSI-1','PURCHASE','55555555-5555-5555-5555-555555555555',
        '11111111-1111-1111-1111-111111111111','33333333-3333-3333-3333-333333333333','2026-06-03','DRAFT',
        1, 500, 0, 500);

\echo '=== 逐条巡检命中（期望：DI-01 / DI-06 / DI-13 / DI-15 > 0，其余 0）'
SELECT 'DI-01' c, count(*) FROM di.c_di01 UNION ALL
SELECT 'DI-02', count(*) FROM di.c_di02 UNION ALL
SELECT 'DI-03', count(*) FROM di.c_di03 UNION ALL
SELECT 'DI-04', count(*) FROM di.c_di04 UNION ALL
SELECT 'DI-05', count(*) FROM di.c_di05 UNION ALL
SELECT 'DI-06', count(*) FROM di.c_di06 UNION ALL
SELECT 'DI-07', count(*) FROM di.c_di07 UNION ALL
SELECT 'DI-08', count(*) FROM di.c_di08 UNION ALL
SELECT 'DI-09', count(*) FROM di.c_di09 UNION ALL
SELECT 'DI-10', count(*) FROM di.c_di10 UNION ALL
SELECT 'DI-11', count(*) FROM di.c_di11 UNION ALL
SELECT 'DI-12', count(*) FROM di.c_di12 UNION ALL
SELECT 'DI-13', count(*) FROM di.c_di13 UNION ALL
SELECT 'DI-14', count(*) FROM di.c_di14 UNION ALL
SELECT 'DI-15', count(*) FROM di.c_di15 ORDER BY 1;

\echo '=== DI-01 明细（三态判定）'
SELECT issue_code, entity_type, entity_no, field_name, snapshot_value, truth_value, drift_kind, severity, affected_count FROM di.c_di01;
\echo '=== DI-06 明细'
SELECT issue_code, entity_type, entity_no, snapshot_value, truth_value, drift_kind, severity FROM di.c_di06;
\echo '=== DI-13 明细'
SELECT issue_code, entity_type, entity_no, snapshot_value, truth_value, affected_count FROM di.c_di13;
\echo '=== DI-15 明细'
SELECT issue_code, entity_no, snapshot_value, severity FROM di.c_di15;

\echo '=== 落库幂等演练（同一批缺陷连落两轮，第二轮 open 行数不变）'
INSERT INTO biz_snapshot_drift (issue_code, entity_type, entity_id, entity_no, field_name, drift_kind, severity,
                                snapshot_value, truth_value, truth_source, affected_count)
SELECT issue_code, entity_type, entity_id, entity_no, field_name, drift_kind, severity,
       snapshot_value, truth_value, truth_source, affected_count FROM di.c_di01
ON CONFLICT (issue_code, entity_type, entity_id, field_name) WHERE resolved_at IS NULL
DO UPDATE SET last_seen_at = CURRENT_TIMESTAMP, snapshot_value = EXCLUDED.snapshot_value;
INSERT INTO biz_snapshot_drift (issue_code, entity_type, entity_id, entity_no, field_name, drift_kind, severity,
                                snapshot_value, truth_value, truth_source, affected_count)
SELECT issue_code, entity_type, entity_id, entity_no, field_name, drift_kind, severity,
       snapshot_value, truth_value, truth_source, affected_count FROM di.c_di01
ON CONFLICT (issue_code, entity_type, entity_id, field_name) WHERE resolved_at IS NULL
DO UPDATE SET last_seen_at = CURRENT_TIMESTAMP, snapshot_value = EXCLUDED.snapshot_value;
SELECT count(*) AS drift_rows, count(DISTINCT id) AS distinct_ids FROM biz_snapshot_drift;
SELECT issue_type, biz_table, biz_no, priority, status, drift_kind, affected_count FROM v_biz_data_integrity_issue ORDER BY issue_type, biz_no;
