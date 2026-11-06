-- ============================================================
-- 10_checks.sql —— 巡检项定义（DI-01…DI-26，其中 DI-14 是护栏自检，
--                    DI-17…DI-25 是多态引用巡检（类型列 + id 列，见 ARCH-3 DC-08））
--                    P2-6b 补齐 DI-21 的目标表，并新增 DI-23/24/25（附件 / 提醒 / 抄送）
--                    DI-26 = 供应商 ID→名字映射唯一性（P2-7；合并历史分叉时与 DI-23
--                    附件巡检撞号，故让到 26 —— DI-23/24/25 已在 144 生产口径与证据里，
--                    编号不动，见 docs/evidence/20261105-repo-union-merge/）
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
-- DI-17…DI-25 多态引用巡检（ARCH-3 §5 DC-08 / 卡 P2-6 / P2-6b 补口）
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
    -- FIN-2 新增的三个来源：`VoucherSourceType::ALL` 已包含，登记表必须同步，
    -- 否则 DI-17 的「凭证来源悬空」永远看不到固定资产购入 / 处置 / 折旧三类凭证
    ('VOUCHER_SOURCE', 'FIXED_ASSET_ACQ',      'biz_fixed_asset',        'deleted_at', '固定资产购入生成（FIN-2）：source_id = 资产卡片 id，见 asset_handler 购入凭证'),
    ('VOUCHER_SOURCE', 'FIXED_ASSET_DISPOSAL', 'biz_fixed_asset',        'deleted_at', '固定资产处置生成（FIN-2）：source_id = 资产卡片 id，见 asset_bridge::post_disposal_vouchers'),
    ('VOUCHER_SOURCE', 'DEPRECIATION',         'biz_depreciation_batch', NULL,         '折旧计提生成（FIN-2）：source_id = 折旧批次 id（批次行无软删）'),
    -- FIN-3 报销的两个来源（expense_bridge::SOURCE_REPORT / SOURCE_PAYMENT）：
    -- 入账与付款两张凭证都挂在**报销单**上（source_id = biz_expense_report.id，
    -- 付款记录不是独立业务对象），漏登记 DI-22 会报登记缺口、DI-17 查不到悬空。
    ('VOUCHER_SOURCE', 'EXPENSE_REPORT',       'biz_expense_report',     'deleted_at', '费用报销入账生成（FIN-3）：source_id = 报销单 id'),
    ('VOUCHER_SOURCE', 'EXPENSE_PAYMENT',      'biz_expense_report',     'deleted_at', '费用报销付款生成（FIN-3）：source_id = 报销单 id（付款挂在报销单上）'),
    -- ② 库存流水来源单据：biz_inventory_txn.doc_type + doc_id
    ('TXN_DOC', 'STOCK_IN',        'biz_stock_in',        'deleted_at', '收货过账的流水'),
    ('TXN_DOC', 'STOCK_OUT',       'biz_stock_out',       'deleted_at', '出库过账的流水'),
    ('TXN_DOC', 'STOCK_TRANSFER',  'biz_stock_transfer',  'deleted_at', '调拨发出的流水'),
    ('TXN_DOC', 'STOCK_TAKE',      'biz_stock_take',      'deleted_at', '盘点过账的流水（doc_id = 盘点单 id；TxnDocType::StockTake，P1-FIX-9 补登记，漏一行 DI-18 就查不到盘点流水悬空）'),
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
    ('FLOW_BIZ', 'EXPENSE',          'biz_expense_report', 'deleted_at', '费用报销审批（FIN-3：biz_id = 报销单 id）'),
    ('FLOW_BIZ', 'OTHER',            NULL,                 NULL,         '客户端自建流程兜底类型：设计上不参与自检'),
    -- FIN-1 / FIN-2 新增的两个审批动作：`FlowBizType::ALL` 已包含，登记表必须同步，
    -- 否则 DI-19 的「流程实例业务单据悬空」永远看不到预算审批 / 资产处置审批
    ('FLOW_BIZ', 'BUDGET_APPROVE',   'biz_budget',         'deleted_at', '预算审批（FIN-1）：biz_id = 预算行 id（budget_bridge 按 biz_budget.id 取制单人）'),
    ('FLOW_BIZ', 'ASSET_DISPOSAL',   'biz_fixed_asset',    'deleted_at', '资产处置审批（FIN-2）：biz_id = 资产卡片 id（一张卡同时只能有一个在跑处置单）'),
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
    -- FIN-1 / FIN-2 新增的三个 DocKind：`DocKind::ALL` 已包含，登记表必须同步，
    -- 否则 DI-20 的「状态日志实体悬空」永远看不到预算 / 固资 / 折旧三类对象
    ('STATUS_ENTITY', 'BUDGET',            'biz_budget',            'deleted_at', '预算单（FIN-1）'),
    ('STATUS_ENTITY', 'FIXED_ASSET',       'biz_fixed_asset',       'deleted_at', '固定资产卡片（FIN-2）'),
    ('STATUS_ENTITY', 'DEPRECIATION',      'biz_depreciation_batch', NULL,        '折旧批次（FIN-2：批次行无软删）'),
    ('STATUS_ENTITY', 'EXPENSE_REPORT',    'biz_expense_report',    'deleted_at', '费用报销单（FIN-3）'),
    -- 销售链 SO-05：销售订单进状态日志（`sales_order_handler` 的建单 / 流转都写 entity_type=SALE_ORDER）
    ('STATUS_ENTITY', 'SALE_ORDER',        'biz_sales_order',       'deleted_at', '销售订单（销售链 SO-05）'),
    -- 销售链 SO-09：发货单进状态日志（`shipment_handler` 的建单 / 流转都写 entity_type=SHIPMENT）
    ('STATUS_ENTITY', 'SHIPMENT',          'biz_shipment',          'deleted_at', '发货单（销售链 SO-09）'),
    -- ⑤ 序列号来源：biz_material_serial.source_type + source_id（P2-6b 补全映射）
    ('SERIAL_SOURCE', 'PURCHASE', 'biz_stock_in',       'deleted_at', '收货过账生成（in_type=PURCHASE），source_id = 收货单 id'),
    ('SERIAL_SOURCE', 'RETURN',   'biz_stock_in',       'deleted_at', '退货入库生成（in_type=RETURN，同一张收货单表），source_id = 收货单 id'),
    ('SERIAL_SOURCE', 'TRANSFER', 'biz_stock_transfer', 'deleted_at', '调拨入库单；目前调拨只搬已有序列号、不新建号，本条登记备查'),
    ('SERIAL_SOURCE', 'PRODUCE',  NULL,                 NULL,         '生产入库：生产模块未落地，没有单据表 ⇒ 只允许「有类型、无 id」'),
    -- ⑥ 单据附件：biz_doc_attachment.doc_type + doc_id（P2-6b，取值域 = ATTACHMENT_DOC_KINDS）
    ('ATTACHMENT_DOC', 'CONTRACT',       'biz_contract',       'deleted_at', '合同扫描件 / 签署回执'),
    ('ATTACHMENT_DOC', 'PURCHASE_ORDER', 'biz_purchase_order', 'deleted_at', '采购单附件'),
    ('ATTACHMENT_DOC', 'STOCK_IN',       'biz_stock_in',       'deleted_at', '收货单附件（发票 / 签收单）'),
    ('ATTACHMENT_DOC', 'PAYABLE',        'biz_payable',        'deleted_at', '应付单附件'),
    ('ATTACHMENT_DOC', 'PAYMENT',        'biz_payment',        'deleted_at', '付款单附件'),
    ('ATTACHMENT_DOC', 'FIXED_ASSET',    'biz_fixed_asset',    'deleted_at', '固定资产卡片附件（FIN-2：处置凭证 / 验收单）'),
    ('ATTACHMENT_DOC', 'EXPENSE_REPORT', 'biz_expense_report', 'deleted_at', '报销单附件（FIN-3：发票影像）'),
    -- ⑦ 业务提醒台账：biz_reminder_log.doc_type + doc_id（P2-6b，取值域 = REMINDER_DOC_KINDS）
    ('REMINDER_DOC', 'PAYABLE',        'biz_payable',        'deleted_at', '应付到期提醒'),
    ('REMINDER_DOC', 'PURCHASE_ORDER', 'biz_purchase_order', 'deleted_at', '采购超期提醒'),
    ('REMINDER_DOC', 'CONTRACT',       'biz_contract',       'deleted_at', '合同到期提醒'),
    ('REMINDER_DOC', 'INVENTORY',      'biz_inventory',      NULL,         '低库存提醒（库存行无软删）'),
    -- P1-FIX-10：结账催办（PERIOD_REMIND / YEAR_END_REMIND）的挡路单据进台账，值域 5 → 8
    ('REMINDER_DOC', 'VOUCHER',        'biz_voucher',        'deleted_at', '结账催办：草稿凭证挡路'),
    ('REMINDER_DOC', 'STOCK_IN',       'biz_stock_in',       'deleted_at', '结账催办：收货单未过账挡路'),
    ('REMINDER_DOC', 'STOCK_TRANSFER', 'biz_stock_transfer', 'deleted_at', '结账催办：调拨单未完结挡路'),
    ('REMINDER_DOC', 'EXPENSE_REPORT', 'biz_expense_report', 'deleted_at', '报销待付款提醒（FIN-3）'),
    -- ⑧ 流程抄送来源：biz_flow_cc.source_type + source_ref（P2-6b，取值域 = flow::graph::AssigneeKind）
    --    只有「角色 / 部门」两类会把来源 id 落进 source_ref；人本身记在 receiver_id。
    ('FLOW_CC_SOURCE', 'ROLE',               'sys_role', 'deleted_at', '角色展开的抄送，source_ref = 角色 id'),
    ('FLOW_CC_SOURCE', 'ADMIN',              'sys_role', 'deleted_at', '超管兜底（role_code=SUPER_ADMIN），source_ref = 角色 id'),
    ('FLOW_CC_SOURCE', 'DEPT_LEADER',        'sys_dept', 'deleted_at', '部门负责人，source_ref = 部门 id'),
    ('FLOW_CC_SOURCE', 'INITIATOR_SUPERIOR', 'sys_dept', 'deleted_at', '发起人上级部门负责人，source_ref = 部门 id'),
    ('FLOW_CC_SOURCE', 'USER',               NULL, NULL, '点名用户：人写在 receiver_id，source_ref 为空'),
    ('FLOW_CC_SOURCE', 'INITIATOR',          NULL, NULL, '发起人本人：没有来源对象'),
    ('FLOW_CC_SOURCE', 'TASK_OWNER',         NULL, NULL, '任务负责人（运行期从 biz_task 解析）'),
    ('FLOW_CC_SOURCE', 'TASK_CREATOR',       NULL, NULL, '任务创建人（运行期从 biz_task 解析）'),
    ('FLOW_CC_SOURCE', 'TASK_ASSIGNER',      NULL, NULL, '任务分派人（运行期从 biz_task 解析）'),
    ('FLOW_CC_SOURCE', 'FORM_VAR',           NULL, NULL, '表单变量点名：来源是发起人填的值'),
    ('FLOW_CC_SOURCE', 'EXPRESSION',         NULL, NULL, '规则树求值（本身不落库，落到具体来源）')
  ) AS t(ref_domain, ref_type, target_table, soft_delete_col, note);

-- ------------------------------------------------------------
-- 已核查豁免台账：**精确到行**的「这条已核对过、不必再报」清单
--   为什么是视图 + VALUES 而不是一张表（关键设计，别改成表）：
--   豁免会永久关掉一条真实告警，所以它必须**走 git review + 发版**——
--   改一行 = 一次提交 = 有人签字；而库里的一张表谁都能加一行（写行不用过 review），
--   「谁能悄悄关掉告警」就等于「巡检守不住的东西」，这种口子不能开。
--   三条硬约束（缺一不可，见设计文档 §2.4c）：
--     ① 精确三元组 (check_id, src_table, src_id)：**禁止**按 doc_type / 表名 /
--        前缀 / 通配豁免（那样一次豁免会顺手盖住未来所有同类真问题）；
--     ② 必须写 reason（为什么可以不管）+ checked_by（谁核对的）+ checked_on（哪天）；
--     ③ 必须写 expires_on：**到期自然失效、自动回红**，不靠人记得回来删。
--   使用方式（其它巡检要豁免时照抄，把 check_id 换成自己的）：
--     LEFT JOIN di.v_poly_ref_waiver w
--            ON w.check_id = 'DI-xx' AND w.src_table = d.src_table
--           AND w.src_id = d.src_id::text AND w.expires_on > current_date
-- ------------------------------------------------------------
CREATE OR REPLACE VIEW di.v_poly_ref_waiver AS
SELECT check_id, src_table, src_id, reason, checked_by, checked_on, expires_on
  FROM (VALUES
    -- ① DI-18 / biz_inventory_txn / 01a19108-ee53-73c6-bdaa-0109614ea963
    --    DATAFIX-20261101-P14：该批次 +10（2026-10-31 16:17:24 本行 IN）与修数 −10
    --    （2026-11-01 00:20:44 doc_no=DATAFIX-20261101-P14 的 OUT）对平，
    --    biz_inventory 该物料该仓现存 qty=0 / amount=0 / avg_cost=0，
    --    修数前快照在 datafix_p14_snapshot（10 条序列号 + 1 条库存行）。
    --    流水**必须保留**（审计证据），序列号软删是修数本意 ⇒ 它指向的序列号
    --    永远「悬空」，属历史正常；到期（2027-02-04）自动回 ERROR 再复核一次。
    ('DI-18', 'biz_inventory_txn', '01a19108-ee53-73c6-bdaa-0109614ea963',
     'DATAFIX-20261101-P14 修数留档：+10/-10 对平、库存净 0、成本 0、修数前快照在 datafix_p14_snapshot；流水保留作审计证据',
     'manong', DATE '2026-11-04', DATE '2027-02-04')
  ) AS t(check_id, src_table, src_id, reason, checked_by, checked_on, expires_on);

COMMENT ON VIEW di.v_poly_ref_waiver IS
  '已核查豁免台账（精确到行 id + 带期限）：只允许 (check_id, src_table, src_id) 三元组全等匹配，到期（expires_on <= current_date）自动失效回红；豁免变更必须走 git review + 发版';

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
-- FIN-2：购入 / 处置凭证都挂在资产卡片上（source_id = biz_fixed_asset.id）
UNION ALL
SELECT m.ref_domain, 'biz_voucher', v.id, v.voucher_no::text, 'source_id', m.ref_type, v.source_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_voucher v
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'VOUCHER_SOURCE' AND m.ref_type = v.source_type
  LEFT JOIN biz_fixed_asset t ON t.id = v.source_id
 WHERE v.deleted_at IS NULL AND v.source_id IS NOT NULL
   AND v.source_type IN ('FIXED_ASSET_ACQ', 'FIXED_ASSET_DISPOSAL')
   AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
-- FIN-2：折旧凭证挂折旧批次（批次行无软删，只判存在）
UNION ALL
SELECT m.ref_domain, 'biz_voucher', v.id, v.voucher_no::text, 'source_id', m.ref_type, v.source_id,
       m.target_table, 'DANGLING', 'ROW_MISSING', (m.target_table || '.id')::text
  FROM biz_voucher v
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'VOUCHER_SOURCE' AND m.ref_type = v.source_type
  LEFT JOIN biz_depreciation_batch t ON t.id = v.source_id
 WHERE v.deleted_at IS NULL AND v.source_id IS NOT NULL
   AND v.source_type = 'DEPRECIATION' AND t.id IS NULL
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
-- FIN-1：预算审批的 biz_id = 预算行 id
UNION ALL
SELECT m.ref_domain, 'biz_flow_instance', i.id, i.instance_no::text, 'biz_id', m.ref_type, i.biz_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_flow_instance i
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'FLOW_BIZ' AND m.ref_type = i.biz_type
  LEFT JOIN biz_budget t ON t.id = i.biz_id
 WHERE i.deleted_at IS NULL AND i.biz_id IS NOT NULL
   AND i.biz_type = 'BUDGET_APPROVE' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
-- FIN-2：资产处置审批的 biz_id = 资产卡片 id
UNION ALL
SELECT m.ref_domain, 'biz_flow_instance', i.id, i.instance_no::text, 'biz_id', m.ref_type, i.biz_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_flow_instance i
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'FLOW_BIZ' AND m.ref_type = i.biz_type
  LEFT JOIN biz_fixed_asset t ON t.id = i.biz_id
 WHERE i.deleted_at IS NULL AND i.biz_id IS NOT NULL
   AND i.biz_type = 'ASSET_DISPOSAL' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
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
-- FIN-1 / FIN-2：登记表早有 BUDGET / FIXED_ASSET / DEPRECIATION 三行，
-- 但**没有分支**时 DI-20 一行都看不到（「登记了但不查」= 比不登记更糟）
UNION ALL
SELECT m.ref_domain, 'biz_status_log', l.id, l.entity_no::text, 'entity_id', m.ref_type, l.entity_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_status_log l
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'STATUS_ENTITY' AND m.ref_type = l.entity_type
  LEFT JOIN biz_budget t ON t.id = l.entity_id
 WHERE l.entity_type = 'BUDGET' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_status_log', l.id, l.entity_no::text, 'entity_id', m.ref_type, l.entity_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_status_log l
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'STATUS_ENTITY' AND m.ref_type = l.entity_type
  LEFT JOIN biz_fixed_asset t ON t.id = l.entity_id
 WHERE l.entity_type = 'FIXED_ASSET' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_status_log', l.id, l.entity_no::text, 'entity_id', m.ref_type, l.entity_id,
       m.target_table, 'DANGLING', 'ROW_MISSING', (m.target_table || '.id')::text
  FROM biz_status_log l
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'STATUS_ENTITY' AND m.ref_type = l.entity_type
  LEFT JOIN biz_depreciation_batch t ON t.id = l.entity_id
 WHERE l.entity_type = 'DEPRECIATION' AND t.id IS NULL
-- ========== ⑤ 序列号来源 ==========
-- 序列号：PURCHASE / RETURN → 收货单（同一张表，靠 source_type 区分）
UNION ALL
SELECT m.ref_domain, 'biz_material_serial', s.id, s.serial_no::text, 'source_id', m.ref_type, s.source_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_material_serial s
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'SERIAL_SOURCE' AND m.ref_type = s.source_type
  LEFT JOIN biz_stock_in t ON t.id = s.source_id
 WHERE s.deleted_at IS NULL AND s.source_id IS NOT NULL
   AND s.source_type IN ('PURCHASE', 'RETURN')
   AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
-- 序列号：TRANSFER → 调拨单
UNION ALL
SELECT m.ref_domain, 'biz_material_serial', s.id, s.serial_no::text, 'source_id', m.ref_type, s.source_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_material_serial s
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'SERIAL_SOURCE' AND m.ref_type = s.source_type
  LEFT JOIN biz_stock_transfer t ON t.id = s.source_id
 WHERE s.deleted_at IS NULL AND s.source_id IS NOT NULL AND s.source_type = 'TRANSFER'
   AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
-- 序列号：有实体单据表的类型却没 id（P2-6b：PRODUCE 没有单据表，不算缺）
UNION ALL
SELECT m.ref_domain, 'biz_material_serial', s.id, s.serial_no::text, 'source_id', m.ref_type, NULL,
       m.target_table, 'MISSING_ID', 'SOURCE_ID_REQUIRED', 'biz_material_serial.source_id'::text
  FROM biz_material_serial s
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'SERIAL_SOURCE' AND m.ref_type = s.source_type
 WHERE s.deleted_at IS NULL AND s.source_id IS NULL AND m.target_table IS NOT NULL
-- 序列号：填了 source_id 但类型没有目标表 ⇒ NO_TARGET
UNION ALL
SELECT m.ref_domain, 'biz_material_serial', s.id, s.serial_no::text, 'source_id', m.ref_type, s.source_id,
       m.target_table, 'NO_TARGET', 'NO_TARGET_TABLE', 'README:source_type=' || m.ref_type
  FROM biz_material_serial s
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'SERIAL_SOURCE' AND m.ref_type = s.source_type
 WHERE s.deleted_at IS NULL AND s.source_id IS NOT NULL AND m.target_table IS NULL
-- ========== ⑥ 单据附件（P2-6b） ==========
UNION ALL
SELECT m.ref_domain, 'biz_doc_attachment', a.id, a.id::text, 'doc_id', m.ref_type, a.doc_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_doc_attachment a
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'ATTACHMENT_DOC' AND m.ref_type = a.doc_type
  LEFT JOIN biz_contract t ON t.id = a.doc_id
 WHERE a.deleted_at IS NULL AND a.doc_type = 'CONTRACT' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_doc_attachment', a.id, a.id::text, 'doc_id', m.ref_type, a.doc_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_doc_attachment a
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'ATTACHMENT_DOC' AND m.ref_type = a.doc_type
  LEFT JOIN biz_purchase_order t ON t.id = a.doc_id
 WHERE a.deleted_at IS NULL AND a.doc_type = 'PURCHASE_ORDER' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_doc_attachment', a.id, a.id::text, 'doc_id', m.ref_type, a.doc_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_doc_attachment a
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'ATTACHMENT_DOC' AND m.ref_type = a.doc_type
  LEFT JOIN biz_stock_in t ON t.id = a.doc_id
 WHERE a.deleted_at IS NULL AND a.doc_type = 'STOCK_IN' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_doc_attachment', a.id, a.id::text, 'doc_id', m.ref_type, a.doc_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_doc_attachment a
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'ATTACHMENT_DOC' AND m.ref_type = a.doc_type
  LEFT JOIN biz_payable t ON t.id = a.doc_id
 WHERE a.deleted_at IS NULL AND a.doc_type = 'PAYABLE' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_doc_attachment', a.id, a.id::text, 'doc_id', m.ref_type, a.doc_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_doc_attachment a
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'ATTACHMENT_DOC' AND m.ref_type = a.doc_type
  LEFT JOIN biz_payment t ON t.id = a.doc_id
 WHERE a.deleted_at IS NULL AND a.doc_type = 'PAYMENT' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
-- FIN-2：固定资产卡片附件（登记表已加 ATTACHMENT_DOC.FIXED_ASSET，这里必须有对应分支，
-- 否则「登记了但不查」= DI-22 绿、DI-23 漏报，比不登记更糟）
UNION ALL
SELECT m.ref_domain, 'biz_doc_attachment', a.id, a.id::text, 'doc_id', m.ref_type, a.doc_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_doc_attachment a
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'ATTACHMENT_DOC' AND m.ref_type = a.doc_type
  LEFT JOIN biz_fixed_asset t ON t.id = a.doc_id
 WHERE a.deleted_at IS NULL AND a.doc_type = 'FIXED_ASSET' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
-- ========== ⑦ 业务提醒台账（P2-6b） ==========
--   `biz_reminder_log` 是幂等台账（只增不改）：单据被删是正常生命周期，
--   所以这里只报 DANGLING，不加 MISSING_ID（`doc_id` 列 NOT NULL，缺 id 写不进来）。
UNION ALL
SELECT m.ref_domain, 'biz_reminder_log', r.id, coalesce(r.doc_no, r.id::text), 'doc_id', m.ref_type, r.doc_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_reminder_log r
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'REMINDER_DOC' AND m.ref_type = r.doc_type
  LEFT JOIN biz_payable t ON t.id = r.doc_id
 WHERE r.doc_type = 'PAYABLE' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_reminder_log', r.id, coalesce(r.doc_no, r.id::text), 'doc_id', m.ref_type, r.doc_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_reminder_log r
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'REMINDER_DOC' AND m.ref_type = r.doc_type
  LEFT JOIN biz_purchase_order t ON t.id = r.doc_id
 WHERE r.doc_type = 'PURCHASE_ORDER' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_reminder_log', r.id, coalesce(r.doc_no, r.id::text), 'doc_id', m.ref_type, r.doc_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_reminder_log r
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'REMINDER_DOC' AND m.ref_type = r.doc_type
  LEFT JOIN biz_contract t ON t.id = r.doc_id
 WHERE r.doc_type = 'CONTRACT' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_reminder_log', r.id, coalesce(r.doc_no, r.id::text), 'doc_id', m.ref_type, r.doc_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_reminder_log r
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'REMINDER_DOC' AND m.ref_type = r.doc_type
  LEFT JOIN biz_inventory t ON t.id = r.doc_id
 WHERE r.doc_type = 'INVENTORY' AND t.id IS NULL
-- P1-FIX-10：结账催办的挡路单据（凭证 / 收货单 / 调拨单）也进台账，三块与新登记行一一对应
UNION ALL
SELECT m.ref_domain, 'biz_reminder_log', r.id, coalesce(r.doc_no, r.id::text), 'doc_id', m.ref_type, r.doc_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_reminder_log r
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'REMINDER_DOC' AND m.ref_type = r.doc_type
  LEFT JOIN biz_voucher t ON t.id = r.doc_id
 WHERE r.doc_type = 'VOUCHER' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_reminder_log', r.id, coalesce(r.doc_no, r.id::text), 'doc_id', m.ref_type, r.doc_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_reminder_log r
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'REMINDER_DOC' AND m.ref_type = r.doc_type
  LEFT JOIN biz_stock_in t ON t.id = r.doc_id
 WHERE r.doc_type = 'STOCK_IN' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_reminder_log', r.id, coalesce(r.doc_no, r.id::text), 'doc_id', m.ref_type, r.doc_id,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_reminder_log r
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'REMINDER_DOC' AND m.ref_type = r.doc_type
  LEFT JOIN biz_stock_transfer t ON t.id = r.doc_id
 WHERE r.doc_type = 'STOCK_TRANSFER' AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
-- ========== ⑧ 流程抄送来源（P2-6b） ==========
--   角色（ROLE / ADMIN）→ sys_role；部门（DEPT_LEADER / INITIATOR_SUPERIOR）→ sys_dept。
--   其余来源本来就不写 source_ref（人写在 receiver_id），登记表 target_table 为空。
UNION ALL
SELECT m.ref_domain, 'biz_flow_cc', c.id, (c.node_key || '#' || c.round)::text, 'source_ref', m.ref_type, c.source_ref,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_flow_cc c
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'FLOW_CC_SOURCE' AND m.ref_type = c.source_type
  LEFT JOIN sys_role t ON t.id = c.source_ref
 WHERE c.source_type IN ('ROLE', 'ADMIN') AND c.source_ref IS NOT NULL
   AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
UNION ALL
SELECT m.ref_domain, 'biz_flow_cc', c.id, (c.node_key || '#' || c.round)::text, 'source_ref', m.ref_type, c.source_ref,
       m.target_table, 'DANGLING',
       CASE WHEN t.id IS NULL THEN 'ROW_MISSING' ELSE 'SOFT_DELETED' END,
       (m.target_table || '.' || coalesce(m.soft_delete_col, 'id'))::text
  FROM biz_flow_cc c
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'FLOW_CC_SOURCE' AND m.ref_type = c.source_type
  LEFT JOIN sys_dept t ON t.id = c.source_ref
 WHERE c.source_type IN ('DEPT_LEADER', 'INITIATOR_SUPERIOR') AND c.source_ref IS NOT NULL
   AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
-- 抄送：角色/部门来源却没记 source_ref ⇒ 不知道抄送对象是谁（追溯断链）
UNION ALL
SELECT m.ref_domain, 'biz_flow_cc', c.id, (c.node_key || '#' || c.round)::text, 'source_ref', m.ref_type, NULL,
       m.target_table, 'MISSING_ID', 'SOURCE_REF_REQUIRED',
       ('biz_flow_cc.source_ref / ' || m.target_table)::text
  FROM biz_flow_cc c
  JOIN di.v_poly_ref_map m ON m.ref_domain = 'FLOW_CC_SOURCE' AND m.ref_type = c.source_type
 WHERE c.source_ref IS NULL AND m.target_table IS NOT NULL;

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
-- DI-18 库存流水单据悬空（BLOCKER，**已核查豁免可降级**）
--   为什么是 BLOCKER：库存流水是「账动了多少」的唯一审计证据，必须能回跳到造成它的单据
--   （收货 / 出库 / 调拨 / 序列号）。流水无源 ⇒ 账实不符时说不出「谁动的」。
--   分级：悬空 = ERROR；doc_type 本来没有实体表（OPENING/ADJUST）却填了 doc_id = INFO
--         （手工修数的痕迹，账本身是完整的，只是来源栏位填了个不存在的 id）。
--   已核查豁免（D-1 收口口径）：命中行若在 di.v_poly_ref_waiver 里有**精确到行 id**
--   且**未过期**的登记 ⇒ 降 INFO，并在 snapshot_value 前缀 `已核查豁免#<checked_on>：`
--   （原值保留，可见性不能丢）。expires_on <= current_date 的那一刻，JOIN 不再命中，
--   本行**自动回到 ERROR** —— 没有任何额外逻辑，也不靠人记得回来删豁免。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di18 AS
SELECT 'DI-18'::text AS issue_code, 'INVENTORY_TXN'::text AS entity_type,
       d.src_id AS entity_id, d.src_no AS entity_no, d.ref_field AS field_name,
       (CASE WHEN w.src_id IS NOT NULL
             THEN '已核查豁免#' || to_char(w.checked_on, 'YYYY-MM-DD') || '：'
             ELSE '' END
        || d.ref_type || ' → ' || coalesce(d.target_table, '(无实体表)'))::text AS snapshot_value,
       d.miss_kind::text AS truth_value,
       d.truth_source,
       'DEFECT'::text AS drift_kind,
       CASE WHEN w.src_id IS NOT NULL    THEN 'INFO'   -- 已核对并留档（带期限）
            WHEN d.ref_shape = 'NO_TARGET' THEN 'INFO'  -- 本来没有实体表却填了 id
            ELSE 'ERROR' END::text AS severity,
       1 AS affected_count
  FROM di.v_poly_ref_dangling d
  LEFT JOIN di.v_poly_ref_waiver w
         ON w.check_id = 'DI-18'
        AND w.src_table = d.src_table
        AND w.src_id = d.src_id::text
        AND w.expires_on > current_date
 WHERE d.ref_domain = 'TXN_DOC';

COMMENT ON VIEW di.c_di18 IS
  'DI-18 库存流水单据悬空（BLOCKER，悬空=ERROR/无实体表的 doc_type=INFO/已核查豁免=INFO）：doc_type 指出的单据已软删/不存在（ARCH-3 DC-08）；豁免只认 di.v_poly_ref_waiver 里精确到行 id 且未过期的登记，到期自动回 ERROR';

-- ============================================================
-- DI-19 流程实例业务单据悬空（**按实例状态分级**：PENDING=WARN / 终态=INFO）
--   为什么不再一律 WARN：审批实例**不随业务对象删除而删**——
--   任务删除只终止 PENDING 的那张审批（task_bridge.rs:650「任务已删除，审批单自动终止」），
--   合同删除同理（contract_handler.rs::cancel_approval_of）。终态实例是**审计证据**
--   （谁在什么时候批的），永远保留，所以「终态实例指向已删对象」是**历史正常**
--   （与 T12 / R-P1-1 口径一致：审批终态 = 审批链的终点，可回溯，不随业务清场消失）。
--   反之「PENDING 实例指向已删对象」= 活待办挂在死对象上，审批人批的是一个不存在的东西
--   ⇒ 这才是需要人看的（WARN）。
--   分级：DANGLING + 实例 status='PENDING' ⇒ WARN；
--         DANGLING + 其它终态（APPROVED / CANCELLED / WITHDRAWN / REJECTED / 将来新增的）
--         ⇒ INFO（**仍然列出**，可见性不能丢，只是不进告警队列）；
--         MISSING_ID / NO_TARGET（有 biz_type 没 biz_id、或类型本无实体表）⇒ 保持 WARN
--         （这类与实例状态无关，无法用「终态」解释）。
--   注：实例行读不到（LEFT JOIN 落空，正常不会发生）⇒ 无法判断状态 ⇒ 宁可报 WARN。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di19 AS
SELECT 'DI-19'::text AS issue_code, 'FLOW_INSTANCE'::text AS entity_type,
       d.src_id AS entity_id, d.src_no AS entity_no, d.ref_field AS field_name,
       (d.ref_type || ' → ' || coalesce(d.target_table, '(无实体表)'))::text AS snapshot_value,
       d.miss_kind::text AS truth_value,
       d.truth_source,
       'DEFECT'::text AS drift_kind,
       CASE WHEN d.ref_shape <> 'DANGLING' THEN 'WARN'  -- 缺 id / 无实体表：与实例状态无关
            WHEN i.id IS NULL            THEN 'WARN'   -- 判不出状态就不降级
            WHEN i.status = 'PENDING'    THEN 'WARN'   -- 活待办挂在死对象上
            ELSE 'INFO' END::text AS severity,         -- 终态实例：审计证据，历史正常
       1 AS affected_count
  FROM di.v_poly_ref_dangling d
  LEFT JOIN biz_flow_instance i
         ON d.src_table = 'biz_flow_instance' AND i.id = d.src_id
 WHERE d.ref_domain = 'FLOW_BIZ';

COMMENT ON VIEW di.c_di19 IS
  'DI-19 流程实例业务单据悬空（按实例状态分级：PENDING=WARN/终态=INFO，缺 biz_id 仍 WARN）：终态实例是审计证据、不随业务对象删除（T12/R-P1-1）；只有「活待办指向已删对象」才需要人看（ARCH-3 DC-08）';

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
-- DI-21 序列号来源未记录 / 悬空（WARN / INFO）
--   为什么不是 BLOCKER：序列号的价值主要在「一物一码」的库存与成本（C5 巡检盯着件数），
--   来源单据缺失不影响账面数量与金额，只影响「这件货是哪张单进来的」的追责。
--   P2-6b 起四种来源**都有明确目标表**（见 di.v_poly_ref_map ⑤）：
--     PURCHASE / RETURN → biz_stock_in、TRANSFER → biz_stock_transfer、PRODUCE → 无单据表。
--   写入点也补上了：收货过账按 `in_type` 写 `source_type` + `source_id`（= 收货单 id），
--   批量生成走 `serial_model::ensure_source_pair` 拒绝「半个来源」。
--   所以本项现在能查出**真正的悬空**，不再只是「有类型无 id」的残缺报告。
--   分级：悬空（DANGLING）= WARN；有类型无 id（MISSING_ID）= INFO（历史残缺，可人工补）；
--        无表映射却填了 id（NO_TARGET，只剩 PRODUCE）= WARN（指向不明）。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di21 AS
SELECT 'DI-21'::text AS issue_code, 'MATERIAL_SERIAL'::text AS entity_type,
       d.src_id AS entity_id, d.src_no AS entity_no, d.ref_field AS field_name,
       (d.ref_type || ' → ' || coalesce(d.target_table, '(无实体表)'))::text AS snapshot_value,
       d.miss_kind::text AS truth_value,
       d.truth_source,
       'DEFECT'::text AS drift_kind,
       CASE WHEN d.ref_shape = 'MISSING_ID' THEN 'INFO' ELSE 'WARN' END::text AS severity,
       1 AS affected_count
  FROM di.v_poly_ref_dangling d
 WHERE d.ref_domain = 'SERIAL_SOURCE';

COMMENT ON VIEW di.c_di21 IS
  'DI-21 序列号来源悬空/未记录（WARN=悬空或无表映射却有 id / INFO=有类型无 id）：P2-6b 起四种来源都有登记表，能查真悬空（ARCH-3 DC-08 附项）';

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
    UNION ALL
    SELECT 'ATTACHMENT_DOC', doc_type::text, 'biz_doc_attachment', count(*)
      FROM biz_doc_attachment
     WHERE deleted_at IS NULL AND coalesce(trim(doc_type), '') <> ''
     GROUP BY 2
    UNION ALL
    SELECT 'REMINDER_DOC', doc_type::text, 'biz_reminder_log', count(*)
      FROM biz_reminder_log
     WHERE coalesce(trim(doc_type), '') <> ''
     GROUP BY 2
    UNION ALL
    SELECT 'FLOW_CC_SOURCE', source_type::text, 'biz_flow_cc', count(*)
      FROM biz_flow_cc
     WHERE coalesce(trim(source_type), '') <> ''
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
  'DI-22 多态引用登记缺口（INFO）：数据里出现 di.v_poly_ref_map 未登记的类型 ⇒ 该类型的悬空不会被 DI-17…DI-25 查到（护栏自检）';

-- ============================================================
-- DI-23 单据附件指向的单据悬空（WARN）· P2-6b 补口
--   依赖：V0212（biz_doc_attachment）
--   为什么是 WARN 而不是 BLOCKER：附件是**证据的副本**（发票扫描件 / 签收单），
--   账、库存、凭证都不依赖它；但单据软删后附件还在 ⇒ 点「查看附件」跳到 404，
--   归档时也不知道这些文件归属谁（DC-17 的存量形态）。
--   生产当前 0 行（附件功能刚上线），本项上线后**从第一条附件起就有人在看**。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di23 AS
SELECT 'DI-23'::text AS issue_code, 'DOC_ATTACHMENT'::text AS entity_type,
       d.src_id AS entity_id, d.src_no AS entity_no, d.ref_field AS field_name,
       (d.ref_type || ' → ' || coalesce(d.target_table, '(无实体表)'))::text AS snapshot_value,
       d.miss_kind::text AS truth_value,
       d.truth_source,
       'DEFECT'::text AS drift_kind, 'WARN'::text AS severity, 1 AS affected_count
  FROM di.v_poly_ref_dangling d
 WHERE d.ref_domain = 'ATTACHMENT_DOC';

COMMENT ON VIEW di.c_di23 IS
  'DI-23 单据附件指向的单据悬空（WARN）：doc_type 指出的单据已软删/不存在 ⇒ 附件点开 404、归档不知归属（ARCH-3 DC-17 / P2-6b）';

-- ============================================================
-- DI-24 业务提醒台账指向的单据悬空（INFO）· P2-6b 补口
--   依赖：V0205（biz_reminder_log）
--   为什么是 INFO：提醒台账是**只增不改的发送历史**（「这单在 11-01 已经提醒过谁」）。
--   单据被删是正常生命周期，台账必须留着 —— 要求它跟着删才是销毁审计证据
--   （与 DI-20 同一口径）。生产当前 0 行。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di24 AS
SELECT 'DI-24'::text AS issue_code, 'REMINDER_LOG'::text AS entity_type,
       d.src_id AS entity_id, d.src_no AS entity_no, d.ref_field AS field_name,
       (d.ref_type || ' → ' || coalesce(d.target_table, '(无实体表)'))::text AS snapshot_value,
       d.miss_kind::text AS truth_value,
       d.truth_source,
       'DEFECT'::text AS drift_kind, 'INFO'::text AS severity, 1 AS affected_count
  FROM di.v_poly_ref_dangling d
 WHERE d.ref_domain = 'REMINDER_DOC';

COMMENT ON VIEW di.c_di24 IS
  'DI-24 业务提醒台账指向的单据悬空（INFO，只记录不告警）：提醒历史是只增不改的台账，单据被删是正常生命周期（ARCH-3 DC-06/DC-12 / P2-6b）';

-- ============================================================
-- DI-25 流程抄送来源悬空 / 缺来源 id（INFO）· P2-6b 补口
--   依赖：V0156（biz_flow_cc）
--   为什么是 INFO：抄送是**通知侧**行为，审批结论与账都不依赖它；
--   角色/部门被删后历史抄送记录仍然真实（当时确实抄给了那个角色的人）。
--   但「角色来源却没记 source_ref」是写入缺陷（无法解释抄送对象是怎么来的），
--   同一条巡检里报出来，级别一致（都是追溯信息）。
--   生产当前 0 行。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di25 AS
SELECT 'DI-25'::text AS issue_code, 'FLOW_CC'::text AS entity_type,
       d.src_id AS entity_id, d.src_no AS entity_no, d.ref_field AS field_name,
       (d.ref_type || ' → ' || coalesce(d.target_table, '(无实体表)'))::text AS snapshot_value,
       d.miss_kind::text AS truth_value,
       d.truth_source,
       'DEFECT'::text AS drift_kind, 'INFO'::text AS severity, 1 AS affected_count
  FROM di.v_poly_ref_dangling d
 WHERE d.ref_domain = 'FLOW_CC_SOURCE';

COMMENT ON VIEW di.c_di25 IS
  'DI-25 流程抄送来源悬空/缺来源 id（INFO）：source_type 指出的角色/部门已删，或角色来源没记 source_ref（ARCH-3 DC-08 / P2-6b）';

-- ============================================================
-- DI-26 供应商 ID→名字 映射唯一性 / 快照名 vs 现名（WARN / INFO）
--   为什么需要这条（ARCH-3 §5 DC-07 / 卡 P2-7）：
--     同一个供应商在系统里有**三套名字**：`biz_supplier.supplier_name`（全名，主档现值）、
--     `biz_supplier.short_name`（简称，只用于检索/导入）、以及各处**冻结的快照名**
--     （`biz_purchase_order.supplier_name_snapshot` / `biz_contract.party_b_name` /
--     `biz_fixed_asset.supplier_name` / 审批单 `form_data.supplier_name`）。
--     ID 是唯一关联键，但「名字」不是：DI-06 / DI-10 只保证「同一个 id 被抄到了对的地方」，
--     **不保证名字映射唯一**。主档改名后，历史单据的快照名不动（这是对的，审计要
--     「当时是谁」），可是「同一处显示取哪个来源」并没有被任何东西守住 ⇒ 按名字汇总/人工
--     识别就会把两家并成一家、或把一家拆成两家。
--   本项查五类，逐类定级（为什么没有 ERROR）：
--     ① NAME_DUP           两个**活跃**供应商同名（归一 trim+lower）     WARN
--        同名 ⇒ 下拉框里两个选项长得一样、按名字汇总必然合并 ⇒ 选错主体的人为风险。
--        但 id 关联本身没错、账也没错，够不上 BLOCKER（§2.3：钱没动、链没断）。
--     ② SHORT_DUP          两个活跃供应商同简称                          WARN
--        简称只用于检索（model_master 关键字搜索 / 导入），重复会让「搜 A 出 B」。
--     ③ SHORT_SHADOWS_NAME 某家的简称 == 别家的全名                      WARN
--        这是三套名字互相污染的最短路径：单据上出现这个名字时，人分不清是全名还是简称。
--     ④ SNAPSHOT_DRIFT     快照名 ≠ 主档现名（PO / 合同 / 资产 / 审批单）  INFO
--        合法历史（§1.1「不能重算的偏移是历史」）：改名后历史单据**必须**保持旧名。
--        只留痕、不进告警队列；它同时是「按名字汇总会对不上」的实测口径。
--     ⑤ SNAPSHOT_MISSING   采购单有 supplier_id 但快照 NULL               INFO
--        V0198 不回填存量（历史事实），显示只能回落主档现值 ⇒ 这些行的名字会「跟着改名变」。
--     ⑥ SNAPSHOT_LATE_WRITE SPOT 采购单建单**晚于**主档最后一次修改，却写了旧名   WARN
--        可重算的偏移 = 写路径缺陷（§1.1）：建单时主档已经是新名，快照却写旧名 ⇒
--        写入点（purchase_handler::resolve_head）漏了「取主档现值」。
--        CONTRACT 采购单**不在此列**：它的快照名按设计来自合同乙方（签约名），
--        与主档现名不同是合法的（那一段由 DI-01 盯 PO↔合同）。
-- ============================================================
CREATE OR REPLACE VIEW di.c_di26 AS
WITH act AS (
    SELECT s.id, s.supplier_code,
           btrim(s.supplier_name) AS full_name,
           nullif(btrim(s.short_name), '') AS short_name,
           s.updated_at
      FROM biz_supplier s
     WHERE s.deleted_at IS NULL
),
name_dup AS (
    SELECT (array_agg(a.id ORDER BY a.supplier_code))[1] AS entity_id,
           string_agg(a.supplier_code, ' / ' ORDER BY a.supplier_code) AS entity_no,
           min(a.full_name) AS display_name,
           count(*) AS n
      FROM act a
     GROUP BY lower(a.full_name)
    HAVING count(*) > 1
),
short_dup AS (
    SELECT (array_agg(a.id ORDER BY a.supplier_code))[1] AS entity_id,
           string_agg(a.supplier_code, ' / ' ORDER BY a.supplier_code) AS entity_no,
           min(a.short_name) AS display_name,
           count(*) AS n
      FROM act a
     WHERE a.short_name IS NOT NULL
     GROUP BY lower(a.short_name)
    HAVING count(*) > 1
),
short_shadow AS (
    SELECT a.id AS entity_id, a.supplier_code AS entity_no,
           a.short_name AS short_name,
           string_agg(b.supplier_code, ' / ' ORDER BY b.supplier_code) AS clashes_with,
           count(*) AS n
      FROM act a
      JOIN act b ON b.id <> a.id AND lower(b.full_name) = lower(a.short_name)
     WHERE a.short_name IS NOT NULL
     GROUP BY a.id, a.supplier_code, a.short_name
),
snap AS (
    -- ④ 快照名 vs 主档现名：四张快照来源合成一个分支，entity_type 区分来源
    SELECT 'PURCHASE_ORDER'::text AS entity_type, p.id AS entity_id, p.order_no AS entity_no,
           'supplier_name_snapshot'::text AS field_name,
           btrim(p.supplier_name_snapshot) AS snap_name,
           btrim(s.supplier_name) AS cur_name, s.id AS supplier_id,
           p.created_at AS doc_at, s.updated_at AS master_at,
           (p.purchase_type = 'SPOT') AS is_spot
      FROM di.v_po p
      JOIN biz_supplier s ON s.id = p.supplier_id AND s.deleted_at IS NULL
     WHERE p.deleted_at IS NULL
       AND coalesce(btrim(p.supplier_name_snapshot), '') <> ''
    UNION ALL
    SELECT 'CONTRACT', c.id, c.contract_no, 'party_b_name',
           btrim(c.party_b_name), btrim(s.supplier_name), s.id,
           c.created_at, s.updated_at, FALSE
      FROM di.v_contract c
      JOIN biz_supplier s ON s.id = c.party_b_id AND s.deleted_at IS NULL
     WHERE c.deleted_at IS NULL
       AND coalesce(btrim(c.party_b_name), '') <> ''
    UNION ALL
    SELECT 'FIXED_ASSET', a.id, a.asset_no, 'supplier_name',
           btrim(a.supplier_name), btrim(s.supplier_name), s.id,
           a.created_at, s.updated_at, FALSE
      FROM di.v_asset a
      JOIN biz_supplier s ON s.id = a.supplier_id AND s.deleted_at IS NULL
     WHERE a.deleted_at IS NULL
       AND coalesce(btrim(a.supplier_name), '') <> ''
    UNION ALL
    SELECT 'FLOW_INSTANCE', f.id, f.biz_no, 'form_data.supplier_name',
           btrim(f.form_data ->> 'supplier_name'), btrim(s.supplier_name), s.id,
           f.started_at, s.updated_at, FALSE
      FROM biz_flow_instance f
      JOIN biz_supplier s ON s.id::text = btrim(f.form_data ->> 'supplier_id')
                          AND s.deleted_at IS NULL
     WHERE f.deleted_at IS NULL
       AND coalesce(btrim(f.form_data ->> 'supplier_name'), '') <> ''
),
snap_diff AS (
    SELECT * FROM snap WHERE lower(snap_name) <> lower(cur_name)
),
snap_late AS (
    -- 晚写：SPOT 采购单建单**晚于**主档最后一次修改，却写了旧名 ⇒ 写路径漏取现值（可重算的偏移）
    SELECT * FROM snap_diff WHERE is_spot AND doc_at > master_at
),
snap_drift AS (
    -- 合法历史：快照名与现名不同但**不是**晚写（晚写的行只报一次，不重复计数）
    SELECT d.* FROM snap_diff d
     WHERE NOT EXISTS (SELECT 1 FROM snap_late l
                        WHERE l.entity_type = d.entity_type AND l.entity_id = d.entity_id)
),
snap_missing AS (
    SELECT p.id AS entity_id, p.order_no AS entity_no, s.id AS supplier_id,
           btrim(s.supplier_name) AS cur_name
      FROM di.v_po p
      JOIN biz_supplier s ON s.id = p.supplier_id AND s.deleted_at IS NULL
     WHERE p.deleted_at IS NULL
       AND p.supplier_id IS NOT NULL
       AND coalesce(btrim(p.supplier_name_snapshot), '') = ''
       -- 快照列本身就是 V0198 才加的：列不存在时 di.v_po 全返回 NULL，
       -- 那种阶段「快照缺失」不是缺陷而是「还没这列」，不能报（0 行 ≠ 无缺陷的反面）
       AND (SELECT d.has_po_contract_id FROM di.di_capability d)
)
SELECT 'DI-26'::text AS issue_code, 'SUPPLIER'::text AS entity_type,
       d.entity_id, d.entity_no, 'supplier_name'::text AS field_name,
       d.display_name AS snapshot_value,
       '活跃供应商里这个名字只能出现一次'::text AS truth_value,
       'biz_supplier.supplier_name 归一 lower(btrim()) + deleted_at IS NULL'::text AS truth_source,
       'DEFECT'::text AS drift_kind, 'WARN'::text AS severity, d.n AS affected_count
  FROM name_dup d
UNION ALL
SELECT 'DI-26', 'SUPPLIER', d.entity_id, d.entity_no, 'short_name',
       d.display_name, '活跃供应商里这个简称只能出现一次',
       'biz_supplier.short_name 归一 lower(btrim()) + deleted_at IS NULL',
       'DEFECT', 'WARN', d.n
  FROM short_dup d
UNION ALL
SELECT 'DI-26', 'SUPPLIER', d.entity_id, d.entity_no, 'short_name',
       ('简称「' || d.short_name || '」= 供应商 ' || d.clashes_with || ' 的全名'),
       '简称不得与任何其它活跃供应商的全名相同',
       'biz_supplier.short_name vs biz_supplier.supplier_name（归一 lower(btrim())）',
       'DEFECT', 'WARN', d.n
  FROM short_shadow d
UNION ALL
SELECT 'DI-26', d.entity_type, d.entity_id, d.entity_no, d.field_name,
       d.snap_name,
       d.cur_name,
       ('biz_supplier.supplier_name (id=' || d.supplier_id::text || ')'),
       'SNAPSHOT', 'INFO', 1
  FROM snap_drift d
UNION ALL
SELECT 'DI-26', p.entity_type, p.entity_id, p.entity_no, p.field_name,
       p.snap_name, p.cur_name,
       ('biz_supplier.supplier_name (id=' || p.supplier_id::text || ') 主档最后修改='
        || to_char(p.master_at, 'YYYY-MM-DD HH24:MI:SS') || '，本单创建='
        || to_char(p.doc_at, 'YYYY-MM-DD HH24:MI:SS')),
       'DEFECT', 'WARN', 1
  FROM snap_late p
UNION ALL
SELECT 'DI-26', 'PURCHASE_ORDER', m.entity_id, m.entity_no, 'supplier_name_snapshot',
       '(快照 NULL：存量行 V0198 未回填)', m.cur_name,
       ('biz_supplier.supplier_name (id=' || m.supplier_id::text || ')'),
       'SNAPSHOT', 'INFO', 1
  FROM snap_missing m;

COMMENT ON VIEW di.c_di26 IS
  'DI-26 供应商 ID→名字映射唯一性（WARN=同名/同简称/简称撞他人全名/建单晚于改名却写旧名；INFO=快照名≠现名、存量快照 NULL）：关联键只认 id，名字用于显示，快照漂移只报不修（ARCH-3 DC-07 / 卡 P2-7）';

COMMIT;
