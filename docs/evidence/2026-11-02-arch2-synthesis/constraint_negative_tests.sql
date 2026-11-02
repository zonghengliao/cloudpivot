-- 约束真的拦得住（不是纸面约束）：每条反例在子事务里执行，捕获 SQLSTATE 后回滚
\set ON_ERROR_STOP on
\echo '=== 反例 1：合同 amount = 0（期望 23514 ck_biz_contract_amount）'
DO $$ BEGIN
  INSERT INTO biz_contract (contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount)
  VALUES ('FIXBAD-1','零额','PURCHASE','SUPPLIER','11111111-1111-1111-1111-111111111111','甲方供应商', 0);
  RAISE EXCEPTION 'UNEXPECTED_PASS';
EXCEPTION WHEN check_violation THEN RAISE NOTICE 'BLOCKED [%] %', SQLSTATE, SQLERRM;
END $$;

\echo '=== 反例 2：SALES 合同却挂 SUPPLIER（期望 23514 ck_biz_contract_type_party_pair）'
DO $$ BEGIN
  INSERT INTO biz_contract (contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount)
  VALUES ('FIXBAD-2','类型错配','SALES','SUPPLIER','11111111-1111-1111-1111-111111111111','甲方供应商', 100);
  RAISE EXCEPTION 'UNEXPECTED_PASS';
EXCEPTION WHEN check_violation THEN RAISE NOTICE 'BLOCKED [%] %', SQLSTATE, SQLERRM;
END $$;

\echo '=== 反例 3：status=SIGNED 但三个日期全空（期望 23514 ck_biz_contract_signed_dates）'
DO $$ BEGIN
  INSERT INTO biz_contract (contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount, status)
  VALUES ('FIXBAD-3','已签无日期','PURCHASE','SUPPLIER','11111111-1111-1111-1111-111111111111','甲方供应商', 100,'SIGNED');
  RAISE EXCEPTION 'UNEXPECTED_PASS';
EXCEPTION WHEN check_violation THEN RAISE NOTICE 'BLOCKED [%] %', SQLSTATE, SQLERRM;
END $$;

\echo '=== 反例 4：活跃行合同号重号（期望 23505 uk_biz_contract_no）'
DO $$ BEGIN
  INSERT INTO biz_contract (contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount, sign_date, start_date, end_date, status)
  VALUES ('FIXCT-1','重号','PURCHASE','SUPPLIER','11111111-1111-1111-1111-111111111111','甲方供应商', 100,'2026-01-01','2026-01-01','2026-12-31','ACTIVE');
  RAISE EXCEPTION 'UNEXPECTED_PASS';
EXCEPTION WHEN unique_violation THEN RAISE NOTICE 'BLOCKED [%] %', SQLSTATE, SQLERRM;
END $$;

\echo '=== 反例 5：CONTRACT 采购单缺 contract_id（期望 23514 ck_biz_po_contract_pair）'
DO $$ BEGIN
  INSERT INTO biz_purchase_order (order_no, supplier_id, order_date, warehouse_id, status,
                                  purchase_type, contract_no_snapshot, total_amount, total_tax, total_with_tax)
  VALUES ('FIXBAD-PO-1','11111111-1111-1111-1111-111111111111','2026-06-01','33333333-3333-3333-3333-333333333333','DRAFT',
          'CONTRACT','FIXCT-1', 1,0,1);
  RAISE EXCEPTION 'UNEXPECTED_PASS';
EXCEPTION WHEN check_violation THEN RAISE NOTICE 'BLOCKED [%] %', SQLSTATE, SQLERRM;
END $$;

\echo '=== 反例 6：SPOT 采购单带 contract_id（期望 23514 ck_biz_po_contract_pair）'
DO $$ BEGIN
  INSERT INTO biz_purchase_order (order_no, supplier_id, order_date, warehouse_id, status,
                                  contract_id, purchase_type, total_amount, total_tax, total_with_tax)
  VALUES ('FIXBAD-PO-2','11111111-1111-1111-1111-111111111111','2026-06-01','33333333-3333-3333-3333-333333333333','DRAFT',
          '44444444-4444-4444-4444-444444444444','SPOT', 1,0,1);
  RAISE EXCEPTION 'UNEXPECTED_PASS';
EXCEPTION WHEN check_violation THEN RAISE NOTICE 'BLOCKED [%] %', SQLSTATE, SQLERRM;
END $$;

\echo '=== 反例 7：非采购入库带供应商（期望 23514 ck_biz_si_supplier_scope）'
DO $$ BEGIN
  INSERT INTO biz_stock_in (in_no, in_type, supplier_id, warehouse_id, in_date, status,
                            total_qty, total_amount, total_tax, total_with_tax)
  VALUES ('FIXBAD-SI-1','OTHER','11111111-1111-1111-1111-111111111111','33333333-3333-3333-3333-333333333333','2026-06-01','DRAFT',
          1,1,0,1);
  RAISE EXCEPTION 'UNEXPECTED_PASS';
EXCEPTION WHEN check_violation THEN RAISE NOTICE 'BLOCKED [%] %', SQLSTATE, SQLERRM;
END $$;

\echo '=== 反例 8：漂移表 issue_code 非 DI-NN 形状（期望 23514 ck_biz_drift_code）'
DO $$ BEGIN
  INSERT INTO biz_snapshot_drift (issue_code, entity_type, entity_id, entity_no, field_name, drift_kind, severity)
  VALUES ('I-P1','CONTRACT','44444444-4444-4444-4444-444444444444','FIXCT-1','party_b_id','DEFECT','ERROR');
  RAISE EXCEPTION 'UNEXPECTED_PASS';
EXCEPTION WHEN check_violation THEN RAISE NOTICE 'BLOCKED [%] %', SQLSTATE, SQLERRM;
END $$;

\echo '=== 正例：同一 (issue_code,entity,field) 未解决行只能有一条（期望 23505）'
DO $$ BEGIN
  INSERT INTO biz_snapshot_drift (issue_code, entity_type, entity_id, entity_no, field_name, drift_kind, severity)
  VALUES ('DI-01','CONTRACT','44444444-4444-4444-4444-444444444444','FIXCT-1','party_b_id','DEFECT','ERROR');
  RAISE EXCEPTION 'UNEXPECTED_PASS';
EXCEPTION WHEN unique_violation THEN RAISE NOTICE 'BLOCKED [%] 重复的未解决漂移被唯一键挡住', SQLSTATE;
END $$;
