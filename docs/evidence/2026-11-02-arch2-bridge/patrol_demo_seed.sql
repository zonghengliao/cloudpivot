-- 巡检 SQL 的真库演练数据：把 4 类问题各造 1 条，跑 patrol_snapshot_drift.sql 必须全部命中
\pset pager off

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

DELETE FROM biz_stock_in  WHERE in_no  LIKE 'IN-PATROL-%';
DELETE FROM biz_purchase_order WHERE order_no LIKE 'PO-PATROL-%';
DELETE FROM biz_contract  WHERE contract_no LIKE 'HT-PATROL-%';
DELETE FROM biz_supplier  WHERE supplier_code LIKE 'PATROL-SUP-%';

INSERT INTO biz_supplier (id, supplier_code, supplier_name, status) VALUES
 ('c0000000-0000-4000-8000-000000000001','PATROL-SUP-1','演练供应商一',1),
 ('c0000000-0000-4000-8000-000000000002','PATROL-SUP-2','演练供应商二',1);

-- 合同 C1：乙方 = 供应商一，2026-11-01 改过（updated_at 晚于部分采购单的 created_at）
INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type, party_b_id,
                          party_b_name, amount, status, created_at, updated_at) VALUES
 ('c0000000-0000-4000-8000-0000000000c1','HT-PATROL-0001','演练采购合同','PURCHASE','SUPPLIER',
  'c0000000-0000-4000-8000-000000000001','演练供应商一',0,'ACTIVE',
  TIMESTAMPTZ '2026-10-01 00:00:00+08', TIMESTAMPTZ '2026-11-01 00:00:00+08');

-- PO-A 干净；PO-B 合同事后换乙方（SNAPSHOT）；PO-C 建单时就不一致（DEFECT）；PO-D 合同悬空（DEFECT）
INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status,
                                total_qty, total_amount, total_tax, total_with_tax,
                                contract_id, purchase_type, contract_no_snapshot, supplier_name_snapshot, created_at) VALUES
 ('c0000000-0000-4000-8000-0000000000a1','PO-PATROL-A','c0000000-0000-4000-8000-000000000001',
  DATE '2026-10-05','d1370001-0000-4000-8000-000000000002','APPROVED',1,100,13,113,
  'c0000000-0000-4000-8000-0000000000c1','CONTRACT','HT-PATROL-0001','演练供应商一',
  TIMESTAMPTZ '2026-10-05 09:00:00+08'),
 ('c0000000-0000-4000-8000-0000000000b1','PO-PATROL-B','c0000000-0000-4000-8000-000000000002',
  DATE '2026-10-06','d1370001-0000-4000-8000-000000000002','APPROVED',1,100,13,113,
  'c0000000-0000-4000-8000-0000000000c1','CONTRACT','HT-PATROL-0001','演练供应商二',
  TIMESTAMPTZ '2026-10-06 09:00:00+08'),
 ('c0000000-0000-4000-8000-0000000000c1','PO-PATROL-C','c0000000-0000-4000-8000-000000000002',
  DATE '2026-11-02','d1370001-0000-4000-8000-000000000002','APPROVED',1,100,13,113,
  'c0000000-0000-4000-8000-0000000000c1','CONTRACT','HT-PATROL-0001','演练供应商二',
  TIMESTAMPTZ '2026-11-02 09:00:00+08'),
 ('c0000000-0000-4000-8000-0000000000d1','PO-PATROL-D','c0000000-0000-4000-8000-000000000001',
  DATE '2026-11-02','d1370001-0000-4000-8000-000000000002','APPROVED',1,100,13,113,
  'c0000000-0000-4000-8000-00000000dead','CONTRACT','HT-DELETED-9999','演练供应商一',
  TIMESTAMPTZ '2026-11-02 09:00:00+08');

-- SI-1 干净；SI-2 供应商漂移（DEFECT）；SI-3 来源悬空（DEFECT）；SI-4 无来源（GAUGE，合法）
INSERT INTO biz_stock_in (id, in_no, in_type, source_order_id, supplier_id, warehouse_id, in_date, status,
                          total_qty, total_amount, total_tax, total_with_tax) VALUES
 ('c0000000-0000-4000-8000-0000000000e1','IN-PATROL-1','PURCHASE',
  'c0000000-0000-4000-8000-0000000000a1','c0000000-0000-4000-8000-000000000001',
  'd1370001-0000-4000-8000-000000000002',DATE '2026-10-07','POSTED',1,10,1.3,11.3),
 ('c0000000-0000-4000-8000-0000000000e2','IN-PATROL-2','PURCHASE',
  'c0000000-0000-4000-8000-0000000000a1','c0000000-0000-4000-8000-000000000002',
  'd1370001-0000-4000-8000-000000000002',DATE '2026-10-08','POSTED',1,10,1.3,11.3),
 ('c0000000-0000-4000-8000-0000000000e3','IN-PATROL-3','PURCHASE',
  'c0000000-0000-4000-8000-00000000dead','c0000000-0000-4000-8000-000000000001',
  'd1370001-0000-4000-8000-000000000002',DATE '2026-10-09','POSTED',1,10,1.3,11.3),
 ('c0000000-0000-4000-8000-0000000000e4','IN-PATROL-4','PURCHASE',
  NULL,'c0000000-0000-4000-8000-000000000001',
  'd1370001-0000-4000-8000-000000000002',DATE '2026-10-10','POSTED',1,10,1.3,11.3);

SELECT '演练数据就绪：' || (SELECT count(*) FROM biz_purchase_order WHERE order_no LIKE 'PO-PATROL-%') || ' 张采购单 / '
     || (SELECT count(*) FROM biz_stock_in WHERE in_no LIKE 'IN-PATROL-%') || ' 张入库单' AS ready;
