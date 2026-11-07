-- P2-01 询比价 · DB 侧断言（跑在隔离库 cloudpivot_pi01r2，与同一轮 API 实测是同一份数据）
-- 用法：PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_pi01r2 -f 02_db_assert.sql
-- 判据：正文每一段都要有「结论行」，且 === 收尾段的 verdict 全为 PASS 才算过。
\pset pager off
\echo '=== A. 询价单头：状态 / 定标 / 转单 / po_refs ==='
SELECT inquiry_no,
       status,
       COALESCE(award_mode, '-') AS award_mode,
       (award_supplier_id IS NOT NULL) AS has_award_supplier,
       (awarded_at IS NOT NULL)        AS has_awarded_at,
       (converted_at IS NOT NULL)      AS has_converted_at,
       jsonb_array_length(po_refs)     AS po_count
  FROM biz_price_inquiry
 ORDER BY inquiry_no;

\echo '=== B. PIQ-000001 比价与中标：3 物料 × 3 家 = 9 条报价，中标 3 条且只落一家 ==='
SELECT count(*)                                        AS quotes,
       count(*) FILTER (WHERE q.is_awarded)             AS awarded,
       count(DISTINCT q.supplier_id)                    AS bidders,
       count(DISTINCT q.item_id)                        AS lines
  FROM biz_price_quote q
  JOIN biz_price_inquiry i ON i.id = q.inquiry_id
 WHERE i.inquiry_no = 'PIQ-20261108-000001';

SELECT count(DISTINCT q.supplier_id) AS awarded_suppliers
  FROM biz_price_quote q
  JOIN biz_price_inquiry i ON i.id = q.inquiry_id
 WHERE i.inquiry_no = 'PIQ-20261108-000001' AND q.is_awarded;

\echo '=== C. 单价传递：采购明细价 = 中标报价价（逐行，0 差异才算过） ==='
SELECT count(*) AS compared_lines,
       count(*) FILTER (WHERE poi.price = q.price_excl) AS price_equal_lines,
       count(*) FILTER (WHERE poi.qty = pi.qty)         AS qty_equal_lines,
       count(*) FILTER (WHERE poi.tax_rate = q.tax_rate) AS tax_equal_lines
  FROM biz_price_inquiry i
  JOIN biz_price_inquiry_item pi ON pi.inquiry_id = i.id
  JOIN biz_price_quote q ON q.item_id = pi.id AND q.is_awarded
  JOIN biz_purchase_order po ON po.id = (i.po_refs -> 0 ->> 'po_id')::uuid
  JOIN biz_purchase_order_item poi ON poi.order_id = po.id AND poi.material_id = pi.material_id
 WHERE i.inquiry_no = 'PIQ-20261108-000001';

\echo '=== D. 转单后单据：采购单头金额 ==='
SELECT po.order_no, po.status, po.total_amount, po.total_tax, po.total_with_tax, po.warehouse_id
  FROM biz_purchase_order po
 ORDER BY po.order_no;

\echo '=== E. 一单拆多单：PIQ-000004 产出 2 张采购单，分别挂 A / B ==='
SELECT i.inquiry_no,
       jsonb_array_length(i.po_refs)                          AS po_count,
       (SELECT count(DISTINCT (ref ->> 'supplier_id')) FROM jsonb_array_elements(i.po_refs) ref) AS distinct_suppliers,
       (SELECT sum((ref ->> 'line_count')::int) FROM jsonb_array_elements(i.po_refs) ref)        AS total_lines
  FROM biz_price_inquiry i
 WHERE i.inquiry_no = 'PIQ-20261108-000004';

\echo '=== F. 单位止血：询价明细单位 = 物料基础单位（DC-09） ==='
SELECT count(*) AS items,
       count(*) FILTER (WHERE pi.unit_id = m.unit_id) AS unit_matches_material
  FROM biz_price_inquiry_item pi
  JOIN biz_material m ON m.id = pi.material_id;

\echo '=== G. 状态不变式：AWARDED/CONVERTED ⇔ awarded_at 非空；含税价独立复算 ==='
SELECT count(*) FILTER (WHERE status IN ('AWARDED','CONVERTED') AND awarded_at IS NULL) AS bad_missing_awarded_at,
       count(*) FILTER (WHERE status NOT IN ('AWARDED','CONVERTED') AND awarded_at IS NOT NULL) AS bad_extra_awarded_at,
       (SELECT count(*) FROM biz_price_quote
         WHERE price_incl <> round(price_excl * (1 + tax_rate / 100), 6)) AS bad_price_incl
  FROM biz_price_inquiry;

\echo '=== H. 单源例外留痕：SINGLE_SOURCE 必须有 ≥10 字理由 ==='
SELECT inquiry_no, award_mode, length(btrim(award_reason)) AS reason_len
  FROM biz_price_inquiry
 WHERE award_mode = 'SINGLE_SOURCE';

\echo '=== I. 取号：biz_seq(PIQ) 已推进（本次跑满 4 张单） ==='
SELECT seq_type, prefix, date_pattern, period_key, current_no
  FROM biz_seq WHERE seq_type = 'PIQ';

\echo '=== J. 菜单与权限码（5 个码各有且仅有 1 行承载） ==='
SELECT auth_code, name,
       (SELECT count(*) FROM sys_menu m2 WHERE m2.auth_code = m.auth_code AND m2.deleted_at IS NULL) AS carriers
  FROM sys_menu m
 WHERE m.auth_code LIKE 'business:erp:price-inquiry:%'
   AND m.menu_type IN (2,3)
   AND m.deleted_at IS NULL
 ORDER BY auth_code;

\echo '=== K. 菜单树：页面节点挂在「采购管理」下，按钮挂页面下 ==='
SELECT child.id, child.name, child.menu_type, child.path, child.component, parent.name AS parent_name
  FROM sys_menu child
  LEFT JOIN sys_menu parent ON parent.id = child.parent_id
 WHERE child.id IN ('20000000-0000-0000-0000-000000003321'::uuid,
                    '20000000-0000-0000-0000-000000003322'::uuid,
                    '20000000-0000-0000-0000-000000003323'::uuid,
                    '20000000-0000-0000-0000-000000003324'::uuid,
                    '20000000-0000-0000-0000-000000003325'::uuid)
 ORDER BY child.menu_type, child.sort_order;

\echo '=== L. 角色授权：PURCHASE_MANAGER 拿到 5 个码 / WAREHOUSE_MANAGER 只读 ==='
SELECT r.role_code, count(*) AS granted_menus
  FROM sys_role_menu rm
  JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
 WHERE rm.menu_id IN ('20000000-0000-0000-0000-000000003321'::uuid,
                      '20000000-0000-0000-0000-000000003322'::uuid,
                      '20000000-0000-0000-0000-000000003323'::uuid,
                      '20000000-0000-0000-0000-000000003324'::uuid,
                      '20000000-0000-0000-0000-000000003325'::uuid)
 GROUP BY r.role_code ORDER BY r.role_code;

\echo '=== M. 第三方真相复核：用纯 SQL 重算「中标单价 × 数量」= 采购单不含税额 ==='
WITH award AS (
  SELECT i.inquiry_no, pi.material_id, q.price_excl, pi.qty
    FROM biz_price_inquiry i
    JOIN biz_price_inquiry_item pi ON pi.inquiry_id = i.id
    JOIN biz_price_quote q ON q.item_id = pi.id AND q.is_awarded
   WHERE i.inquiry_no = 'PIQ-20261108-000001'
)
SELECT (SELECT round(sum(price_excl * qty), 4) FROM award) AS expected_amount,
       (SELECT total_amount FROM biz_purchase_order WHERE order_no = 'PO-20261108-000001') AS po_amount,
       (SELECT round(sum(price_excl * qty), 4) FROM award)
         = (SELECT total_amount FROM biz_purchase_order WHERE order_no = 'PO-20261108-000001') AS match;

\echo '=== N. 汇总判据（全 PASS 才算过） ==='
WITH
 head AS (
   SELECT count(*) FILTER (WHERE status = 'CONVERTED' AND jsonb_array_length(po_refs) >= 1) AS ok_converted,
          count(*) FILTER (WHERE status IN ('AWARDED','CONVERTED') AND awarded_at IS NULL) AS bad_awarded_at
     FROM biz_price_inquiry
 ),
 q3 AS (
   SELECT count(*) AS quotes, count(*) FILTER (WHERE q.is_awarded) AS awarded,
          count(DISTINCT q.supplier_id) AS bidders,
          count(DISTINCT q.supplier_id) FILTER (WHERE q.is_awarded) AS awarded_suppliers
     FROM biz_price_quote q JOIN biz_price_inquiry i ON i.id = q.inquiry_id
    WHERE i.inquiry_no = 'PIQ-20261108-000001'
 ),
 trans AS (
   SELECT count(*) AS lines,
          count(*) FILTER (WHERE poi.price = q.price_excl AND poi.qty = pi.qty
                             AND poi.tax_rate = q.tax_rate) AS same
     FROM biz_price_inquiry i
     JOIN biz_price_inquiry_item pi ON pi.inquiry_id = i.id
     JOIN biz_price_quote q ON q.item_id = pi.id AND q.is_awarded
     JOIN biz_purchase_order po ON po.id = (i.po_refs -> 0 ->> 'po_id')::uuid
     JOIN biz_purchase_order_item poi ON poi.order_id = po.id AND poi.material_id = pi.material_id
    WHERE i.inquiry_no = 'PIQ-20261108-000001'
 ),
 split AS (
   SELECT jsonb_array_length(i.po_refs) AS po_count
     FROM biz_price_inquiry i WHERE i.inquiry_no = 'PIQ-20261108-000004'
 ),
 menus AS (
   SELECT count(*) AS carriers, count(DISTINCT auth_code) AS codes
     FROM sys_menu WHERE auth_code LIKE 'business:erp:price-inquiry:%'
        AND menu_type IN (2,3) AND deleted_at IS NULL
 )
SELECT 'A 转单头' AS check, (SELECT ok_converted FROM head)::text AS got, '>=1' AS want,
       CASE WHEN (SELECT ok_converted FROM head) >= 1 THEN 'PASS' ELSE 'FAIL' END AS verdict
UNION ALL SELECT 'G 缺 awarded_at', (SELECT bad_awarded_at FROM head)::text, '0',
       CASE WHEN (SELECT bad_awarded_at FROM head) = 0 THEN 'PASS' ELSE 'FAIL' END
UNION ALL SELECT 'B 报价/中标/家数', format('%s/%s/%s', quotes, awarded, bidders), '9/3/3',
       CASE WHEN quotes = 9 AND awarded = 3 AND bidders = 3 THEN 'PASS' ELSE 'FAIL' END FROM q3
UNION ALL SELECT 'B 中标只落一家', (SELECT awarded_suppliers FROM q3)::text, '1',
       CASE WHEN (SELECT awarded_suppliers FROM q3) = 1 THEN 'PASS' ELSE 'FAIL' END
UNION ALL SELECT 'C 单价/数量/税率逐行一致', format('%s/%s', same, lines), '3/3',
       CASE WHEN same = 3 AND lines = 3 THEN 'PASS' ELSE 'FAIL' END FROM trans
UNION ALL SELECT 'E 一单拆两张', (SELECT po_count FROM split)::text, '2',
       CASE WHEN (SELECT po_count FROM split) = 2 THEN 'PASS' ELSE 'FAIL' END
UNION ALL SELECT 'J 权限码承载行', format('%s码/%s行', (SELECT codes FROM menus), (SELECT carriers FROM menus)), '5码/5行',
       CASE WHEN (SELECT codes FROM menus) = 5 AND (SELECT carriers FROM menus) = 5 THEN 'PASS' ELSE 'FAIL' END;

\echo '=== O. 库级兜底：uk_biz_price_quote_awarded 存在，且真能拒「同一明细两家中标」 ==='
SELECT indexname, indexdef
  FROM pg_indexes
 WHERE schemaname = current_schema() AND indexname = 'uk_biz_price_quote_awarded';

BEGIN;
DO $$
DECLARE
    v_inq  uuid := gen_random_uuid();
    v_item uuid := gen_random_uuid();
    v_s1   uuid := gen_random_uuid();
    v_s2   uuid := gen_random_uuid();
BEGIN
    INSERT INTO biz_price_inquiry (id, inquiry_no, warehouse_id, status)
    VALUES (v_inq, 'SELFTEST-AWARDED-ONLY', v_inq, 'DRAFT');
    INSERT INTO biz_price_inquiry_item (id, inquiry_id, line_no, material_id, unit_id, qty)
    VALUES (v_item, v_inq, 1, v_inq, v_inq, 1);
    INSERT INTO biz_price_quote (inquiry_id, item_id, supplier_id, price_excl, price_incl, tax_rate, is_awarded)
    VALUES (v_inq, v_item, v_s1, 1, 1.13, 13, TRUE);
    BEGIN
        INSERT INTO biz_price_quote (inquiry_id, item_id, supplier_id, price_excl, price_incl, tax_rate, is_awarded)
        VALUES (v_inq, v_item, v_s2, 2, 2.26, 13, TRUE);
        RAISE EXCEPTION 'FAIL: 同一明细第二家中标竟然被接受（部分唯一索引没生效）';
    EXCEPTION WHEN unique_violation THEN
        RAISE NOTICE 'PASS: 同一明细第二家中标被 uk_biz_price_quote_awarded 拒绝';
    END;
END $$;
-- 上面只是负向注入，插进去的 SELFTEST 行必须回滚掉，不能留在证据库里
ROLLBACK;

\echo '=== P. 收尾：确认 SELFTEST-AWARDED-ONLY 没有残留（期望 0） ==='
SELECT count(*) AS selftest_rows FROM biz_price_inquiry WHERE inquiry_no = 'SELFTEST-AWARDED-ONLY';
