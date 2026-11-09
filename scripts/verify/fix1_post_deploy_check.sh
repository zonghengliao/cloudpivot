#!/usr/bin/env bash
# V0355 部署后验证：
#   ① 迁移记录 / 版本状态
#   ② 坏口令指纹（password 非 argon2）登录 **401 而不是 500**，且服务日志有点名告警
#   ③ 口令指纹巡检（N3）在当前库为绿
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE=http://127.0.0.1:8033/api
SQ() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off -c "$1"; }

echo "== 1) 迁移与流程版本"
SQ "select 'V0355 已应用='||success||' 于 '||to_timestamp(applied_at) from schema_migrations where version='355'"
SQ "select 'v'||version_no||' '||version_status||' current='||is_current||' hash='||btrim(graph_hash)
      from biz_flow_def_version where def_id='20000000-0000-4000-8000-0000000c0228' order by version_no"
SQ "select 'n_dept → '||assignee_type||' / '||approval_mode||' / superior_levels='||superior_levels||
           ' / 审批人行='||(select count(*) from biz_flow_node_assignee a where a.node_id=n.id)
      from biz_flow_node n join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id
     where d.flow_code='budget_approve' and v.is_current and n.node_key='n_dept'"

echo
echo "== 2) 坏口令指纹：登录 401（改造前 500「服务器内部错误」）"
ADMIN=$(curl -s -m 30 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" | jq -r '.data.access_token')
PROBE=fix1_badpw_probe
if [ -z "$(SQ "select id from sys_user where username='$PROBE'")" ]; then
  curl -s -m 30 -o /tmp/probe_user.json -X POST "$BASE/system/user" --oauth2-bearer "$ADMIN" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"$PROBE\",\"password\":\"Drill@123456\",\"real_name\":\"指纹探针\",\"status\":1,\"role_ids\":[],\"remark\":\"t_1fafa2ae 验证：故意写坏 password 列\"}" >/dev/null
fi
GOOD=$(SQ "select password from sys_user where username='$PROBE'")
SQ "update sys_user set status=1 where username='$PROBE'" >/dev/null   # 上次跑完会停用，这里复位
echo "   探针账号初始指纹前缀：$(printf '%s' "$GOOD" | cut -c1-12)"
SQ "update sys_user set password='x' where username='$PROBE'" >/dev/null
BADPFX=$(SQ "select left(password,4) from sys_user where username='$PROBE'")
echo "   写坏后指纹：$BADPFX（非 argon2）"
HTTP=$(curl -s -m 30 -o /tmp/probe_login.json -w '%{http_code}' -X POST "$BASE/system/auth/login" \
  -H 'Content-Type: application/json' -d "{\"username\":\"$PROBE\",\"password\":\"Drill@123456\"}")
echo "   登录 http=$HTTP  body=$(head -c 160 /tmp/probe_login.json)"
echo "   期望：401 + 20001（用户名或密码错误），而不是 500 + 10004"
echo "   ---- 服务日志里的点名告警（journalctl 最近 3 分钟）："
journalctl -u cloudpivot-test --since '-3min' --no-pager 2>/dev/null \
  | grep -F "$PROBE" | tail -3 | sed 's/^/     /'
echo "   ---- 复位探针（恢复口令指纹 + 停用，避免留一个可登录的号）"
SQ "update sys_user set password='$GOOD', status=0 where username='$PROBE'" >/dev/null
echo "   复位后指纹前缀：$(SQ "select left(password,12) from sys_user where username='$PROBE'")，status=$(SQ "select status from sys_user where username='$PROBE'")"

echo
echo "== 3) 当前库口令指纹统计"
SQ "select '非 argon2 行数='||count(*) from sys_user where deleted_at is null and password not like '\$argon2id\$%'"
