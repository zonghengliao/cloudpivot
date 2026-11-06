-- SO-31 证据库收尾：删掉「按钮布局量测」用的那张 DRAFT（DRAFT 未过账 = 没有任何账务影响）
DELETE FROM biz_sales_return_item WHERE return_id = '01a1b211-315a-71e0-ae2e-d5a44fed582b'::uuid;
DELETE FROM biz_sales_return WHERE id = '01a1b211-315a-71e0-ae2e-d5a44fed582b'::uuid;
SELECT return_no, status, total_with_tax FROM biz_sales_return WHERE deleted_at IS NULL ORDER BY return_no;
