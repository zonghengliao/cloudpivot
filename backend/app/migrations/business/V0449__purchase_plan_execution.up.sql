-- ============================================================================
-- V0449：采购计划执行率跟踪（采购链 M4-04）—— 已下单 / 计划量
--
--   ① 视图 `v_purchase_plan_item_execution`  明细级执行率（一行 = 计划的一格：物料 × 仓库）
--   ② 视图 `v_purchase_plan_execution`       单头级执行率（一张计划一行，含行状态计数）
--   ③ 菜单 1 行：`business:erp:purchase-plan:execution`（menu_type=3，
--      挂在 M4-02 建的采购计划页面行 `…004471` 下，sort=70）
--   ④ 角色授权：采购经理（编制口径）= 财务经理（复核口径）
--
-- ── 号段核对（动手前 `ls backend/app/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail -3`）──
--   实测最大号 = V0448（采购计划 → 采购申请转换，M4-03）⇒ 本卡顺延取 **V0449**。
--   sys_menu id 取 `…004478`（与 V0447 的 …4471–4476、V0448 的 …4477 连号，无交叠）。
--   V0446 的 `biz_seq` 行 id 用到 …4461 ⇒ 本迁移不碰 biz_seq（执行率不需要取号）。
--
-- ── 为什么是「视图」而不是「加一列 ordered_qty 再回写」──────────────────────
--   1) 执行率的分母/分子都必须是**单据链的真相**，不是某一时刻的快照：
--        计划明细 ──(plan_item_id, V0448)──▶ 申请明细 ──(申请单头 po_id)──▶ 采购订单
--      回写列一旦被动过（单被作废 / 软删 / 退货），列值就与单据链分叉，而**没有任何
--      入口会去把它减回来**（采购订单的作废入口在 purchase_handler，不在采购计划域）。
--      视图现算 = 永远自愈；写死在列里 = 又多一份要人工对账的账。
--   2) 已申请量已经有回写列 `biz_purchase_plan_item.executed_qty`（M4-03 转申请时 += ），
--      视图里以 `booked_qty` 原样带出，与 `req_qty`（按单据现算）并排 ——
--      两者不一致就是「回写列与单据链分叉」，报表直接看得见，不需要另开对账脚本。
--   3) 「已下单」不能靠申请明细的 `po_item_id` 反查采购明细行：采购订单改单是
--      **明细致删旧插新**（purchase_handler `PUT` 的既有口径），改一次量，
--      `po_item_id` 就指向一行已经不存在的采购明细 —— 用它会静默把执行率打成 0。
--      申请单头的 `po_id` 指向的是采购**单头**，改单不动单头 id，所以口径取
--      「这张申请单已经被并进哪张采购单」，而不是「并进了采购单的哪一行」。
--   4) 粒度差异（合并采购）：多张申请单的同物料会并进采购单的**同一行**。
--      所以「已下单量」按**申请明细的 qty** 计（每张申请单自己那部分量），
--      不按采购明细行的合并量计 —— 否则同一批量会被每张申请单各记一遍。
--
-- ── 口径（逐条写死，后端 dto 的纯函数与这里逐字一致）─────────────────────
--   · 计划量      `planned_qty`（> 0，库层 ck_biz_ppi_qty 保证，所以执行率恒有分母）
--   · 已申请量    Σ 申请明细 qty（`plan_item_id` 指回本行、申请单未软删）
--     —— DRAFT / SUBMITTED / APPROVED / CONVERTED **都算**：量已经从计划里拨出去了，
--        报表若只算审批过的，计划会显示「还有余量」→ 用户再转一次 → 同一段量转两遍。
--   · 已下单量    Σ 申请明细 qty，且该申请单的 `po_id` 指向的采购单**存活且未作废**
--     （`deleted_at IS NULL AND status <> 'CANCELLED'`）。
--     —— 采购单草稿（DRAFT）**算已下单**：单已经生成、量已经拨走，只是还没走审批；
--        不算的话「刚转出来的单」会让执行率回落到 0，用户会重复下单。
--        作废（CANCELLED）与软删**不算**：那条链已经断了。
--   · 执行率      `ordered_qty / planned_qty`，6 位小数（0.4 = 40%）；分母恒 > 0
--   · 剩余可下单  `planned_qty - ordered_qty`（**可为负** —— 超计划执行）
--   · 行状态      NOT_STARTED（已下单 0）/ PARTIAL（0 < 已下单 < 计划）/
--                 DONE（恰好等于）/ OVER（超过计划，执行率 > 100%）
--     —— V0446 头注释 4) 已经把「超计划」的口子留着（库层刻意不拦），
--        这里就是它露头的地方：OVER 行必须显示得出来，不许把负数截成 0。
--
-- ── 幂等与回滚 ───────────────────────────────────────────────────────────
--   `CREATE OR REPLACE VIEW`（重跑只换定义）/ 菜单 `ON CONFLICT (id) DO UPDATE` /
--   授权 `ON CONFLICT DO NOTHING`；末尾 DO 块自检：两个视图在位 + 关键列名齐全 +
--   **探针数据的执行率逐格验算**（见 ④，先算 → 先清 → 再断言，不留残留行）。
--   回滚见 V0449__purchase_plan_execution.down.sql：先删授权、再删菜单、最后删两个视图。
-- ============================================================================

-- ---------- ① 明细级：一行 = 计划的一格（物料 × 仓库） ----------
CREATE OR REPLACE VIEW v_purchase_plan_item_execution AS
SELECT
    it.id                                   AS plan_item_id,
    it.plan_id                              AS plan_id,
    it.line_no                              AS line_no,
    it.material_id                          AS material_id,
    COALESCE(m.material_code, '')           AS material_code,
    COALESCE(m.material_name, '')           AS material_name,
    it.warehouse_id                         AS warehouse_id,
    COALESCE(w.warehouse_name, '')          AS warehouse_name,
    it.unit_id                              AS unit_id,
    COALESCE(u.unit_name, '')               AS unit_name,
    it.planned_qty                          AS planned_qty,
    it.planned_price                        AS planned_price,
    it.planned_amount                       AS planned_amount,
    -- 回写列快照（M4-03 转申请时 += ），与下面的 req_qty 并排供对账
    it.executed_qty                         AS booked_qty,
    COALESCE(req.req_qty, 0)                AS req_qty,
    COALESCE(req.req_count, 0)              AS req_count,
    COALESCE(req.req_doc_count, 0)          AS req_doc_count,
    COALESCE(po.ordered_qty, 0)             AS ordered_qty,
    COALESCE(po.po_doc_count, 0)            AS po_doc_count,
    it.planned_qty - COALESCE(po.ordered_qty, 0)                          AS remaining_qty,
    round(COALESCE(po.ordered_qty, 0) / it.planned_qty, 6)                AS exec_rate,
    round(COALESCE(req.req_qty, 0) / it.planned_qty, 6)                   AS req_rate,
    CASE
        WHEN COALESCE(po.ordered_qty, 0) >  it.planned_qty THEN 'OVER'
        WHEN COALESCE(po.ordered_qty, 0) >= it.planned_qty THEN 'DONE'
        WHEN COALESCE(po.ordered_qty, 0) >  0              THEN 'PARTIAL'
        ELSE 'NOT_STARTED'
    END                                     AS exec_state
  FROM biz_purchase_plan_item it
  LEFT JOIN biz_material  m ON m.id = it.material_id  AND m.deleted_at IS NULL
  LEFT JOIN biz_warehouse w ON w.id = it.warehouse_id AND w.deleted_at IS NULL
  LEFT JOIN biz_unit      u ON u.id = it.unit_id
  -- 已申请：申请明细指回本行即可，不看申请单状态（见头注释「已申请量」）
  LEFT JOIN (
        SELECT ri.plan_item_id                  AS plan_item_id,
               SUM(ri.qty)                      AS req_qty,
               count(*)                         AS req_count,
               count(DISTINCT ri.requisition_id) AS req_doc_count
          FROM biz_purchase_requisition_item ri
          JOIN biz_purchase_requisition rq
            ON rq.id = ri.requisition_id AND rq.deleted_at IS NULL
         WHERE ri.plan_item_id IS NOT NULL
         GROUP BY ri.plan_item_id
  ) req ON req.plan_item_id = it.id
  -- 已下单：申请单已被并进一张「存活且未作废」的采购单（按申请明细的 qty 计）
  LEFT JOIN (
        SELECT ri.plan_item_id                  AS plan_item_id,
               SUM(ri.qty)                      AS ordered_qty,
               count(DISTINCT po.id)            AS po_doc_count
          FROM biz_purchase_requisition_item ri
          JOIN biz_purchase_requisition rq
            ON rq.id = ri.requisition_id AND rq.deleted_at IS NULL
          JOIN biz_purchase_order po
            ON po.id = rq.po_id AND po.deleted_at IS NULL AND po.status <> 'CANCELLED'
         WHERE ri.plan_item_id IS NOT NULL
         GROUP BY ri.plan_item_id
  ) po ON po.plan_item_id = it.id;

COMMENT ON VIEW v_purchase_plan_item_execution IS
    '采购计划明细执行率（M4-04）：一行 = 计划的一格（物料 × 仓库）；已申请量按单据现算，已下单量 = 申请量中已并进存活且未作废采购单的部分；exec_rate = ordered_qty / planned_qty（6 位小数），exec_state ∈ NOT_STARTED / PARTIAL / DONE / OVER';
COMMENT ON COLUMN v_purchase_plan_item_execution.booked_qty IS
    '回写列快照 biz_purchase_plan_item.executed_qty（M4-03 转申请时 += ）；与 req_qty 不等即「回写列与单据链分叉」，报表并排展示供对账';
COMMENT ON COLUMN v_purchase_plan_item_execution.req_qty IS
    '已申请量 = Σ 申请明细 qty（plan_item_id 指回本行、申请单未软删；DRAFT/SUBMITTED/APPROVED/CONVERTED 都算）';
COMMENT ON COLUMN v_purchase_plan_item_execution.ordered_qty IS
    '已下单量 = Σ 申请明细 qty，且申请单的 po_id 指向的采购单存活且未作废（草稿采购单也算：量已拨走，避免重复下单）';
COMMENT ON COLUMN v_purchase_plan_item_execution.remaining_qty IS
    '剩余可下单 = planned_qty - ordered_qty；**可为负**（超计划执行，V0446 头注释 4 刻意不拦）';
COMMENT ON COLUMN v_purchase_plan_item_execution.exec_state IS
    'NOT_STARTED 未开工 / PARTIAL 部分下单 / DONE 已下满 / OVER 超计划（>100%）';

-- ---------- ② 单头级：一张计划一行（聚合 + 行状态计数） ----------
CREATE OR REPLACE VIEW v_purchase_plan_execution AS
SELECT
    p.id                                    AS plan_id,
    p.plan_no                               AS plan_no,
    p.plan_name                             AS plan_name,
    p.plan_type                             AS plan_type,
    p.plan_year                             AS plan_year,
    p.plan_month                            AS plan_month,
    p.parent_plan_id                        AS parent_plan_id,
    p.dept_id                               AS dept_id,
    -- 约定 13：编制部门属「业务展示」，快照为空时实时 join 取现名
    COALESCE(p.dept_name, d.dept_name)      AS dept_name,
    p.planner_name                          AS planner_name,
    p.status                                AS status,
    p.total_planned_amount                  AS total_planned_amount,
    p.created_at                            AS created_at,
    COALESCE(agg.line_count, 0)             AS line_count,
    COALESCE(agg.planned_qty, 0)            AS planned_qty,
    COALESCE(agg.booked_qty, 0)             AS booked_qty,
    COALESCE(agg.req_qty, 0)                AS req_qty,
    COALESCE(agg.ordered_qty, 0)            AS ordered_qty,
    COALESCE(agg.planned_qty, 0) - COALESCE(agg.ordered_qty, 0)           AS remaining_qty,
    CASE WHEN COALESCE(agg.planned_qty, 0) > 0
         THEN round(COALESCE(agg.ordered_qty, 0) / agg.planned_qty, 6) END AS exec_rate,
    CASE WHEN COALESCE(agg.planned_qty, 0) > 0
         THEN round(COALESCE(agg.req_qty, 0) / agg.planned_qty, 6)     END AS req_rate,
    COALESCE(agg.not_started_lines, 0)      AS not_started_lines,
    COALESCE(agg.partial_lines, 0)          AS partial_lines,
    COALESCE(agg.done_lines, 0)             AS done_lines,
    COALESCE(agg.over_lines, 0)             AS over_lines,
    -- 整单状态：与明细的 exec_state 同一套判据（按整单已下单 / 整单计划量判，
    -- 不是「明细行状态的多数」）。报表与 SQL 侧读同一个值，避免两边各算一套。
    CASE
        WHEN COALESCE(agg.ordered_qty, 0) >  COALESCE(agg.planned_qty, 0) THEN 'OVER'
        WHEN COALESCE(agg.ordered_qty, 0) >= COALESCE(agg.planned_qty, 0) THEN 'DONE'
        WHEN COALESCE(agg.ordered_qty, 0) >  0                            THEN 'PARTIAL'
        ELSE 'NOT_STARTED'
    END                                     AS exec_state
  FROM biz_purchase_plan p
  LEFT JOIN sys_dept d ON d.id = p.dept_id AND d.deleted_at IS NULL
  LEFT JOIN (
        SELECT plan_id,
               count(*)                                                   AS line_count,
               SUM(planned_qty)                                           AS planned_qty,
               SUM(booked_qty)                                            AS booked_qty,
               SUM(req_qty)                                               AS req_qty,
               SUM(ordered_qty)                                           AS ordered_qty,
               count(*) FILTER (WHERE exec_state = 'NOT_STARTED')         AS not_started_lines,
               count(*) FILTER (WHERE exec_state = 'PARTIAL')             AS partial_lines,
               count(*) FILTER (WHERE exec_state = 'DONE')                AS done_lines,
               count(*) FILTER (WHERE exec_state = 'OVER')                AS over_lines
          FROM v_purchase_plan_item_execution
         GROUP BY plan_id
  ) agg ON agg.plan_id = p.id
 WHERE p.deleted_at IS NULL;

COMMENT ON VIEW v_purchase_plan_execution IS
    '采购计划执行率总览（M4-04）：一张存活计划一行；planned/req/ordered 三列都是明细视图的合计，exec_rate = Σordered / Σplanned；四个 *_lines 是行状态计数（未开工 / 部分 / 下满 / 超计划）。已软删的计划**不进**本视图（报表不展示已删单）';
COMMENT ON COLUMN v_purchase_plan_execution.exec_rate IS
    '整单执行率 = Σ已下单量 / Σ计划量（6 位小数）；计划没有明细时为 NULL（不是 0 —— 0 会被误读成「一行都没下」）';
COMMENT ON COLUMN v_purchase_plan_execution.exec_state IS
    '整单状态 NOT_STARTED / PARTIAL / DONE / OVER：按「整单已下单 / 整单计划量」判，与明细 exec_state 同一套判据（不是明细行状态的多数）';

-- ---------- ③ 菜单：采购计划页面下加一个「执行率跟踪」按钮 ----------
-- 权限码语义：`{模块}:{资源}:{动作}`；页面显形码 `…:page` **不动**（约定 14：只给前端）。
-- title 是侧边栏/按钮显示名（2–6 字），功能说明写在 description 语义里（这里放 remark 之外，
-- 说明在本文档头与 dto 注释里，不塞进 title）。
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000004478'::uuid, '计划执行率', 3,
     'business:erp:purchase-plan:execution', NULL,
     '20000000-0000-0000-0000-000000004471'::uuid,
     NULL, 70, 1, NULL, '执行率跟踪', TRUE, 'business')
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
-- 采购经理：执行率是它编制下一期计划的输入（谁没按计划下单、谁超了）。
-- 财务经理：≥50 万的计划本来就要它复核（pp_approve），复核完要能看执行情况。
-- 部门主管 / 普通员工**不给**：计划编制是公司级滚动文件（V0446 头注释 8 SCOPE-EXEMPT），
-- 执行率是编制考核口径；部门的需求表达走采购申请（pr_approve），不靠这张报表。
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, '20000000-0000-0000-0000-000000004478'::uuid
  FROM sys_role r
 WHERE r.role_code IN ('PURCHASE_MANAGER', 'FINANCE_MANAGER') AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- ⑤ 自检 ----------
DO $$
DECLARE
    v_cnt       integer;
    v_cols      integer;
    v_missing   text;
    v_lines     integer;

    ia_planned  numeric;  ia_req      numeric;  ia_ordered  numeric;
    ia_remain   numeric;  ia_rate     numeric;  ia_state    text;
    ib_planned  numeric;  ib_req      numeric;  ib_ordered  numeric;
    ib_remain   numeric;  ib_rate     numeric;  ib_state    text;
    h_planned   numeric;  h_req       numeric;  h_ordered   numeric;
    h_remain    numeric;  h_rate      numeric;
    h_lines     integer;  h_ns        integer;  h_partial   integer;
    h_done      integer;  h_over      integer;  h_state     text;

    p1  uuid := gen_random_uuid();
    ia  uuid := gen_random_uuid();
    ib  uuid := gen_random_uuid();
    r1  uuid := gen_random_uuid();
    r2  uuid := gen_random_uuid();
    r3  uuid := gen_random_uuid();
    r4  uuid := gen_random_uuid();
    po1 uuid := gen_random_uuid();
    po2 uuid := gen_random_uuid();
    po3 uuid := gen_random_uuid();
BEGIN
    -- ⑤.1 两个视图必须在位
    IF to_regclass('public.v_purchase_plan_item_execution') IS NULL THEN
        RAISE EXCEPTION 'V0449 自检失败：视图 v_purchase_plan_item_execution 未建出';
    END IF;
    IF to_regclass('public.v_purchase_plan_execution') IS NULL THEN
        RAISE EXCEPTION 'V0449 自检失败：视图 v_purchase_plan_execution 未建出';
    END IF;

    -- ⑤.2 关键列名一个都不能少（改名时这里先炸，而不是等到 handler 运行期）
    SELECT string_agg(c, ', ') INTO v_missing
      FROM (VALUES ('plan_item_id'),('plan_id'),('line_no'),('material_id'),('material_code'),
                   ('material_name'),('warehouse_id'),('warehouse_name'),('unit_id'),('unit_name'),
                   ('planned_qty'),('planned_price'),('planned_amount'),('booked_qty'),
                   ('req_qty'),('req_count'),('req_doc_count'),('ordered_qty'),('po_doc_count'),
                   ('remaining_qty'),('exec_rate'),('req_rate'),('exec_state')) AS v(c)
     WHERE NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'v_purchase_plan_item_execution'
                          AND column_name = v.c);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0449 自检失败：v_purchase_plan_item_execution 缺列：%', v_missing;
    END IF;
    SELECT string_agg(c, ', ') INTO v_missing
      FROM (VALUES ('plan_id'),('plan_no'),('plan_name'),('plan_type'),('plan_year'),('plan_month'),
                   ('dept_id'),('dept_name'),('status'),('total_planned_amount'),
                   ('line_count'),('planned_qty'),('booked_qty'),('req_qty'),('ordered_qty'),
                   ('remaining_qty'),('exec_rate'),('req_rate'),
                   ('not_started_lines'),('partial_lines'),('done_lines'),('over_lines'),('exec_state')) AS v(c)
     WHERE NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'v_purchase_plan_execution'
                          AND column_name = v.c);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0449 自检失败：v_purchase_plan_execution 缺列：%', v_missing;
    END IF;
    SELECT count(*) INTO v_cnt FROM information_schema.views
     WHERE table_schema = 'public'
       AND table_name IN ('v_purchase_plan_item_execution','v_purchase_plan_execution');
    IF v_cnt <> 2 THEN
        RAISE EXCEPTION 'V0449 自检失败：两个视图只识别出 % / 2 个', v_cnt;
    END IF;

    -- ⑤.3 探针数据：把四段「算不算已下单」的边界一次铺齐
    --   计划：A 行计划 100 / B 行计划 10
    --   r1（APPROVED，没转单）：A 60 + B 10        → 只算已申请
    --   r2（CONVERTED → po1 APPROVED）：A 40       → 算已下单
    --   r3（CONVERTED → po2 CANCELLED）：A 5       → **不**算已下单（作废链断了）
    --   r4（CONVERTED → po3 已软删）：A 7          → **不**算已下单（软删链断了）
    --   期望：A 行 计划 100 / 已申请 112 / 已下单 40 / 剩 60 / 0.4 / PARTIAL
    --        B 行 计划 10  / 已申请 10  / 已下单 0  / 剩 10 / 0    / NOT_STARTED
    --        单头   计划 110 / 已申请 122 / 已下单 40 / 剩 70 / 0.363636
    INSERT INTO biz_purchase_plan (id, plan_no, plan_name, plan_type, plan_year, plan_month)
    VALUES (p1, 'PPL-21991231-900001', 'V0449 自检探针', 'ANNUAL', 2199, NULL);

    INSERT INTO biz_purchase_plan_item (id, plan_id, line_no, material_id, warehouse_id, unit_id, planned_qty, planned_price, planned_amount)
    VALUES (ia, p1, 1, gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 100, 1, 100),
           (ib, p1, 2, gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 10, 1, 10);

    INSERT INTO biz_purchase_requisition (id, req_no, dept_id, warehouse_id, status, po_id, po_no, converted_at)
    VALUES (r1, 'PRQ-21991231-900001', gen_random_uuid(), gen_random_uuid(), 'APPROVED', NULL, NULL, NULL);

    INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status, deleted_at)
    VALUES (po1, 'PO-21991231-900001', gen_random_uuid(), DATE '2199-12-31', gen_random_uuid(), 'APPROVED', NULL),
           (po2, 'PO-21991231-900002', gen_random_uuid(), DATE '2199-12-31', gen_random_uuid(), 'CANCELLED', NULL),
           (po3, 'PO-21991231-900003', gen_random_uuid(), DATE '2199-12-31', gen_random_uuid(), 'APPROVED', now());

    INSERT INTO biz_purchase_requisition (id, req_no, dept_id, warehouse_id, status, po_id, po_no, converted_at)
    VALUES (r2, 'PRQ-21991231-900002', gen_random_uuid(), gen_random_uuid(), 'CONVERTED', po1, 'PO-21991231-900001', now()),
           (r3, 'PRQ-21991231-900003', gen_random_uuid(), gen_random_uuid(), 'CONVERTED', po2, 'PO-21991231-900002', now()),
           (r4, 'PRQ-21991231-900004', gen_random_uuid(), gen_random_uuid(), 'CONVERTED', po3, 'PO-21991231-900003', now());

    INSERT INTO biz_purchase_requisition_item (requisition_id, line_no, material_id, unit_id, qty, plan_item_id)
    SELECT r1, 1, material_id, unit_id, 60, ia FROM biz_purchase_plan_item WHERE id = ia;
    INSERT INTO biz_purchase_requisition_item (requisition_id, line_no, material_id, unit_id, qty, plan_item_id)
    SELECT r1, 2, material_id, unit_id, 10, ib FROM biz_purchase_plan_item WHERE id = ib;
    INSERT INTO biz_purchase_requisition_item (requisition_id, line_no, material_id, unit_id, qty, plan_item_id)
    SELECT r2, 1, material_id, unit_id, 40, ia FROM biz_purchase_plan_item WHERE id = ia;
    INSERT INTO biz_purchase_requisition_item (requisition_id, line_no, material_id, unit_id, qty, plan_item_id)
    SELECT r3, 1, material_id, unit_id, 5, ia FROM biz_purchase_plan_item WHERE id = ia;
    INSERT INTO biz_purchase_requisition_item (requisition_id, line_no, material_id, unit_id, qty, plan_item_id)
    SELECT r4, 1, material_id, unit_id, 7, ia FROM biz_purchase_plan_item WHERE id = ia;

    -- 验算（读视图）：A 行 / B 行 / 单头三段各用各的变量，别互相覆盖
    SELECT planned_qty, req_qty, ordered_qty, remaining_qty, exec_rate, exec_state
      INTO ia_planned, ia_req, ia_ordered, ia_remain, ia_rate, ia_state
      FROM v_purchase_plan_item_execution WHERE plan_item_id = ia;
    SELECT planned_qty, req_qty, ordered_qty, remaining_qty, exec_rate, exec_state
      INTO ib_planned, ib_req, ib_ordered, ib_remain, ib_rate, ib_state
      FROM v_purchase_plan_item_execution WHERE plan_item_id = ib;
    SELECT planned_qty, req_qty, ordered_qty, remaining_qty, exec_rate,
           line_count, not_started_lines, partial_lines, done_lines, over_lines, exec_state
      INTO h_planned, h_req, h_ordered, h_remain, h_rate, h_lines, h_ns, h_partial, h_done, h_over, h_state
      FROM v_purchase_plan_execution WHERE plan_id = p1;

    -- ⑤.5 软删的计划不进总览视图（在清场之前验，否则验的是「行已经没了」）
    UPDATE biz_purchase_plan SET deleted_at = now() WHERE id = p1;
    SELECT count(*) INTO v_cols FROM v_purchase_plan_execution WHERE plan_id = p1;
    -- 再软删回来（下面 DELETE 才按 id 删得掉，且状态与断言前提一致）
    UPDATE biz_purchase_plan SET deleted_at = NULL WHERE id = p1;

    -- 先清（视图只是读，清完不会影响已经取到的变量），再断言 —— 失败也不留残留行
    DELETE FROM biz_purchase_requisition WHERE id IN (r1, r2, r3, r4);
    DELETE FROM biz_purchase_order WHERE id IN (po1, po2, po3);
    DELETE FROM biz_purchase_plan WHERE id = p1;

    -- ⑤.6 断言（每一条都是头注释里的一句口径）
    SELECT count(*) INTO v_lines FROM biz_purchase_plan WHERE id = p1;
    IF v_lines <> 0 THEN
        RAISE EXCEPTION 'V0449 自检失败：探针计划没清掉（% 行残留）', v_lines;
    END IF;
    IF v_cols <> 0 THEN
        RAISE EXCEPTION 'V0449 自检失败：已软删的计划还在总览视图里（WHERE deleted_at IS NULL 失效）';
    END IF;
    -- A 行：100 / 112 / 40 / 60 / 0.4 / PARTIAL
    IF ia_planned IS DISTINCT FROM 100 OR ia_req IS DISTINCT FROM 112
       OR ia_ordered IS DISTINCT FROM 40 OR ia_remain IS DISTINCT FROM 60 THEN
        RAISE EXCEPTION 'V0449 自检失败：A 行 计划/已申请/已下单/剩余 = % / % / % / %（期望 100 / 112 / 40 / 60；作废与软删的采购单不许算已下单）',
            ia_planned, ia_req, ia_ordered, ia_remain;
    END IF;
    IF ia_rate IS DISTINCT FROM round(40::numeric / 100::numeric, 6) THEN
        RAISE EXCEPTION 'V0449 自检失败：A 行执行率 % ≠ 0.400000', ia_rate;
    END IF;
    IF ia_state IS DISTINCT FROM 'PARTIAL' THEN
        RAISE EXCEPTION 'V0449 自检失败：A 行状态 % ≠ PARTIAL', ia_state;
    END IF;
    -- B 行：10 / 10 / 0 / 10 / 0 / NOT_STARTED（申请了但一单没下 = 未开工）
    IF ib_planned IS DISTINCT FROM 10 OR ib_req IS DISTINCT FROM 10
       OR ib_ordered IS DISTINCT FROM 0 OR ib_remain IS DISTINCT FROM 10
       OR ib_rate IS DISTINCT FROM 0 THEN
        RAISE EXCEPTION 'V0449 自检失败：B 行 计划/已申请/已下单/剩余/率 = % / % / % / % / %（期望 10 / 10 / 0 / 10 / 0）',
            ib_planned, ib_req, ib_ordered, ib_remain, ib_rate;
    END IF;
    IF ib_state IS DISTINCT FROM 'NOT_STARTED' THEN
        RAISE EXCEPTION 'V0449 自检失败：B 行状态 % ≠ NOT_STARTED', ib_state;
    END IF;
    -- 单头：110 / 122 / 40 / 70 / 0.363636，2 行（1 未开工 + 1 部分，0 下满 0 超计划）
    IF h_planned IS DISTINCT FROM 110 OR h_req IS DISTINCT FROM 122
       OR h_ordered IS DISTINCT FROM 40 OR h_remain IS DISTINCT FROM 70 THEN
        RAISE EXCEPTION 'V0449 自检失败：单头 计划/已申请/已下单/剩余 = % / % / % / %（期望 110 / 122 / 40 / 70）',
            h_planned, h_req, h_ordered, h_remain;
    END IF;
    IF h_rate IS DISTINCT FROM round(40::numeric / 110::numeric, 6) THEN
        RAISE EXCEPTION 'V0449 自检失败：单头执行率 % ≠ 0.363636', h_rate;
    END IF;
    IF h_lines IS DISTINCT FROM 2 OR h_ns IS DISTINCT FROM 1 OR h_partial IS DISTINCT FROM 1
       OR h_done IS DISTINCT FROM 0 OR h_over IS DISTINCT FROM 0 THEN
        RAISE EXCEPTION 'V0449 自检失败：单头行计数 = % 行（未开工 % / 部分 % / 下满 % / 超计划 %），期望 2（1/1/0/0）',
            h_lines, h_ns, h_partial, h_done, h_over;
    END IF;
    -- 整单状态与明细同一套判据：40 / 110 = 部分下单（不是「未开工」，也不是「下满」）
    IF h_state IS DISTINCT FROM 'PARTIAL' THEN
        RAISE EXCEPTION 'V0449 自检失败：单头 exec_state % ≠ PARTIAL（40 已下单 / 110 计划量）', h_state;
    END IF;

    -- ⑤.7 菜单 1 行 + 授给「库里存在的」采购经理 / 财务经理（角色不在就 0 行，不种悬空授权）
    SELECT count(*) INTO v_lines FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004478'::uuid
       AND deleted_at IS NULL AND menu_type = 3
       AND auth_code = 'business:erp:purchase-plan:execution'
       AND parent_id = '20000000-0000-0000-0000-000000004471'::uuid;
    IF v_lines <> 1 THEN
        RAISE EXCEPTION 'V0449 自检失败：执行率按钮行不在位（% 行）', v_lines;
    END IF;
    SELECT count(*) INTO v_lines FROM sys_role_menu
     WHERE menu_id = '20000000-0000-0000-0000-000000004478'::uuid;
    SELECT count(*) INTO v_cnt FROM sys_role
     WHERE role_code IN ('PURCHASE_MANAGER', 'FINANCE_MANAGER') AND deleted_at IS NULL;
    IF v_lines <> v_cnt THEN
        RAISE EXCEPTION 'V0449 自检失败：执行率按钮授了 % 行，但库里存在的采购/财务经理角色有 % 个（必须逐个授到）',
            v_lines, v_cnt;
    END IF;
EXCEPTION WHEN OTHERS THEN
    -- 兜底清理：任何一步炸掉都不留探针行（迁移事务回滚时这段无副作用）
    DELETE FROM biz_purchase_requisition WHERE id IN (r1, r2, r3, r4);
    DELETE FROM biz_purchase_order WHERE id IN (po1, po2, po3);
    DELETE FROM biz_purchase_plan WHERE id = p1;
    RAISE;
END $$;
