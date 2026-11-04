\pset pager off
\pset footer off
\echo '=== Y1 库存 vs 流水（按 物料×仓库 汇总，两种口径） ==='
select 'sum_inv' k, coalesce(sum(qty),0) qty, coalesce(sum(amount),0) amt from biz_inventory;
select 'sum_txn' k, coalesce(sum(qty),0) qty from biz_inventory_txn;
select 'per_mat_wh' k, i.material_id, i.warehouse_id, i.inv_qty, coalesce(t.txn_qty,0) txn_qty, i.inv_qty - coalesce(t.txn_qty,0) diff
from (select material_id, warehouse_id, sum(qty) inv_qty from biz_inventory group by 1,2) i
left join (select material_id, warehouse_id, sum(qty) txn_qty from biz_inventory_txn group by 1,2) t
  on t.material_id=i.material_id and t.warehouse_id=i.warehouse_id
order by 1,2;
select 'loc_sum' k, coalesce(sum(qty),0) from biz_inventory_location;

\echo '=== Y2 应付构成（来源类型） ==='
select 'payable_src' k, source_type, count(*), coalesce(sum(amount),0), coalesce(sum(paid_amount),0) from biz_payable where deleted_at is null group by 2 order by 2;
select 'payable_vs_stockin_detail' k, p.payable_no, p.source_type, p.source_no, p.amount,
  (select s.total_amount from biz_stock_in s where s.in_no = p.source_no) stockin_amt
from biz_payable p where p.deleted_at is null order by 2;

\echo '=== Y3 孤儿审批实例明细 ==='
select 'orphan' k, f.biz_no, f.biz_type, f.status, f.created_at from biz_flow_instance f
  where f.biz_type in ('PO_APPROVE','STOCK_IN_POST','CONTRACT_APPROVE','STOCK_TRANSFER')
    and not exists (select 1 from biz_purchase_order p where p.approval_instance_id=f.id)
    and not exists (select 1 from biz_stock_in s where s.approval_instance_id=f.id)
    and not exists (select 1 from biz_contract c where c.approval_instance_id=f.id)
    and not exists (select 1 from biz_stock_transfer t where t.approval_instance_id=f.id)
  order by f.created_at;

\echo '=== Y4 E2E 残留（活动行） ==='
select 'task_e2e_active' k, count(*) filter (where deleted_at is null) active, count(*) total from biz_task where title like '%E2E%' or task_no like 'E2E%';
select 'task_e2e_titles' k, title from biz_task where (title like '%E2E%' or task_no like 'E2E%') and deleted_at is null limit 5;
select 'warehouse_e2e' k, warehouse_code, warehouse_name, deleted_at is not null soft_del from biz_warehouse order by 2;
select 'user_test' k, username, real_name from sys_user where username like '%test%' or username like 'qa.%' or username like 'E2E%' order by 2;

\echo '=== Y5 科目叶子数 ==='
select 'acc_leaf' k, count(*) filter (where is_leaf = true) leaf, count(*) total from biz_account where deleted_at is null;

\echo '=== Y6 任务语义/看板列（自定义列） ==='
select 'task_semantic_null' k, count(*) from biz_task where status_semantic is null and deleted_at is null;

\echo '=== Y7 审批实例耗时（超时/长挂） ==='
select 'inst_pending' k, count(*) from biz_flow_instance where status='PENDING';
select 'task_pending' k, count(*) , min(created_at) oldest from biz_flow_task where status='PENDING';

\echo '=== Y8 文件存储策略/实际后端 ==='
select 'file_by_storage' k, storage_code, count(*), sum(file_size) from sys_file where deleted_at is null group by 2 order by 3 desc;
select 'storage_cfg' k, storage_code, backend_type, is_default from sys_storage_config;

\echo '=== Y9 变更日志覆盖 ==='
select 'changelog_module' k, module, count(*) from sys_change_log group by 2 order by 3 desc limit 15;

\echo '=== Y10 通知模板与已读率 ==='
select 'notice_read_rate' k, count(*) recv, count(*) filter (where read_flag) readed, round(100.0*count(*) filter (where read_flag)/greatest(count(*),1),1) pct from sys_notice_receiver;

\echo '=== Y11 期间结账状态与凭证数量 ==='
select 'period_docs' k, p.period_code, p.status,
  (select count(*) from biz_voucher v where v.period=p.period_code) vouchers
from biz_accounting_period p order by 2;
