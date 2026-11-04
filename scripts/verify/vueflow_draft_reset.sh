#!/usr/bin/env bash
# 把测试用流程定义重置回「只有已发布版本」的状态（VUE-FLOW-2B 复现专用）
# 用法: bash scripts/verify/vueflow_draft_reset.sh [def_id] [keep_version_no]
set -euo pipefail

DEF="${1:-20000000-0000-4000-8000-0000000a0182}"
KEEP="${2:-2}"
PSQL="env PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_vf2 -v ON_ERROR_STOP=1"

$PSQL <<SQL
delete from biz_flow_edge where version_id in (
  select id from biz_flow_def_version where def_id='$DEF' and version_no > $KEEP);
delete from biz_flow_node where version_id in (
  select id from biz_flow_def_version where def_id='$DEF' and version_no > $KEEP);
delete from biz_flow_def_version where def_id='$DEF' and version_no > $KEEP;
update biz_flow_def_version set is_current = (version_no = $KEEP) where def_id='$DEF';
SQL
$PSQL -c "select version_no, version_status, is_current, node_count, edge_count from biz_flow_def_version where def_id='$DEF' order by version_no;"
