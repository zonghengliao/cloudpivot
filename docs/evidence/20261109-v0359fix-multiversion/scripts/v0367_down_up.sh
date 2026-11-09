#!/bin/bash
# V0367 down / up 往返验证（在 cloudpivot_preview 上：它已由 psql 路径跑过 V0367）
set -uo pipefail
DB=cloudpivot_preview
ps() { su - postgres -c "psql -d $DB -tAc \"$1\""; }
show() {
  su - postgres -c "psql -d $DB -c \"select d.flow_code, v.version_no, v.version_status, v.is_current, d.current_version_no from biz_flow_def d join biz_flow_def_version v on v.def_id=d.id where d.flow_code in ('po_approve','contract_approve') order by 1,2\""
}

echo "=== 回滚前 ==="
show
ps "select 'v0367_versions=' || count(*) from biz_flow_def_version where change_note like 'V0367：%'"

echo "=== 执行 down ==="
su - postgres -c "psql -d $DB -v ON_ERROR_STOP=1 -1 -f /tmp/V0367__flow_all_role_approval_to_any_multiversion.down.sql" 2>&1 | grep -E "NOTICE|ERROR" | head -20

echo "=== 回滚后（po_approve 应回到 v2 PUBLISHED，v5 已删） ==="
show
ps "select 'v0367_versions=' || count(*) from biz_flow_def_version where change_note like 'V0367：%'"
ps "select 'bad_nodes_after_down=' || count(*) from biz_flow_def d join biz_flow_def_version v on v.def_id=d.id and v.is_current join biz_flow_node n on n.version_id=v.id where d.deleted_at is null and n.node_type='APPROVAL' and n.assignee_type='ROLE' and n.approval_mode='ALL'"

echo "=== 再执行 up（应重新发版） ==="
su - postgres -c "psql -d $DB -v ON_ERROR_STOP=1 -1 -f /tmp/V0367__flow_all_role_approval_to_any_multiversion.up.sql" 2>&1 | grep -E "V0367 完成|收口|ERROR" | head -5
show
ps "select 'bad_nodes_after_reup=' || count(*) from biz_flow_def d join biz_flow_def_version v on v.def_id=d.id and v.is_current join biz_flow_node n on n.version_id=v.id where d.deleted_at is null and n.node_type='APPROVAL' and n.assignee_type='ROLE' and n.approval_mode='ALL'"
