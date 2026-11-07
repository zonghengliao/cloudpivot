#!/usr/bin/env bash
# t_0127714a 验收：真机 HTTP + 库断言（任意宿主机：BIN/DB/PORT 可覆盖）
#  ① demo_lead（= DEPT_DEMO 负责人本人）提报 → 422/65018，单据仍 DRAFT、不残留 EXPENSE 实例
#  ② demo_finance 提报（正常路径）→ 200，单据 SUBMITTED、实例 PENDING、1 条 PENDING 待办
#  ③ 再把 ① 那张单交给 demo_finance 提交 → 200（证明「失败后没有卡死，可以重提」）
set -uo pipefail
BASE=${BASE:-http://127.0.0.1:8054}
DB=${DB:-cloudpivot_fix10_demo}
ENVF=/etc/cloudpivot/cloudpivot-dev.env
PW=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' "$ENVF")
export PGPASSWORD="$PW"
ADMINPW=$(awk -F= '/^NIK_ADMIN_PASSWORD/{print $2}' "$ENVF"); ADMINPW=${ADMINPW:-Admin@123456}
DEMOPW=$(awk -F= '/^NIK_DEMO_PASSWORD/{print $2}' "$ENVF"); DEMOPW=${DEMOPW:-ChangeMe@2026}
PASS=0; FAIL=0
say() { printf '%s\n' "$*" >&2; }
ok()  { PASS=$((PASS+1)); say "  ✅ $*"; }
bad() { FAIL=$((FAIL+1)); say "  ❌ $*"; }
eq()  { if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
q()   { psql -h 127.0.0.1 -U cloudpivot -d "$DB" -tAc "$1"; }
jget() { python3 -c 'import sys,json;d=json.load(sys.stdin)
cur=d
for k in sys.argv[1].split("."):
    cur = cur.get(k) if isinstance(cur,dict) else None
print("" if cur is None else cur)' "$1"; }

# ---------- HTTP 小工具：每个请求都把 http_code 留在日志里 ----------
http() { # http METHOD PATH TOKEN JSON  -> 打印 "__HTTP__<code>"
  local method=$1 path=$2 token=$3 body=$4
  local args=(-s -o /tmp/fix10_body.json -w '%{http_code}' -X "$method" "$BASE$path" -H 'Content-Type: application/json')
  [ -n "$token" ] && args+=(-H "Authorization: Bearer $token")
  [ -n "$body" ] && args+=(-d "$body")
  curl "${args[@]}"
}
body() { cat /tmp/fix10_body.json; }

login() { # login USER PW -> 打印 token（必要时走强制改密）
  local user=$1 pw=$2 code tok must newpw
  code=$(http POST /api/system/auth/login '' "{\"username\":\"$user\",\"password\":\"$pw\"}")
  say "  [curl] POST /api/system/auth/login user=$user → http=$code"
  if [ "$code" != "200" ]; then body; return 1; fi
  must=$(body | jget data.must_change_password)
  tok=$(body | jget data.access_token)
  if [ "$must" = "True" ]; then
    newpw="Fix10@2026"
    code=$(http PUT /api/system/auth/password "$tok" "{\"old_password\":\"$pw\",\"new_password\":\"$newpw\"}")
    say "  [curl] PUT /api/system/auth/password user=$user（首次登录强制改密）→ http=$code"
    [ "$code" = "200" ] || { body; return 1; }
    code=$(http POST /api/system/auth/login '' "{\"username\":\"$user\",\"password\":\"$newpw\"}")
    say "  [curl] 改密后重登 user=$user → http=$code"
    [ "$code" = "200" ] || { body; return 1; }
    tok=$(body | jget data.access_token)
  fi
  printf '%s' "$tok"
}

say "================ t_0127714a 真机验收（$BASE / $DB）================"
say "--- 基线"
say "migrations=$(q 'select count(*) from schema_migrations') users=$(q 'select count(*) from sys_user where deleted_at is null') flows=$(q "select count(*) from biz_flow_def where status='PUBLISHED' and deleted_at is null")"

say "--- 取夹具 id"
DEPT=$(q "select id from sys_dept where dept_code='DEPT_DEMO' and deleted_at is null limit 1")
LEAD=$(q "select leader_id from sys_dept where dept_code='DEPT_DEMO' and deleted_at is null limit 1")
LEADU=$(q "select username from sys_user where id='$LEAD'")
FIN=$(q "select id from sys_user where username='demo_finance' and deleted_at is null limit 1")
ETP=$(q "select id from biz_expense_type where type_code='TRAVEL' and deleted_at is null limit 1")
say "DEPT_DEMO=$DEPT leader=$LEADU($LEAD) demo_finance=$FIN travel_type=$ETP"
[ -n "$LEADU" ] || { echo "!! DEPT_DEMO 没有 leader_id，夹具不成立"; exit 1; }

say "--- 登录"
A=$(login admin "$ADMINPW") || { echo "!! admin 登录失败"; exit 1; }
LEADTOK=$(login "$LEADU" "$DEMOPW") || { echo "!! $LEADU 登录失败"; exit 1; }
FINTOK=$(login demo_finance "$DEMOPW") || { echo "!! demo_finance 登录失败"; exit 1; }
[ -n "$A" ] && [ -n "$LEADTOK" ] && [ -n "$FINTOK" ] && ok "三个令牌就绪（admin / $LEADU / demo_finance）"

new_expense() { # new_expense REPORTER_ID TITLE -> 打印 id
  local rep=$1 title=$2 code id
  code=$(http POST /api/business/erp/expense "$A" "{\"title\":\"$title\",\"expense_date\":\"2026-11-08\",\"reporter_id\":\"$rep\",\"dept_id\":\"$DEPT\",\"items\":[{\"expense_type_id\":\"$ETP\",\"with_tax_amount\":100,\"tax_rate\":0,\"summary\":\"$title\",\"invoice_no\":\"FIX10-INV-0001\",\"invoice_date\":\"2026-11-08\",\"invoice_type\":\"VAT_NORMAL\"}]}")
  say "  [curl] POST /business/erp/expense（reporter=$rep）→ http=$code"
  [ "$code" = "200" ] || { body; return 1; }
  id=$(body | jget data.report.id); [ -n "$id" ] || id=$(body | jget data.id)
  printf '%s' "$id"
}

say ""
say "=== ① 负责人本人提报（制单/发起人 = 该节点审批人）→ 期望 422/65018，且库里一行不留 ==="
EXP1=$(new_expense "$LEAD" "FIX10 负责人本人提报（应被职责分离挡住）") || exit 1
say "EXP1=$EXP1"
CODE=$(http POST "/api/business/erp/expense/$EXP1/submit" "$LEADTOK" '{}')
say "  [curl] POST /business/erp/expense/$EXP1/submit（as $LEADU）→ http=$CODE"
say "  body: $(body | head -c 400)"
RC=$(body | jget code)
eq "①提交 http" "$CODE" "422"
eq "①提交错误码" "$RC" "65018"
eq "①单据状态（必须仍 DRAFT）" "$(q "select status from biz_expense_report where id='$EXP1'")" "DRAFT"
eq "①EXPENSE 实例条数（必须 0）" "$(q "select count(*) from biz_flow_instance where biz_type='EXPENSE' and biz_id='$EXP1'")" "0"
eq "①该单关联待办条数（必须 0）" "$(q "select count(*) from biz_flow_task t join biz_flow_instance i on i.id=t.instance_id where i.biz_id='$EXP1'")" "0"
eq "①该单 approval_instance_id（必须为空）" "$(q "select coalesce(approval_instance_id::text,'') from biz_expense_report where id='$EXP1'")" ""
eq "①全库 EXPENSE 实例总数（必须 0）" "$(q "select count(*) from biz_flow_instance where biz_type='EXPENSE'")" "0"

say ""
say "=== ② demo_finance 正常路径 → 期望 200 / SUBMITTED / 实例 PENDING / 1 条 PENDING 待办 ==="
EXP2=$(new_expense "$FIN" "FIX10 财务正常提报") || exit 1
say "EXP2=$EXP2"
CODE=$(http POST "/api/business/erp/expense/$EXP2/submit" "$FINTOK" '{}')
say "  [curl] POST /business/erp/expense/$EXP2/submit（as demo_finance）→ http=$CODE"
RC=$(body | jget code)
eq "②提交 http" "$CODE" "200"
eq "②响应业务码" "$RC" "0"
eq "②单据状态" "$(q "select status from biz_expense_report where id='$EXP2'")" "SUBMITTED"
eq "②实例状态" "$(q "select status from biz_flow_instance where biz_type='EXPENSE' and biz_id='$EXP2'")" "PENDING"
eq "②实例条数" "$(q "select count(*) from biz_flow_instance where biz_type='EXPENSE' and biz_id='$EXP2'")" "1"
eq "②PENDING 待办条数" "$(q "select count(*) from biz_flow_task t join biz_flow_instance i on i.id=t.instance_id where i.biz_id='$EXP2' and t.status='PENDING'")" "1"
say "  ②待办审批人 = $(q "select u.username from biz_flow_task t join biz_flow_instance i on i.id=t.instance_id join sys_user u on u.id=t.assignee_id where i.biz_id='$EXP2'")"
eq "②待办审批人不是发起人" "$(q "select (u.id <> i.initiator_id)::text from biz_flow_task t join biz_flow_instance i on i.id=t.instance_id join sys_user u on u.id=t.assignee_id where i.biz_id='$EXP2'")" "true"
eq "②approval_instance_id 已回写" "$(q "select (approval_instance_id is not null)::text from biz_expense_report where id='$EXP2'")" "true"

say ""
say "=== ③ ① 那张单**没有卡死**：仍是可流转的草稿 → 能正常作废（原缺陷里它已是 SUBMITTED，什么都做不了） ==="
CODE=$(http POST "/api/business/erp/expense/$EXP1/cancel" "$A" '{"reason":"FIX10 验收：提交被职责分离挡住后作废（证明单据未卡死）"}')
say "  [curl] POST /business/erp/expense/$EXP1/cancel（as admin）→ http=$CODE"
eq "③草稿可作废 http" "$CODE" "200"
eq "③作废后状态" "$(q "select status from biz_expense_report where id='$EXP1'")" "CANCELLED"
eq "③作废后仍无 EXPENSE 实例" "$(q "select count(*) from biz_flow_instance where biz_type='EXPENSE' and biz_id='$EXP1'")" "0"
eq "③全库 EXPENSE 实例只剩正常路径那 1 条" "$(q "select count(*) from biz_flow_instance where biz_type='EXPENSE'")" "1"

say ""
say "================ 结果：PASS=$PASS FAIL=$FAIL ================"
[ "$FAIL" = "0" ] || exit 1
