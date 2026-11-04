\pset pager off
\pset footer off
\echo '=== X1 软删除残留（total vs active） ==='
select 'task' t, count(*) total, count(*) filter (where deleted_at is null) active from biz_task
union all select 'flow_def', count(*), count(*) filter (where deleted_at is null) from biz_flow_def
union all select 'stock_in', count(*), count(*) filter (where deleted_at is null) from biz_stock_in
union all select 'stock_out', count(*), count(*) filter (where deleted_at is null) from biz_stock_out
union all select 'po', count(*), count(*) filter (where deleted_at is null) from biz_purchase_order
union all select 'contract', count(*), count(*) filter (where deleted_at is null) from biz_contract
union all select 'material', count(*), count(*) filter (where deleted_at is null) from biz_material
union all select 'warehouse', count(*), count(*) filter (where deleted_at is null) from biz_warehouse
union all select 'supplier', count(*), count(*) filter (where deleted_at is null) from biz_supplier
union all select 'transfer', count(*), count(*) filter (where deleted_at is null) from biz_stock_transfer
order by 1;

\echo '=== X2 E2E 残留命名（E2E- / TI- / 测试 前缀） ==='
select 'e2e_warehouse' k, count(*) from biz_warehouse where warehouse_code like 'E2E-%';
select 'e2e_task' k, count(*) from biz_task where title like '%E2E%' or task_no like 'E2E%';
select 'e2e_stockin' k, count(*) from biz_stock_in where in_no like 'E2E%';
select 'test_user' k, count(*) from sys_user where username like '%test%' or username like 'e2e%';

\echo '=== X3 会计勾稽：库存金额 vs 1405 科目余额 ==='
select 'inv_amount' k, coalesce(sum(amount),0) from biz_inventory where qty > 0;
select 'acc_1405' k, coalesce(sum(e.debit_amount - e.credit_amount),0) bal
  from biz_voucher_entry e join biz_voucher v on v.id = e.voucher_id
  where e.account_code like '1405%' and v.status='POSTED';
select 'acc_all_leaf' k, count(*) from biz_account where is_leaf and deleted_at is null;

\echo '=== X4 凭证头-明细一致性 ==='
select 'voucher_head_vs_entry' k, count(*) bad from (
  select v.id from biz_voucher v join biz_voucher_entry e on e.voucher_id=v.id
  group by v.id, v.total_debit, v.total_credit, v.entry_count
  having abs(sum(e.debit_amount)-v.total_debit) > 0.0001 or abs(sum(e.credit_amount)-v.total_credit) > 0.0001 or count(*) <> v.entry_count
) t;
select 'voucher_red_cnt' k, count(*) filter (where is_red = 1) red, count(*) filter (where reverse_of is not null) rev from biz_voucher;

\echo '=== X5 单据头-明细一致性（收货/出库/采购） ==='
select 'stockin_head_vs_item' k, count(*) bad from (
  select s.id from biz_stock_in s join biz_stock_in_item i on i.stock_in_id=s.id
  group by s.id, s.total_amount, s.total_qty
  having abs(sum(i.amount)-s.total_amount) > 0.0001 or abs(sum(i.qty)-s.total_qty) > 0.0001
) t;
select 'po_head_vs_item' k, count(*) bad from (
  select p.id from biz_purchase_order p join biz_purchase_order_item i on i.order_id=p.id
  group by p.id, p.total_amount, p.total_qty
  having abs(sum(i.amount)-p.total_amount) > 0.0001 or abs(sum(i.qty)-p.total_qty) > 0.0001
) t;
select 'stockin_over_receipt' k, count(*) over_recv from (
  select i.source_item_id, sum(i.qty) got from biz_stock_in_item i
  join biz_stock_in s on s.id=i.stock_in_id
  where s.status='POSTED' and i.source_item_id is not null group by 1
) g join biz_purchase_order_item poi on poi.id = g.source_item_id where g.got > poi.qty + 0.0001;

\echo '=== X6 库存流水累计 = 库存（B4 不变量预演） ==='
select 'txn_vs_inv' k, count(*) mismatched from (
  select i.material_id, i.warehouse_id, i.qty as inv_qty, coalesce(sum(t.qty),0) txn_qty
  from biz_inventory i left join biz_inventory_txn t
    on t.material_id=i.material_id and t.warehouse_id=i.warehouse_id
  group by i.material_id, i.warehouse_id, i.qty
  having abs(coalesce(sum(t.qty),0) - i.qty) > 0.0001
) s;

\echo '=== X7 应付 vs 收货（勾稽） ==='
select 'payable_vs_stockin' k, count(*) payables, coalesce(sum(p.amount),0) amt,
   (select coalesce(sum(s.total_amount),0) from biz_stock_in s where s.status='POSTED') posted_stockin_amt
from biz_payable p where p.deleted_at is null;
select 'payable_paid_detail' k, count(*) payables, coalesce(sum(paid_amount),0) paid, coalesce(sum(unpaid_amount),0) unpaid from biz_payable where deleted_at is null;
select 'payment_any' k, count(*) from biz_payment;

\echo '=== X8 审批实例 ↔ 业务单据 双向孤儿 ==='
select 'po_instance_missing' k, count(*) from biz_purchase_order p
  where p.approval_instance_id is not null and not exists (select 1 from biz_flow_instance f where f.id=p.approval_instance_id);
select 'stockin_instance_missing' k, count(*) from biz_stock_in s
  where s.approval_instance_id is not null and not exists (select 1 from biz_flow_instance f where f.id=s.approval_instance_id);
select 'payable_instance_missing' k, count(*) from biz_payable p
  where p.payment_approval_instance_id is not null and not exists (select 1 from biz_flow_instance f where f.id=p.payment_approval_instance_id);
select 'voucher_instance_missing' k, count(*) from biz_voucher v
  where v.pending_reverse_instance_id is not null and not exists (select 1 from biz_flow_instance f where f.id=v.pending_reverse_instance_id);
select 'instance_orphan_doc' k, count(*) from biz_flow_instance f
  where f.biz_type in ('PO_APPROVE','STOCK_IN_POST','CONTRACT_APPROVE','STOCK_TRANSFER')
    and not exists (select 1 from biz_purchase_order p where p.approval_instance_id=f.id)
    and not exists (select 1 from biz_stock_in s where s.approval_instance_id=f.id)
    and not exists (select 1 from biz_contract c where c.approval_instance_id=f.id)
    and not exists (select 1 from biz_stock_transfer t where t.approval_instance_id=f.id);

\echo '=== X9 任务层级最大值（8 层声称） ==='
with recursive t as (
  select id, parent_id, 1 lvl from biz_task where deleted_at is null and parent_id is null
  union all
  select b.id, b.parent_id, t.lvl+1 from biz_task b join t on b.parent_id=t.id where b.deleted_at is null
) select 'max_depth' k, max(lvl) from t;

\echo '=== X10 状态机/配置类表内容 ==='
select 'state_edge_rows' k, count(*) from biz_flow_state_edge;
select 'linkage_rule_rows' k, count(*) from biz_flow_linkage_rule;
select 'field_mask_detail' k, table_name, column_name, mask_type, role_id from sys_field_mask order by 2,3;

\echo '=== X11 通知投递渠道 ==='
select 'notice_delivery' k, count(*), string_agg(distinct status,',') st from sys_notice_delivery;
select 'notice_status' k, status, count(*) from sys_notice group by 2 order by 2;

\echo '=== X12 期间与凭证覆盖 ==='
select 'voucher_period' k, period, count(*) from biz_voucher group by 2 order by 2;
select 'period_status' k, period_code, status from biz_accounting_period order by 2;

\echo '=== X13 用户/角色/权限 ==='
select 'user_list' k, username, real_name, status from sys_user where deleted_at is null order by 2;
select 'role_list' k, role_code, role_name from sys_role where deleted_at is null order by 2;
select 'dead_permission' k, count(*) from sys_menu m where m.auth_code is not null and m.auth_code <> '' and m.deleted_at is null;
