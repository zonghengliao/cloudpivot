#!/usr/bin/env bash
# =============================================================================
# N-7 验收脚本：① 数据权限变更留痕（before/after）  ② 流程保存/发布校验兜底人
#
# 覆盖卡面验收 ①–⑤，全部用**真实 HTTP + 真实令牌**打后端，落库事实用 psql 复核：
#
#   ① 改数据权限 → `sys_change_log` 有 diff（data_scope {b,a}）+
#                  `audit_logs` 有 DATA_SCOPE_UPDATE（before_data / after_data）
#   ② 流程保存时配**不存在的用户**当兜底 → 400 + 65042 + 点名节点
#   ③ 流程保存时配**停用用户**当兜底 → 400 + 65042
#   ④ 升级链指向**已删角色** → 400 + 65040（点名角色与节点）
#   ⑤ 合法配置（兜底人是在职用户）→ 保存 200、发布 200
#
# 用法（在目标机器上跑，需要能直连后端库做复核）：
#   bash scripts/verify/n7_acceptance.sh                  # 默认读 /etc/cloudpivot/cloudpivot-dev.env
#   N7_BASE_URL=http://127.0.0.1:18077 N7_SKIP_DB=1 bash ...   # 只验 HTTP（不做库内复核）
#
# 约束：令牌只在本进程内使用，**不写进任何输出文件**。夹具全带 `n7acc_` 前缀并在末尾清理。
# =============================================================================
set -uo pipefail

OUT_DIR="${1:-/tmp/n7-acceptance}"
mkdir -p "$OUT_DIR"

if [ -f /etc/cloudpivot/cloudpivot-dev.env ]; then
  # shellcheck disable=SC1091
  set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
fi

BASE="${N7_BASE_URL:-http://127.0.0.1:8033}"
ADMIN_USER="${N7_ADMIN_USER:-admin}"
ADMIN_PASS="${N7_ADMIN_PASSWORD:-${NIK_ADMIN_PASSWORD:-}}"
DB_USER="${NIK_DATABASE_USER:-}"
DB_PASS="${NIK_DATABASE_PASSWORD:-}"
DB_NAME="${NIK_DATABASE_NAME:-}"
DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"
SKIP_DB="${N7_SKIP_DB:-0}"
TS="$(date +%s)"
TAG="n7acc_${TS}"

FAIL=0
pass() { echo "  [PASS] $*"; }
fail() { echo "  [FAIL] $*"; FAIL=$((FAIL + 1)); }

if [ -z "$ADMIN_PASS" ]; then
  echo "缺少 NIK_ADMIN_PASSWORD（先 source /etc/cloudpivot/cloudpivot-dev.env 或设 N7_ADMIN_PASSWORD）"
  exit 2
fi

psql_q() {
  PGPASSWORD="$DB_PASS" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -At -F'|' -c "$1"
}
can_db() {
  [ "$SKIP_DB" = "1" ] && return 1
  [ -n "$DB_NAME" ] && [ -n "$DB_USER" ] && psql_q "select 1" >/dev/null 2>&1
}
# HTTP 调用：$1=method $2=path $3=body(可空) → 写 $OUT_DIR/last_code 与 last_body
call() {
  local method=$1 path=$2 body=${3:-}
  local -a args=(-s -o "$OUT_DIR/last_body.json" -w '%{http_code}' -X "$method"
    -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json')
  if [ -n "$body" ]; then
    printf '%s' "$body" > "$OUT_DIR/last_req.json"
    CODE=$(curl "${args[@]}" --data @"$OUT_DIR/last_req.json" "$BASE$path")
  else
    CODE=$(curl "${args[@]}" "$BASE$path")
  fi
  echo "$CODE"
}
bodyf() { jq -r "$1" "$OUT_DIR/last_body.json" 2>/dev/null; }

echo "==================== 0. 环境 ===================="
echo "BASE=$BASE  DB=$DB_NAME@$DB_HOST:$DB_PORT  库内复核=$([ "$SKIP_DB" = 1 ] && echo off || echo on)"
curl -s -o /dev/null -w 'health=%{http_code}\n' "$BASE/health"

echo "==================== 1. 登录（真 token） ===================="
LOGIN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS\"}")
TOKEN=$(printf '%s' "$LOGIN" | jq -r '.data.access_token // empty')
if [ -z "$TOKEN" ]; then
  echo "登录失败（已脱敏）：$(printf '%s' "$LOGIN" | jq -c 'del(.data.access_token,.data.refresh_token)')"
  exit 1
fi
echo "登录成功：token 长度 ${#TOKEN}（不落盘）"

# 全新环境里 `admin` 带着「首登强制改密」标记（must_change_password=true）：
# 除登录/改密之外的接口一律 403「首次登录必须先修改初始密码」。
# 本脚本因此在测试目标上**先把初始口令改成 N7_NEW_PASSWORD**再继续（145 上早已改过，这段自动跳过）。
MUST_CHANGE=$(printf '%s' "$LOGIN" | jq -r '.data.must_change_password // false')
if [ "$MUST_CHANGE" = "true" ]; then
  NEW_PASS="${N7_NEW_PASSWORD:-Admin@1234567}"
  CODE=$(curl -s -o "$OUT_DIR/last_body.json" -w '%{http_code}' -X PUT "$BASE/api/system/auth/password" \
    -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    -d "$(jq -nc --arg o "$ADMIN_PASS" --arg n "$NEW_PASS" '{old_password:$o,new_password:$n}')")
  echo "  首次登录：已把初始口令改成 N7_NEW_PASSWORD（HTTP $CODE）"
  [ "$CODE" = "200" ] || { echo "改密失败：$(jq -c . "$OUT_DIR/last_body.json")"; exit 1; }
  ADMIN_PASS="$NEW_PASS"
  LOGIN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS\"}")
  TOKEN=$(printf '%s' "$LOGIN" | jq -r '.data.access_token // empty')
  [ -n "$TOKEN" ] || { echo "改密后重新登录失败"; exit 1; }
  echo "  改密后重新登录成功：token 长度 ${#TOKEN}"
fi

call GET /api/system/auth/me >/dev/null
ADMIN_ID=$(jq -r '.data.id // .data.user_id // .data.user.id // empty' "$OUT_DIR/last_body.json")
echo "操作者 id=${ADMIN_ID:-?}"

echo "==================== 2. 夹具（角色 / 用户） ===================="
ROLE_BODY=$(jq -nc --arg c "N7_ACC_${TS}" --arg n "N7 验收角色 ${TS}" '{role_code:$c,role_name:$n,status:1,data_scope:3,sort_order:999}')
CODE=$(call POST /api/system/role "$ROLE_BODY")
ROLE_ID=$(bodyf '.data.id')
if [ "$CODE" = "200" ] && [ -n "$ROLE_ID" ] && [ "$ROLE_ID" != "null" ]; then
  pass "建角色 $ROLE_ID（data_scope=3 本部门及以下）"
else
  fail "建角色失败：HTTP $CODE $(jq -c 'del(.data.token)' "$OUT_DIR/last_body.json")"; exit 1
fi

USER_BODY=$(jq -nc --arg u "${TAG}_u" '{username:$u,password:"Admin@123456",real_name:"N7 在职用户",status:1}')
CODE=$(call POST /api/system/user "$USER_BODY")
ACTIVE_USER=$(bodyf '.data.id')
[ "$CODE" = "200" ] && [ -n "$ACTIVE_USER" ] && [ "$ACTIVE_USER" != "null" ] \
  && pass "建在职用户 $ACTIVE_USER" || { fail "建在职用户失败：HTTP $CODE"; }

STOP_BODY=$(jq -nc --arg u "${TAG}_s" '{username:$u,password:"Admin@123456",real_name:"N7 停用用户",status:0}')
CODE=$(call POST /api/system/user "$STOP_BODY")
STOPPED_USER=$(bodyf '.data.id')
[ "$CODE" = "200" ] && [ -n "$STOPPED_USER" ] && [ "$STOPPED_USER" != "null" ] \
  && pass "建停用用户 $STOPPED_USER（status=0）" || { fail "建停用用户失败：HTTP $CODE"; }

# 上级角色（稍后软删，用来造「升级链指向已删角色」）
UP_BODY=$(jq -nc --arg c "N7_ACC_UP_${TS}" --arg n "N7 上级角色 ${TS}" '{role_code:$c,role_name:$n,status:1,data_scope:1,sort_order:998}')
CODE=$(call POST /api/system/role "$UP_BODY")
UP_ROLE_ID=$(bodyf '.data.id')
[ "$CODE" = "200" ] && [ -n "$UP_ROLE_ID" ] && pass "建上级角色 $UP_ROLE_ID" || fail "建上级角色失败：HTTP $CODE"

ORPHAN_BODY=$(jq -nc --arg c "N7_ACC_ORPHAN_${TS}" --arg n "N7 孤儿链角色 ${TS}" --arg up "$UP_ROLE_ID" \
  '{role_code:$c,role_name:$n,status:1,data_scope:2,sort_order:997,escalation_role_id:$up}')
CODE=$(call POST /api/system/role "$ORPHAN_BODY")
ORPHAN_ROLE=$(bodyf '.data.id')
[ "$CODE" = "200" ] && [ -n "$ORPHAN_ROLE" ] && pass "建角色 $ORPHAN_ROLE（escalation_role_id=$UP_ROLE_ID）" \
  || fail "建孤儿链角色失败：HTTP $CODE $(jq -c . "$OUT_DIR/last_body.json")"

# 再建一个「有成员」的兜底角色：给在职用户挂上
FB_ROLE_BODY=$(jq -nc --arg c "N7_ACC_FB_${TS}" --arg n "N7 兜底角色 ${TS}" '{role_code:$c,role_name:$n,status:1,data_scope:2,sort_order:996}')
CODE=$(call POST /api/system/role "$FB_ROLE_BODY")
FB_ROLE=$(bodyf '.data.id')
[ "$CODE" = "200" ] && pass "建兜底角色 $FB_ROLE" || fail "建兜底角色失败：HTTP $CODE"

echo "==================== 3. 验收①：改数据权限 → 留痕（含 before/after） ===================="
SCOPE_BODY='{"data_scope":1,"custom_dept_ids":null}'
CODE=$(call PUT "/api/business/permission/scope/role/$ROLE_ID" "$SCOPE_BODY")
echo "  PUT 数据范围 3→1：HTTP $CODE  $(jq -c '.data|{data_scope,data_scope_label,custom_dept_ids}' "$OUT_DIR/last_body.json")"
[ "$CODE" = "200" ] && pass "数据范围已改成「全部数据」(1)" || fail "改数据范围失败：HTTP $CODE $(jq -c . "$OUT_DIR/last_body.json")"

if can_db; then
  # sys_change_log：字段级 diff（data_scope 的 b/a）
  CL_ROW=$(psql_q "select changed_fields::text, diff::text from sys_change_log
                    where resource='sys_role' and resource_id='$ROLE_ID'
                    order by created_at desc limit 1")
  echo "  sys_change_log: $CL_ROW" | tee "$OUT_DIR/01_change_log.txt"
  CL_FIELDS=$(printf '%s' "$CL_ROW" | cut -d'|' -f1)
  case "$CL_FIELDS" in
    *data_scope*) pass "变更日志记了 data_scope（changed_fields=$CL_FIELDS）" ;;
    *) fail "变更日志缺 data_scope（changed_fields=$CL_FIELDS）" ;;
  esac
  if printf '%s' "$CL_ROW" | grep -q '"data_scope": *{"b": *3, *"a": *1}'; then
    pass "变更日志 before/after 正确：{b:3(本部门及以下), a:1(全部)}"
  else
    # 容忍 JSON 空白差异：再按 jq 判一次
    DIFF=$(printf '%s' "$CL_ROW" | cut -d'|' -f2-)
    B=$(printf '%s' "$DIFF" | jq -r '.fields.data_scope.b // empty' 2>/dev/null)
    A=$(printf '%s' "$DIFF" | jq -r '.fields.data_scope.a // empty' 2>/dev/null)
    if [ "$B" = "3" ] && [ "$A" = "1" ]; then
      pass "变更日志 before/after 正确：{b:3, a:1}"
    else
      fail "变更日志 before/after 不符：b=$B a=$A diff=$DIFF"
    fi
  fi

  # audit_logs：事件级 before/after 快照（异步投递，最多等 10s）
  AL_ROW=""
  for _ in $(seq 1 10); do
    AL_ROW=$(psql_q "select operation, before_data::text, after_data::text from audit_logs
                      where target_type='Role' and target_id='$ROLE_ID'
                        and operation='DATA_SCOPE_UPDATE'
                      order by created_at desc limit 1")
    [ -n "$AL_ROW" ] && break
    sleep 1
  done
  echo "  audit_logs: $AL_ROW" | tee -a "$OUT_DIR/01_change_log.txt"
  if [ -z "$AL_ROW" ]; then
    fail "audit_logs 没有 DATA_SCOPE_UPDATE 行（等待 10s 仍无）"
  else
    AB=$(printf '%s' "$AL_ROW" | cut -d'|' -f2)
    AA=$(printf '%s' "$AL_ROW" | cut -d'|' -f3)
    echo "$AB" | grep -q '"data_scope": *3' && echo "$AA" | grep -q '"data_scope": *1' \
      && pass "审计日志 before/after 快照正确（3→1）" \
      || fail "审计日志快照不符：before=$AB after=$AA"
  fi
else
  echo "  [SKIP] 未连库（N7_SKIP_DB=1 或无 DB 凭证），跳过 sys_change_log / audit_logs 复核"
fi

echo "==================== 3b. 验收①（续）：只改自定义部门集合也必须留痕 ===================="
if can_db; then
  DEPT_ID=$(psql_q "select id from sys_dept where deleted_at is null order by sort_order, id limit 1")
else
  DEPT_ID=""
fi
if [ -z "$DEPT_ID" ]; then
  echo "  [SKIP] 库里没有可用部门，跳过「自定义部门集合」留痕实测（纯逻辑由单测 n7_scope_diff_records_dept_set_only_change 覆盖）"
else
  echo "  探针部门：$DEPT_ID"
  CODE=$(call PUT "/api/business/permission/scope/role/$ROLE_ID" "$(jq -nc --arg d "$DEPT_ID" '{data_scope:5,custom_dept_ids:[$d]}')")
  echo "  PUT 数据范围 1→5 + 自定义部门：HTTP $CODE"
  if [ "$CODE" = "200" ]; then
    pass "改成「自定义部门」+ 指定部门 200"
    CL_ROW=$(psql_q "select changed_fields::text, diff::text from sys_change_log
                      where resource='sys_role' and resource_id='$ROLE_ID'
                      order by created_at desc limit 1")
    CL_FIELDS=$(printf '%s' "$CL_ROW" | cut -d'|' -f1)
    DIFF=$(printf '%s' "$CL_ROW" | cut -d'|' -f2-)
    printf '%s\n' "$CL_ROW" > "$OUT_DIR/01b_change_log_dept_set.txt"
    case "$CL_FIELDS" in
      *custom_dept_ids*) pass "只改集合也进了变更日志：changed_fields=$CL_FIELDS" ;;
      *) fail "自定义部门集合变更没留痕：changed_fields=$CL_FIELDS" ;;
    esac
    printf '%s' "$DIFF" | grep -q "$DEPT_ID" && pass "diff 里点名了部门 $DEPT_ID" \
      || fail "diff 里没有部门 $DEPT_ID：$DIFF"
  else
    fail "改自定义部门失败：HTTP $CODE $(jq -c . "$OUT_DIR/last_body.json")"
  fi
fi

echo "==================== 4. 建流程定义（拿到 DRAFT 版本） ===================="
DEF_BODY=$(jq -nc --arg c "${TAG}_flow" --arg n "N7 验收流程 ${TS}" '{flow_code:$c,flow_name:$n,biz_type:"N7_ACCEPT",is_default:false,allow_auto_approve:false}')
CODE=$(call POST /api/business/flow/def "$DEF_BODY")
DEF_ID=$(bodyf '.data.id')
VER_ID=$(bodyf '.data.version_id')
if [ "$CODE" = "200" ] && [ -n "$VER_ID" ] && [ "$VER_ID" != "null" ]; then
  pass "建流程 $DEF_ID（DRAFT 版本 $VER_ID）"
else
  fail "建流程失败：HTTP $CODE $(jq -c . "$OUT_DIR/last_body.json")"; exit 1
fi

# 组图：START → APPROVAL(可带兜底/角色) → END
graph_body() { # $1=审批节点 JSON 片段
  jq -nc --argjson appr "$1" '{
    nodes: [
      {key:"n_start", name:"开始", type:"START"},
      $appr,
      {key:"n_end", name:"结束", type:"END"}
    ],
    edges: [
      {key:"e1", from:"n_start", to:"n_appr"},
      {key:"e2", from:"n_appr", to:"n_end"}
    ],
    change_note: "N-7 验收"
  }'
}

echo "==================== 5. 验收②：兜底人=不存在的用户 → 400 + 点名节点 ===================="
GHOST=$(psql_q "select gen_random_uuid()" 2>/dev/null || echo "0192f0c0-0000-7000-8000-000000000001")
APPR=$(jq -nc --arg u "$GHOST" '{key:"n_appr",type:"APPROVAL",name:"财务复核",assignee_type:"INITIATOR",fallback_user_id:$u,empty_assignee:"ERROR",timeout_action:"REMIND"}')
CODE=$(call PUT "/api/business/flow/version/$VER_ID/graph" "$(graph_body "$APPR")")
MSG=$(bodyf '.msg // .message')
CODEVAL=$(bodyf '.code')
echo "  HTTP $CODE  code=$CODEVAL  msg=$MSG"
if [ "$CODE" = "400" ] && [ "$CODEVAL" = "65042" ] && printf '%s' "$MSG" | grep -q "n_appr"; then
  pass "② 不存在的兜底人 → 400/65042，点名节点 n_appr"
else
  fail "② 期望 400/65042 且点名 n_appr，实际 HTTP $CODE code=$CODEVAL msg=$MSG"
fi
printf 'HTTP %s\ncode=%s\nmsg=%s\n' "$CODE" "$CODEVAL" "$MSG" > "$OUT_DIR/02_ghost_fallback.txt"

echo "==================== 6. 验收③：兜底人=停用用户 → 400 ===================="
APPR=$(jq -nc --arg u "$STOPPED_USER" '{key:"n_appr",type:"APPROVAL",name:"财务复核",assignee_type:"INITIATOR",fallback_user_id:$u,empty_assignee:"ERROR",timeout_action:"REMIND"}')
CODE=$(call PUT "/api/business/flow/version/$VER_ID/graph" "$(graph_body "$APPR")")
MSG=$(bodyf '.msg // .message'); CODEVAL=$(bodyf '.code')
echo "  HTTP $CODE  code=$CODEVAL  msg=$MSG"
if [ "$CODE" = "400" ] && [ "$CODEVAL" = "65042" ] && printf '%s' "$MSG" | grep -q "已停用"; then
  pass "③ 停用的兜底人 → 400/65042（文案含「已停用」）"
else
  fail "③ 期望 400/65042 且文案含「已停用」，实际 HTTP $CODE code=$CODEVAL msg=$MSG"
fi
printf 'HTTP %s\ncode=%s\nmsg=%s\n' "$CODE" "$CODEVAL" "$MSG" > "$OUT_DIR/03_stopped_fallback.txt"

echo "==================== 7. 验收④：升级链指向已删角色 → 400 ===================="
# 先把「上级角色」软删（它在 ORPHAN_ROLE.escalation_role_id 上）
CODE=$(call DELETE "/api/system/role/$UP_ROLE_ID")
[ "$CODE" = "200" ] && pass "上级角色 $UP_ROLE_ID 已软删（孤儿链就绪）" || fail "删上级角色失败：HTTP $CODE $(jq -c . "$OUT_DIR/last_body.json")"
APPR=$(jq -nc --arg r "$ORPHAN_ROLE" '{key:"n_appr",type:"APPROVAL",name:"采购经理审批",assignee_type:"ROLE",assignees:[{type:"ROLE",ref_id:$r,sort_order:1}],empty_assignee:"ERROR",timeout_action:"REMIND"}')
CODE=$(call PUT "/api/business/flow/version/$VER_ID/graph" "$(graph_body "$APPR")")
MSG=$(bodyf '.msg // .message'); CODEVAL=$(bodyf '.code')
echo "  HTTP $CODE  code=$CODEVAL  msg=$MSG"
if [ "$CODE" = "400" ] && [ "$CODEVAL" = "65040" ]; then
  pass "④ 升级链指向已删角色 → 400/65040"
else
  fail "④ 期望 400/65040，实际 HTTP $CODE code=$CODEVAL msg=$MSG"
fi
printf 'HTTP %s\ncode=%s\nmsg=%s\n' "$CODE" "$CODEVAL" "$MSG" > "$OUT_DIR/04_escalation_deleted.txt"

echo "==================== 8. 验收⑤：合法配置 → 保存 200 + 发布 200 ===================="
APPR=$(jq -nc --arg u "$ACTIVE_USER" '{key:"n_appr",type:"APPROVAL",name:"财务复核",assignee_type:"INITIATOR",fallback_user_id:$u,empty_assignee:"ERROR",timeout_action:"REMIND"}')
CODE=$(call PUT "/api/business/flow/version/$VER_ID/graph" "$(graph_body "$APPR")")
MSG=$(bodyf '.msg // .message')
echo "  保存：HTTP $CODE  msg=$MSG  warnings=$(bodyf '.data.warnings|length')"
[ "$CODE" = "200" ] && pass "⑤ 合法配置（兜底人=在职用户）→ 保存 200" \
  || fail "⑤ 合法配置保存应为 200，实际 $CODE $MSG"
printf 'save HTTP %s\n%s\n' "$CODE" "$(jq -c '.data // .' "$OUT_DIR/last_body.json")" > "$OUT_DIR/05_legal_save.txt"

CODE=$(call POST "/api/business/flow/def/$DEF_ID/publish")
MSG=$(bodyf '.msg // .message')
echo "  发布：HTTP $CODE  msg=$MSG"
[ "$CODE" = "200" ] && pass "⑤ 发布 200（全流程预演通过）" || fail "⑤ 发布应为 200，实际 $CODE $MSG"
printf 'publish HTTP %s\n%s\n' "$CODE" "$(jq -c '.data // .' "$OUT_DIR/last_body.json")" > "$OUT_DIR/06_legal_publish.txt"

echo "==================== 9. 清理夹具 ===================="
for path in "/api/business/flow/def/$DEF_ID" "/api/system/role/$ORPHAN_ROLE" "/api/system/role/$FB_ROLE" \
            "/api/system/role/$ROLE_ID" "/api/system/user/$ACTIVE_USER" "/api/system/user/$STOPPED_USER"; do
  code=$(call DELETE "$path")
  echo "  DELETE $path → $code"
done

echo "==================== 结果 ===================="
if [ "$FAIL" -eq 0 ]; then
  echo "RESULT: PASS（① 留痕含 before/after；②③ 兜底人 400；④ 升级链 400；⑤ 合法保存+发布 200）"
  exit 0
else
  echo "RESULT: FAIL（$FAIL 项不通过）"
  exit 1
fi
