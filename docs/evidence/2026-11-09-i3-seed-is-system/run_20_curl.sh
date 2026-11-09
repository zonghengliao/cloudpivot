#!/usr/bin/env bash
# 145 · 验收②（最终）：干净库 cloudpivot_i3_clean 上起隔离实例 :18039，真 token curl 实测
set -uo pipefail
DB=cloudpivot_i3_clean
PORT=18039
BASE=http://127.0.0.1:$PORT
LOG=/tmp/i3/i3_instance.log
PIDFILE=/tmp/i3/i3_instance.pid
EVID=/tmp/i3/curl_evidence.txt
: > "$EVID"

say() { echo "$@" | tee -a "$EVID"; }
psql_db() { su - postgres -c "psql -d $DB -Atc \"$1\""; }

set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export NIK_DATABASE_NAME=$DB NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:$PORT RUST_LOG=info
unset NIK_REDIS_URL

say "===== 0) 起隔离实例（库 $DB，端口 $PORT）====="
[ -f "$PIDFILE" ] && kill "$(cat $PIDFILE)" 2>/dev/null
sleep 1
cd /opt/cloudpivot/backend || exit 1
setsid nohup /opt/cloudpivot/backend/target/debug/cloudpivot > "$LOG" 2>&1 < /dev/null &
MY_PID=$!
echo $MY_PID > "$PIDFILE"
code=000
for i in $(seq 1 60); do
  code=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/health" 2>/dev/null)
  [ "$code" = "200" ] && break
  sleep 1
done
# 确认监听者就是我起的这个进程（防「打到别人的实例上」——上一轮就踩过）
listed=$(ss -ltnp "sport = :$PORT" 2>/dev/null | grep -o "pid=[0-9]*" | head -1)
say "health=$code  my_pid=$MY_PID  listener=$listed"

login() { curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$1\"}"; }
field() { python3 -c "import json,sys;d=json.load(sys.stdin);print(d$1)"; }

say
say "===== 1) 登录（真实 token；初始口令取自服务 env，不回显明文）====="
INIT_PW="${NIK_ADMIN_PASSWORD:?先 . /etc/cloudpivot/cloudpivot-dev.env（干净库里 admin 就是用这个口令建的）}"
R=$(login "$INIT_PW")
say "login code=$(echo "$R" | field "['code']")  must_change=$(echo "$R" | field "['data'].get('must_change_password')")"
T=$(echo "$R" | field "['data'].get('access_token','')")
say "token_len=${#T}"

NEW_PW='Admin@2026i3'
say
say "===== 2) 首登改密（种子 admin 恒 must_change_password=true）====="
say "PUT /api/system/auth/password → $(curl -s -X PUT "$BASE/api/system/auth/password" \
  -H "Authorization: Bearer $T" -H 'Content-Type: application/json' \
  -d "{\"old_password\":\"$INIT_PW\",\"new_password\":\"$NEW_PW\"}")"
R=$(login "$NEW_PW")
T=$(echo "$R" | field "['data'].get('access_token','')")
say "relogin code=$(echo "$R" | field "['code']")  token_len=${#T}"

api() { curl -s -H "Authorization: Bearer $T" "$BASE$1"; }

say
say "===== 3) N2：超管有部门（/auth/me + /system/dept/tree）====="
api /api/system/auth/me | python3 -c '
import json,sys
d=json.load(sys.stdin); x=d.get("data") or {}
print("  /auth/me → code=%s username=%s dept_id=%s must_change=%s roles=%s" % (
  d.get("code"), x.get("username"), x.get("dept_id"), x.get("must_change_password"), x.get("roles")))' | tee -a "$EVID"
api /api/system/dept/tree | python3 -c '
import json,sys
d=json.load(sys.stdin); rows=d.get("data") or []
def walk(ns, dep=0):
    for n in ns:
        print("  dept → %s %s parent_id=%s dept_path=%s" % ("  "*dep, n.get("dept_code"), n.get("parent_id"), n.get("dept_path")))
        walk(n.get("children") or [], dep+1)
walk(rows)' | tee -a "$EVID"

say
say "===== 4) H5/H6 的业务后果：模板库默认口径（SQL 是 AND d.is_system）====="
dump_tpl() {
  api '/api/business/flow/def/templates' | python3 -c '
import json,sys
d=json.load(sys.stdin)
data=d.get("data") or {}
items=[i for g in (data.get("groups") or []) for i in (g.get("items") or [])]
print("  HTTP code=%s total=%s 条目数=%s" % (d.get("code"), data.get("total"), len(items)))
for i in items:
    if i.get("flow_code") in ("pr_approve","prt_approve"):
        print("  ✔ %-12s %s | is_system=%s | status=%s" % (i["flow_code"], i["flow_name"], i["is_system"], i["status"]))
print("  含 pr_approve=%s  含 prt_approve=%s" % (
  any(i["flow_code"]=="pr_approve" for i in items),
  any(i["flow_code"]=="prt_approve" for i in items)))'
}
say "-- A. 修复态（V0366 之后）--"
dump_tpl | tee -a "$EVID"

say
say "-- B. 对照：两条 is_system 改回 false（= V0366 之前的库状态）→ 标准流程列表里应消失 --"
psql_db "update biz_flow_def set is_system=false where flow_code in ('pr_approve','prt_approve') and deleted_at is null" >/dev/null
dump_tpl | tee -a "$EVID"

say
say "-- C. 复原（再跑一次 V0366 的 UPDATE）→ 应重新出现 --"
psql_db "update biz_flow_def set is_system=true, updated_at=now() where id in ('20000000-0000-4000-8000-0000000d0331'::uuid,'20000000-0000-4000-8000-0000000e0331'::uuid) and flow_code in ('pr_approve','prt_approve') and is_system is distinct from true" >/dev/null
dump_tpl | tee -a "$EVID"

say
say "===== 5) 库侧复核（与上面接口一致）====="
psql_db "select 'biz_flow_def: ' || flow_code || ' is_system=' || is_system::text from biz_flow_def where flow_code in ('pr_approve','prt_approve') and deleted_at is null order by 1" | tee -a "$EVID"
psql_db "select 'admin dept=' || coalesce(d.dept_code,'NULL') from sys_user u left join sys_dept d on d.id=u.dept_id where username='admin'" | tee -a "$EVID"

say
say "===== 6) 收尾：关掉隔离实例 ====="
kill "$MY_PID" 2>/dev/null
sleep 1
say "instance stopped (pid $MY_PID)"
