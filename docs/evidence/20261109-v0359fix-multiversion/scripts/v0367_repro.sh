#!/bin/bash
# 复现生产故障：144 备份副本 + V0359 → uk_biz_flow_def_version（不改 V0359，纯复现）
set -uo pipefail
DB=cloudpivot_preview3
DUMP=/tmp/prod-221715.dump
LOG=/tmp/repro_v0359.log

su - postgres -c "dropdb --if-exists $DB; createdb -O cloudpivot $DB" || exit 1
su - postgres -c "pg_restore --role=cloudpivot -d $DB $DUMP" 2>&1 | tail -2

{
echo "======================================================================"
echo "复现环境：145 上从 144 生产备份（cloudpivot-20261109-221715.dump）恢复的 $DB"
echo "生产库状态：295 条迁移，V0359 被手工跳过（manual-skip-deploy-incident），V0359 从未真跑"
echo "======================================================================"
echo
echo "--- ① 复现前的版本分布（po_approve 有 v1..v4，其中 v3/v4 是 DRAFT） ---"
su - postgres -c "psql -d $DB -c \"select d.flow_code, v.version_no, v.version_status, v.is_current, d.current_version_no from biz_flow_def d join biz_flow_def_version v on v.def_id=d.id where d.flow_code in ('po_approve','contract_approve','payable_pay') order by 1,2\""
echo "--- ② 当前版本里 APPROVAL+ROLE+ALL 的节点（V0359 要改的目标） ---"
su - postgres -c "psql -d $DB -c \"select d.flow_code, v.version_no, n.node_key, n.approval_mode from biz_flow_def d join biz_flow_def_version v on v.def_id=d.id and v.is_current join biz_flow_node n on n.version_id=v.id where d.deleted_at is null and n.node_type='APPROVAL' and n.assignee_type='ROLE' and n.approval_mode='ALL' and d.flow_code in ('po_approve','contract_approve','payable_pay') order by 1,3\""
echo
echo "--- ③ 跑 V0359（单事务，与迁移器同口径：失败即整体回滚） ---"
su - postgres -c "psql -d $DB -v ON_ERROR_STOP=1 -1 -f /tmp/V0359.up.sql"
echo "psql_exit=$?"
echo
echo "--- ④ 回滚后复核：一条也没改（事务整体回滚） ---"
su - postgres -c "psql -d $DB -c \"select d.flow_code, v.version_no, v.version_status, v.is_current from biz_flow_def d join biz_flow_def_version v on v.def_id=d.id where d.flow_code in ('po_approve','contract_approve','payable_pay') order by 1,2\""
su - postgres -c "psql -d $DB -tAc \"select 'bad_nodes_after_rollback=' || count(*) from biz_flow_def d join biz_flow_def_version v on v.def_id=d.id and v.is_current join biz_flow_node n on n.version_id=v.id where d.deleted_at is null and n.node_type='APPROVAL' and n.assignee_type='ROLE' and n.approval_mode='ALL'\""
} > "$LOG" 2>&1

grep -nE "NOTICE|ERROR|DETAIL|bad_nodes_after" "$LOG" | head -20
