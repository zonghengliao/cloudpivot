#!/usr/bin/env bash
# T1 接口走查（真服务 + 真库 + 真登录）：逐条打印证据
#
# 用法（172.18.10.144 上）：
#   bash /opt/cloudpivot/scripts/verify/t1_task_api_verify.sh
#
# 前置：验证库 cloudpivot_t1v 已迁到 V0103（见 t1_task_migrate_verify.sh / t1_task_run_tests.sh）。
# 本脚本自己灌夹具（t1_task_api_fixture.sql）、自己起停服务（127.0.0.1:8027），可重复执行。
set -uo pipefail

DB=cloudpivot_t1v
PORT=8027
BASE="http://127.0.0.1:$PORT/api"
REPO=/opt/cloudpivot
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FAIL=0

hdr() { echo; echo "=================================================================="; echo "$*"; echo "=================================================================="; }

jget() { # jget <a.b.0.c> —— 从 stdin 的 JSON 里取值
    python3 -c '
import sys, json
data = json.load(sys.stdin)
cur = data
for part in sys.argv[1].split("."):
    if part.lstrip("-").isdigit() and isinstance(cur, list):
        cur = cur[int(part)]
    else:
        cur = cur[part]
print(cur if not isinstance(cur, (dict, list)) else json.dumps(cur, ensure_ascii=False))
' "$1"
}

check() { # check <标签> <期望> <实际>
    if [ "$2" = "$3" ]; then
        echo "  PASS  $1：$3"
    else
        echo "  FAIL  $1：期望 $2，实际 $3"
        FAIL=$((FAIL + 1))
    fi
}

login() { # login <username> → access_token
    curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
        -d "{\"username\":\"$1\",\"password\":\"$PW\"}" | jget data.access_token
}

psqlq() { su postgres -c "psql -d $DB -tAc \"$1\""; }

# ---------- 0) 起服务 ----------
hdr "0) 起服务（127.0.0.1:$PORT，库 $DB）"
psqlq "DELETE FROM biz_task WHERE title LIKE 'T1走查-%'" >/dev/null
echo "已清掉上一轮走查数据（title LIKE 'T1走查-%'）"

# 夹具（幂等）：/root 不可被 postgres 用户遍历，所以拷到 /tmp 再执行
cp "$HERE/t1_task_api_fixture.sql" /tmp/t1_task_api_fixture.sql
chmod 644 /tmp/t1_task_api_fixture.sql
su postgres -c "psql -q -v ON_ERROR_STOP=1 -d $DB -f /tmp/t1_task_api_fixture.sql" >/tmp/t1_fixture.out 2>&1
echo "夹具已就绪（部门 / 角色 / 4 个账号）"

cd "$REPO/backend" || exit 1
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export ALUN_DATABASE_NAME=$DB
export ALUN_SERVER_LISTEN=127.0.0.1:$PORT
PW="$ALUN_ADMIN_PASSWORD"
nohup ./target/debug/cloudpivot >>/tmp/t1v_api.log 2>&1 </dev/null &
SRV_PID=$!
echo "server pid=$SRV_PID  log=/tmp/t1v_api.log"
for _ in $(seq 1 40); do
    if curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/system/auth/login" \
        -H 'Content-Type: application/json' -d '{"username":"probe","password":"x"}' | grep -qE '401|200|400'; then
        echo "服务已就绪"; break
    fi
    sleep 0.5
done

# ---------- 1) 登录 ----------
hdr "1) 登录（admin + 4 个夹具账号，密码哈希与 admin 相同）"
TOK_ADMIN=$(login admin)
TOK_EMP=$(login t1_emp)
TOK_MGR=$(login t1_mgr)
TOK_CROSS=$(login t1_cross)
TOK_OTHER=$(login t1_other)
echo "admin token 长度 = ${#TOK_ADMIN}"
echo "emp/mgr/cross/other token 长度 = ${#TOK_EMP} / ${#TOK_MGR} / ${#TOK_CROSS} / ${#TOK_OTHER}"

# 期望的默认截止日（本周五，CST = UTC+8）——与 model::default_due_date 同一口径
EXPECT_DUE=$(python3 -c '
import datetime
today = (datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=8)).date()
print((today + datetime.timedelta(days=(4 - today.weekday()) % 7)).isoformat())
')
echo "期望默认截止日（本周五，CST）= $EXPECT_DUE"

api() { # api <token> <method> <path> [json]
    local token=$1 method=$2 path=$3 body=${4:-}
    if [ -n "$body" ]; then
        curl -s -X "$method" "$BASE$path" -H "Authorization: Bearer $token" \
            -H 'Content-Type: application/json' -d "$body"
    else
        curl -s -X "$method" "$BASE$path" -H "Authorization: Bearer $token"
    fi
}

# ---------- 2) 创建 5 条任务（默认值 + task_no） ----------
hdr "2) 创建任务（admin 建，分派给不同部门的人）"
create() { api "$TOK_ADMIN" POST /business/task "$1"; }

R1=$(create '{"title":"T1走查-员工自己的任务","owner_id":"7d000000-0000-0000-0000-000000000001"}')
R2=$(create '{"title":"T1走查-主管的任务","owner_id":"7d000000-0000-0000-0000-000000000002"}')
R3=$(create '{"title":"T1走查-市场部无协作","owner_id":"7d000000-0000-0000-0000-000000000004"}')
R4=$(create '{"title":"T1走查-跨部门协作","owner_id":"7d000000-0000-0000-0000-000000000004","priority":"P1"}')
R5=$(create '{"title":"T1走查-市场部另一条","owner_id":"7d000000-0000-0000-0000-000000000004"}')
echo "第 1 条完整回执（默认值证据）："
printf '%s' "$R1" | python3 -m json.tool
A1=$(printf '%s' "$R1" | jget data.id); N1=$(printf '%s' "$R1" | jget data.task_no)
A2=$(printf '%s' "$R2" | jget data.id); N2=$(printf '%s' "$R2" | jget data.task_no)
A3=$(printf '%s' "$R3" | jget data.id); N3=$(printf '%s' "$R3" | jget data.task_no)
A4=$(printf '%s' "$R4" | jget data.id); N4=$(printf '%s' "$R4" | jget data.task_no)
A5=$(printf '%s' "$R5" | jget data.id); N5=$(printf '%s' "$R5" | jget data.task_no)
echo "5 条编号：$N1 $N2 $N3 $N4 $N5"
check "编号不重复" "5" "$(printf '%s\n' "$N1" "$N2" "$N3" "$N4" "$N5" | sort -u | wc -l | tr -d ' ')"
check "第 1 条状态默认 TODO" "TODO" "$(printf '%s' "$R1" | jget data.status)"
check "第 1 条优先级默认 P2" "P2" "$(printf '%s' "$R1" | jget data.priority)"
check "第 1 条进度默认 0" "0" "$(printf '%s' "$R1" | jget data.progress)"
check "第 1 条截止日默认本周五" "$EXPECT_DUE" "$(printf '%s' "$R1" | jget data.due_date)"

# ---------- 3) 分页（admin 是创建人 → 全见） ----------
hdr "3) 分页 GET /business/task/page（admin）"
P_ADMIN=$(api "$TOK_ADMIN" GET "/business/task/page?page=1&page_size=20")
echo "$P_ADMIN" | python3 -c '
import sys, json
d = json.load(sys.stdin)["data"]
print("total =", d["total"])
for row in d["list"]:
    print("  {}  {:9s} {}  owner={:16s} due={}  {}".format(
        row["task_no"], row["status"], row["priority"], row["owner_name"], row["due_date"], row["title"]))
'
check "admin 可见条数" "5" "$(printf '%s' "$P_ADMIN" | jget data.total)"

# ---------- 4) 详情 ----------
hdr "4) 详情 GET /business/task/{id}（admin 查第 1 条）"
D1=$(api "$TOK_ADMIN" GET "/business/task/$A1")
printf '%s' "$D1" | python3 -c '
import sys, json
d = json.load(sys.stdin)["data"]
for key in ["task_no","title","status","priority","progress","need_review","owner_name","creator_name","dept_name","due_date","source","parent_id","approval_instance_id"]:
    print(f"  {key} = {d[key]}")
print("  children =", len(d["children"]), " members =", len(d["members"]), " attachments =", len(d["attachments"]))
'
check "详情部门名（普通员工=研发一组）" "T1研发一组" "$(printf '%s' "$D1" | jget data.dept_name)"
check "详情子任务数" "0" "$(printf '%s' "$D1" | python3 -c 'import sys,json;print(len(json.load(sys.stdin)["data"]["children"]))')"

# ---------- 5) 编辑（负责人本人） ----------
hdr "5) 编辑 PUT /business/task/{id}（t1_emp 是负责人 → 允许）"
U1=$(api "$TOK_EMP" PUT "/business/task/$A1" '{"title":"T1走查-员工自己的任务（已改）","priority":"P0","need_review":true,"due_date":null}')
printf '%s' "$U1" | python3 -m json.tool
check "编辑后标题" "T1走查-员工自己的任务（已改）" "$(api "$TOK_EMP" GET "/business/task/$A1" | jget data.title)"
check "编辑后优先级" "P0" "$(api "$TOK_EMP" GET "/business/task/$A1" | jget data.priority)"
check "显式 null 清空截止日" "None" "$(api "$TOK_EMP" GET "/business/task/$A1" | jget data.due_date)"

# ---------- 6) 协作人视角（先插协作关系，模拟 T3 的加协作人） ----------
hdr "6) 跨部门协作：把 t1_cross 拉进市场部的 $N4（biz_task_member 直接落库，接口属 T3）"
psqlq "INSERT INTO biz_task_member (task_id, user_id, role) VALUES ('$A4'::uuid, '7d000000-0000-0000-0000-000000000003'::uuid, 'COLLABORATOR') ON CONFLICT DO NOTHING"
psqlq "SELECT 'member rows = ' || count(*) FROM biz_task_member WHERE task_id = '$A4'::uuid"

hdr "6.1) 编辑越权：t1_cross 不是负责人/创建人 → 403"
U_CROSS=$(api "$TOK_CROSS" PUT "/business/task/$A4" '{"title":"协作人想改标题"}')
echo "$U_CROSS" | python3 -m json.tool
check "协作人编辑被拒 HTTP 业务码" "63002" "$(printf '%s' "$U_CROSS" | jget code)"

# ---------- 7) 可见性四场景（真实 HTTP） ----------
hdr "7) 可见性四场景（同一批数据，5 个身份各查一次分页）"
show() { # show <label> <token> → **只把 total 打到 stdout**，其余打到 stderr
    local label=$1 token=$2 resp total
    resp=$(api "$token" GET "/business/task/page?page=1&page_size=50")
    echo "--- $label ---" >&2
    printf '%s' "$resp" | python3 -c '
import sys, json
d = json.load(sys.stdin)["data"]
for row in sorted(d["list"], key=lambda r: r["task_no"]):
    print("    {}  owner={:16s}  {}".format(row["task_no"], row["owner_name"], row["title"]))
' >&2
    total=$(printf '%s' "$resp" | jget data.total)
    echo "  total = $total" >&2
    printf '%s' "$total"
}
T_EMP=$(show "a) 普通员工 t1_emp（data_scope=4 仅本人）" "$TOK_EMP")
T_MGR=$(show "b) 部门主管 t1_mgr（data_scope=3 本部门及以下）" "$TOK_MGR")
T_CROSS=$(show "c) 跨部门协作人 t1_cross（data_scope=4，被拉进 $N4）" "$TOK_CROSS")
T_OTHER=$(show "c') 市场部同事 t1_other（同部门，但不是协作人）" "$TOK_OTHER")
T_ADM=$(show "d) 超管 admin（data_scope=1 全部）" "$TOK_ADMIN")

check "a) 普通员工只看到自己的" "1" "$T_EMP"
check "b) 部门主管看到本部门及下级（研发一组 + 研发部）" "2" "$T_MGR"
check "c) 跨部门协作人只看到被拉进的那一条" "1" "$T_CROSS"
check "c') 同部门同事看到自己名下 3 条（不含协作那条以外）" "3" "$T_OTHER"
check "d) 超管看到全部" "5" "$T_ADM"

hdr "7.1) 跨部门协作人能看详情、看不到其它市场部任务详情（404，不是 403）"
check "协作人查 $N4 详情 code" "0" "$(api "$TOK_CROSS" GET "/business/task/$A4" | jget code)"
D_NOT_FOUND=$(api "$TOK_CROSS" GET "/business/task/$A3")
echo "$D_NOT_FOUND" | python3 -m json.tool
check "协作人查 $N3 详情的业务码" "63001" "$(printf '%s' "$D_NOT_FOUND" | jget code)"

# ---------- 8) 软删 ----------
hdr "8) 软删除 DELETE /business/task/{id}（admin）"
DEL=$(api "$TOK_ADMIN" DELETE "/business/task/$A5")
echo "$DEL" | python3 -m json.tool
check "删除回执 deleted" "True" "$(printf '%s' "$DEL" | jget data.deleted)"
check "删除后 admin 分页 total" "4" "$(api "$TOK_ADMIN" GET "/business/task/page?page=1&page_size=20" | jget data.total)"
check "删除后 t1_other 分页 total（原 3 条剩 2 条）" "2" "$(api "$TOK_OTHER" GET "/business/task/page?page=1&page_size=20" | jget data.total)"
check "已删任务详情 404 码" "63001" "$(api "$TOK_ADMIN" GET "/business/task/$A5" | jget code)"

# ---------- 9) 我的任务 ----------
hdr "9) 我的任务 GET /business/task/my（t1_emp）"
MY=$(api "$TOK_EMP" GET "/business/task/my")
printf '%s' "$MY" | python3 -c '
import sys, json
d = json.load(sys.stdin)["data"]
print("  counts =", json.dumps(d["counts"], ensure_ascii=False), " total =", d["total"])
for group in ["overdue","today","this_week","later","no_due"]:
    for row in d[group]:
        print("    [{:9s}] {}  due={}  {}".format(group, row["task_no"], row["due_date"], row["title"]))
'
check "我的任务 total" "1" "$(printf '%s' "$MY" | jget data.total)"

# ---------- 10) 筛选 ----------
hdr "10) 筛选：状态 / 优先级 / 关键词 / 负责人"
check "status=DOING（当前无进行中）" "0" "$(api "$TOK_ADMIN" GET '/business/task/page?status=DOING' | jget data.total)"
check "priority=P1（只第 4 条）" "1" "$(api "$TOK_ADMIN" GET '/business/task/page?priority=P1' | jget data.total)"
check "keyword=跨部门" "1" "$(api "$TOK_ADMIN" GET '/business/task/page?keyword=%E8%B7%A8%E9%83%A8%E9%97%A8' | jget data.total)"
check "owner=t1_other（删了 1 条后剩 2 条）" "2" "$(api "$TOK_ADMIN" GET '/business/task/page?owner_id=7d000000-0000-0000-0000-000000000004' | jget data.total)"
check "status=BAD（非法值 → 400）" "400" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/business/task/page?status=BAD" -H "Authorization: Bearer $TOK_ADMIN")"

# ---------- 11) 收尾 ----------
hdr "11) 停服务"
kill "$SRV_PID" 2>/dev/null
sleep 1
ps -p "$SRV_PID" >/dev/null 2>&1 && kill -9 "$SRV_PID" 2>/dev/null
echo "server stopped"

echo
if [ "$FAIL" -eq 0 ]; then
    echo "T1 接口走查：全部断言通过"
else
    echo "T1 接口走查：$FAIL 条断言失败"
fi
exit 0
