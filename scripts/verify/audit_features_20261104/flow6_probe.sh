#!/usr/bin/env bash
# 取流程 V3 校验结果 + 节点坐标是否落库（链路⑥ 佐证）
set -uo pipefail
DB=cloudpivot_audit
DEF=20000000-0000-4000-8000-0000000a0182
VID=$(PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc "select id from biz_flow_def_version where def_id='$DEF' and version_no=3")
echo "vid=$VID"
LOGIN=$(curl -s -X POST http://127.0.0.1:18099/api/system/auth/login -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456-audit1"}')
TOKEN=$(printf '%s' "$LOGIN" | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')
echo "token_len=${#TOKEN}"
echo "--- POST /version/$VID/validate ---"
curl -s -X POST "http://127.0.0.1:18099/api/business/flow/version/$VID/validate" -H "Authorization: Bearer $TOKEN" | head -c 2000
echo
echo "--- POST /def/$DEF/publish ---"
curl -s -X POST "http://127.0.0.1:18099/api/business/flow/def/$DEF/publish" -H "Authorization: Bearer $TOKEN" | head -c 800
echo
echo "--- v3 节点/边数（草稿存普通表，publish 才写 graph_snapshot）---"
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc "select (select count(*) from biz_flow_node where version_id='$VID') as nodes, (select count(*) from biz_flow_edge where version_id='$VID') as edges"
echo "--- 节点坐标列 ---"
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc "select column_name from information_schema.columns where table_name='biz_flow_node' order by ordinal_position" | tr '\n' ' '
echo
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -F'|' -Atc "select node_key, coalesce(pos_x::text,'-'), coalesce(pos_y::text,'-') from biz_flow_node where version_id='$VID' order by node_key" | head -12
