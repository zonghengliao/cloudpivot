-- ============================================================
-- 快照/勾稽一致性巡检（只读；不建表）
--
-- 为什么不建 biz_snapshot_drift：落库的巡检结果表归「快照巡检机制」卡（t_c7610b31），
-- 本卡只给**判据 SQL**，避免两卡各建一张 drift 表造出「第二真相」。
-- 实现卡把下面 5 段挂到任务调度（每日 02:00），命中即告警。
--
-- 三类结论的语义（写死在这里，别在代码里各说各话）：
--   DEFECT   ：可以由上游**无歧义重算**却对不上（入库单 → 采购单 → 合同）→ 必须修数据
--   SNAPSHOT ：上游事后被合法修改（合同换乙方 / 供应商改名）→ 只告警、可 ACCEPTED，不回写历史
--   GAUGE    ：口径统计（无来源入库占比、合同/零星分布）→ 不是错误，是经营口径
-- ============================================================

-- ---------- ① DEFECT：入库单供应商 ≠ 其采购单供应商 ----------
-- 真相：入库单.supplier_id 是「从采购单带出」的拷贝，同一时刻必然可重算 → 不等即缺陷。
-- 期望：0 行。上线后新写的单据由应用层 400 拦住，这里兜的是「直接改库 / 历史数据」。
SELECT 'STOCK_IN_SUPPLIER_DRIFT' AS check_name, 'DEFECT' AS kind,
       s.in_no AS doc_no, s.id AS doc_id,
       s.supplier_id AS snapshot_value, o.supplier_id AS truth_value,
       'biz_purchase_order.supplier_id' AS truth_source
  FROM biz_stock_in s
  JOIN biz_purchase_order o ON o.id = s.source_order_id AND o.deleted_at IS NULL
 WHERE s.deleted_at IS NULL
   AND s.source_order_id IS NOT NULL
   AND s.supplier_id IS DISTINCT FROM o.supplier_id
 ORDER BY s.in_date DESC;

-- ---------- ② DEFECT：入库单的来源采购单不存在 / 已软删 ----------
SELECT 'STOCK_IN_DANGLING_SOURCE' AS check_name, 'DEFECT' AS kind,
       s.in_no AS doc_no, s.id AS doc_id,
       s.source_order_id::text AS snapshot_value, NULL AS truth_value,
       'biz_purchase_order(id)' AS truth_source
  FROM biz_stock_in s
  LEFT JOIN biz_purchase_order o ON o.id = s.source_order_id
 WHERE s.deleted_at IS NULL
   AND s.source_order_id IS NOT NULL
   AND (o.id IS NULL OR o.deleted_at IS NOT NULL)
 ORDER BY s.in_date DESC;

-- ---------- ③ SNAPSHOT / DEFECT：采购单供应商 ≠ 合同乙方 ----------
-- 定性规则（可证伪，不靠人嘴）：
--   · 合同 updated_at <= 采购单 created_at  → 建单时就不一致 = DEFECT（带错了）
--   · 合同 updated_at >  采购单 created_at  → 合同事后换过乙方 = SNAPSHOT（历史保持原样）
SELECT CASE WHEN c.updated_at > o.created_at THEN 'SNAPSHOT' ELSE 'DEFECT' END AS kind,
       'PURCHASE_ORDER_SUPPLIER_DRIFT' AS check_name,
       o.order_no AS doc_no, o.id AS doc_id,
       o.supplier_id AS snapshot_value, c.party_b_id AS truth_value,
       'biz_contract.party_b_id' AS truth_source,
       o.created_at AS order_created_at, c.updated_at AS contract_updated_at
  FROM biz_purchase_order o
  JOIN biz_contract c ON c.id = o.contract_id AND c.deleted_at IS NULL
 WHERE o.deleted_at IS NULL
   AND o.supplier_id IS DISTINCT FROM c.party_b_id
 ORDER BY o.order_date DESC;

-- ---------- ④ DEFECT：采购单的合同引用悬空（合同被软删/不存在） ----------
SELECT 'PURCHASE_ORDER_DANGLING_CONTRACT' AS check_name, 'DEFECT' AS kind,
       o.order_no AS doc_no, o.id AS doc_id,
       o.contract_id::text AS snapshot_value, NULL AS truth_value,
       'biz_contract(id)' AS truth_source
  FROM biz_purchase_order o
  LEFT JOIN biz_contract c ON c.id = o.contract_id
 WHERE o.deleted_at IS NULL
   AND o.contract_id IS NOT NULL
   AND (c.id IS NULL OR c.deleted_at IS NOT NULL)
 ORDER BY o.order_date DESC;

-- ---------- ⑤ GAUGE：口径统计（不是错误；给运营/财务看规模） ----------
SELECT
    (SELECT count(*) FROM biz_purchase_order WHERE deleted_at IS NULL)                       AS po_total,
    (SELECT count(*) FROM biz_purchase_order WHERE deleted_at IS NULL AND purchase_type = 'CONTRACT') AS po_contract,
    (SELECT count(*) FROM biz_purchase_order WHERE deleted_at IS NULL AND purchase_type = 'SPOT')     AS po_spot,
    (SELECT count(*) FROM biz_stock_in WHERE deleted_at IS NULL)                            AS si_total,
    (SELECT count(*) FROM biz_stock_in WHERE deleted_at IS NULL AND source_order_id IS NULL) AS si_no_source,
    (SELECT count(*) FROM biz_stock_in WHERE deleted_at IS NULL AND in_type <> 'PURCHASE')    AS si_non_purchase,
    (SELECT count(*) FROM biz_stock_in WHERE deleted_at IS NULL AND in_type = 'PURCHASE' AND source_order_id IS NULL) AS si_purchase_no_source;
