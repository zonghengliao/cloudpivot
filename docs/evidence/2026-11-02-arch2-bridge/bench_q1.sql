-- Q1 性能量测：列表页「现状 / 方案A(纯关联) / 方案B(同表快照)」三路对比
-- 场景：biz_purchase_order 10 万行，biz_contract 2 千行，biz_supplier 2 千行（PG 18.6，单机）
\pset pager off

-- 合同表最小替身（正式结构见「合同表」卡 V0193；本文件只做量测用）
CREATE TABLE IF NOT EXISTS biz_contract (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    contract_no   VARCHAR(32)  NOT NULL,
    contract_name VARCHAR(200) NOT NULL,
    contract_type VARCHAR(16)  NOT NULL,
    party_b_type  VARCHAR(16)  NOT NULL,
    party_b_id    UUID         NOT NULL,
    party_b_name  VARCHAR(200) NOT NULL,
    amount        NUMERIC(20,4) NOT NULL DEFAULT 0,
    status        VARCHAR(16)  NOT NULL DEFAULT 'DRAFT',
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at    TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted_at    TIMESTAMPTZ
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_contract_no ON biz_contract (contract_no) WHERE deleted_at IS NULL;

TRUNCATE biz_purchase_order_item, biz_purchase_order, biz_stock_in_item, biz_stock_in CASCADE;
TRUNCATE biz_contract CASCADE;
DELETE FROM biz_supplier WHERE supplier_code LIKE 'BENCH-SUP-%';

INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
SELECT gen_random_uuid(), 'BENCH-SUP-' || lpad(g::text, 5, '0'), '基准供应商 ' || g, 1
  FROM generate_series(1, 2000) g;

INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name, amount, status)
SELECT gen_random_uuid(), 'HT-BENCH-' || lpad(g::text, 6, '0'), '基准合同 ' || g,
       'PURCHASE', 'SUPPLIER',
       (SELECT id FROM biz_supplier WHERE supplier_code = 'BENCH-SUP-' || lpad((1 + (g % 2000))::text, 5, '0')),
       '基准供应商 ' || (1 + (g % 2000)), 0, 'ACTIVE'
  FROM generate_series(1, 2000) g;

-- 10 万张采购单：9 万张挂合同（CONTRACT），1 万张零星（SPOT）
INSERT INTO biz_purchase_order (order_no, supplier_id, order_date, warehouse_id, status,
                                total_qty, total_amount, total_tax, total_with_tax,
                                contract_id, purchase_type, contract_no_snapshot, supplier_name_snapshot)
SELECT 'PO-BENCH-' || lpad(g::text, 8, '0'),
       (SELECT id FROM biz_supplier WHERE supplier_code = 'BENCH-SUP-' || lpad((1 + (g % 2000))::text, 5, '0')),
       DATE '2026-01-01' + (g % 300),
       'd1370001-0000-4000-8000-000000000002',
       'APPROVED', 1, 100, 13, 113,
       CASE WHEN g % 10 = 0 THEN NULL ELSE (SELECT id FROM biz_contract WHERE contract_no = 'HT-BENCH-' || lpad((1 + (g % 2000))::text, 6, '0')) END,
       CASE WHEN g % 10 = 0 THEN 'SPOT' ELSE 'CONTRACT' END,
       CASE WHEN g % 10 = 0 THEN NULL ELSE 'HT-BENCH-' || lpad((1 + (g % 2000))::text, 6, '0') END,
       CASE WHEN g % 10 = 0 THEN '基准供应商 ' || (1 + (g % 2000)) ELSE '基准供应商 ' || (1 + (g % 2000)) END
  FROM generate_series(1, 100000) g;

ANALYZE biz_purchase_order;
ANALYZE biz_contract;
ANALYZE biz_supplier;

\echo ''
\echo '===== A. 现状：列表页单表查询（无 JOIN，供应商名由前端字典补） ====='
EXPLAIN (ANALYZE, BUFFERS OFF, SUMMARY ON)
SELECT id, order_no, supplier_id, order_date, status, total_with_tax
  FROM biz_purchase_order
 WHERE deleted_at IS NULL
 ORDER BY order_date DESC, order_no DESC
 LIMIT 20;

\echo ''
\echo '===== B. 方案A（纯关联）：列表必须 JOIN 合同 + 供应商才出名称 ====='
EXPLAIN (ANALYZE, BUFFERS OFF, SUMMARY ON)
SELECT o.id, o.order_no, o.supplier_id, o.order_date, o.status, o.total_with_tax,
       c.contract_no, s.supplier_name
  FROM biz_purchase_order o
  LEFT JOIN biz_contract c ON c.id = o.contract_id AND c.deleted_at IS NULL
  LEFT JOIN biz_supplier s ON s.id = c.party_b_id  AND s.deleted_at IS NULL
 WHERE o.deleted_at IS NULL
 ORDER BY o.order_date DESC, o.order_no DESC
 LIMIT 20;

\echo ''
\echo '===== C. 方案B（同表快照）：列表 0 JOIN，合同号/供应商名同表出列 ====='
EXPLAIN (ANALYZE, BUFFERS OFF, SUMMARY ON)
SELECT id, order_no, supplier_id, order_date, status, total_with_tax,
       contract_no_snapshot, supplier_name_snapshot
  FROM biz_purchase_order
 WHERE deleted_at IS NULL
 ORDER BY order_date DESC, order_no DESC
 LIMIT 20;

\echo ''
\echo '===== D. 方案A 深翻页（OFFSET 2000）：JOIN 代价每页重做 ====='
EXPLAIN (ANALYZE, BUFFERS OFF, SUMMARY ON)
SELECT o.id, o.order_no, o.supplier_id, o.order_date, o.status, o.total_with_tax,
       c.contract_no, s.supplier_name
  FROM biz_purchase_order o
 LEFT JOIN biz_contract c ON c.id = o.contract_id AND c.deleted_at IS NULL
 LEFT JOIN biz_supplier s ON s.id = c.party_b_id  AND s.deleted_at IS NULL
 WHERE o.deleted_at IS NULL
 ORDER BY o.order_date DESC, o.order_no DESC
 LIMIT 20 OFFSET 2000;

\echo ''
\echo '===== E. 合同维度回查（合同执行情况页）：部分索引是否生效 ====='
EXPLAIN (ANALYZE, BUFFERS OFF, SUMMARY ON)
SELECT o.order_no, o.order_date, o.total_with_tax
  FROM biz_purchase_order o
 WHERE o.contract_id = (SELECT id FROM biz_contract WHERE contract_no = 'HT-BENCH-000007')
   AND o.deleted_at IS NULL
 ORDER BY o.order_date DESC;
