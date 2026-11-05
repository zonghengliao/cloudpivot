-- P2-7 DI-26 夹具：在**一个事务里**造齐 6 类命中，跑完 ROLLBACK（库侧零残留）
\set ON_ERROR_STOP on
BEGIN;

\echo == 夹具前基线（干净库上只有存量快照 NULL 一类）==
SELECT drift_kind, severity, count(*) AS n FROM di.c_di26 GROUP BY 1, 2 ORDER BY 1, 2;

-- ① NAME_DUP：两个活跃供应商同名（只有首尾空格不同 → 归一后同名）
INSERT INTO biz_supplier (supplier_code, supplier_name, short_name, status, credit_days, credit_limit)
VALUES ('P27-DUP-A', 'P2-7 同名测试供应商', NULL, 1, 0, 0),
       ('P27-DUP-B', 'p2-7 同名测试供应商  ', NULL, 1, 0, 0);

-- ② SHORT_DUP：两个活跃供应商同简称（首尾空格不同）
INSERT INTO biz_supplier (supplier_code, supplier_name, short_name, status, credit_days, credit_limit)
VALUES ('P27-SHT-A', 'P2-7 简称甲（全名不同）', '同一简称', 1, 0, 0),
       ('P27-SHT-B', 'P2-7 简称乙（全名不同）', '  同一简称 ', 1, 0, 0);

-- ③ SHORT_SHADOWS_NAME：新供应商的简称撞了已有供应商的全名（P7-SUP）
INSERT INTO biz_supplier (supplier_code, supplier_name, short_name, status, credit_days, credit_limit)
VALUES ('P27-SHW-A', 'P2-7 撞名测试供应商', 'P7 付款测试供应商', 1, 0, 0);

-- ④ SNAPSHOT_LATE_WRITE：SPOT 采购单建单晚于主档最后修改，却写了旧名
UPDATE biz_purchase_order
   SET supplier_name_snapshot = 'W1B 旧名（改名前的名字）', created_at = now()
 WHERE order_no = 'W1B-PO-0001';

-- ⑤ SNAPSHOT_DRIFT（合同）：签约快照名与主档现名不同（合法历史的形态）
UPDATE biz_contract
   SET party_b_name = party_b_name || '（旧名）'
 WHERE id = (SELECT id FROM biz_contract
              WHERE deleted_at IS NULL AND party_b_id IS NOT NULL
              ORDER BY contract_no LIMIT 1);

-- ⑥ SNAPSHOT_DRIFT（审批单）：form_data 里冻结的显示名与主档现名不同
UPDATE biz_flow_instance
   SET form_data = jsonb_set(form_data, '{supplier_name}',
                             to_jsonb('P17 付款审批测试供应商（旧名）'::text))
 WHERE biz_no = 'PM-20261105-000048';

\echo == 夹具后：逐条明细 ==
SELECT entity_type, entity_no, field_name, snapshot_value, truth_value, drift_kind, severity, affected_count
  FROM di.c_di26 ORDER BY drift_kind, severity, entity_type, entity_no;

\echo == 断言：6 类分支命中数（期望 NAME_DUP=1 SHORT_DUP=1 SHORT_SHADOWS_NAME=1 LATE_WRITE=1 DRIFT=2 MISSING=3）==
WITH b AS (
    SELECT truth_value,
           CASE
             WHEN truth_value = '活跃供应商里这个名字只能出现一次'            THEN 'NAME_DUP'
             WHEN truth_value = '活跃供应商里这个简称只能出现一次'            THEN 'SHORT_DUP'
             WHEN truth_value = '简称不得与任何其它活跃供应商的全名相同'      THEN 'SHORT_SHADOWS_NAME'
             WHEN drift_kind = 'DEFECT' AND field_name = 'supplier_name_snapshot' THEN 'SNAPSHOT_LATE_WRITE'
             WHEN snapshot_value LIKE '(快照 NULL%'                          THEN 'SNAPSHOT_MISSING'
             WHEN drift_kind = 'SNAPSHOT'                                   THEN 'SNAPSHOT_DRIFT'
             ELSE 'UNKNOWN'
           END AS branch
      FROM di.c_di26
)
SELECT k.branch, k.expected, coalesce(b.actual, 0) AS actual,
       coalesce(b.actual, 0) = k.expected AS ok
  FROM (VALUES ('NAME_DUP', 1), ('SHORT_DUP', 1), ('SHORT_SHADOWS_NAME', 1),
               ('SNAPSHOT_LATE_WRITE', 1), ('SNAPSHOT_DRIFT', 2), ('SNAPSHOT_MISSING', 3))
       AS k(branch, expected)
  LEFT JOIN (SELECT branch, count(*) AS actual FROM b GROUP BY 1) b USING (branch)
 ORDER BY k.branch;

\echo == 断言：无 UNKNOWN 分支 / WARN 总计 4 条 ==
SELECT (SELECT count(*) FROM di.c_di26 WHERE drift_kind = 'DEFECT') AS defect_warn_total,
       (SELECT count(*) FROM di.c_di26 WHERE severity = 'ERROR')     AS error_total;

ROLLBACK;

\echo == 回滚后：夹具残留必须 0，命中数回到基线，快照恢复 NULL ==
SELECT (SELECT count(*) FROM biz_supplier WHERE supplier_code LIKE 'P27-%') AS residual_supplier,
       (SELECT count(*) FROM di.c_di26) AS hits_after_rollback,
       (SELECT count(*) FROM biz_purchase_order
         WHERE order_no = 'W1B-PO-0001' AND supplier_name_snapshot IS NOT NULL) AS residual_po_snapshot,
       (SELECT count(*) FROM biz_flow_instance
         WHERE biz_no = 'PM-20261105-000048' AND form_data->>'supplier_name' LIKE '%旧名%') AS residual_flow;
