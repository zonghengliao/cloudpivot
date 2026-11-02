-- ============================================================
-- 10_checks.sql —— 巡检项定义（13 条 + 1 条护栏自检）
--
-- 每个巡检项 = 一个只读视图 di.c_diNN，输出统一 11 列：
--   issue_code / entity_type / entity_id / entity_no / field_name
--   snapshot_value(实际观测) / truth_value(期望·真相) / truth_source
--   drift_kind(SNAPSHOT|DEFECT) / severity(INFO|WARN|ERROR) / affected_count
--
-- 为什么是视图而不是存储过程：① 可直接 `select * from di.c_diNN` 人工复核；
--   ② 巡检任务只需一条 INSERT ... SELECT（见 20_upsert_converge.sql）；
--   ③ 视图无状态，重建无害，不会成为第二真相。
--
-- 依赖：00_install_compat.sql（di.v_po / di.v_contract / di.v_party_change / di.di_capability）
-- 全文件只读业务表，不写任何业务数据。可重复执行。
-- ============================================================

BEGIN;

-- ============================================================
-- DI-01 采购单供应商快照 ≠ 合同乙方（卡面要求第 1 条）
--   真相：biz_contract.party_b_id。三态：
--     ① 合同变更有审批台账、且采购单建单 ≤ 变更时刻 → 合法历史快照（SNAPSHOT/INFO，合同粒度聚合，不告警）
--     ② 合同变更有台账、但采购单建单 > 变更时刻 → 建单时快照没跟随真相（DEFECT/WARN，单据粒度）
--     ③ 无任何变更台账 → 主体被事后改过（DEFECT/ERROR，合同粒度聚合 = P0 疑似篡改）
-- ============================================================
CREATE OR REPLACE VIEW di.c_di01 AS
WITH po AS (
    SELECT o.id AS po_id, o.order_no, o.supplier_id, o.status AS po_status,
           o.contract_id, o.created_at,
           c.contract_no, c.party_b_id,
           (SELECT max(pc.changed_at) FROM di.v_party_change pc WHERE pc.contract_id = c.id) AS changed_at
      FROM di.v_po o
      JOIN di.v_contract c ON c.id = o.contract_id
     WHERE o.deleted_at IS NULL
       AND c.deleted_at IS NULL
       AND o.purchase_type = 'CONTRACT'
       AND o.supplier_id IS DISTINCT FROM c.party_b_id
)
SELECT 'DI-01'::text AS issue_code, 'CONTRACT'::text AS entity_type,
       p.contract_id AS entity_id, p.contract_no::text AS entity_no,
       'party_b_id'::text AS field_name,
       min(p.supplier_id::text) AS snapshot_value, min(p.party_b_id::text) AS truth_value,
       'biz_contract.party_b_id'::text AS truth_source,
       'SNAPSHOT'::text AS drift_kind, 'INFO'::text AS severity,
       count(*)::int AS affected_count
  FROM po p
 WHERE p.changed_at IS NOT NULL AND p.created_at <= p.changed_at
 GROUP BY p.contract_id, p.contract_no, p.party_b_id

UNION ALL
SELECT 'DI-01'::text, 'PURCHASE_ORDER'::text, p.po_id, p.order_no::text, 'supplier_id'::text,
       p.supplier_id::text, p.party_b_id::text, 'biz_contract.party_b_id'::text,
       'DEFECT'::text, 'WARN'::text, 1
  FROM po p
 WHERE p.changed_at IS NOT NULL AND p.created_at > p.changed_at

UNION ALL
SELECT 'DI-01'::text, 'CONTRACT'::text, p.contract_id, p.contract_no::text, 'party_b_id'::text,
       min(p.supplier_id::text), min(p.party_b_id::text), 'biz_contract.party_b_id'::text,
       'DEFECT'::text, 'ERROR'::text, count(*)::int
  FROM po p
 WHERE p.changed_at IS NULL
 GROUP BY p.contract_id, p.contract_no;

COMMENT ON VIEW di.c_di01 IS
  'DI-01 采购单供应商快照 vs 合同乙方：无变更台账 ⇒ P0 疑似篡改；有台账且建单晚于变更 ⇒ P1 快照未跟随；有台账且建单更早 ⇒ INFO 合法历史快照';

-- ============================================================
-- DI-02 采购单引用的合同已软删（卡面要求第 3 条）
--   含义：单据指向的合同被删了。合同删除必须被单据链阻断（应用层 409），
--   一旦出现说明「删除时没看下游」——已审批/已入库的单据失去合同依据（涉资 → P0）。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di02 AS
SELECT 'DI-02'::text AS issue_code, 'PURCHASE_ORDER'::text AS entity_type,
       o.id AS entity_id, o.order_no::text AS entity_no, 'contract_id'::text AS field_name,
       ('DELETED@' || to_char(c.deleted_at, 'YYYY-MM-DD HH24:MI:SS'))::text AS snapshot_value,
       ('CONTRACT# ' || c.contract_no)::text                                AS truth_value,
       'biz_contract.deleted_at'::text AS truth_source,
       'DEFECT'::text AS drift_kind,
       CASE WHEN o.status IN ('APPROVED','PARTIAL_RECEIVED','RECEIVED','CLOSED')
             OR EXISTS (SELECT 1 FROM biz_stock_in s
                         WHERE s.source_order_id = o.id AND s.deleted_at IS NULL AND s.status = 'POSTED')
            THEN 'ERROR' ELSE 'WARN' END AS severity,
       1 AS affected_count
  FROM di.v_po o
  JOIN di.v_contract c ON c.id = o.contract_id
 WHERE o.deleted_at IS NULL
   AND c.deleted_at IS NOT NULL;

COMMENT ON VIEW di.c_di02 IS 'DI-02 采购单指向的合同已软删：在途/已过账 → P0；草稿 → P1';

-- ============================================================
-- DI-03 采购单 contract_id 悬空（指向的行根本不存在）
--   含义：弱引用被打断（迁移漏行/手工改库/跨库拷贝）。追溯链到此断掉 → 一律 P0。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di03 AS
SELECT 'DI-03'::text AS issue_code, 'PURCHASE_ORDER'::text AS entity_type,
       o.id AS entity_id, o.order_no::text AS entity_no, 'contract_id'::text AS field_name,
       o.contract_id::text AS snapshot_value, 'CONTRACT_ROW_MISSING'::text AS truth_value,
       'biz_contract.id'::text AS truth_source,
       'DEFECT'::text AS drift_kind, 'ERROR'::text AS severity, 1 AS affected_count
  FROM di.v_po o
  LEFT JOIN di.v_contract c ON c.id = o.contract_id
 WHERE o.deleted_at IS NULL AND o.contract_id IS NOT NULL AND c.id IS NULL;

-- ============================================================
-- DI-04 purchase_type='CONTRACT' 但 contract_id 为空（卡面要求第 4 条前半）
--   含义：合同采购没有合同 = 这条单据的乙方没有任何真相来源。
--   V0193 的 ck_biz_po_contract_pair 已双向钉死；本条是「约束被绕过/被 drop」时的兜底。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di04 AS
SELECT 'DI-04'::text AS issue_code, 'PURCHASE_ORDER'::text AS entity_type,
       o.id AS entity_id, o.order_no::text AS entity_no, 'contract_id'::text AS field_name,
       'NULL'::text AS snapshot_value, 'CONTRACT_REQUIRED'::text AS truth_value,
       'ck_biz_po_contract_pair'::text AS truth_source,
       'DEFECT'::text AS drift_kind, 'WARN'::text AS severity, 1 AS affected_count
  FROM di.v_po o
 WHERE o.deleted_at IS NULL AND o.purchase_type = 'CONTRACT' AND o.contract_id IS NULL;

-- ============================================================
-- DI-05 purchase_type='SPOT' 但 supplier_id 为空（卡面要求第 4 条后半）
--   含义：零星采购没有供应商 = 应付/凭证无法生成。
--   注意：biz_purchase_order.supplier_id 当前是 NOT NULL，本条在现状恒返回 0 行；
--        它的价值是「约束被放宽后立刻现形」而不是「现在有缺陷」。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di05 AS
SELECT 'DI-05'::text AS issue_code, 'PURCHASE_ORDER'::text AS entity_type,
       o.id AS entity_id, o.order_no::text AS entity_no, 'supplier_id'::text AS field_name,
       'NULL'::text AS snapshot_value, 'SUPPLIER_REQUIRED'::text AS truth_value,
       'biz_purchase_order.supplier_id NOT NULL'::text AS truth_source,
       'DEFECT'::text AS drift_kind, 'WARN'::text AS severity, 1 AS affected_count
  FROM di.v_po o
 WHERE o.deleted_at IS NULL AND o.purchase_type = 'SPOT' AND o.supplier_id IS NULL;

-- ============================================================
-- DI-06 入库单供应商 ≠ 其来源采购单供应商（卡面要求第 2 条）
--   真相：biz_purchase_order.supplier_id（入库单这一层是「冗余拷贝」而非快照 ——
--   它能由上游无歧义重算，漂移不可能是合法业务变更，只能是缺陷）。
--   已过账或已生成应付（钱已动）→ P0。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di06 AS
SELECT 'DI-06'::text AS issue_code, 'STOCK_IN'::text AS entity_type,
       s.id AS entity_id, s.in_no::text AS entity_no, 'supplier_id'::text AS field_name,
       s.supplier_id::text AS snapshot_value, o.supplier_id::text AS truth_value,
       'biz_purchase_order.supplier_id'::text AS truth_source,
       'DEFECT'::text AS drift_kind,
       CASE WHEN s.status = 'POSTED'
             OR EXISTS (SELECT 1 FROM biz_payable p
                         WHERE p.source_type = 'STOCK_IN' AND p.source_id = s.id
                           AND p.deleted_at IS NULL AND p.status <> 'CANCELLED')
            THEN 'ERROR' ELSE 'WARN' END AS severity,
       1 AS affected_count
  FROM biz_stock_in s
  JOIN biz_purchase_order o ON o.id = s.source_order_id
 WHERE s.deleted_at IS NULL AND o.deleted_at IS NULL
   AND s.supplier_id IS DISTINCT FROM o.supplier_id;

COMMENT ON VIEW di.c_di06 IS
  'DI-06 入库单供应商 vs 来源采购单供应商：入库单这层是冗余拷贝（可无歧义重算），任何偏移都是 DEFECT；已过账/已生成应付 ⇒ P0';

-- ============================================================
-- DI-07 入库单的来源采购单已软删
--   含义：采购单被删但货已收（或未收）—— 收货失去采购依据，库存流水与单据链不一致。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di07 AS
SELECT 'DI-07'::text AS issue_code, 'STOCK_IN'::text AS entity_type,
       s.id AS entity_id, s.in_no::text AS entity_no, 'source_order_id'::text AS field_name,
       ('PO_DELETED@' || to_char(o.deleted_at, 'YYYY-MM-DD'))::text AS snapshot_value,
       ('PO# ' || o.order_no)::text AS truth_value,
       'biz_purchase_order.deleted_at'::text AS truth_source,
       'DEFECT'::text AS drift_kind,
       CASE WHEN s.status = 'POSTED' THEN 'ERROR' ELSE 'WARN' END AS severity,
       1 AS affected_count
  FROM biz_stock_in s
  JOIN biz_purchase_order o ON o.id = s.source_order_id
 WHERE s.deleted_at IS NULL AND o.deleted_at IS NOT NULL;

-- ============================================================
-- DI-08 入库单来源采购单悬空（source_order_id 指向不存在的行）
-- ============================================================
CREATE OR REPLACE VIEW di.c_di08 AS
SELECT 'DI-08'::text AS issue_code, 'STOCK_IN'::text AS entity_type,
       s.id AS entity_id, s.in_no::text AS entity_no, 'source_order_id'::text AS field_name,
       s.source_order_id::text AS snapshot_value, 'PO_ROW_MISSING'::text AS truth_value,
       'biz_purchase_order.id'::text AS truth_source,
       'DEFECT'::text AS drift_kind, 'ERROR'::text AS severity, 1 AS affected_count
  FROM biz_stock_in s
  LEFT JOIN biz_purchase_order o ON o.id = s.source_order_id
 WHERE s.deleted_at IS NULL AND s.source_order_id IS NOT NULL AND o.id IS NULL;

-- ============================================================
-- DI-09 采购入库（in_type='PURCHASE'）但供应商为空
--   含义：应付与凭证无法生成。ck_biz_si_supplier 已拦，本条为约束缺失时的兜底（现状恒 0 行）。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di09 AS
SELECT 'DI-09'::text AS issue_code, 'STOCK_IN'::text AS entity_type,
       s.id AS entity_id, s.in_no::text AS entity_no, 'supplier_id'::text AS field_name,
       'NULL'::text AS snapshot_value, 'SUPPLIER_REQUIRED'::text AS truth_value,
       'ck_biz_si_supplier'::text AS truth_source,
       'DEFECT'::text AS drift_kind, 'WARN'::text AS severity, 1 AS affected_count
  FROM biz_stock_in s
 WHERE s.deleted_at IS NULL AND s.in_type = 'PURCHASE' AND s.supplier_id IS NULL;

-- ============================================================
-- DI-10 应付单供应商 ≠ 来源入库单供应商（资金链）
--   含义：钱付给了和实物入库不同的人 —— 这是「涉资」的典型形态，一律 P0。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di10 AS
SELECT 'DI-10'::text AS issue_code, 'PAYABLE'::text AS entity_type,
       p.id AS entity_id, p.payable_no::text AS entity_no, 'supplier_id'::text AS field_name,
       p.supplier_id::text AS snapshot_value, s.supplier_id::text AS truth_value,
       'biz_stock_in.supplier_id'::text AS truth_source,
       'DEFECT'::text AS drift_kind,
       CASE WHEN p.status = 'CANCELLED' THEN 'WARN' ELSE 'ERROR' END AS severity,
       1 AS affected_count
  FROM biz_payable p
  JOIN biz_stock_in s ON s.id = p.source_id
 WHERE p.deleted_at IS NULL AND s.deleted_at IS NULL
   AND p.source_type = 'STOCK_IN'
   AND p.supplier_id IS DISTINCT FROM s.supplier_id;

-- ============================================================
-- DI-11 应付单来源入库单悬空 / 已软删（资金链断链）
--   扩展项（超出卡面最小集）：与 DI-08 同理，应付必须能追回入库单，否则无法对账。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di11 AS
SELECT 'DI-11'::text AS issue_code, 'PAYABLE'::text AS entity_type,
       p.id AS entity_id, p.payable_no::text AS entity_no, 'source_id'::text AS field_name,
       p.source_id::text AS snapshot_value,
       CASE WHEN s.id IS NULL THEN 'STOCK_IN_ROW_MISSING' ELSE 'STOCK_IN_DELETED' END::text AS truth_value,
       'biz_stock_in.id'::text AS truth_source,
       'DEFECT'::text AS drift_kind, 'ERROR'::text AS severity, 1 AS affected_count
  FROM biz_payable p
  LEFT JOIN biz_stock_in s ON s.id = p.source_id
 WHERE p.deleted_at IS NULL AND p.source_type = 'STOCK_IN'
   AND (s.id IS NULL OR s.deleted_at IS NOT NULL);

-- ============================================================
-- DI-12 采购单供应商主档悬空 / 已软删
--   含义：供应商被删了但单据还在（主档软删不该影响历史单据的追溯能力）。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di12 AS
SELECT 'DI-12'::text AS issue_code, 'PURCHASE_ORDER'::text AS entity_type,
       o.id AS entity_id, o.order_no::text AS entity_no, 'supplier_id'::text AS field_name,
       o.supplier_id::text AS snapshot_value,
       CASE WHEN sp.id IS NULL THEN 'SUPPLIER_ROW_MISSING' ELSE 'SUPPLIER_DELETED' END::text AS truth_value,
       'biz_supplier.deleted_at'::text AS truth_source,
       'DEFECT'::text AS drift_kind,
       CASE WHEN sp.id IS NULL THEN 'ERROR' ELSE 'WARN' END AS severity,
       1 AS affected_count
  FROM di.v_po o
  LEFT JOIN biz_supplier sp ON sp.id = o.supplier_id
 WHERE o.deleted_at IS NULL AND (sp.id IS NULL OR sp.deleted_at IS NOT NULL);

-- ============================================================
-- DI-13 同一合同下出现多个不同的供应商快照
--   含义：同一份合同的乙方只有一个真相，若历史单据快照互不相同，
--        至多一张是正确的 —— 剩下的是建单时取错/改错（不是合同变更能解释的）。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di13 AS
SELECT 'DI-13'::text AS issue_code, 'CONTRACT'::text AS entity_type,
       o.contract_id AS entity_id, c.contract_no::text AS entity_no, 'supplier_id'::text AS field_name,
       string_agg(DISTINCT o.supplier_id::text, ',') AS snapshot_value,
       c.party_b_id::text AS truth_value,
       'biz_contract.party_b_id'::text AS truth_source,
       'DEFECT'::text AS drift_kind, 'WARN'::text AS severity,
       count(DISTINCT o.supplier_id)::int AS affected_count
  FROM di.v_po o
  JOIN di.v_contract c ON c.id = o.contract_id
 WHERE o.deleted_at IS NULL AND c.deleted_at IS NULL AND o.purchase_type = 'CONTRACT'
 GROUP BY o.contract_id, c.contract_no, c.party_b_id
HAVING count(DISTINCT o.supplier_id) > 1;

-- ============================================================
-- DI-14 结构护栏自检（巡检自己的可信度）
--   含义：V0193 的 CHECK 约束被人 drop 掉，则 DI-04/DI-05 的缺陷会先落库再被巡检发现，
--        且「约束兜底」这一层防护消失。护栏缺失本身就该是 P0。
--   现状阶段（contract_checks_ready=false）只要求 ck_biz_po_tax_sum。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di14 AS
WITH expect AS (
    SELECT 'CONSTRAINT'::text AS kind, 'ck_biz_po_tax_sum'::text AS obj,
           'public.biz_purchase_order'::text AS rel
    UNION ALL
    SELECT 'CONSTRAINT', 'ck_biz_po_purchase_type', 'public.biz_purchase_order'
     WHERE (SELECT contract_checks_ready FROM di.di_capability)
    UNION ALL
    SELECT 'CONSTRAINT', 'ck_biz_po_contract_pair', 'public.biz_purchase_order'
     WHERE (SELECT contract_checks_ready FROM di.di_capability)
    UNION ALL
    SELECT 'CONSTRAINT', 'ck_biz_si_supplier', 'public.biz_stock_in'
    UNION ALL
    -- 巡检自身的去重护栏：唯一索引必须存在 **且仍是部分索引**（丢了 WHERE resolved_at IS NULL
    -- 会导致「同一缺陷解决后再发生」永远写不进来，等于漏报）
    SELECT 'PARTIAL_INDEX', 'uk_biz_drift_open', 'public.biz_snapshot_drift'
     WHERE to_regclass('public.biz_snapshot_drift') IS NOT NULL
),
missing AS (
    SELECT e.kind, e.obj
      FROM expect e
     WHERE (e.kind = 'CONSTRAINT'
            AND NOT EXISTS (SELECT 1 FROM pg_constraint c
                             WHERE c.conname = e.obj AND c.conrelid = e.rel::regclass))
        OR (e.kind = 'PARTIAL_INDEX'
            AND NOT EXISTS (SELECT 1 FROM pg_indexes i
                             WHERE i.indexname = e.obj
                               AND i.schemaname = split_part(e.rel, '.', 1)
                               AND i.tablename  = split_part(e.rel, '.', 2)
                               AND i.indexdef LIKE '%resolved_at IS NULL%'))
)
SELECT 'DI-14'::text AS issue_code, 'SYSTEM'::text AS entity_type,
       '00000000-0000-0000-0000-000000000000'::uuid AS entity_id,
       'schema_guard'::text AS entity_no, 'constraint'::text AS field_name,
       ('MISSING:' || string_agg(m.kind || ':' || m.obj, ',' ORDER BY m.obj))::text AS snapshot_value,
       'ALL_PRESENT'::text AS truth_value,
       'pg_constraint / pg_indexes'::text AS truth_source,
       'DEFECT'::text AS drift_kind, 'ERROR'::text AS severity, 1 AS affected_count
  FROM missing m
 GROUP BY 1;

COMMIT;
