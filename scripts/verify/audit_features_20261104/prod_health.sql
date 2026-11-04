\pset pager off
\pset footer off
\timing off
\echo '=== 00 meta ==='
select current_database() db, now() at time zone 'Asia/Shanghai' ts,
       (select max(version::int) from schema_migrations where success) maxver,
       (select count(*) from information_schema.tables where table_schema='public') tbls;

\echo '=== 00b 全表行数（0 行 = 从未被使用） ==='
select t.table_name, (xpath('/row/c/text()', query_to_xml(format('select count(*) as c from public.%I', t.table_name), false, true, '')))[1]::text::int as rows
from information_schema.tables t
where t.table_schema='public' and t.table_type='BASE TABLE'
order by 2 desc, 1;

\echo '=== 01 任务管理 biz_task ==='
select 'task_status' k, status, count(*) from biz_task where deleted_at is null group by status order by 2;
select 'task_total' k, count(*) total, count(distinct project_id) proj, count(distinct parent_id) parents, count(distinct owner_id) owners, count(*) filter (where parent_id is not null) with_parent, count(*) filter (where need_review) need_review from biz_task where deleted_at is null;
select 'task_semantic' k, status_semantic, count(*) from biz_task where deleted_at is null group by status_semantic order by 2;
select 'task_exec' k, exec_mode, count(*) from biz_task where deleted_at is null group by exec_mode order by 2;
select 'task_depth' k, lvl, count(*) from (
  with recursive t as (
    select id, parent_id, 1 lvl from biz_task where deleted_at is null and parent_id is null and project_id is null
    union all
    select b.id, b.parent_id, t.lvl+1 from biz_task b join t on b.parent_id = t.id where b.deleted_at is null
  ) select lvl from t
) s group by lvl order by 2;
select 'task_dep' k, count(*) from biz_task_dependency;
select 'task_worklog' k, count(*), coalesce(sum(hours),0) from biz_task_worklog;
select 'task_template' k, count(*) from biz_task_template;
select 'task_comment' k, count(*) from biz_task_comment;
select 'task_member' k, count(*) from biz_task_member;
select 'task_attachment' k, count(*) from biz_task_attachment;
select 'task_hermes' k, count(*) filter (where hermes_task_id is not null) synced from biz_task;
select 'task_ai_token' k, count(*) from biz_task_ai_token;

\echo '=== 02 审批流 flow ==='
select 'flowdef' k, count(*) defs, count(*) filter (where is_system) system, count(*) filter (where is_default) dft, string_agg(distinct status,',') st from biz_flow_def where deleted_at is null;
select 'flowdef_list' k, flow_code, flow_name, status, current_version_no from biz_flow_def where deleted_at is null order by 2;
select 'flowdef_ver' k, count(*), count(*) filter (where is_current) cur from biz_flow_def_version;
select 'flowinst' k, status, count(*) from biz_flow_instance group by status order by 3 desc;
select 'flowinst_biz' k, biz_type, count(*) from biz_flow_instance group by biz_type order by 3 desc;
select 'flowtask' k, status, count(*) from biz_flow_task group by status order by 3 desc;
select 'flownode' k, count(*) from biz_flow_node;
select 'flowedge' k, count(*) from biz_flow_edge;
select 'flowassignee' k, count(*) from biz_flow_node_assignee;
select 'flowcc' k, count(*) from biz_flow_cc;
select 'linkage_rule' k, count(*), count(*) filter (where enabled) enabled from biz_flow_linkage_rule;
select 'state_edge' k, count(*), count(*) filter (where enabled) enabled from biz_flow_state_edge;
select 'flow_last' k, max(created_at) last from biz_flow_instance;

\echo '=== 03 采购 purchase + contract ==='
select 'contract' k, status, count(*) from biz_contract where deleted_at is null group by status order by 2;
select 'contract_type' k, contract_type, count(*) from biz_contract where deleted_at is null group by 2 order by 3 desc;
select 'po' k, status, count(*) from biz_purchase_order where deleted_at is null group by status order by 2;
select 'po_type' k, purchase_type, price_mode, count(*) from biz_purchase_order where deleted_at is null group by 2,3 order by 4 desc;
select 'po_total' k, count(*) pos, coalesce(sum(total_amount),0) amt, count(distinct supplier_id) sups, count(*) filter (where over_credit_limit) over_credit from biz_purchase_order where deleted_at is null;
select 'po_item' k, count(*), coalesce(sum(qty),0) qty, coalesce(sum(amount),0) amt from biz_purchase_order_item;
select 'po_link' k, count(*) filter (where contract_id is not null) with_contract, count(*) filter (where contract_id is null) spot from biz_purchase_order where deleted_at is null;

\echo '=== 04 收货 stockin ==='
select 'stockin' k, status, count(*) from biz_stock_in where deleted_at is null group by status order by 2;
select 'stockin_type' k, in_type, count(*) from biz_stock_in where deleted_at is null group by 2 order by 3 desc;
select 'stockin_total' k, count(*), coalesce(sum(total_amount),0) amt from biz_stock_in where deleted_at is null;
select 'stockin_item' k, count(*), coalesce(sum(qty),0) qty, coalesce(sum(amount),0) amt from biz_stock_in_item;
select 'stockin_src' k, count(*) filter (where source_order_id is not null) from_po, count(*) filter (where is_sourceless) sourceless from biz_stock_in where deleted_at is null;
select 'stockin_posted_novoucher' k, count(*) from biz_stock_in where status='POSTED' and voucher_id is null and deleted_at is null;
select 'stockin_cancel' k, count(*) filter (where cancelled_at is not null) cancelled from biz_stock_in where deleted_at is null;

\echo '=== 05 出库 stockout ==='
select 'stockout' k, status, count(*) from biz_stock_out where deleted_at is null group by status order by 2;
select 'stockout_type' k, out_type, count(*) from biz_stock_out where deleted_at is null group by 2 order by 3 desc;
select 'stockout_total' k, count(*), coalesce(sum(total_amount),0) from biz_stock_out where deleted_at is null;
select 'stockout_novoucher' k, count(*) from biz_stock_out where status='POSTED' and voucher_id is null and deleted_at is null;

\echo '=== 06 库存 inventory ==='
select 'inv' k, count(*) rows, coalesce(sum(qty),0) qty, coalesce(sum(amount),0) amt, count(*) filter (where qty < 0) neg, count(*) filter (where location_id is not null) with_loc from biz_inventory;
select 'inv_loc' k, count(*), coalesce(sum(qty),0) qty, count(*) filter (where qty<0) neg from biz_inventory_location;
select 'inv_txn' k, count(*), min(created_at) first, max(created_at) last from biz_inventory_txn;
select 'inv_txn_type' k, txn_type, count(*) from biz_inventory_txn group by 2 order by 3 desc;
select 'serial' k, status, count(*) from biz_material_serial group by 2 order by 2;
select 'stocktake' k, status, count(*) from biz_stock_take group by 2 order by 2;
select 'opening_inv' k, count(*), coalesce(sum(qty),0) qty, coalesce(sum(amount),0) amt from biz_opening_inventory;

\echo '=== 07 调拨 transfer ==='
select 'transfer' k, status, count(*) from biz_stock_transfer where deleted_at is null group by 2 order by 2;
select 'transfer_item' k, count(*), coalesce(sum(qty),0) qty, coalesce(sum(shipped_qty),0) shipped, coalesce(sum(received_qty),0) recv from biz_stock_transfer_item;
select 'transfer_transit' k, count(*) filter (where status='IN_TRANSIT') transit, count(*) filter (where overdue_alert_count > 0) alerted from biz_stock_transfer where deleted_at is null;

\echo '=== 08 财务 voucher/payable/payment ==='
select 'voucher' k, status, count(*) from biz_voucher group by 2 order by 2;
select 'voucher_src' k, source_type, count(*) from biz_voucher group by 2 order by 3 desc;
select 'voucher_entry' k, count(*), coalesce(sum(debit_amount),0) dr, coalesce(sum(credit_amount),0) cr,
       coalesce(sum(debit_amount)-sum(credit_amount),0) diff from biz_voucher_entry;
select 'voucher_imbalance' k, count(*) from (select voucher_id from biz_voucher_entry group by voucher_id having abs(sum(debit_amount)-sum(credit_amount)) > 0.0001) t;
select 'voucher_red' k, count(*) filter (where is_red) red from biz_voucher;
select 'voucher_rule' k, count(*) from biz_voucher_rule;
select 'payable' k, status, count(*) from biz_payable where deleted_at is null group by 2 order by 2;
select 'payable_amt' k, coalesce(sum(amount),0) total, coalesce(sum(paid_amount),0) paid, coalesce(sum(unpaid_amount),0) unpaid, count(*) filter (where abs(amount-paid_amount-unpaid_amount)>0.0001) bad from biz_payable where deleted_at is null;
select 'payment' k, count(*), coalesce(sum(amount),0) from biz_payment where deleted_at is null;

\echo '=== 09 报表 report ==='
select 'report_item' k, report_code, count(*) from biz_report_item group by 2 order by 2;
select 'cashflow_item' k, count(*) from biz_cashflow_item;

\echo '=== 10 期间/年结 period ==='
select 'period' k, status, count(*) from biz_accounting_period group by 2 order by 2;
select 'period_all' k, count(*), min(period_code) minp, max(period_code) maxp from biz_accounting_period;
select 'opening_bal' k, count(*), coalesce(sum(debit_opening),0) dr, coalesce(sum(credit_opening),0) cr from biz_opening_balance;

\echo '=== 11 总账：科目余额 vs 凭证 ==='
select 'gl_vs_entry' k, coalesce(sum(debit_opening),0) bal_dr, coalesce(sum(credit_opening),0) bal_cr from biz_opening_balance;

\echo '=== 12 基础数据 master ==='
select 'material' k, count(*), count(*) filter (where status=1) enabled, count(*) filter (where serial_required) serial from biz_material where deleted_at is null;
select 'material_type' k, material_type, count(*) from biz_material where deleted_at is null group by 2 order by 3 desc;
select 'supplier' k, count(*), count(*) filter (where status=1) enabled from biz_supplier where deleted_at is null;
select 'warehouse' k, count(*) from biz_warehouse where deleted_at is null;
select 'location' k, count(*) from biz_location where deleted_at is null;
select 'account' k, count(*) from biz_account where deleted_at is null;
select 'unit' k, count(*) from biz_unit where deleted_at is null;

\echo '=== 13 项目 project ==='
select 'project' k, status, count(*) from biz_project where deleted_at is null group by 2 order by 2;
select 'project_member' k, count(*) from biz_project_member;

\echo '=== 14 系统 system ==='
select 'user' k, count(*), count(*) filter (where status=1) enabled, count(*) filter (where last_login_at is not null) logged from sys_user where deleted_at is null;
select 'role' k, count(*) from sys_role where deleted_at is null;
select 'menu' k, count(*), count(*) filter (where auth_code is not null and auth_code <> '') with_code from sys_menu where deleted_at is null;
select 'dept' k, count(*) from sys_dept where deleted_at is null;
select 'dict_type' k, count(*) from sys_dict_type where deleted_at is null;
select 'auth_log' k, count(*), max(created_at) last from auth_logs;

\echo '=== 15 数据权限 ==='
select 'field_mask' k, count(*) from sys_field_mask;
select 'role_dept' k, count(*) from sys_role_dept;
select 'role_mutex' k, count(*) from sys_role_mutex;

\echo '=== 16 协作 collab ==='
select 'announcement' k, status, count(*) from sys_announcement where deleted_at is null group by 2 order by 2;
select 'ann_read' k, count(*) from sys_announcement_read;
select 'notice' k, count(*) from sys_notice;
select 'notice_recv' k, count(*), count(*) filter (where read_flag) readed from sys_notice_receiver;
select 'notice_tpl' k, count(*) from sys_notice_template;
select 'notice_delivery' k, count(*), count(*) filter (where status=0) st0, string_agg(distinct status::text,',') st from sys_notice_delivery;

\echo '=== 17 文件存储 storage ==='
select 'file' k, count(*), coalesce(sum(file_size),0) bytes from sys_file where deleted_at is null;
select 'storage_cfg' k, count(*), count(*) filter (where is_default) dft from sys_storage_config;
select 'storage_policy' k, count(*) from sys_storage_policy;
select 'doc_attach' k, count(*) from biz_doc_attachment;
select 'ann_attach' k, count(*) from sys_announcement_attachment;

\echo '=== 18 导入导出 impexp ==='
select 'import_task' k, status, count(*) from sys_import_task group by 2 order by 2;
select 'import_err' k, count(*) from sys_import_task_error;
select 'export_task' k, status, count(*) from sys_export_task group by 2 order by 2;

\echo '=== 19 帮助/变更日志 ==='
select 'help_doc' k, count(*), count(*) filter (where status='PUBLISHED') published from sys_help_doc where deleted_at is null;
select 'help_ver' k, count(*) from sys_help_doc_version;
select 'help_cat' k, count(*) from sys_help_category;
select 'change_log' k, count(*), min(created_at) first, max(created_at) last from sys_change_log;

\echo '=== 20 定时任务 job ==='
select 'job' k, status, count(*) from sys_job where deleted_at is null group by 2 order by 2;
select 'job_log' k, count(*), max(created_at) last from sys_job_log;
select 'job_list' k, job_name, cron_expr, status from sys_job where deleted_at is null order by 1;

\echo '=== 24 巡检/日志 ==='
select 'operation_log' k, count(*), max(created_at) last from operation_logs;
select 'audit_log' k, count(*) from audit_logs;
select 'reminder_log' k, count(*) from biz_reminder_log;
select 'status_log' k, count(*) from biz_status_log;
