-- ============================================================================
-- V0453：修 V0452 单头视图的**除零 500** —— 协议没有物料行时（item_count = 0）
--
-- ── 什么时候发现 ────────────────────────────────────────────────────────────
--   M4-11（框架协议前端页面）的真栈演练：造了一份「APPROVED + 生效中 + 一行明细都没有」
--   的协议，`GET /business/erp/framework-agreement/execution/page`（不带任何筛选）当场
--   500：`division by zero [22012]`（后端日志 `查询框架协议执行量总览`）。
--   带 `over_only` / `validity_state` / `keyword` 的查询反而正常 —— 因为 WHERE 恰好把
--   那行排掉了。也就是说：**只要库里存在一份空物料范围的协议，整个执行量报表就 500**，
--   而且不带筛选才触发，最容易被「带关键字测过」骗过去。
--
-- ── 根因（V0452 的两处写法在「零行」这个边界上不成立）───────────────────────
--   `item_agg` 是 `GROUP BY agreement_id` 的聚合：协议没有任何物料行时**根本没有这一组**，
--   于是 `i.item_max_qty_sum_raw` 是 NULL，`COALESCE(..., 0)` 把「没有明细」变成了 **0**：
--
--     item_max_qty_sum = 0        -- 「明细上限合计 = 0」← 假的
--     cap_qty          = 0        -- 于是一份什么都没有的协议「封顶 0」
--     exec_qty_rate    = 0 / 0    -- ← 22012 division by zero（整个 SELECT 直接报错）
--
--   即使不报错，语义也是错的：`executed_qty(0) >= cap_qty(0)` ⇒ `DONE`（已用满），
--   一份从没被用过的空协议在报表上显示「已用满」。
--
-- ── 怎么修（与后端纯函数对齐，不是就地打补丁）───────────────────────────────
--   ① `item_max_qty_sum` / `cap_qty` 的「明细合计」分支加 `item_count > 0` 前置条件 ——
--      这正是 `framework_agreement_execution_dto::summary_of` 的判据：
--      `cap_qty = if unlimited_items == 0 && item_count > 0 { Some(max_sum) } else { None }`。
--      视图与参考实现从此逐字一致（真库用例本来就在拿 `summary_of` 交叉校验视图合计，
--      两边在「零行」这一格上原来是对不上的）。
--   ② 两个比率一律走 `NULLIF(…, 0)`：分母为 0 或 NULL 都返回 NULL（= 不限 / 无分母），
--      与 `dto::rate_of` 的「没有分母就没有比率」同口径。
--      金额封顶由 `ck_biz_fwa_caps` 保证 > 0，但视图**不该**依赖上游约束来避免 500。
--
--   修完的效果（空物料范围的协议）：`cap_qty = NULL`、`remaining_qty = NULL`、
--   `exec_qty_rate = NULL`、`exec_state = 'UNLIMITED'` —— 与纯函数一致；
--   页面侧「无物料范围」由 `item_count = 0` 这列显形（不要把它读成「不限量」）。
--
-- ── 幂等与回滚 ──────────────────────────────────────────────────────────────
--   `CREATE OR REPLACE VIEW`（重跑只换定义，列名 / 列序与 V0452 完全一致）；
--   末尾 DO 块自检：视图在位 + **探针逐格验算**（造一份零明细协议 → 断言四个值 →
--   删掉，不留残留行）。回滚见 V0453__..._view_fix.down.sql：把单头视图恢复成 V0452 的
--   定义（即恢复「零明细协议会 500」的老行为 —— 回滚就该回到老行为，不假装修过）。
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
           -- 明细上限合计：只有「有明细且明细全部设了上限」时才有值
           --   ⚠ V0453：`item_count > 0` 不能省 —— 零明细时聚合列是 NULL，
           --     少这个条件会把「没有明细」变成「上限合计 0」（进而 cap_qty = 0 → 除零 500）。
           --     判据与 `framework_agreement_execution_dto::summary_of` 逐字一致。
           CASE WHEN COALESCE(i.item_count, 0) > 0
                     AND COALESCE(i.unlimited_items, 0) = 0
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
           -- 有效数量封顶：先单头，再（有明细且全有上限时）明细合计
           COALESCE(a.total_max_qty,
                    CASE WHEN COALESCE(i.item_count, 0) > 0
                              AND COALESCE(i.unlimited_items, 0) = 0
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
       -- ⚠ V0453：NULLIF 兜底 —— 分母 0 / NULL 都给 NULL（= 无分母），不许再抛 22012
       round(b.executed_qty / NULLIF(b.cap_qty, 0), 6)               AS exec_qty_rate,
       round(b.executed_amount_capped / NULLIF(b.cap_amount, 0), 6)  AS exec_amount_rate,
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
    '框架协议执行量总览（M4-09，V0453 修除零）：一张存活协议一行；executed_* 是物料行视图的合计（按单据链现算），remaining_* = 封顶 − 已用（可为负），cap_qty = 单头 total_max_qty，没写则用「有明细且明细全部设了上限」时的合计（零明细 = NULL）；exec_state ∈ UNLIMITED / NOT_STARTED / PARTIAL / DONE / OVER；validity_state 是有效期的人话翻译（不是新状态列）';
COMMENT ON COLUMN v_framework_agreement_execution.cap_qty IS
    '有效数量封顶：total_max_qty，没写则取 item_max_qty_sum（有明细且全部设了上限时）；都没有 = NULL（不限）—— 零明细协议也是 NULL，不是 0（0 会导致除零与「已用满」误读）';
COMMENT ON COLUMN v_framework_agreement_execution.exec_qty_rate IS
    '数量执行率 = executed_qty / cap_qty（6 位）；cap_qty 为 NULL 或 0 时为 NULL（没有分母就没有比率），**不**抛除零错';
COMMENT ON COLUMN v_framework_agreement_execution.exec_amount_rate IS
    '金额执行率 = executed_amount_capped / cap_amount（6 位）；cap_amount 为 NULL 或 0 时为 NULL';

-- ---------- 自检：视图在位 + 零明细协议的逐格验算（先造 → 断言 → 再清） ----------
DO $$
DECLARE
    v_ag    uuid := gen_random_uuid();
    v_cap   numeric;
    v_rate  numeric;
    v_state text;
    v_rem   numeric;
BEGIN
    IF to_regclass('v_framework_agreement_execution') IS NULL THEN
        RAISE EXCEPTION 'V0453 自检失败：视图 v_framework_agreement_execution 不存在';
    END IF;

    -- 探针：一份 APPROVED / 生效中 / **一行明细都没有**的协议
    INSERT INTO biz_framework_agreement
        (id, agreement_no, agreement_name, supplier_id, start_date, end_date, status)
    VALUES
        (v_ag, 'FWA-19990101-995301', 'V0453 探针（无物料范围）', gen_random_uuid(),
         DATE '2020-01-01', DATE '2099-12-31', 'APPROVED');

    SELECT cap_qty, exec_qty_rate, exec_state, remaining_qty
      INTO v_cap, v_rate, v_state, v_rem
      FROM v_framework_agreement_execution WHERE agreement_id = v_ag;

    IF v_cap IS NOT NULL THEN
        RAISE EXCEPTION 'V0453 自检失败：零明细协议的 cap_qty 应为 NULL，实际 %', v_cap;
    END IF;
    IF v_rate IS NOT NULL THEN
        RAISE EXCEPTION 'V0453 自检失败：零明细协议的 exec_qty_rate 应为 NULL，实际 %', v_rate;
    END IF;
    IF v_rem IS NOT NULL THEN
        RAISE EXCEPTION 'V0453 自检失败：零明细协议的 remaining_qty 应为 NULL，实际 %', v_rem;
    END IF;
    IF v_state <> 'UNLIMITED' THEN
        RAISE EXCEPTION 'V0453 自检失败：零明细协议的 exec_state 应为 UNLIMITED，实际 %', v_state;
    END IF;

    DELETE FROM biz_framework_agreement WHERE id = v_ag;
END $$;
