#!/bin/bash
# E2E-12 收尾：只清本卡前缀数据（E2E12 / e2e12_），不 DROP/TRUNCATE，不动他人数据
set -u
export PGPASSWORD=$(grep -E '^NIK_DATABASE_PASSWORD=' /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2-)
psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_dev -At -F'|' -P pager=off <<'SQL'
-- 1) 我的采购单附件绑定 + 文件（先绑定后文件：子表外键在绑定行上）
delete from biz_doc_attachment a using sys_file f
 where f.id = a.file_id and f.original_name like 'E2E12%';
-- 2) 合同上我传的附件（按文件名前缀）
delete from biz_doc_attachment a using sys_file f
 where f.id = a.file_id and f.original_name like 'E2E12%';
-- 3) 我的盘点单附件（文件名可能不带前缀 → 按 doc 归属）
delete from biz_doc_attachment a
 using biz_stock_take t
 where a.doc_id = t.id and a.doc_type = 'STOCK_TAKE' and t.remark like 'E2E12%';
-- 4) sys_file（软删 + 硬删都按前缀；只删物理无人引用的）
delete from sys_file f
 where f.original_name like 'E2E12%'
   and not exists (select 1 from biz_doc_attachment a where a.file_id = f.id);
-- 5) 采购明细 + 采购单
delete from biz_purchase_order_item i using biz_purchase_order o
 where i.order_id = o.id and o.remark like 'E2E12%';
delete from biz_purchase_order where remark like 'E2E12%';
-- 6) 盘点单（明细 → 单头）
delete from biz_stock_take_item i using biz_stock_take t
 where i.take_id = t.id and t.remark like 'E2E12%';
delete from biz_stock_take where remark like 'E2E12%';
-- 7) 规则（软删的也清掉，避免残留）
delete from biz_attachment_rule where remark like 'E2E12%';
-- 8) 用户 / 角色
delete from sys_user_role where user_id in (select id from sys_user where username like 'e2e12%');
delete from sys_role_menu where role_id in (select id from sys_role where role_code like 'E2E12%');
delete from sys_user where username like 'e2e12%';
delete from sys_role where role_code like 'E2E12%';
\echo '=== 收尾后残留校验（全部应为 0） ==='
select 'po='||(select count(*) from biz_purchase_order where remark like 'E2E12%')
     ||' st='||(select count(*) from biz_stock_take where remark like 'E2E12%')
     ||' att='||(select count(*) from biz_doc_attachment a join sys_file f on f.id=a.file_id where f.original_name like 'E2E12%')
     ||' file='||(select count(*) from sys_file where original_name like 'E2E12%')
     ||' role='||(select count(*) from sys_role where role_code like 'E2E12%')
     ||' user='||(select count(*) from sys_user where username like 'e2e12%')
     ||' rule='||(select count(*) from biz_attachment_rule where remark like 'E2E12%')
     ||' po_item='||(select count(*) from biz_purchase_order_item i
        where not exists (select 1 from biz_purchase_order o where o.id=i.order_id));
SQL
