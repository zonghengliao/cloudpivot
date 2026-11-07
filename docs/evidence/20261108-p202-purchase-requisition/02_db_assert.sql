-- ============================================================================
-- P2-02 采购申请 · DB 侧判据（只读 + 两个事务内负向注入）
-- 库：cloudpivot_prq03（隔离实例 18105 用），psql -v ON_ERROR_STOP=1 执行
-- ============================================================================
\set ON_ERROR_STOP on
\pset pager off

\echo '=== A 表结构 / 索引 / 约束在位 ==='
SELECT
    (SELECT count(*) FROM information_schema.tables
      WHERE table_name IN ('biz_purchase_requisition','biz_purchase_requisition_item')) AS tables_expect_2,
    (SELECT count(*) FROM pg_indexes
      WHERE indexname IN ('uk_biz_purchase_requisition_no','uk_biz_prqi_line',
                          'idx_biz_purchase_requisition_status','idx_biz_prqi_material')) AS indexes_expect_4,
    (SELECT count(*) FROM pg_constraint
      WHERE conname IN ('ck_biz_prq_status','ck_biz_prq_converted','ck_biz_prq_instance',
                        'ck_biz_prq_over_consistency','ck_biz_prqi_qty','ck_biz_prqi_tax')) AS checks_expect_6;

\echo '=== B 验收 4/5：两张申请单都 CONVERTED、指向同一 PO，PO 只有一行 150 ==='
SELECT r.req_no, r.status, r.po_no, r.po_id,
       r.est_amount, i.qty AS req_qty, i.est_price, i.po_item_id
  FROM biz_purchase_requisition r
  JOIN biz_purchase_requisition_item i ON i.requisition_id = r.id
 WHERE r.status = 'CONVERTED'
 ORDER BY r.req_no;

SELECT
    count(DISTINCT r.po_id) AS distinct_po,
    count(*) FILTER (WHERE r.status = 'CONVERTED') AS converted_rows
  FROM biz_purchase_requisition r
 WHERE r.po_no IS NOT NULL;

\echo '=== C 采购订单：一行 150 / 单价 10 / 税率 13 / 价税合计 1695 ==='
SELECT o.order_no, o.status, o.total_amount, o.total_tax, o.total_with_tax,
       count(*) AS lines, sum(i.qty) AS qty_sum,
       min(i.price) AS price_min, max(i.price) AS price_max,
       min(i.tax_rate) AS tax_min, max(i.tax_rate) AS tax_max
  FROM biz_purchase_order o
  JOIN biz_purchase_order_item i ON i.order_id = o.id
 WHERE o.order_no = 'PO-20261108-000001'
 GROUP BY o.order_no, o.status, o.total_amount, o.total_tax, o.total_with_tax;

\echo '=== D 合并对应关系：两条申请明细都指向采购单的同一条明细 ==='
SELECT count(*) AS req_items, count(DISTINCT i.po_item_id) AS distinct_po_items,
       bool_and(i.po_item_id = po.id) AS all_match_po_line
  FROM biz_purchase_requisition_item i
  JOIN biz_purchase_order_item po ON po.id = i.po_item_id
 WHERE i.requisition_id IN (SELECT id FROM biz_purchase_requisition WHERE status = 'CONVERTED');

\echo '=== E 审批流：PR_APPROVE 两个实例；超预算那张走了 2 个节点 ==='
SELECT r.req_no, r.over_budget, fi.instance_no, fi.status AS instance_status, fi.biz_type,
       (SELECT count(*) FROM biz_flow_task t WHERE t.instance_id = fi.id) AS task_count,
       (SELECT string_agg(t.node_key, ' → ' ORDER BY t.created_at)
          FROM biz_flow_task t WHERE t.instance_id = fi.id) AS nodes
  FROM biz_purchase_requisition r
  JOIN biz_flow_instance fi ON fi.biz_id = r.id AND fi.biz_type = 'PR_APPROVE'
 WHERE r.req_no IN ('PRQ-20261108-000001','PRQ-20261108-000002')
 ORDER BY r.req_no;

\echo '=== F 超预算快照落库（B 单）：over_budget / over_amount / budget 科目与期间 ==='
SELECT req_no, budget_checked, over_budget, over_amount, budget_period_code,
       (budget_account_id IS NOT NULL) AS has_account, left(coalesce(budget_reason,''), 40) AS reason_head
  FROM biz_purchase_requisition
 WHERE req_no IN ('PRQ-20261108-000001','PRQ-20261108-000002')
 ORDER BY req_no;

\echo '=== G 权限码 / 菜单 / 角色授权 ==='
SELECT m.menu_type, m.auth_code, m.path, m.parent_id
  FROM sys_menu m
 WHERE m.auth_code LIKE 'business:erp:purchase-requisition:%' AND m.deleted_at IS NULL
 ORDER BY m.auth_code;

SELECT r.role_code, count(*) AS granted
  FROM sys_role_menu rm
  JOIN sys_role r ON r.id = rm.role_id
 WHERE rm.menu_id IN (SELECT id FROM sys_menu
                       WHERE auth_code LIKE 'business:erp:purchase-requisition:%')
 GROUP BY r.role_code
 ORDER BY r.role_code;

\echo '=== H 流程种子：pr_approve（PUBLISHED / 默认 / 5 节点 5 边 / 条件边） ==='
SELECT d.flow_code, d.biz_type, d.status, d.is_default, d.current_version_no,
       v.graph_hash, v.node_count, v.edge_count,
       (SELECT count(*) FROM biz_flow_edge e
         WHERE e.version_id = v.id AND NOT e.is_default AND e.condition_expr IS NOT NULL) AS conditional_edges,
       (SELECT count(*) FROM biz_flow_node_assignee a
          JOIN biz_flow_node n ON n.id = a.node_id WHERE n.version_id = v.id) AS assignees
  FROM biz_flow_def d
  JOIN biz_flow_def_version v ON v.id = d.current_version_id
 WHERE d.flow_code = 'pr_approve';

\echo '=== I 负向注入 ①：已转单却没有 PO 号 → 必须被 ck_biz_prq_converted 拒 ==='
BEGIN;
DO $$
BEGIN
    INSERT INTO biz_purchase_requisition (req_no, dept_id, warehouse_id, status)
    VALUES ('P202-NEG-CONVERTED', gen_random_uuid(), gen_random_uuid(), 'CONVERTED');
    RAISE EXCEPTION 'P2-02 负向注入失败：已转单却没有 PO 号竟然被接受';
EXCEPTION WHEN check_violation THEN
    RAISE NOTICE 'PASS: 已转单没有 PO 号被 ck_biz_prq_converted 拒绝（%）', SQLERRM;
END $$;
ROLLBACK;

\echo '=== J 负向注入 ②：审批中却没有审批实例 → 必须被 ck_biz_prq_instance 拒 ==='
BEGIN;
DO $$
BEGIN
    INSERT INTO biz_purchase_requisition (req_no, dept_id, warehouse_id, status)
    VALUES ('P202-NEG-INSTANCE', gen_random_uuid(), gen_random_uuid(), 'SUBMITTED');
    RAISE EXCEPTION 'P2-02 负向注入失败：审批中却没有审批实例竟然被接受';
EXCEPTION WHEN check_violation THEN
    RAISE NOTICE 'PASS: 审批中缺少审批实例被 ck_biz_prq_instance 拒绝（%）', SQLERRM;
END $$;
ROLLBACK;

\echo '=== K 负向注入 ③：超预算标记与超出额不一致 → 必须被 ck_biz_prq_over_consistency 拒 ==='
BEGIN;
DO $$
BEGIN
    INSERT INTO biz_purchase_requisition (req_no, dept_id, warehouse_id, status, over_budget, over_amount)
    VALUES ('P202-NEG-OVER', gen_random_uuid(), gen_random_uuid(), 'DRAFT', TRUE, 0);
    RAISE EXCEPTION 'P2-02 负向注入失败：over_budget=TRUE 但 over_amount=0 竟然被接受';
EXCEPTION WHEN check_violation THEN
    RAISE NOTICE 'PASS: 超预算标记与金额不一致被 ck_biz_prq_over_consistency 拒绝（%）', SQLERRM;
END $$;
ROLLBACK;

\echo '=== L 负向注入 ④：同一张申请单两个相同行号 → 必须被 uk_biz_prqi_line 拒 ==='
BEGIN;
DO $$
DECLARE
    v_req uuid;
BEGIN
    SELECT id INTO v_req FROM biz_purchase_requisition LIMIT 1;
    INSERT INTO biz_purchase_requisition_item (requisition_id, line_no, material_id, unit_id, qty)
    VALUES (v_req, 1, gen_random_uuid(), gen_random_uuid(), 1);
    RAISE EXCEPTION 'P2-02 负向注入失败：同单重复行号竟然被接受';
EXCEPTION WHEN unique_violation THEN
    RAISE NOTICE 'PASS: 同单重复行号被 uk_biz_prqi_line 拒绝（%）', SQLERRM;
END $$;
ROLLBACK;

\echo '=== M 负向注入不污染证据库（残留必须为 0） ==='
SELECT count(*) AS negative_residue
  FROM biz_purchase_requisition
 WHERE req_no LIKE 'P202-NEG-%';

\echo '=== N 汇总判据（一行看结果） ==='
SELECT
    (SELECT count(*) FROM biz_purchase_requisition WHERE status = 'CONVERTED') AS converted_expect_2,
    (SELECT count(DISTINCT po_id) FROM biz_purchase_requisition WHERE po_id IS NOT NULL) AS distinct_po_expect_1,
    (SELECT count(*) FROM biz_purchase_requisition_item WHERE po_item_id IS NOT NULL) AS items_with_po_line_expect_2,
    (SELECT count(*) FROM biz_purchase_order_item i
       JOIN biz_purchase_order o ON o.id = i.order_id
      WHERE o.order_no = 'PO-20261108-000001' AND i.qty = 150 AND i.price = 10) AS po_line_expect_1,
    (SELECT count(*) FROM biz_flow_instance fi
       JOIN biz_purchase_requisition r ON r.id = fi.biz_id
      WHERE fi.biz_type = 'PR_APPROVE'
        AND r.req_no IN ('PRQ-20261108-000001','PRQ-20261108-000002')) AS acceptance_instances_expect_2;
