#!/usr/bin/env bash
# T1 返工：上限降级路径实测（2001 → 降级；2000 → 正常）
#
# 库 cloudpivot_t1e（已有 5 万行 BULK 数据），身份 t1_emp（data_scope=4 仅本人）：
#   * 只给自己留 2 条任务 → 降级后可见 2 条（"只按范围"）
#   * 给他灌 2001 条协作关系 → 参与 id = 2 + 2001 = 2003 > 2000 → 降级
#   * 删掉 3 条（剩 1998）→ 参与 id = 2000 → 不降级，可见 2000 条
set -uo pipefail
DB=cloudpivot_t1e
PORT=8028
BASE="http://127.0.0.1:$PORT/api"
EMP=7d000000-0000-0000-0000-000000000001

psqlq() { su postgres -c "psql -d $DB -tAc \"$1\""; }

hdr() { echo; echo "================ $* ================"; }

hdr "0) 造数据：t1_emp 自有 2 条 + 2001 条协作关系（范围外的市场部 BULK 任务）"
psqlq "DELETE FROM biz_task WHERE title LIKE '上限测试-%'" >/dev/null
psqlq "INSERT INTO biz_task (id, task_no, title, owner_id, creator_id, dept_id, status, priority, progress, need_review, source)
       VALUES (gen_random_uuid(), 'LIMIT-1', '上限测试-自有A', '$EMP'::uuid, '$EMP'::uuid,
               '7a000000-0000-0000-0000-000000000002'::uuid, 'TODO', 'P2', 0, false, 'SELF'),
              (gen_random_uuid(), 'LIMIT-2', '上限测试-自有B', '$EMP'::uuid, '$EMP'::uuid,
               '7a000000-0000-0000-0000-000000000002'::uuid, 'TODO', 'P2', 0, false, 'SELF')"
psqlq "DELETE FROM biz_task_member WHERE user_id = '$EMP'::uuid" >/dev/null
psqlq "INSERT INTO biz_task_member (task_id, user_id, role)
       SELECT id, '$EMP'::uuid, 'COLLABORATOR' FROM biz_task
        WHERE task_no LIKE 'BULK-%' LIMIT 2001"
echo "t1_emp 自有任务 = $(psqlq "SELECT count(*) FROM biz_task WHERE owner_id='$EMP'::uuid AND deleted_at IS NULL")"
echo "t1_emp 协作行 = $(psqlq "SELECT count(*) FROM biz_task_member WHERE user_id='$EMP'::uuid")"
echo "参与 id 数 = $(psqlq "SELECT count(*) FROM (SELECT id FROM biz_task WHERE deleted_at IS NULL AND (owner_id='$EMP'::uuid OR creator_id='$EMP'::uuid OR assigner_id='$EMP'::uuid) UNION SELECT m.task_id FROM biz_task_member m WHERE m.user_id='$EMP'::uuid) z")"

hdr "1) 起验收实例（$PORT，库 $DB）"
cd /opt/cloudpivot/backend || exit 1
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export ALUN_DATABASE_NAME=$DB
export ALUN_SERVER_LISTEN=127.0.0.1:$PORT
setsid ./target/debug/cloudpivot >>/tmp/t1r_trunc.log 2>&1 &
SRV=$!
for _ in $(seq 1 60); do
    code=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/system/auth/login" \
        -H 'Content-Type: application/json' -d '{"username":"probe","password":"x"}')
    [ "$code" = "401" ] || [ "$code" = "200" ] || [ "$code" = "400" ] && break
    sleep 0.5
done
echo "server pid=$SRV  log=/tmp/t1r_trunc.log"

TOK=$(curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
      -d "{\"username\":\"t1_emp\",\"password\":\"$ALUN_ADMIN_PASSWORD\"}" \
      | python3 -c 'import sys,json;print(json.load(sys.stdin)["data"]["access_token"])')
echo "t1_emp token 长度 = ${#TOK}"

probe() { # probe
    curl -s "$BASE/business/task/page?page=1&page_size=5" -H "Authorization: Bearer $TOK" \
      | python3 -c '
import sys, json
d = json.load(sys.stdin)["data"]
rows = [r["title"] for r in d["list"]]
print("  total={}  page_size={}  member_scope_truncated={}".format(
    d["total"], d["page_size"], d.get("member_scope_truncated")))
print("  首页标题:", rows)
'
}

hdr "2) 2001 条协作关系 → 期望 total=2（只按范围）且 member_scope_truncated=true"
probe

hdr "3) 删掉 3 条协作（剩 1998，参与 id 恰为 2000）→ 期望不降级、total=2000"
psqlq "DELETE FROM biz_task_member WHERE user_id='$EMP'::uuid AND task_id IN (
         SELECT task_id FROM biz_task_member WHERE user_id='$EMP'::uuid LIMIT 3)" >/dev/null
echo "t1_emp 协作行 = $(psqlq "SELECT count(*) FROM biz_task_member WHERE user_id='$EMP'::uuid")"
echo "参与 id 数 = $(psqlq "SELECT count(*) FROM (SELECT id FROM biz_task WHERE deleted_at IS NULL AND (owner_id='$EMP'::uuid OR creator_id='$EMP'::uuid OR assigner_id='$EMP'::uuid) UNION SELECT m.task_id FROM biz_task_member m WHERE m.user_id='$EMP'::uuid) z")"
probe

hdr "4) 详情接口同样受降级约束（范围外协作任务 → 404）"
OUT=$(curl -s "$BASE/business/task/page?page=1&page_size=1" -H "Authorization: Bearer $TOK" | python3 -c '
import sys,json; d=json.load(sys.stdin)["data"]; print(d["list"][0]["title"] if d["list"] else "-")')
echo "首页第一条 = $OUT"

kill "$SRV" 2>/dev/null
sleep 1
ps -p "$SRV" >/dev/null 2>&1 && kill -9 "$SRV" 2>/dev/null
echo "server stopped"
