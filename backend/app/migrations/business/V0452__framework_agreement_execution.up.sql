-- ============================================================================
-- V0452：框架协议「执行量统计」—— 已用 / 剩余（采购链 M4-09）
--
--   ① 视图 `v_framework_agreement_item_execution`  物料行级：一行 = 协议的一格（物料）
--   ② 视图 `v_framework_agreement_execution`       单头级：一张存活协议一行（物料行合计 + 行状态计数）
--   ③ 菜单 2 行：页面 `business:erp:framework-agreement:page`（`…004480`，menu_type=2）
--                + 按钮 `business:erp:framework-agreement:execution`（`…004481`，menu_type=3）
--   ④ 角色授权：采购经理（编制/控盘）= 财务经理（复核口径）
--
-- ── 号段核对（动手前 `ls backend/app/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail -3`）──
--   写入本文件时全局最大号 = V0451（框架协议订单引用，M4-08）⇒ 本卡顺延取 **V0452**。
--   sys_menu id：V0447 占用 …4471–…4476、V0448 占用 …4477、V0449 占用 …4478、
--   V0450 占用 …4479（biz_seq 行）⇒ 本迁移取 …4480 / …4481，无交叠。
--   biz_seq 一行都不碰（执行量统计不需要取号）。
--
-- ── 为什么是「视图」而不是「读回写列 executed_qty」───────────────────────────
--   1) 回写列（`biz_framework_agreement_item.executed_qty/executed_amount`）是 M4-08
--      在下单 / 作废 / 删除时**按单据链重算**出来的快照。它只在「有人动过单」的那一刻对，
--      任何人绕过 handler 改库（数据修复、导入、并发失败回滚残留）都会让它与单据链分叉。
--      报表拿它当真相 = 又多一份要对账的账；视图现算 = 永远自愈。
--   2) 本卡同时把**回写列并排带出**（`booked_qty` / `booked_amount`）：
--      两者不一致就是「回写与单据链分叉」，报表上直接看得见，不需要另开对账脚本
--      （与 V0449 的 `booked_qty` / `req_qty` 并排同款）。
--   3) 准入判据（M4-08 `framework_agreement_ref::resolve`）也现算，不读回写列 ——
--      报表与准入**读同一份真相**，否则「报表说还剩 10、下单却被拒」这类争议无从解释。
--
-- ── 口径（逐条写死；后端 dto 的纯函数与这里逐字一致）─────────────────────────
--   · 计入的采购单：`deleted_at IS NULL AND status <> 'CANCELLED'`
--     —— 草稿（DRAFT）**算**（单已生成、量已占住，与合同额度守卫 committed_amount 同口径）；
--        `SUBTRACTED` / `CLOSED` 算（关闭只是不再收货，量已经下出去了）；
--        作废 / 软删**不算**（链已断），它们只进 `released_doc_count` 供解释
--        「为什么已用量没涨」。
--   · 已用量   `executed_qty` = Σ 采购明细 qty（引用该协议行）
--   · 已用金额 `executed_amount` = Σ 采购明细 amount（**不含税**，恒可算）
--              `executed_amount_with_tax` = Σ with_tax_amount（**含税**）
--              `executed_amount_capped` = 按协议价口径二选一
--                —— 协议价含税（`price_includes_tax`）比含税、否则比不含税；
--                   与 M4-08 `amount_for_cap` 同一口径（金额上限比较时不许换口径）
--   · 单价上限 `cap_amount`（行级）= `max_qty × agreed_price`
--     —— 协议价含税时它天然是含税口径，与 `executed_amount_capped` 一致
--   · 剩余     `remaining_qty` = `max_qty − executed_qty`（**可为负**：超上限执行）
--              `max_qty IS NULL`（不限）⇒ 剩余为 NULL，**不是** 0
--              （0 会被读成「一点都不剩」，与「不限」正好相反）
--   · 执行率   `exec_qty_rate` = executed_qty / max_qty（6 位小数）；分母 NULL ⇒ NULL
--   · 行状态   UNLIMITED（max_qty 未设）/ NOT_STARTED（已用 0）/ PARTIAL（0 < 已用 < 上限）/
--              DONE（恰好用满）/ OVER（超上限，率 > 100%）
--     —— V0450 头注释把「超上限」的口子刻意留给报表（库层不拦、M4-08 拒绝新单）；
--        报表必须让 OVER 露头，不许把负数剩余截成 0。
--   · 单头封顶 先看 `total_max_qty`；没写就看「明细行上限合计」，但**只有明细全部设了上限**
--     （`unlimited_items = 0`）时它才是有效封顶 —— 有一行不限，合计就不封顶。
--     金额封顶只认 `total_max_amount`（明细没有金额上限字段）。
--   · 有效期   与 `framework_agreement_model::is_effective_on` **逐字一致**：
--              `status = 'APPROVED' AND start_date <= 当日 <= end_date`；
--              `validity_state` 只是把它翻译成人话（DRAFT / SUBMITTED / NOT_STARTED /
--              ACTIVE / EXPIRED），**不是**新增的状态列（V0450 头注释 1：到期是时间的函数）。
--
-- ── 数据权限 ────────────────────────────────────────────────────────────────
--   SCOPE-EXEMPT：与 V0450 头注释 8 同口径 —— 框架协议是公司级采购文件（采购要看全公司、
--   财务要复核），`dept_id` 是「经办部门」快照语义，不是数据范围列。可见性由权限码
--   `business:erp:framework-agreement:execution` 控制，总览走 `scope::paginate_unscoped`。
--
-- ── 菜单为什么本卡就落（页面属 M4-11）────────────────────────────────────────
--   按钮权限点（menu_type=3）**必须挂在页面行下**（menu_invariants ①：孤儿 parent 报红），
--   而框架协议页的页面行至今没有任何迁移建过。于是本卡把页面行一并落下（与 V0447 在
--   M4-02 就落「采购计划」页面行、前端 M4-06 才补 Vue 页同款），**Vue 页面文件属 M4-11**，
--   本卡一行前端都不写。页面码 `…:page` 只给前端路由判显形，后端零引用（约定 14）。
--
-- ── 幂等与回滚 ──────────────────────────────────────────────────────────────
--   `CREATE OR REPLACE VIEW`（重跑只换定义）/ 菜单 `ON CONFLICT (id) DO UPDATE` /
--   授权 `ON CONFLICT DO NOTHING`；末尾 DO 块自检：两个视图在位 + 关键列名齐全 +
--   **探针数据的逐格验算**（先造 → 先断言 → 再清，不留残留行）。
--   回滚见 V0452__framework_agreement_execution.down.sql：先删授权、再删菜单、最后删两个视图。
-- ============================================================================

-- ---------- ① 物料行级：一行 = 协议的一格（物料） ----------
CREATE OR REPLACE VIEW v_framework_agreement_item_execution AS
SELECT
    it.id                                   AS agreement_item_id,
    it.agreement_id                         AS agreement_id,
    a.agreement_no                          AS agreement_no,
    it.line_no                              AS line_no,
    it.material_id                          AS material_id,
    COALESCE(m.material_code, '')           AS material_code,
    COALESCE(m.material_name, '')           AS material_name,
    it.unit_id                              AS unit_id,
    COALESCE(u.unit_name, '')               AS unit_name,
    it.agreed_price                         AS agreed_price,
    it.tax_rate                             AS tax_rate,
    a.price_includes_tax                    AS price_includes_tax,
    a.price_currency                        AS price_currency,
    it.max_qty                              AS max_qty,
    -- 已用量：按单据链现算（视图的唯一真相）
    COALESCE(usg.qty, 0)                    AS executed_qty,
    -- 回写列快照（M4-08 重算出来的），与上面的 executed_qty 并排供对账
    it.executed_qty                         AS booked_qty,
    COALESCE(usg.amount, 0)                 AS executed_amount,
    COALESCE(usg.with_tax, 0)               AS executed_amount_with_tax,
    -- 按协议价口径二选一（含税协议比含税、不含税协议比不含税）—— 与 M4-08 amount_for_cap 同源
    CASE WHEN a.price_includes_tax
         THEN COALESCE(usg.with_tax, 0)
         ELSE COALESCE(usg.amount, 0) END   AS executed_amount_capped,
    -- 剩余（NULL = 不限；可为负 = 超上限执行）
    it.max_qty - COALESCE(usg.qty, 0)       AS remaining_qty,
    -- 行金额上限 = 数量上限 × 协议价（协议价含税时它就是含税口径）
    it.max_qty * it.agreed_price            AS cap_amount,
    it.max_qty * it.agreed_price
        - CASE WHEN a.price_includes_tax
               THEN COALESCE(usg.with_tax, 0)
               ELSE COALESCE(usg.amount, 0) END AS remaining_amount,
    round(COALESCE(usg.qty, 0) / it.max_qty, 6)                     AS exec_qty_rate,
    round((CASE WHEN a.price_includes_tax THEN COALESCE(usg.with_tax, 0)
                ELSE COALESCE(usg.amount, 0) END)
          / NULLIF(it.max_qty * it.agreed_price, 0), 6)             AS exec_amount_rate,
    COALESCE(usg.doc_count, 0)              AS order_doc_count,
    COALESCE(usg.line_count, 0)             AS order_line_count,
    usg.last_order_on                       AS last_order_on,
    CASE
        WHEN it.max_qty IS NULL                      THEN 'UNLIMITED'
        WHEN COALESCE(usg.qty, 0) >  it.max_qty      THEN 'OVER'
        WHEN COALESCE(usg.qty, 0) >= it.max_qty      THEN 'DONE'
        WHEN COALESCE(usg.qty, 0) >  0               THEN 'PARTIAL'
        ELSE 'NOT_STARTED'
    END                                     AS exec_state
  FROM biz_framework_agreement_item it
  -- 协议已软删 ⇒ 它的物料行也不进报表（报表不展示已删单）
  JOIN biz_framework_agreement a ON a.id = it.agreement_id AND a.deleted_at IS NULL
  LEFT JOIN biz_material m ON m.id = it.material_id AND m.deleted_at IS NULL
  LEFT JOIN biz_unit     u ON u.id = it.unit_id
  -- 已用量：引用本行的采购明细，且采购单**存活且未作废**（见头注释「计入的采购单」）
  LEFT JOIN (
        SELECT i.framework_agreement_item_id         AS item_id,
               SUM(i.qty)                            AS qty,
               SUM(i.amount)                         AS amount,
               SUM(i.with_tax_amount)                AS with_tax,
               count(DISTINCT o.id)                  AS doc_count,
               count(*)                              AS line_count,
               max(o.order_date)                     AS last_order_on
          FROM biz_purchase_order_item i
          JOIN biz_purchase_order o
            ON o.id = i.order_id
           AND o.deleted_at IS NULL AND o.status <> 'CANCELLED'
         WHERE i.framework_agreement_item_id IS NOT NULL
         GROUP BY i.framework_agreement_item_id
  ) usg ON usg.item_id = it.id;

COMMENT ON VIEW v_framework_agreement_item_execution IS
    '框架协议物料行执行量（M4-09）：一行 = 协议的一格（物料）；已用量按单据现算（存活的采购单明细 Σ），booked_qty 是 M4-08 的回写快照并排供对账；exec_qty_rate = executed_qty / max_qty（6 位），max_qty 为 NULL（不限）时率与剩余都是 NULL；exec_state ∈ UNLIMITED / NOT_STARTED / PARTIAL / DONE / OVER';
COMMENT ON COLUMN v_framework_agreement_item_execution.executed_qty IS
    '已用量 = Σ 引用本行的「存活（deleted_at IS NULL）且未作废（status <> CANCELLED）」采购明细 qty；草稿也算（量已占住）';
COMMENT ON COLUMN v_framework_agreement_item_execution.booked_qty IS
    '回写快照（biz_framework_agreement_item.executed_qty，M4-08 重算）；与 executed_qty 不一致 = 回写与单据链分叉，报表直接显形';
COMMENT ON COLUMN v_framework_agreement_item_execution.executed_amount_capped IS
    '已用金额（按协议价口径）：协议价含税取 Σ with_tax_amount，否则取 Σ amount —— 与 M4-08 金额上限比较同一口径';
COMMENT ON COLUMN v_framework_agreement_item_execution.remaining_qty IS
    '剩余 = max_qty − executed_qty；**可为负**（超上限执行，V0450 刻意不在库层拦）；max_qty 为 NULL（不限）时为 NULL，不是 0';
COMMENT ON COLUMN v_framework_agreement_item_execution.exec_state IS
    '行状态：UNLIMITED（未设上限）/ NOT_STARTED（已用 0）/ PARTIAL / DONE（恰好用满）/ OVER（超上限）';

-- ---------- ② 单头级：一张存活协议一行 ----------
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

-- ---------- ③ 菜单：框架协议页面行 + 执行量按钮 ----------
-- 页面行：`…:page` 只给前端路由判显形（约定 14，后端零引用）；Vue 页面文件属 M4-11。
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000004480'::uuid, '框架协议', 2,
     'business:erp:framework-agreement:page', '/business/erp/framework-agreement',
     '20000000-0000-0000-0000-000000000946'::uuid, 'business/erp/framework-agreement/index',
     39, 1, 'file-paste', '框架协议', TRUE, 'business')
ON CONFLICT (id) DO UPDATE
   SET name        = EXCLUDED.name,
       menu_type   = EXCLUDED.menu_type,
       auth_code   = EXCLUDED.auth_code,
       path        = EXCLUDED.path,
       parent_id   = EXCLUDED.parent_id,
       component   = EXCLUDED.component,
       sort_order  = EXCLUDED.sort_order,
       status      = EXCLUDED.status,
       icon        = EXCLUDED.icon,
       title       = EXCLUDED.title,
       is_visible  = EXCLUDED.is_visible,
       module_code = EXCLUDED.module_code,
       deleted_at  = NULL,
       updated_at  = now();

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000004481'::uuid, '协议执行量', 3,
     'business:erp:framework-agreement:execution', NULL,
     '20000000-0000-0000-0000-000000004480'::uuid,
     NULL, 10, 1, NULL, '执行量统计', TRUE, 'business')
ON CONFLICT (id) DO UPDATE
   SET name        = EXCLUDED.name,
       menu_type   = EXCLUDED.menu_type,
       auth_code   = EXCLUDED.auth_code,
       path        = EXCLUDED.path,
       parent_id   = EXCLUDED.parent_id,
       component   = EXCLUDED.component,
       sort_order  = EXCLUDED.sort_order,
       status      = EXCLUDED.status,
       icon        = EXCLUDED.icon,
       title       = EXCLUDED.title,
       is_visible  = EXCLUDED.is_visible,
       module_code = EXCLUDED.module_code,
       deleted_at  = NULL,
       updated_at  = now();

-- ---------- ④ 授权 ----------
-- 采购经理：控盘就是看「哪些协议快用完 / 已经超了、哪家供应商还没下单」。
-- 财务经理：框架协议是月度付款与成本归集的输入，复核口径与采购一致。
-- 部门主管 / 普通员工**不给**：协议是公司级采购文件（SCOPE-EXEMPT），
-- 部门的需求表达走采购申请，不靠这张报表。
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
 CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000004480'::uuid),
                    ('20000000-0000-0000-0000-000000004481'::uuid)) AS m(id)
 WHERE r.role_code IN ('PURCHASE_MANAGER', 'FINANCE_MANAGER') AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- ⑤ 自检 ----------
DO $$
DECLARE
    v_cnt      integer;
    v_missing  text;

    -- 探针数据（全部 UUID 固定，先造 → 先断言 → 再清）
    p_ag   uuid := '00000000-0000-4000-8000-0000045200a1';
    p_ita  uuid := '00000000-0000-4000-8000-0000045200a2';
    p_itb  uuid := '00000000-0000-4000-8000-0000045200a3';
    p_po1  uuid := '00000000-0000-4000-8000-0000045200b1';  -- 存活：吃额度
    p_po2  uuid := '00000000-0000-4000-8000-0000045200b2';  -- 作废：不吃
    p_po3  uuid := '00000000-0000-4000-8000-0000045200b3';  -- 软删：不吃

    v_qty    numeric;  v_booked  numeric;  v_remain  numeric;
    v_amount numeric;  v_capped  numeric;  v_state   text;
    v_rate   numeric;  v_docs    integer;  v_last    date;
    h_qty    numeric;  h_remain  numeric;  h_rate    numeric;
    h_state  text;     h_docs    integer;  h_released integer;
    h_items  integer;  h_unlim   integer;  h_partial integer;
    h_cap    numeric;  h_valid   text;     h_eff     boolean;
BEGIN
    -- ⑤.1 两个视图必须在位
    IF to_regclass('public.v_framework_agreement_item_execution') IS NULL THEN
        RAISE EXCEPTION 'V0452 自检失败：视图 v_framework_agreement_item_execution 未建出';
    END IF;
    IF to_regclass('public.v_framework_agreement_execution') IS NULL THEN
        RAISE EXCEPTION 'V0452 自检失败：视图 v_framework_agreement_execution 未建出';
    END IF;

    -- ⑤.2 关键列名一个都不能少（改名时这里先炸，而不是等 handler 运行期 500）
    SELECT string_agg(c, ', ') INTO v_missing
      FROM (VALUES ('agreement_item_id'),('agreement_id'),('agreement_no'),('line_no'),
                   ('material_id'),('material_code'),('material_name'),('unit_id'),('unit_name'),
                   ('agreed_price'),('tax_rate'),('price_includes_tax'),('price_currency'),('max_qty'),
                   ('executed_qty'),('booked_qty'),('executed_amount'),('executed_amount_with_tax'),
                   ('executed_amount_capped'),('remaining_qty'),('cap_amount'),('remaining_amount'),
                   ('exec_qty_rate'),('exec_amount_rate'),('order_doc_count'),('order_line_count'),
                   ('last_order_on'),('exec_state')) AS v(c)
     WHERE NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'v_framework_agreement_item_execution'
                          AND column_name = v.c);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0452 自检失败：v_framework_agreement_item_execution 缺列：%', v_missing;
    END IF;
    SELECT string_agg(c, ', ') INTO v_missing
      FROM (VALUES ('agreement_id'),('agreement_no'),('agreement_name'),('supplier_id'),('supplier_name'),
                   ('supplier_display_name'),('dept_id'),('dept_name'),('owner_id'),('owner_name'),
                   ('start_date'),('end_date'),('sign_date'),('status'),('price_mode'),('price_includes_tax'),
                   ('price_currency'),('price_tolerance'),('total_max_qty'),('total_max_amount'),
                   ('cap_qty'),('cap_amount'),('item_max_qty_sum'),('item_count'),('unlimited_items'),
                   ('over_items'),('done_items'),('partial_items'),('not_started_items'),
                   ('executed_qty'),('executed_amount'),('executed_amount_with_tax'),('executed_amount_capped'),
                   ('booked_qty'),('booked_total_qty'),('booked_total_amount'),
                   ('remaining_qty'),('remaining_amount'),('exec_qty_rate'),('exec_amount_rate'),
                   ('order_doc_count'),('released_doc_count'),('last_order_on'),('days_left'),
                   ('is_effective'),('validity_state'),('exec_state'),('created_at')) AS v(c)
     WHERE NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'v_framework_agreement_execution'
                          AND column_name = v.c);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0452 自检失败：v_framework_agreement_execution 缺列：%', v_missing;
    END IF;

    -- ⑤.3 造探针：一份协议（APPROVED、不含税、单价 10 / 5）：
    --      物料 A 上限 100，物料 B 不限；一张存活采购单吃 30 + 10，
    --      一张作废单 + 一张软删单各吃 20 / 5（都不许算进来）。
    INSERT INTO biz_framework_agreement
        (id, agreement_no, agreement_name, supplier_id, supplier_name,
         start_date, end_date, price_mode, price_includes_tax, price_currency,
         price_tolerance, total_max_qty, total_max_amount, status)
    VALUES (p_ag, 'FWA-19990101-900452', 'V0452 自检探针协议', gen_random_uuid(), '探针供应商',
            CURRENT_DATE - 10, CURRENT_DATE + 10, 'FIXED', FALSE, 'CNY',
            0, 200, 5000, 'APPROVED');
    INSERT INTO biz_framework_agreement_item
        (id, agreement_id, line_no, material_id, unit_id, agreed_price, tax_rate, max_qty, executed_qty, executed_amount)
    VALUES (p_ita, p_ag, 1, gen_random_uuid(), gen_random_uuid(), 10, 0.13, 100, 30, 300),
           (p_itb, p_ag, 2, gen_random_uuid(), gen_random_uuid(),  5, 0.13, NULL, 10, 50);

    INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status,
                                    framework_agreement_id, framework_agreement_no_snapshot)
    VALUES (p_po1, 'PO-19990101-900452', gen_random_uuid(), CURRENT_DATE - 1, gen_random_uuid(), 'APPROVED',
            p_ag, 'FWA-19990101-900452'),
           (p_po2, 'PO-19990101-900453', gen_random_uuid(), CURRENT_DATE - 1, gen_random_uuid(), 'CANCELLED',
            p_ag, 'FWA-19990101-900452'),
           (p_po3, 'PO-19990101-900454', gen_random_uuid(), CURRENT_DATE - 1, gen_random_uuid(), 'APPROVED',
            p_ag, 'FWA-19990101-900452');
    UPDATE biz_purchase_order SET deleted_at = now() WHERE id = p_po3;

    -- ⚠ `ck_biz_po_item_tax` 要求 `with_tax_amount = amount + tax_amount`，三列必须自洽
    INSERT INTO biz_purchase_order_item
        (id, order_id, line_no, material_id, unit_id, qty, price, amount, tax_amount, with_tax_amount,
         framework_agreement_item_id)
    VALUES (gen_random_uuid(), p_po1, 1, gen_random_uuid(), gen_random_uuid(), 30, 10, 300, 39, 339, p_ita),
           (gen_random_uuid(), p_po1, 2, gen_random_uuid(), gen_random_uuid(), 10,  5,  50, 6.5, 56.5, p_itb),
           (gen_random_uuid(), p_po2, 1, gen_random_uuid(), gen_random_uuid(), 20, 10, 200, 26, 226, p_ita),
           (gen_random_uuid(), p_po3, 1, gen_random_uuid(), gen_random_uuid(),  5, 10,  50, 6.5, 56.5, p_ita);

    -- ⑤.4 物料行：只算存活单；剩余 / 率 / 状态逐格验算
    SELECT executed_qty, booked_qty, remaining_qty, executed_amount, executed_amount_capped,
           exec_qty_rate, exec_state, order_doc_count, last_order_on
      INTO v_qty, v_booked, v_remain, v_amount, v_capped, v_rate, v_state, v_docs, v_last
      FROM v_framework_agreement_item_execution WHERE agreement_item_id = p_ita;
    IF v_qty <> 30 THEN
        RAISE EXCEPTION 'V0452 自检失败：限量物料已用量应为 30（作废 20 + 软删 5 不许算），实际 %', v_qty;
    END IF;
    IF v_booked <> 30 THEN
        RAISE EXCEPTION 'V0452 自检失败：回写快照应为 30，实际 %', v_booked;
    END IF;
    IF v_remain <> 70 THEN
        RAISE EXCEPTION 'V0452 自检失败：剩余应为 100−30=70，实际 %', v_remain;
    END IF;
    IF v_amount <> 300 OR v_capped <> 300 THEN
        RAISE EXCEPTION 'V0452 自检失败：不含税协议已用金额应为 300（amount 口径），实际 amount=% capped=%', v_amount, v_capped;
    END IF;
    IF v_rate <> 0.3 THEN
        RAISE EXCEPTION 'V0452 自检失败：执行率应为 0.3，实际 %', v_rate;
    END IF;
    IF v_state <> 'PARTIAL' THEN
        RAISE EXCEPTION 'V0452 自检失败：限量物料行状态应为 PARTIAL，实际 %', v_state;
    END IF;
    IF v_docs <> 1 THEN
        RAISE EXCEPTION 'V0452 自检失败：限量物料行单据数应为 1（作废/软删不算），实际 %', v_docs;
    END IF;
    IF v_last IS NULL THEN
        RAISE EXCEPTION 'V0452 自检失败：最近下单日不许为空（last_order_on 应取存活单的 order_date）';
    END IF;

    -- 不限量物料：剩余 / 率必须是 NULL，状态 UNLIMITED（0 会被读成「一点都不剩」）
    SELECT remaining_qty, exec_qty_rate, exec_state
      INTO v_remain, v_rate, v_state
      FROM v_framework_agreement_item_execution WHERE agreement_item_id = p_itb;
    IF v_remain IS NOT NULL THEN
        RAISE EXCEPTION 'V0452 自检失败：不限量物料的剩余必须为 NULL（不是 0 / 负数），实际 %', v_remain;
    END IF;
    IF v_rate IS NOT NULL THEN
        RAISE EXCEPTION 'V0452 自检失败：不限量物料的执行率必须为 NULL，实际 %', v_rate;
    END IF;
    IF v_state <> 'UNLIMITED' THEN
        RAISE EXCEPTION 'V0452 自检失败：不限量物料行状态应为 UNLIMITED，实际 %', v_state;
    END IF;

    -- ⑤.5 单头：合计 = 明细之和；单据数按单头去重（同一张单两行只算一张）
    SELECT executed_qty, remaining_qty, exec_qty_rate, exec_state, order_doc_count,
           released_doc_count, item_count, unlimited_items, partial_items, cap_qty, validity_state, is_effective
      INTO h_qty, h_remain, h_rate, h_state, h_docs, h_released, h_items, h_unlim, h_partial, h_cap, h_valid, h_eff
      FROM v_framework_agreement_execution WHERE agreement_id = p_ag;
    IF h_qty <> 40 THEN
        RAISE EXCEPTION 'V0452 自检失败：单头已用量应为 30+10=40，实际 %', h_qty;
    END IF;
    IF h_cap <> 200 THEN
        RAISE EXCEPTION 'V0452 自检失败：单头有效封顶应取 total_max_qty=200，实际 %', h_cap;
    END IF;
    IF h_remain <> 160 THEN
        RAISE EXCEPTION 'V0452 自检失败：单头剩余应为 200−40=160，实际 %', h_remain;
    END IF;
    IF h_rate <> 0.2 THEN
        RAISE EXCEPTION 'V0452 自检失败：单头执行率应为 0.2，实际 %', h_rate;
    END IF;
    IF h_state <> 'PARTIAL' THEN
        RAISE EXCEPTION 'V0452 自检失败：单头状态应为 PARTIAL，实际 %', h_state;
    END IF;
    IF h_docs <> 1 THEN
        RAISE EXCEPTION 'V0452 自检失败：单头计入单据数应为 1（同一张单的 2 行只算 1 张），实际 %', h_docs;
    END IF;
    IF h_released <> 2 THEN
        RAISE EXCEPTION 'V0452 自检失败：释放单据数应为 2（作废 1 + 软删 1），实际 %', h_released;
    END IF;
    IF h_items <> 2 OR h_unlim <> 1 OR h_partial <> 1 THEN
        RAISE EXCEPTION 'V0452 自检失败：行计数应为 2 行 / 1 不限 / 1 部分，实际 % / % / %', h_items, h_unlim, h_partial;
    END IF;
    IF h_valid <> 'ACTIVE' OR h_eff IS NOT TRUE THEN
        RAISE EXCEPTION 'V0452 自检失败：探针协议（今天在效期内且 APPROVED）应为 ACTIVE / effective，实际 % / %', h_valid, h_eff;
    END IF;

    -- ⑤.6 明细行视图合计必须与单头视图逐项相等（两套聚合口径不许分叉）
    SELECT COALESCE(SUM(executed_qty), 0) INTO v_qty
      FROM v_framework_agreement_item_execution WHERE agreement_id = p_ag;
    IF v_qty <> h_qty THEN
        RAISE EXCEPTION 'V0452 自检失败：单头已用量 % 与明细合计 % 不等', h_qty, v_qty;
    END IF;

    -- ⑤.7 清场：删协议 ⇒ 物料行 CASCADE；采购单明细 CASCADE
    DELETE FROM biz_framework_agreement WHERE id = p_ag;
    SELECT count(*) INTO v_cnt FROM biz_framework_agreement_item WHERE agreement_id = p_ag;
    IF v_cnt <> 0 THEN
        RAISE EXCEPTION 'V0452 自检失败：删协议后物料行还留着（% 行，ON DELETE CASCADE 失效）', v_cnt;
    END IF;
    DELETE FROM biz_purchase_order WHERE id IN (p_po1, p_po2, p_po3);
    SELECT count(*) INTO v_cnt FROM biz_purchase_order_item
     WHERE framework_agreement_item_id IN (p_ita, p_itb);
    IF v_cnt <> 0 THEN
        RAISE EXCEPTION 'V0452 自检失败：删探针单后采购明细还留着（% 行）', v_cnt;
    END IF;

    -- ⑤.8 菜单两行在位，且父子 / 权限码 / 标题长度 / 排序都对
    SELECT count(*) INTO v_cnt FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000004480'::uuid,
                  '20000000-0000-0000-0000-000000004481'::uuid)
       AND deleted_at IS NULL;
    IF v_cnt <> 2 THEN
        RAISE EXCEPTION 'V0452 自检失败：菜单应有 2 行（1 页面 + 1 按钮），实际 %', v_cnt;
    END IF;
    SELECT count(*) INTO v_cnt FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004480'::uuid
       AND menu_type = 2
       AND auth_code = 'business:erp:framework-agreement:page'
       AND parent_id = '20000000-0000-0000-0000-000000000946'::uuid
       AND path = '/business/erp/framework-agreement'
       AND length(title) <= 12
       AND deleted_at IS NULL;
    IF v_cnt <> 1 THEN
        RAISE EXCEPTION 'V0452 自检失败：框架协议页面行（类型 / 显形码 / 父级 / path / 标题长度）不对';
    END IF;
    SELECT count(*) INTO v_cnt FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004481'::uuid
       AND menu_type = 3
       AND auth_code = 'business:erp:framework-agreement:execution'
       AND parent_id = '20000000-0000-0000-0000-000000004480'::uuid
       AND length(title) <= 12
       AND deleted_at IS NULL;
    IF v_cnt <> 1 THEN
        RAISE EXCEPTION 'V0452 自检失败：执行量按钮行（类型 / 接口码 / 父级页面 / 标题长度）不对';
    END IF;

    RAISE NOTICE 'M4-09/V0452: 框架协议执行量统计（已用 / 剩余 两视图 + 页面与执行量菜单）自检通过';
END $$;
