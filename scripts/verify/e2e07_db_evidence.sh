#!/usr/bin/env bash
# E2E-07 DB 层证据快照（在 145 上跑）
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
P() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off "$@"; }
echo "===== 1) 四类合同（biz_contract）====="
P -c "select contract_no,contract_type,status,amount,party_b_type,party_b_name,sign_date,start_date,end_date,coalesce(approval_instance_id::text,'-') from biz_contract where contract_name like 'E2E07-%' order by contract_type"

echo "===== 2) 状态日志（biz_status_log）====="
P -c "select c.contract_no,l.from_status,l.to_status,coalesce(l.reason,'-'),l.operator_name,l.created_at from biz_status_log l join biz_contract c on c.id=l.entity_id where c.contract_name like 'E2E07-%' order by c.contract_no,l.created_at"

echo "===== 3) 变更留痕（sys_change_log）====="
P -c "select resource,resource_id,action,changed_fields,operator_name,diff::text,created_at from sys_change_log where resource='biz_contract' and resource_id in (select id::text from biz_contract where contract_name like 'E2E07-%') order by created_at"

echo "===== 4) 履约：采购单 ====="
P -c "select c.contract_no,o.order_no,o.status,o.total_with_tax,o.supplier_id=o.supplier_id as x,o.contract_no_snapshot from biz_purchase_order o join biz_contract c on c.id=o.contract_id where c.contract_name like 'E2E07-%' order by o.order_no"
echo "===== 5) 履约：销售单 ====="
P -c "select c.contract_no,o.so_no,o.status,o.total_with_tax from biz_sales_order o join biz_contract c on c.id=o.contract_id where c.contract_name like 'E2E07-%' order by o.so_no"

echo "===== 6) 履约口径（DB 现算 vs 合同额）====="
P -c "select c.contract_no,c.contract_type,c.status,c.amount,
 coalesce((select sum(o.total_with_tax) from biz_purchase_order o where o.contract_id=c.id and o.status<>'CANCELLED' and o.deleted_at is null),0)
 +coalesce((select sum(o.total_with_tax) from biz_sales_order o where o.contract_id=c.id and o.status<>'CANCELLED' and o.deleted_at is null),0) as committed,
 (select count(*) from biz_purchase_order o where o.contract_id=c.id and o.status not in ('RECEIVED','CLOSED','CANCELLED') and o.deleted_at is null)
 +(select count(*) from biz_sales_order o where o.contract_id=c.id and o.status not in ('OUTBOUND','CLOSED','CANCELLED') and o.deleted_at is null) as open_orders
 from biz_contract c where c.contract_name like 'E2E07-%' order by c.contract_type"

echo "===== 7) 审批任务（biz_flow_task）====="
P -c "select c.contract_no,t.node_key,t.status,coalesce(u.username,'-'),t.is_self_approval from biz_flow_task t join biz_flow_instance i on i.id=t.instance_id join biz_contract c on c.id=i.biz_id left join sys_user u on u.id=t.assignee_id where c.contract_name like 'E2E07-%' order by c.contract_no,t.node_key,t.created_at"

echo "===== 8) 联动/提醒通知（sys_notice）====="
P -c "select n.biz_type,n.title,n.idempotency_key,n.receiver_count,n.status from sys_notice n where n.biz_id in (select id::text from biz_contract where contract_name like 'E2E07-%') order by n.created_at"

echo "===== 9) 到期提醒台账（biz_reminder_log）====="
P -c "select c.contract_no,r.remind_kind,r.stage,r.remind_date,r.receiver_count,r.sent_at from biz_reminder_log r join biz_contract c on c.id::text=r.doc_id where c.contract_name like 'E2E07-%'"

echo "===== 10) 合同附件（biz_doc_attachment + sys_file）====="
P -c "select c.contract_no,a.file_role,a.uploader_name,coalesce(f.name,'-'),a.created_at from biz_doc_attachment a join biz_contract c on c.id=a.doc_id left join sys_file f on f.id=a.file_id where c.contract_name like 'E2E07-%'"

echo "===== 11) 临时联动规则已清理 ====="
P -c "select count(*) from biz_flow_linkage_rule where name like 'E2E07 临时%'"

echo "===== 12) 合同不记账（凭证 / 库存流水）====="
P -c "select count(*) from biz_voucher where source_no in (select contract_no from biz_contract where contract_name like 'E2E07-%')"
