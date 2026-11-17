-- ============================================================================
-- V0453（down）：把单头视图恢复成 V0452 的定义
--
-- 回滚 = 回到老行为，**包括老 bug**：
--   V0452 的写法在「协议一行明细都没有」时会 `0 / 0` → `division by zero [22012]`，
--   整个 `GET /business/erp/framework-agreement/execution/page`（不带筛选）直接 500。
--   回滚文件不修这个，否则「回滚后与回滚前一样」就是假的 —— 要修就重新应用 V0453。
--
-- 幂等：`CREATE OR REPLACE VIEW`，列名 / 列序与 up 完全一致，重跑只换定义。
-- 本文件内容**逐字来自 V0452**（`V0452__framework_agreement_execution.up.sql` 的单头视图段），
-- 不手抄，避免回滚线漂移。
-- ============================================================================

CREATE OR REPLACE VIEW v_framework_agreement_execution AS
WITH item_agg AS (
    SELECT e.agreement_id                                     AS agreement_id,
           count(*)                                           AS item_count,
           count(*) FILTER (WHERE e.exec_state = 'UNLIMITED')  AS unlimited_items,
           count(*) FILTER (WHERE e.exec_state = 'OVER')       AS over_items,
           count(*) FILTER (WHERE e.exec_state = 'DONE')       AS done_items,
           count(*) FILTER (WHERE e.exec_state = 'PARTIAL')    AS partial_items,
           count(*) FILTER (WHERE e.exec_state = 'NOT_STARTED') AS not_started_items,
           COALESCE(SUM(e.executed_qty), 0)                    AS executed_qty,
           COALESCE(SUM(e.executed_amount), 0)                 AS executed_amount,
           COALESCE(SUM(e.executed_amount_with_tax), 0)        AS executed_amount_with_tax,
           COALESCE(SUM(e.executed_amount_capped), 0)          AS executed_amount_capped,
           COALESCE(SUM(e.booked_qty), 0)                      AS booked_qty,
           SUM(e.max_qty) FILTER (WHERE e.max_qty IS NOT NULL)  AS item_max_qty_sum_raw
      FROM v_framework_agreement_item_execution e
     GROUP BY e.agreement_id
),
po_agg AS (
    SELECT o.framework_agreement_id AS agreement_id,
           count(DISTINCT o.id) FILTER (
               WHERE o.deleted_at IS NULL AND o.status <> 'CANCELLED')          AS order_doc_count,
           count(DISTINCT o.id) FILTER (
               WHERE o.deleted_at IS NOT NULL OR o.status = 'CANCELLED')        AS released_doc_count,
           max(o.order_date) FILTER (
               WHERE o.deleted_at IS NULL AND o.status <> 'CANCELLED')          AS last_order_on
      FROM biz_purchase_order o
     WHERE o.framework_agreement_id IS NOT NULL
     GROUP BY o.framework_agreement_id
),
base AS (
    SELECT a.id                                             AS agreement_id,
           a.agreement_no                                   AS agreement_no,
           a.agreement_name                                 AS agreement_name,
           a.supplier_id                                    AS supplier_id,
           a.supplier_name                                  AS supplier_name,
           COALESCE(s.supplier_name, a.supplier_name, '')   AS supplier_display_name,
           a.dept_id                                        AS dept_id,
           a.dept_name                                      AS dept_name,
           a.owner_id                                       AS owner_id,
           a.owner_name                                     AS owner_name,
           a.start_date                                     AS start_date,
           a.end_date                                       AS end_date,
           a.sign_date                                      AS sign_date,
           a.status                                         AS status,
           a.price_mode                                     AS price_mode,
           a.price_includes_tax                             AS price_includes_tax,
           a.price_currency                                 AS price_currency,
           a.price_tolerance                                AS price_tolerance,
           a.total_max_qty                                  AS total_max_qty,
           a.total_max_amount                               AS total_max_amount,
           a.total_executed_qty                             AS booked_total_qty,
           a.total_executed_amount                          AS booked_total_amount,
           COALESCE(i.item_count, 0)                        AS item_count,
           COALESCE(i.unlimited_items, 0)                   AS unlimited_items,
           COALESCE(i.over_items, 0)                        AS over_items,
           COALESCE(i.done_items, 0)                        AS done_items,
           COALESCE(i.partial_items, 0)                     AS partial_items,
           COALESCE(i.not_started_items, 0)                 AS not_started_items,
           COALESCE(i.executed_qty, 0)                      AS executed_qty,
           COALESCE(i.executed_amount, 0)                   AS executed_amount,
           COALESCE(i.executed_amount_with_tax, 0)          AS executed_amount_with_tax,
           COALESCE(i.executed_amount_capped, 0)            AS executed_amount_capped,
           COALESCE(i.booked_qty, 0)                        AS booked_qty,
           -- 明细上限合计：只有「明细全部设了上限」时才是有效封顶（有一行不限就整体不封顶）
           CASE WHEN COALESCE(i.unlimited_items, 0) = 0
                THEN COALESCE(i.item_max_qty_sum_raw, 0) END AS item_max_qty_sum,
           COALESCE(p.order_doc_count, 0)                   AS order_doc_count,
           COALESCE(p.released_doc_count, 0)                AS released_doc_count,
           p.last_order_on                                  AS last_order_on,
           a.created_at                                     AS created_at,
           (a.end_date - CURRENT_DATE)                      AS days_left,
           -- 有效期判据：与 framework_agreement_model::is_effective_on 逐字一致
           (a.status = 'APPROVED'
            AND CURRENT_DATE >= a.start_date
            AND CURRENT_DATE <= a.end_date)                 AS is_effective,
           CASE
               WHEN a.status IN ('DRAFT', 'SUBMITTED') THEN a.status
               WHEN CURRENT_DATE < a.start_date        THEN 'NOT_STARTED'
               WHEN CURRENT_DATE > a.end_date          THEN 'EXPIRED'
               ELSE 'ACTIVE'
           END                                              AS validity_state,
           -- 有效数量封顶：先单头，再（全有上限时）明细合计
           COALESCE(a.total_max_qty,
                    CASE WHEN COALESCE(i.unlimited_items, 0) = 0
                         THEN COALESCE(i.item_max_qty_sum_raw, 0) END) AS cap_qty,
           a.total_max_amount                               AS cap_amount
      FROM biz_framework_agreement a
      LEFT JOIN biz_supplier s ON s.id = a.supplier_id AND s.deleted_at IS NULL
      LEFT JOIN item_agg i ON i.agreement_id = a.id
      LEFT JOIN po_agg   p ON p.agreement_id = a.id
     WHERE a.deleted_at IS NULL
)
SELECT b.agreement_id,
       b.agreement_no,
       b.agreement_name,
       b.supplier_id,
       b.supplier_name,
       b.supplier_display_name,
       b.dept_id,
       b.dept_name,
       b.owner_id,
       b.owner_name,
       b.start_date,
       b.end_date,
       b.sign_date,
       b.status,
       b.price_mode,
       b.price_includes_tax,
       b.price_currency,
       b.price_tolerance,
       b.total_max_qty,
       b.total_max_amount,
       b.cap_qty,
       b.cap_amount,
       b.item_max_qty_sum,
       b.item_count,
       b.unlimited_items,
       b.over_items,
       b.done_items,
       b.partial_items,
       b.not_started_items,
       b.executed_qty,
       b.executed_amount,
       b.executed_amount_with_tax,
       b.executed_amount_capped,
       b.booked_qty,
       b.booked_total_qty,
       b.booked_total_amount,
       b.cap_qty    - b.executed_qty               AS remaining_qty,
       b.cap_amount - b.executed_amount_capped     AS remaining_amount,
       round(b.executed_qty / b.cap_qty, 6)                 AS exec_qty_rate,
       round(b.executed_amount_capped / b.cap_amount, 6)    AS exec_amount_rate,
       b.order_doc_count,
       b.released_doc_count,
       b.last_order_on,
       b.days_left,
       b.is_effective,
       b.validity_state,
       CASE
           WHEN b.cap_qty IS NULL AND b.cap_amount IS NULL         THEN 'UNLIMITED'
           WHEN b.cap_qty IS NOT NULL THEN
               CASE
                   WHEN b.executed_qty >  b.cap_qty THEN 'OVER'
                   WHEN b.executed_qty >= b.cap_qty THEN 'DONE'
                   WHEN b.executed_qty >  0         THEN 'PARTIAL'
                   ELSE 'NOT_STARTED'
               END
           ELSE
               CASE
                   WHEN b.executed_amount_capped >  b.cap_amount THEN 'OVER'
                   WHEN b.executed_amount_capped >= b.cap_amount THEN 'DONE'
                   WHEN b.executed_amount_capped >  0            THEN 'PARTIAL'
                   ELSE 'NOT_STARTED'
               END
       END                                              AS exec_state,
       b.created_at
  FROM base b;

COMMENT ON VIEW v_framework_agreement_execution IS
    '框架协议执行量总览（M4-09）：一张存活协议一行；executed_* 是物料行视图的合计（按单据链现算），remaining_* = 封顶 − 已用（可为负），cap_qty = 单头 total_max_qty，没写则用「明细全部设了上限」时的合计；exec_state ∈ UNLIMITED / NOT_STARTED / PARTIAL / DONE / OVER；validity_state 是有效期的人话翻译（不是新状态列）';
COMMENT ON COLUMN v_framework_agreement_execution.cap_qty IS
    '有效数量封顶：total_max_qty，没写则取 item_max_qty_sum（明细全部设了上限时）；两者都没有 = NULL（不限）';
COMMENT ON COLUMN v_framework_agreement_execution.remaining_qty IS
    '剩余可下单量 = cap_qty − executed_qty；**可为负**（超上限）；cap_qty 为 NULL（不限）时为 NULL，不是 0';
COMMENT ON COLUMN v_framework_agreement_execution.order_doc_count IS
    '已经吃了额度的采购单单数（存活且未作废）；released_doc_count 是作废/软删的（已释放，仅供解释「已用为什么没涨」）';
COMMENT ON COLUMN v_framework_agreement_execution.validity_state IS
    '有效期状态：DRAFT / SUBMITTED（不生效）/ NOT_STARTED（未到生效日）/ ACTIVE（生效中）/ EXPIRED（已过 end_date）—— 由 status + 起止日期现算，不落库';

-- 回滚后自检：视图在位（列集合与 V0452 一致）
DO $$
BEGIN
    IF to_regclass('v_framework_agreement_execution') IS NULL THEN
        RAISE EXCEPTION 'V0453 down 自检失败：视图 v_framework_agreement_execution 不存在';
    END IF;
END $$;
