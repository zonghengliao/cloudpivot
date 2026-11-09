#!/usr/bin/env bash
# 回填「到达时间」（测试夹具）：让卡片同时验到 分钟 / 小时 / 天 / 超过 7 天日期 四种文案。
set -eu
DB=cloudpivot_home5_v4
Q() { sudo -n -u postgres psql -d "$DB" -q -c "$1"; }
R() { Q "UPDATE biz_flow_task t SET created_at = now() - interval '$2' FROM biz_flow_instance i WHERE i.id = t.instance_id AND i.biz_no = '$1';"; }

R PO-20261109-000003 "30 minutes"
R EXP-20261109-0001  "45 minutes"
R PO-20261109-000004 "2 hours"
R EXP-20261109-0002  "3 hours"
R CT-20261109-000001 "5 hours"
R PO-20261108-000002 "6 hours"
R EXP-20261107-0003  "1 day"
R CT-20261108-000002 "2 days"
R PO-20261107-000001 "3 days"
R EXP-20261106-0004 "4 days"
R PO-20261105-000001 "8 days"
R CT-20261101-000003 "9 days"

sudo -n -u postgres psql -d "$DB" -c "SELECT i.biz_no, t.status, t.created_at FROM biz_flow_task t JOIN biz_flow_instance i ON i.id=t.instance_id ORDER BY t.created_at DESC;"
