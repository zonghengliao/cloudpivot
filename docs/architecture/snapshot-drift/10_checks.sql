-- ============================================================
-- 10_checks.sql —— 巡检项定义（DI-01…DI-22，其中 DI-14 是护栏自检，
--                    DI-17…DI-22 是多态引用巡检（类型列 + id 列，见 ARCH-3 DC-08））
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

-- ============================================================
-- DI-15 合同已终止/已完成，但仍有在途采购单引用（P0/P1）
--   含义：合同 TERMINATED/COMPLETED 之后，其名下还有未走完的采购单 ——
--   这些单据的乙方真相来自一份「已经不被允许继续履约」的合同。
--   跨卡对应：通用勾稽规则卡的 I-P5（本卡把它落成可执行 SQL）。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di15 AS
SELECT 'DI-15'::text AS issue_code, 'PURCHASE_ORDER'::text AS entity_type,
       o.id AS entity_id, o.order_no::text AS entity_no, 'contract_id'::text AS field_name,
       ('CONTRACT_STATUS=' || c.status || ' / PO_STATUS=' || o.status)::text AS snapshot_value,
       'CONTRACT_LIVE'::text AS truth_value,
       'biz_contract.status'::text AS truth_source,
       'DEFECT'::text AS drift_kind,
       CASE WHEN o.status IN ('APPROVED','PARTIAL_RECEIVED','RECEIVED') THEN 'ERROR' ELSE 'WARN' END AS severity,
       1 AS affected_count
  FROM di.v_po o
  JOIN di.v_contract c ON c.id = o.contract_id
 WHERE o.deleted_at IS NULL AND c.deleted_at IS NULL
   AND o.purchase_type = 'CONTRACT'
   AND o.status IN ('DRAFT','SUBMITTED','APPROVED','PARTIAL_RECEIVED','RECEIVED')
   AND c.status IN ('TERMINATED','COMPLETED');

-- ============================================================
-- DI-16 已过账收货单的来源采购单已不可收（ARCH-3 DC-03 / R-P0-3）
--   含义：收货单已过账（库存三本账 + 应付 + 进项凭证都已生成），但它引用的采购单
--   已经不处于「可收」状态（`SUBMITTED` / `APPROVED` / `PARTIAL_RECEIVED` / `RECEIVED`）——
--   典型成因是「采购单在建单与过账之间被取消/关闭」，正是 DC-03 那个断点留下的存量。
--   分级：已过账 ⇒ 库存与应付没有上游依据 ⇒ DEFECT / ERROR（P0）；
--        来源单被软删或悬空同属 DEFECT / ERROR（与 DI-07 / DI-08 同口径，只是限定了已过账）。
--   注：`RECEIVED` 也算可收（收满后的正常终态；作废回退也正是从它出发）。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di16 AS
SELECT 'DI-16'::text AS issue_code,
       'STOCK_IN'::text AS entity_type,
       s.id AS entity_id,
       s.in_no::text AS entity_no,
       'source_order_id'::text AS field_name,
       ((coalesce(p.status, '(悬空)')) ||
        CASE WHEN p.id IS NOT NULL AND p.deleted_at IS NOT NULL THEN '(已软删)' ELSE '' END)::text AS snapshot_value,
       'SUBMITTED|APPROVED|PARTIAL_RECEIVED|RECEIVED'::text AS truth_value,
       'biz_purchase_order.status'::text AS truth_source,
       'DEFECT'::text AS drift_kind,
       'ERROR'::text AS severity,
       1 AS affected_count
  FROM biz_stock_in s
  LEFT JOIN biz_purchase_order p ON p.id = s.source_order_id
 WHERE s.deleted_at IS NULL
   AND s.status = 'POSTED'
   AND s.source_order_id IS NOT NULL
   AND (p.id IS NULL
        OR p.deleted_at IS NOT NULL
        OR p.status NOT IN ('SUBMITTED','APPROVED','PARTIAL_RECEIVED','RECEIVED'));

COMMENT ON VIEW di.c_di16 IS
  'DI-16 已过账收货单的来源采购单已不可收（含软删/悬空）：DEFECT/ERROR —— 库存、应付、进项凭证没有上游依据（ARCH-3 DC-03）';

-- ============================================================
-- DI-17…DI-22 多态引用巡检（ARCH-3 §5 DC-08 / 卡 P2-6）
--
-- 「多态引用」= 一个**类型列** + 一个 **id 列**，id 指向哪张表由类型列决定
-- （如 `biz_voucher.source_type` + `source_id`）。这类引用**无法用外键表达**
-- —— 一个列天生对应 N 张表，PG 的 FK 只能指向一张。它只能靠代码自律，
-- 所以单据被软删 / 被清场之后，引用它的流水、凭证、日志仍然挂着，**没人发现**。
--
-- 本组巡检的判据只有一条：
--   类型列 → 登记表 di.v_poly_ref_map 解析出目标表 → 按 id LEFT JOIN 目标表
--   → 目标行不存在（ROW_MISSING）或已软删（SOFT_DELETED）⇒ 悬空。
-- 派生两种「非悬空但同样断链」的形态（同样只报不修）：
--   MISSING_ID  —— 类型说有来源，但 id 是空的：回跳不了，等于没来源。
--   NO_TARGET   —— 类型对应的是一类「本来就没有实体表」的动作（手工调整 / 费用审批 /
--                  手工凭证…），却硬填了一个 id：审计上无法解释它指向谁。
--
-- 分级口径（账务 vs 日志的区别，别一刀切）：
--   * 凭证 / 库存流水 = **审计证据**：必须能回跳到来源单据。链断了 ⇒ 报表按来源
--     汇总出现「幽灵行」、审计单击跳 404、账实不符时查不到谁造的 ⇒ BLOCKER。
--   * 流程实例 / 序列号 = **协作与追溯**：审批挂在已消失的单据上、序列号来源没记，
--     账本身不错，但审批卡死、追责断线 ⇒ WARN（历史归档实例是正常现象，故不 P0）。
--   * 状态日志 = **只增不改的历史**：实体被删是正常生命周期的一部分，
--     要求日志跟着删才是错的（那会毁掉「谁在什么时候改了什么」）⇒ INFO，只记录不告警。
--
-- 依赖：V0119（biz_status_log）/ V0122（flow_instance）/ V0148-49（payable、voucher）
--       / V0157（inventory_txn）/ V0203（stock_out）/ V0211（stock_take）
--       ⇒ 迁移 < V0211 的库装本组视图会**直接报错**（不静默跳过，见设计文档 §3.5）。
-- ============================================================

-- ------------------------------------------------------------
-- 多态引用登记表：类型 → 目标表（机器可读的「盘点清单」）
--   `target_table IS NULL` = 这类引用**本来就没有实体表**（虚拟对象）。
--   DI-22 拿数据里的 distinct 类型与它对账：数据里出现登记表没有的类型 ⇒
--   这个类型的悬空**根本不会被查**（静默漏报）—— 这是本组巡检最容易坏的地方。
-- ------------------------------------------------------------
CREATE OR REPLACE VIEW di.v_poly_ref_map AS
SELECT ref_domain, ref_type, target_table, soft_delete_col, note
  FROM (VALUES
    -- ① 凭证来源：biz_voucher.source_type + source_id（写入点见 voucher.rs::post_voucher）
    ('VOUCHER_SOURCE', 'STOCK_IN',      'biz_stock_in',         'deleted_at', '收货过账生成，source_id = 收货单 id'),
    ('VOUCHER_SOURCE', 'STOCK_OUT',     'biz_stock_out',        'deleted_at', '出库过账生成，source_id = 出库单 id'),
    ('VOUCHER_SOURCE', 'PAYMENT',       'biz_payment',          'deleted_at', '付款登记生成，source_id = 付款单 id'),
    ('VOUCHER_SOURCE', 'STOCK_TAKE',    'biz_stock_take',       'deleted_at', '盘点过账生成，source_id = 盘点单 id'),
    ('VOUCHER_SOURCE', 'SERIAL_BATCH',  'biz_material_serial',  'deleted_at', '序列号批量生成，source_id = 批次首个序列号 id'),
    ('VOUCHER_SOURCE', 'SERIAL_OUT',    'biz_material_serial',  'deleted_at', '序列号出库，source_id = 序列号 id'),
    ('VOUCHER_SOURCE', 'SERIAL_SCRAP',  'biz_material_serial',  'deleted_at', '序列号报废，source_id = 序列号 id'),
    ('VOUCHER_SOURCE', 'OPENING',       NULL,                   NULL,         '期初结转：无 id，期间写在 source_no'),
    ('VOUCHER_SOURCE', 'YEAR_END',      NULL,                   NULL,         '年度损益结转：无 id，年度写在 source_no'),
    ('VOUCHER_SOURCE', 'MANUAL',        NULL,                   NULL,         '手工凭证：本来就没有来源单据'),
    -- ② 库存流水来源单据：biz_inventory_txn.doc_type + doc_id
    ('TXN_DOC', 'STOCK_IN',        'biz_stock_in',        'deleted_at', '收货过账的流水'),
    ('TXN_DOC', 'STOCK_OUT',       'biz_stock_out',       'deleted_at', '出库过账的流水'),
    ('TXN_DOC', 'STOCK_TRANSFER',  'biz_stock_transfer',  'deleted_at', '调拨发出的流水'),
    ('TXN_DOC', 'SERIAL_BATCH',    'biz_material_serial', 'deleted_at', '序列号批量生成：doc_id = 批次首个序列号 id'),
    ('TXN_DOC', 'MATERIAL_SERIAL', 'biz_material_serial', 'deleted_at', '序列号出库/报废：doc_id = 序列号 id'),
    ('TXN_DOC', 'PURCHASE_ORDER',  'biz_purchase_order',  'deleted_at', '遗留值（TxnDocType 已无此值，存量直接挂采购单）'),
    ('TXN_DOC', 'OPENING',         NULL,                  NULL,         '期初导入：没有单据行'),
    ('TXN_DOC', 'ADJUST',          NULL,                  NULL,         '库存手工调整（DATAFIX）：没有单据行'),
    -- ③ 流程实例绑定的业务单据：biz_flow_instance.biz_type + biz_id
    ('FLOW_BIZ', 'PO_APPROVE',       'biz_purchase_order', 'deleted_at', '采购单审批'),
    ('FLOW_BIZ', 'STOCK_IN_POST',    'biz_stock_in',       'deleted_at', '收货过账审批'),
    ('FLOW_BIZ', 'STOCK_IN_CANCEL',  'biz_stock_in',       'deleted_at', '收货作废审批'),
    ('FLOW_BIZ', 'STOCK_TAKE_POST',  'biz_stock_take',     'deleted_at', '盘点过账审批'),
    ('FLOW_BIZ', 'STOCK_TRANSFER',   'biz_stock_transfer', 'deleted_at', '调拨审批'),
    ('FLOW_BIZ', 'PAYABLE_PAY',      'biz_payable',        'deleted_at', '应付付款审批'),
    ('FLOW_BIZ', 'PAYABLE_CANCEL',   'biz_payable',        'deleted_at', '应付作废审批'),
    ('FLOW_BIZ', 'VOUCHER_REVERSE',  'biz_voucher',        'deleted_at', '凭证红冲审批'),
    ('FLOW_BIZ', 'CONTRACT_APPROVE', 'biz_contract',       'deleted_at', '合同审批'),
    ('FLOW_BIZ', 'FLOW_EDGE_CFG',    'biz_flow_edge',      NULL,         '状态机边配置变更审批（配置行无软删）'),
    ('FLOW_BIZ', 'TASK_ACCEPT',      'biz_task',           'deleted_at', '任务验收：biz_id = 任务 id（assignee.rs 解析 TASK_OWNER 用的就是它）'),
    ('FLOW_BIZ', 'EXPENSE',          NULL,                 NULL,         '费用审批：不对应 ERP 实体表'),
    ('FLOW_BIZ', 'OTHER',            NULL,                 NULL,         '客户端自建流程兜底类型：设计上不参与自检'),
    -- ④ 状态日志实体：biz_status_log.entity_type + entity_id（取值域 = DocKind）
    ('STATUS_ENTITY', 'PURCHASE_ORDER',    'biz_purchase_order',    'deleted_at', '采购单'),
    ('STATUS_ENTITY', 'STOCK_IN',          'biz_stock_in',          'deleted_at', '收货单'),
    ('STATUS_ENTITY', 'STOCK_OUT',         'biz_stock_out',         'deleted_at', '出库单'),
    ('STATUS_ENTITY', 'STOCK_TRANSFER',    'biz_stock_transfer',    'deleted_at', '调拨单'),
    ('STATUS_ENTITY', 'STOCK_TAKE',        'biz_stock_take',        'deleted_at', '盘点单'),
    ('STATUS_ENTITY', 'PAYABLE',           'biz_payable',           'deleted_at', '应付单'),
    ('STATUS_ENTITY', 'PAYMENT',           'biz_payment',           'deleted_at', '付款单'),
    ('STATUS_ENTITY', 'VOUCHER',           'biz_voucher',           'deleted_at', '凭证'),
    ('STATUS_ENTITY', 'CONTRACT',          'biz_contract',          'deleted_at', '合同'),
    ('STATUS_ENTITY', 'INVENTORY',         'biz_inventory',         NULL,         '库存（低库存提醒对象，无软删）'),
    ('STATUS_ENTITY', 'ACCOUNTING_PERIOD', 'biz_accounting_period', NULL,         '会计期间（无软删）'),
    ('STATUS_ENTITY', 'FLOW_EDGE_CFG',     'biz_flow_edge',         NULL,         '状态机边配置（配置行无软删）'),
    ('STATUS_ENTITY', 'FLOW_LINKAGE_CFG',  'biz_flow_linkage_rule', 'deleted_at', '联动规则配置'),
    ('STATUS_ENTITY', 'ACCOUNTING_YEAR',   NULL,                    NULL,         '年结：无实体表，entity_id 存结转凭证 id'),
    -- ⑤ 序列号来源：biz_material_serial.source_type + source_id
    ('SERIAL_SOURCE', 'PURCHASE', NULL, NULL, '**目标表未定义**：source_id 写入点恒为 None（serial_handler 4 处）'),
    ('SERIAL_SOURCE', 'TRANSFER', NULL, NULL, '**目标表未定义**：同上'),
    ('SERIAL_SOURCE', 'PRODUCE',  NULL, NULL, '**目标表未定义**：同上'),
    ('SERIAL_SOURCE', 'RETURN',   NULL, NULL, '**目标表未定义**：同上')
  ) AS t(ref_domain, ref_type, target_table, soft_delete_col, note);

-- ------------------------------------------------------------
-- 悬空引用明细（本组 5 条巡检的公共取数层）
--   ref_shape: DANGLING（指向的行不存在/已软删）| MISSING_ID（有类型没 id）| NO_TARGET（无实体表却填了 id）
--   一张视图里逐（域, 类型）LEFT JOIN 目标表 —— 多态引用没法参数化，只能显式展开；
--   目标表名一律取自 di.v_poly_ref_map（JOIN 上去），不在分支里再写一遍。
-- ------------------------------------------------------------
CREATE OR REPLACE VIEW di.v_poly_ref_dangling AS
-- ========== ① 凭证来源 ==========
SELECT m.ref_domain, 'biz_voucher'::text AS src_table, v.id AS src_id, v.voucher_no::text AS src_no,
       'source_id'::text AS ref_field, m.ref_type, v.source_id AS ref_id,
       m.target_table, 'DANGLING'::text AS ref_shape,
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END::text AS miss_kind,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text AS truth_source
  FROM biz_voucher v
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'VOUCHER_SOURCE' AND m.ref_type = v.source_type
  LEFT JOIN biz_stock_in t ON t.id = v.source_id
 WHERE v.deleted_at IS NULL AND v.source_id IS NOT NULL AND v.source_type = 'STOCK_IN'
   AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_voucher', v.id, v.voucher_no::text, 'source_id', m.ref_type, v.source_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_voucher v
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'VOUCHER_SOURCE' AND m.ref_type = v.source_type
  LEFT JOIN biz_stock_out t ON t.id = v.source_id
 WHERE v.deleted_at IS NULL AND v.source_id IS NOT NULL AND v.source_type = 'STOCK_OUT'
   AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_voucher', v.id, v.voucher_no::text, 'source_id', m.ref_type, v.source_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_voucher v
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'VOUCHER_SOURCE' AND m.ref_type = v.source_type
  LEFT JOIN biz_payment t ON t.id = v.source_id
 WHERE v.deleted_at IS NULL AND v.source_id IS NOT NULL AND v.source_type = 'PAYMENT'
   AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_voucher', v.id, v.voucher_no::text, 'source_id', m.ref_type, v.source_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_voucher v
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'VOUCHER_SOURCE' AND m.ref_type = v.source_type
  LEFT JOIN biz_stock_take t ON t.id = v.source_id
 WHERE v.deleted_at IS NULL AND v.source_id IS NOT NULL AND v.source_type = 'STOCK_TAKE'
   AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_voucher', v.id, v.voucher_no::text, 'source_id', m.ref_type, v.source_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_voucher v
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'VOUCHER_SOURCE' AND m.ref_type = v.source_type
  LEFT JOIN biz_material_serial t ON t.id = v.source_id
 WHERE v.deleted_at IS NULL AND v.source_id IS NOT NULL
   AND v.source_type IN ('SERIAL_BATCH', 'SERIAL_OUT', 'SERIAL_SCRAP')
   AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
-- 凭证「有类型没 id」：回跳不了来源单
UNION ALL
SELECT m.ref_domain, 'biz_voucher', v.id, v.voucher_no::text, 'source_id', m.ref_type, NULL,
       m.target_table, 'MISSING_ID', 'SOURCE_ID_REQUIRED',
       (m.target_table || '.id')::text
  FROM biz_voucher v
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'VOUCHER_SOURCE' AND m.ref_type = v.source_type
 WHERE v.deleted_at IS NULL AND v.source_id IS NULL AND m.target_table IS NOT NULL
-- ========== ② 库存流水来源单据 ==========
UNION ALL
SELECT m.ref_domain, 'biz_inventory_txn', t0.id, t0.doc_no::text, 'doc_id', m.ref_type, t0.doc_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_inventory_txn t0
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'TXN_DOC' AND m.ref_type = t0.doc_type
  LEFT JOIN biz_stock_in t ON t.id = t0.doc_id
 WHERE t0.doc_type = 'STOCK_IN' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_inventory_txn', t0.id, t0.doc_no::text, 'doc_id', m.ref_type, t0.doc_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_inventory_txn t0
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'TXN_DOC' AND m.ref_type = t0.doc_type
  LEFT JOIN biz_stock_out t ON t.id = t0.doc_id
 WHERE t0.doc_type = 'STOCK_OUT' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_inventory_txn', t0.id, t0.doc_no::text, 'doc_id', m.ref_type, t0.doc_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_inventory_txn t0
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'TXN_DOC' AND m.ref_type = t0.doc_type
  LEFT JOIN biz_stock_transfer t ON t.id = t0.doc_id
 WHERE t0.doc_type = 'STOCK_TRANSFER' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_inventory_txn', t0.id, t0.doc_no::text, 'doc_id', m.ref_type, t0.doc_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_inventory_txn t0
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'TXN_DOC' AND m.ref_type = t0.doc_type
  LEFT JOIN biz_material_serial t ON t.id = t0.doc_id
 WHERE t0.doc_type IN ('SERIAL_BATCH', 'MATERIAL_SERIAL')
   AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_inventory_txn', t0.id, t0.doc_no::text, 'doc_id', m.ref_type, t0.doc_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_inventory_txn t0
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'TXN_DOC' AND m.ref_type = t0.doc_type
  LEFT JOIN biz_purchase_order t ON t.id = t0.doc_id
 WHERE t0.doc_type = 'PURCHASE_ORDER' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
-- 流水「类型本来没有实体表」却填了 doc_id（手工调整 / 期初导入）
UNION ALL
SELECT m.ref_domain, 'biz_inventory_txn', t0.id, t0.doc_no::text, 'doc_id', m.ref_type, t0.doc_id,
       m.target_table, 'NO_TARGET', 'NO_TARGET_TABLE', 'README:doc_type=' || t0.doc_type
  FROM biz_inventory_txn t0
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'TXN_DOC' AND m.ref_type = t0.doc_type
 WHERE m.target_table IS NULL AND t0.doc_id IS NOT NULL
-- ========== ③ 流程实例绑定的业务单据 ==========
UNION ALL
SELECT m.ref_domain, 'biz_flow_instance', i.id, i.instance_no::text, 'biz_id', m.ref_type, i.biz_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_flow_instance i
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'FLOW_BIZ' AND m.ref_type = i.biz_type
  LEFT JOIN biz_purchase_order t ON t.id = i.biz_id
 WHERE i.deleted_at IS NULL AND i.biz_id IS NOT NULL
   AND i.biz_type IN ('PO_APPROVE') AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_flow_instance', i.id, i.instance_no::text, 'biz_id', m.ref_type, i.biz_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_flow_instance i
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'FLOW_BIZ' AND m.ref_type = i.biz_type
  LEFT JOIN biz_stock_in t ON t.id = i.biz_id
 WHERE i.deleted_at IS NULL AND i.biz_id IS NOT NULL
   AND i.biz_type IN ('STOCK_IN_POST', 'STOCK_IN_CANCEL') AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_flow_instance', i.id, i.instance_no::text, 'biz_id', m.ref_type, i.biz_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_flow_instance i
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'FLOW_BIZ' AND m.ref_type = i.biz_type
  LEFT JOIN biz_stock_take t ON t.id = i.biz_id
 WHERE i.deleted_at IS NULL AND i.biz_id IS NOT NULL
   AND i.biz_type = 'STOCK_TAKE_POST' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_flow_instance', i.id, i.instance_no::text, 'biz_id', m.ref_type, i.biz_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_flow_instance i
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'FLOW_BIZ' AND m.ref_type = i.biz_type
  LEFT JOIN biz_stock_transfer t ON t.id = i.biz_id
 WHERE i.deleted_at IS NULL AND i.biz_id IS NOT NULL
   AND i.biz_type = 'STOCK_TRANSFER' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_flow_instance', i.id, i.instance_no::text, 'biz_id', m.ref_type, i.biz_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_flow_instance i
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'FLOW_BIZ' AND m.ref_type = i.biz_type
  LEFT JOIN biz_payable t ON t.id = i.biz_id
 WHERE i.deleted_at IS NULL AND i.biz_id IS NOT NULL
   AND i.biz_type IN ('PAYABLE_PAY', 'PAYABLE_CANCEL') AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_flow_instance', i.id, i.instance_no::text, 'biz_id', m.ref_type, i.biz_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_flow_instance i
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'FLOW_BIZ' AND m.ref_type = i.biz_type
  LEFT JOIN biz_voucher t ON t.id = i.biz_id
 WHERE i.deleted_at IS NULL AND i.biz_id IS NOT NULL
   AND i.biz_type = 'VOUCHER_REVERSE' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_flow_instance', i.id, i.instance_no::text, 'biz_id', m.ref_type, i.biz_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_flow_instance i
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'FLOW_BIZ' AND m.ref_type = i.biz_type
  LEFT JOIN biz_contract t ON t.id = i.biz_id
 WHERE i.deleted_at IS NULL AND i.biz_id IS NOT NULL
   AND i.biz_type = 'CONTRACT_APPROVE' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_flow_instance', i.id, i.instance_no::text, 'biz_id', m.ref_type, i.biz_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_flow_instance i
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'FLOW_BIZ' AND m.ref_type = i.biz_type
  LEFT JOIN biz_flow_edge t ON t.id = i.biz_id
 WHERE i.deleted_at IS NULL AND i.biz_id IS NOT NULL
   AND i.biz_type = 'FLOW_EDGE_CFG' AND t.id IS NULL
UNION ALL
SELECT m.ref_domain, 'biz_flow_instance', i.id, i.instance_no::text, 'biz_id', m.ref_type, i.biz_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_flow_instance i
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'FLOW_BIZ' AND m.ref_type = i.biz_type
  LEFT JOIN biz_task t ON t.id = i.biz_id
 WHERE i.deleted_at IS NULL AND i.biz_id IS NOT NULL
   AND i.biz_type = 'TASK_ACCEPT' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
-- 流程实例「有类型没 biz_id」
UNION ALL
SELECT m.ref_domain, 'biz_flow_instance', i.id, i.instance_no::text, 'biz_id', m.ref_type, NULL,
       m.target_table, 'MISSING_ID', 'BIZ_ID_REQUIRED', (m.target_table || '.id')::text
  FROM biz_flow_instance i
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'FLOW_BIZ' AND m.ref_type = i.biz_type
 WHERE i.deleted_at IS NULL AND i.biz_id IS NULL AND m.target_table IS NOT NULL
-- ========== ④ 状态日志实体 ==========
UNION ALL
SELECT m.ref_domain, 'biz_status_log', l.id, l.entity_no::text, 'entity_id', m.ref_type, l.entity_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_status_log l
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'STATUS_ENTITY' AND m.ref_type = l.entity_type
  LEFT JOIN biz_purchase_order t ON t.id = l.entity_id
 WHERE l.entity_type = 'PURCHASE_ORDER' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_status_log', l.id, l.entity_no::text, 'entity_id', m.ref_type, l.entity_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_status_log l
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'STATUS_ENTITY' AND m.ref_type = l.entity_type
  LEFT JOIN biz_stock_in t ON t.id = l.entity_id
 WHERE l.entity_type = 'STOCK_IN' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_status_log', l.id, l.entity_no::text, 'entity_id', m.ref_type, l.entity_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_status_log l
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'STATUS_ENTITY' AND m.ref_type = l.entity_type
  LEFT JOIN biz_stock_out t ON t.id = l.entity_id
 WHERE l.entity_type = 'STOCK_OUT' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_status_log', l.id, l.entity_no::text, 'entity_id', m.ref_type, l.entity_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_status_log l
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'STATUS_ENTITY' AND m.ref_type = l.entity_type
  LEFT JOIN biz_stock_transfer t ON t.id = l.entity_id
 WHERE l.entity_type = 'STOCK_TRANSFER' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_status_log', l.id, l.entity_no::text, 'entity_id', m.ref_type, l.entity_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_status_log l
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'STATUS_ENTITY' AND m.ref_type = l.entity_type
  LEFT JOIN biz_stock_take t ON t.id = l.entity_id
 WHERE l.entity_type = 'STOCK_TAKE' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_status_log', l.id, l.entity_no::text, 'entity_id', m.ref_type, l.entity_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_status_log l
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'STATUS_ENTITY' AND m.ref_type = l.entity_type
  LEFT JOIN biz_payable t ON t.id = l.entity_id
 WHERE l.entity_type = 'PAYABLE' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_status_log', l.id, l.entity_no::text, 'entity_id', m.ref_type, l.entity_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_status_log l
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'STATUS_ENTITY' AND m.ref_type = l.entity_type
  LEFT JOIN biz_payment t ON t.id = l.entity_id
 WHERE l.entity_type = 'PAYMENT' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_status_log', l.id, l.entity_no::text, 'entity_id', m.ref_type, l.entity_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_status_log l
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'STATUS_ENTITY' AND m.ref_type = l.entity_type
  LEFT JOIN biz_voucher t ON t.id = l.entity_id
 WHERE l.entity_type = 'VOUCHER' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_status_log', l.id, l.entity_no::text, 'entity_id', m.ref_type, l.entity_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_status_log l
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'STATUS_ENTITY' AND m.ref_type = l.entity_type
  LEFT JOIN biz_contract t ON t.id = l.entity_id
 WHERE l.entity_type = 'CONTRACT' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_status_log', l.id, l.entity_no::text, 'entity_id', m.ref_type, l.entity_id,
       m.target_table, 'DANGLING', 'ROW_MISSING', (m.target_table || '.id')::text
  FROM biz_status_log l
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'STATUS_ENTITY' AND m.ref_type = l.entity_type
  LEFT JOIN biz_inventory t ON t.id = l.entity_id
 WHERE l.entity_type = 'INVENTORY' AND t.id IS NULL
UNION ALL
SELECT m.ref_domain, 'biz_status_log', l.id, l.entity_no::text, 'entity_id', m.ref_type, l.entity_id,
       m.target_table, 'DANGLING', 'ROW_MISSING', (m.target_table || '.id')::text
  FROM biz_status_log l
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'STATUS_ENTITY' AND m.ref_type = l.entity_type
  LEFT JOIN biz_accounting_period t ON t.id = l.entity_id
 WHERE l.entity_type = 'ACCOUNTING_PERIOD' AND t.id IS NULL
UNION ALL
SELECT m.ref_domain, 'biz_status_log', l.id, l.entity_no::text, 'entity_id', m.ref_type, l.entity_id,
       m.target_table, 'DANGLING', 'ROW_MISSING', (m.target_table || '.id')::text
  FROM biz_status_log l
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'STATUS_ENTITY' AND m.ref_type = l.entity_type
  LEFT JOIN biz_flow_edge t ON t.id = l.entity_id
 WHERE l.entity_type = 'FLOW_EDGE_CFG' AND t.id IS NULL
UNION ALL
SELECT m.ref_domain, 'biz_status_log', l.id, l.entity_no::text, 'entity_id', m.ref_type, l.entity_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_status_log l
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'STATUS_ENTITY' AND m.ref_type = l.entity_type
  LEFT JOIN biz_flow_linkage_rule t ON t.id = l.entity_id
 WHERE l.entity_type = 'FLOW_LINKAGE_CFG' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
-- ========== ⑤ 序列号来源 ==========
-- 序列号：类型有、id 恒空（写入点 serial_handler 4 处全是 None）⇒ MISSING_ID
UNION ALL
SELECT m.ref_domain, 'biz_material_serial', s.id, s.serial_no::text, 'source_id', m.ref_type, NULL,
       m.target_table, 'MISSING_ID', 'SOURCE_ID_REQUIRED', 'biz_material_serial.source_id'::text
  FROM biz_material_serial s
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'SERIAL_SOURCE' AND m.ref_type = s.source_type
 WHERE s.deleted_at IS NULL AND s.source_id IS NULL
-- 序列号：填了 source_id 但类型没有目标表（映射未定义）⇒ NO_TARGET
UNION ALL
SELECT m.ref_domain, 'biz_material_serial', s.id, s.serial_no::text, 'source_id', m.ref_type, s.source_id,
       m.target_table, 'NO_TARGET', 'NO_TARGET_TABLE', 'README:source_type=' || m.ref_type
  FROM biz_material_serial s
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'SERIAL_SOURCE' AND m.ref_type = s.source_type
 WHERE s.deleted_at IS NULL AND s.source_id IS NOT NULL AND m.target_table IS NULL;

-- ============================================================
-- DI-17 凭证来源悬空 / 缺 id（BLOCKER）
--   为什么是 BLOCKER：凭证是审计证据链的终点，`source_type` 说「我是收货过账生成的」，
--   但收货单没了 / 软删了 / id 是空的 ⇒ 财务按来源筛凭证会看到点不进去的幽灵行，
--   审计追一张凭证追不到任何单据，账实不符时无法定责。
--   分级：悬空 = ERROR；有类型无 id = WARN（还有 source_no 可人工追）；
--         MANUAL/OPENING/YEAR_END 这些「本来就没来源」不在此项（登记表里 target_table 为空）。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di17 AS
SELECT 'DI-17'::text AS issue_code, 'VOUCHER'::text AS entity_type,
       d.src_id AS entity_id, d.src_no AS entity_no, d.ref_field AS field_name,
       (d.ref_type || ' → ' || coalesce(d.target_table, '(无实体表)'))::text AS snapshot_value,
       d.miss_kind::text AS truth_value,
       d.truth_source,
       'DEFECT'::text AS drift_kind,
       CASE WHEN d.ref_shape = 'MISSING_ID' THEN 'WARN' ELSE 'ERROR' END::text AS severity,
       1 AS affected_count
  FROM di.v_poly_ref_dangling d
 WHERE d.ref_domain = 'VOUCHER_SOURCE';

COMMENT ON VIEW di.c_di17 IS
  'DI-17 凭证来源悬空（BLOCKER，悬空=ERROR/缺 id=WARN）：source_type 指出的单据已软删/不存在，或类型有而 source_id 空 —— 审计证据链断了（ARCH-3 DC-08）';

-- ============================================================
-- DI-18 库存流水单据悬空（BLOCKER）
--   为什么是 BLOCKER：库存流水是「账动了多少」的唯一审计证据，必须能回跳到造成它的单据
--   （收货 / 出库 / 调拨 / 序列号）。流水无源 ⇒ 账实不符时说不出「谁动的」。
--   分级：悬空 = ERROR；doc_type 本来没有实体表（OPENING/ADJUST）却填了 doc_id = INFO
--         （手工修数的痕迹，账本身是完整的，只是来源栏位填了个不存在的 id）。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di18 AS
SELECT 'DI-18'::text AS issue_code, 'INVENTORY_TXN'::text AS entity_type,
       d.src_id AS entity_id, d.src_no AS entity_no, d.ref_field AS field_name,
       (d.ref_type || ' → ' || coalesce(d.target_table, '(无实体表)'))::text AS snapshot_value,
       d.miss_kind::text AS truth_value,
       d.truth_source,
       'DEFECT'::text AS drift_kind,
       CASE WHEN d.ref_shape = 'NO_TARGET' THEN 'INFO' ELSE 'ERROR' END::text AS severity,
       1 AS affected_count
  FROM di.v_poly_ref_dangling d
 WHERE d.ref_domain = 'TXN_DOC';

COMMENT ON VIEW di.c_di18 IS
  'DI-18 库存流水单据悬空（BLOCKER，悬空=ERROR/无实体表的 doc_type=INFO）：doc_type 指出的单据已软删/不存在（ARCH-3 DC-08）';

-- ============================================================
-- DI-19 流程实例业务单据悬空（WARN）
--   为什么是 WARN 而不是 BLOCKER：审批实例可能因为「单据归档 / 测试清场 / 历史清理」
--   而指向已消失的单据，账与库存都不受影响；但它会让审批中心点进去 404、
--   任务归属解析不到（TASK_OWNER 等变量渲染成空）。
--   分级：悬空 = WARN；有类型无 biz_id = WARN（审批对象不明，审批人无法判断在批什么）。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di19 AS
SELECT 'DI-19'::text AS issue_code, 'FLOW_INSTANCE'::text AS entity_type,
       d.src_id AS entity_id, d.src_no AS entity_no, d.ref_field AS field_name,
       (d.ref_type || ' → ' || coalesce(d.target_table, '(无实体表)'))::text AS snapshot_value,
       d.miss_kind::text AS truth_value,
       d.truth_source,
       'DEFECT'::text AS drift_kind, 'WARN'::text AS severity, 1 AS affected_count
  FROM di.v_poly_ref_dangling d
 WHERE d.ref_domain = 'FLOW_BIZ';

COMMENT ON VIEW di.c_di19 IS
  'DI-19 流程实例绑定的业务单据悬空/缺 id（WARN）：审批挂在不存在的单据上，账不受影响但审批中心点进去 404（ARCH-3 DC-08）';

-- ============================================================
-- DI-20 状态日志实体悬空（INFO，**故意不告警**）
--   为什么是 INFO：`biz_status_log` 是**只增不改的历史台账**。实体被删（软删/清场）
--   是正常生命周期的一部分，日志必须保留「谁在什么时候把它改成什么」——
--   让日志跟着删才是错的（那是销毁审计证据）。所以这里只记录、不计入告警。
--   注：`entity_type=ACCOUNTING_YEAR` 是虚拟对象（无实体表，entity_id 存结转凭证 id），
--   登记表里 target_table 为空，不纳入比对。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di20 AS
SELECT 'DI-20'::text AS issue_code, 'STATUS_LOG'::text AS entity_type,
       d.src_id AS entity_id, d.src_no AS entity_no, d.ref_field AS field_name,
       (d.ref_type || ' → ' || coalesce(d.target_table, '(无实体表)'))::text AS snapshot_value,
       d.miss_kind::text AS truth_value,
       d.truth_source,
       'DEFECT'::text AS drift_kind, 'INFO'::text AS severity, 1 AS affected_count
  FROM di.v_poly_ref_dangling d
 WHERE d.ref_domain = 'STATUS_ENTITY';

COMMENT ON VIEW di.c_di20 IS
  'DI-20 状态日志实体悬空（INFO，只记录不告警）：日志是只增不改的历史，实体被删是正常生命周期（ARCH-3 DC-08）';

-- ============================================================
-- DI-21 序列号来源未记录（WARN / INFO）
--   为什么不是 BLOCKER：序列号的价值主要在「一物一码」的库存与成本（C5 巡检盯着件数），
--   来源单据缺失不影响账面数量与金额，只影响「这件货是哪张单进来的」的追责。
--   现状（实测）：`biz_material_serial.source_type` 有值 31 行、`source_id` **0 行有值**
--   —— 写入点 `serial_handler` 4 处全是 `source_id: None`，且四种来源类型
--   （PURCHASE/TRANSFER/PRODUCE/RETURN）**没有登记任何目标表**。
--   所以本项实际输出的是 MISSING_ID（有类型无 id）与 NO_TARGET（无表映射却填了 id）。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di21 AS
SELECT 'DI-21'::text AS issue_code, 'MATERIAL_SERIAL'::text AS entity_type,
       d.src_id AS entity_id, d.src_no AS entity_no, d.ref_field AS field_name,
       (d.ref_type || ' → ' || coalesce(d.target_table, '(无实体表)'))::text AS snapshot_value,
       d.miss_kind::text AS truth_value,
       d.truth_source,
       'DEFECT'::text AS drift_kind,
       CASE WHEN d.ref_shape = 'NO_TARGET' THEN 'WARN' ELSE 'INFO' END::text AS severity,
       1 AS affected_count
  FROM di.v_poly_ref_dangling d
 WHERE d.ref_domain = 'SERIAL_SOURCE';

COMMENT ON VIEW di.c_di21 IS
  'DI-21 序列号来源未记录（INFO=有类型无 id / WARN=无表映射却填了 id）：serial_handler 从不写 source_id（ARCH-3 DC-08 附项）';

-- ============================================================
-- DI-22 多态引用登记缺口（INFO，**本组巡检的可信度护栏**）
--   数据里出现了登记表 `di.v_poly_ref_map` 没有的（域, 类型）⇒ 这个类型的引用
--   **不会被 DI-17…DI-21 查到**（静默漏报，比漏数据更危险）。
--   为什么 INFO：新增一种单据/流水类型本身不是缺陷；缺陷是「加了类型却没进巡检」。
--   典型命中：客户端自建流程的 biz_type（E2E 夹具 / 定制流程）—— 它们按设计不参与自检。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di22 AS
WITH observed AS (
    SELECT 'VOUCHER_SOURCE'::text AS ref_domain, source_type::text AS ref_type,
           'biz_voucher'::text AS src_table, count(*) AS rows_hit
      FROM biz_voucher
     WHERE deleted_at IS NULL AND coalesce(trim(source_type), '') <> ''
     GROUP BY 2
    UNION ALL
    SELECT 'TXN_DOC', doc_type::text, 'biz_inventory_txn', count(*)
      FROM biz_inventory_txn
     WHERE coalesce(trim(doc_type), '') <> ''
     GROUP BY 2
    UNION ALL
    SELECT 'FLOW_BIZ', biz_type::text, 'biz_flow_instance', count(*)
      FROM biz_flow_instance
     WHERE coalesce(trim(biz_type), '') <> ''
     GROUP BY 2
    UNION ALL
    SELECT 'STATUS_ENTITY', entity_type::text, 'biz_status_log', count(*)
      FROM biz_status_log
     WHERE coalesce(trim(entity_type), '') <> ''
     GROUP BY 2
    UNION ALL
    SELECT 'SERIAL_SOURCE', source_type::text, 'biz_material_serial', count(*)
      FROM biz_material_serial
     WHERE deleted_at IS NULL AND coalesce(trim(source_type), '') <> ''
     GROUP BY 2
)
SELECT 'DI-22'::text AS issue_code, 'POLY_REF_MAP'::text AS entity_type,
       NULL::uuid AS entity_id,
       (o.ref_domain || ':' || o.ref_type)::text AS entity_no,
       'ref_type'::text AS field_name,
       (o.ref_type || ' (未登记)')::text AS snapshot_value,
       'IN_V_POLY_REF_MAP'::text AS truth_value,
       ('di.v_poly_ref_map / ' || o.src_table)::text AS truth_source,
       'COVERAGE_GAP'::text AS drift_kind, 'INFO'::text AS severity,
       o.rows_hit AS affected_count
  FROM observed o
  LEFT JOIN di.v_poly_ref_map m
         ON m.ref_domain = o.ref_domain AND m.ref_type = o.ref_type
 WHERE m.ref_type IS NULL;

COMMENT ON VIEW di.c_di22 IS
  'DI-22 多态引用登记缺口（INFO）：数据里出现 di.v_poly_ref_map 未登记的类型 ⇒ 该类型的悬空不会被 DI-17…DI-21 查到（护栏自检）';

COMMIT;
