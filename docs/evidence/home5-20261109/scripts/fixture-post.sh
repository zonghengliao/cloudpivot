#!/usr/bin/env bash
# 夹具收尾：一张报销单去掉金额（验「无金额不渲染」），一张报销单挪到 8 天前（验「超过 7 天显示日期」且仍在首 10 条内）。
set -eu
DB=cloudpivot_home5_v4
sudo -n -u postgres psql -d "$DB" -q -c "UPDATE biz_flow_instance SET form_data = form_data - 'amount' WHERE biz_no='EXP-20261106-0004';"
sudo -n -u postgres psql -d "$DB" -q -c "UPDATE biz_flow_task t SET created_at = now() - interval '8 days' FROM biz_flow_instance i WHERE i.id=t.instance_id AND i.biz_no='EXP-20261106-0004';"
sudo -n -u postgres psql -d "$DB" -c "SELECT i.biz_no, i.form_data->>'amount' AS amount, t.created_at FROM biz_flow_task t JOIN biz_flow_instance i ON i.id=t.instance_id WHERE t.status='PENDING' ORDER BY t.created_at DESC;"
