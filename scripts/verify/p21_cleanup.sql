-- P2-1 验收夹具清理（按前缀 / 固定 UUID，绝不按生产号段删）
--
--   psql -h 127.0.0.1 -U alun -d <测试库> -f scripts/verify/p21_cleanup.sql
--
-- 顺序：状态日志（无外键，必须先按 id 删）→ 凭证 → 付款 → 应付 → 出库 → 收货
--       → 库存三本账 + 流水 → 主数据 → 部门 / 用户部门还原。

delete from biz_status_log where entity_type = 'STOCK_IN' and entity_id in
    (select id from biz_stock_in where warehouse_id = 'e21a0101-0000-4000-8000-000000000003'::uuid);
delete from biz_status_log where entity_type = 'STOCK_OUT' and entity_id in
    (select id from biz_stock_out where warehouse_id = 'e21a0101-0000-4000-8000-000000000003'::uuid);
delete from biz_status_log where entity_type = 'PAYABLE' and entity_id in
    (select id from biz_payable where supplier_id = 'e21a0101-0000-4000-8000-000000000002'::uuid);
delete from biz_status_log where entity_type = 'VOUCHER' and entity_id not in
    (select id from biz_voucher);

delete from biz_voucher where source_id in
    (select id from biz_payment where payable_id in
        (select id from biz_payable where supplier_id = 'e21a0101-0000-4000-8000-000000000002'::uuid));
delete from biz_voucher where source_type = 'STOCK_OUT' and source_id in
    (select id from biz_stock_out where warehouse_id = 'e21a0101-0000-4000-8000-000000000003'::uuid);
delete from biz_voucher where source_type = 'STOCK_IN' and source_id in
    (select id from biz_stock_in where warehouse_id = 'e21a0101-0000-4000-8000-000000000003'::uuid);
delete from biz_voucher where id = 'e21a0101-0000-4000-8000-0000000000f1'::uuid;

delete from biz_payment where payable_id in
    (select id from biz_payable where supplier_id = 'e21a0101-0000-4000-8000-000000000002'::uuid);
delete from biz_payable where supplier_id = 'e21a0101-0000-4000-8000-000000000002'::uuid;
delete from biz_stock_out where warehouse_id = 'e21a0101-0000-4000-8000-000000000003'::uuid;
delete from biz_stock_in where warehouse_id = 'e21a0101-0000-4000-8000-000000000003'::uuid;

delete from biz_inventory_txn where warehouse_id = 'e21a0101-0000-4000-8000-000000000003'::uuid;
delete from biz_inventory_location where warehouse_id = 'e21a0101-0000-4000-8000-000000000003'::uuid;
delete from biz_inventory where warehouse_id = 'e21a0101-0000-4000-8000-000000000003'::uuid;
delete from biz_material_serial where warehouse_id = 'e21a0101-0000-4000-8000-000000000003'::uuid;

delete from biz_material where id in
    ('e21a0101-0000-4000-8000-000000000011'::uuid, 'e21a0101-0000-4000-8000-000000000012'::uuid);
delete from biz_warehouse where id = 'e21a0101-0000-4000-8000-000000000003'::uuid;
delete from biz_supplier where id = 'e21a0101-0000-4000-8000-000000000002'::uuid;
delete from biz_unit where id = 'e21a0101-0000-4000-8000-000000000001'::uuid;

-- 验收期间把 admin 挂到了 P21E2E 采购部，清场时还原为「无部门」
update sys_user set dept_id = null
 where username = 'admin' and dept_id = 'e21a0101-0000-4000-8000-000000000021'::uuid;
delete from sys_dept where id = 'e21a0101-0000-4000-8000-000000000021'::uuid;
