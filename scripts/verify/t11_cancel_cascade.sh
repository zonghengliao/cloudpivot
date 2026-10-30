#!/usr/bin/env bash
# ============================================================================
# T11 验收：父任务取消 → 子任务级联取消 + 父已取消拒绝子流转
#
# 真实 curl 打**隔离实例**（默认 127.0.0.1:18091 / 库 cloudpivot_t11），
# 全程不碰生产 8023、不碰 dev 8024。
#
# 用法：
#   bash scripts/verify/t11_cancel_cascade.sh
#   T11_BASE=http://127.0.0.1:18091 T11_DB=cloudpivot_t11 bash scripts/verify/t11_cancel_cascade.sh
#
# 环境变量：
#   T11_BASE         后端根地址（脚本自己拼 /api），默认 http://127.0.0.1:18091
#   T11_USER         登录账号，默认 admin
#   T11_PASSWORD     登录口令（可空：按 T11_PASSWORD → T11_NEW_PASSWORD → 口令缓存 → env 文件顺序试）
#   T11_NEW_PASSWORD 首登强制改密时改成的口令，默认 T11-Accept-2026!
#   T11_ENV_FILE     读库连接 / 初始口令的 env 文件，默认 /etc/cloudpivot/cloudpivot-dev.env
#   T11_DB           断言用的库名，默认 cloudpivot_t11（必须与实例指向同一个库）
#
# 断言：
#   ① 建父 + 3 子 → 子状态分别推到 TODO / DOING / REVIEW（父任务被 T10 级联进 DOING）
#   ② 父任务推到 CANCELLED → 200
#   ③ 三个子任务全部 CANCELLED（漏洞 1 的修复：不留「父已取消 / 子进行中」）
#   ④ 子任务的 cancel_reason 写的是「父任务已取消，级联取消」（审计能看出来不是人点的）
#   ⑤ 父任务 progress 不变（红线：级联不改 progress 公式）
#   ⑥ 父已取消 + 子被回填成 TODO → 子推 DOING 被拒 409 + 63021（漏洞 2 的修复）
#   ⑦ 父已取消 + 子推 CANCELLED → 200（幂等收敛，不拦）
#   ⑧ 变更日志：父 1 条 + 每个被取消的子任务各 1 条（走 /business/changelog/page）
#   ⑨ 收尾：删任务 + 终止本次验收造出的审批单（不留孤儿）
# ============================================================================
set -uo pipefail

BASE="${T11_BASE:-http://127.0.0.1:18091}"
API="$BASE/api"
USERNAME="${T11_USER:-admin}"
NEW_PASSWORD="${T11_NEW_PASSWORD:-T11-Accept-2026!}"
PWD_FILE="${T11_PWD_FILE:-/tmp/t11_admin_pwd}"
ENV_FILE="${T11_ENV_FILE:-/etc/cloudpivot/cloudpivot-dev.env}"
DB_NAME="${T11_DB:-cloudpivot_t11}"

OK_N=0
BAD_N=0
SKIP_N=0
TOKEN=""
REQ_CODE=""
BODY=""

ok() { printf '[PASS] %s\n' "$1"; OK_N=$((OK_N + 1)); }
bad() { printf '[FAIL] %s\n' "$1"; BAD_N=$((BAD_N + 1)); }
skip() { printf '[SKIP] %s\n' "$1"; SKIP_N=$((SKIP_N + 1)); }
expect_eq() { # $1=描述 $2=期望 $3=实际 [$4=原文]
  if [ "$2" = "$3" ]; then
    ok "$1（实际 $3）"
  else
    bad "$1：期望 [$2] 实际 [$3] ${4:-}"
  fi
}

# ---------------------------------------------------------------- 数据库帮助函数
# 用应用账号直连（表 owner 才对）；口令只从 env 文件读，不落任何输出。
set -a
# shellcheck disable=SC1090
. "$ENV_FILE" 2>/dev/null
set +a
psql_q() { # $1=SQL → stdout（-At，多行用 ~ 拼）
  PGPASSWORD="${NIK_DATABASE_PASSWORD:-}" psql -h "${NIK_DATABASE_HOST:-127.0.0.1}" \
    -p "${NIK_DATABASE_PORT:-5432}" -U "${NIK_DATABASE_USER:-cloudpivot}" -d "$DB_NAME" -Atc "$1" 2>&1 | head -1
}
psql_exec() { # $1=SQL
  PGPASSWORD="${NIK_DATABASE_PASSWORD:-}" psql -h "${NIK_DATABASE_HOST:-127.0.0.1}" \
    -p "${NIK_DATABASE_PORT:-5432}" -U "${NIK_DATABASE_USER:-cloudpivot}" -d "$DB_NAME" -Atc "$1" >/dev/null 2>&1
}

# ---------------------------------------------------------------- HTTP 帮助函数
# 调用后读全局 REQ_CODE / BODY。**不要**用 `x=$(call ...)`：命令替换是子 shell，
# 全局变量改不回来（T10 实测：22 条断言里 11 条假红）。
call() { # $1=方法 $2=url [$3=JSON body]
  local method="$1" url="$2" body="${3:-}" raw
  if [ -n "$body" ]; then
    raw=$(curl -s -m 15 -w $'\n%{http_code}' -X "$method" "$url" \
      -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d "$body")
  else
    raw=$(curl -s -m 15 -w $'\n%{http_code}' -X "$method" "$url" \
      -H "Authorization: Bearer $TOKEN")
  fi
  REQ_CODE=$(printf '%s\n' "$raw" | tail -1)
  BODY=$(printf '%s\n' "$raw" | sed '$d')
}

login_with() { # $1=口令 → 打印登录响应
  curl -s -m 10 -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d "$(jq -nc --arg u "$USERNAME" --arg p "$1" '{username:$u,password:$p}')"
}

# ---------------------------------------------------------------------- 登录
BOOT_PWD="$(sed -n 's/^NIK_ADMIN_PASSWORD=//p' "$ENV_FILE" 2>/dev/null | head -1)"
CANDIDATES=()
[ -n "${T11_PASSWORD:-}" ] && CANDIDATES+=("${T11_PASSWORD}")
CANDIDATES+=("$NEW_PASSWORD")
[ -f "$PWD_FILE" ] && CANDIDATES+=("$(cat "$PWD_FILE")")
[ -n "$BOOT_PWD" ] && CANDIDATES+=("$BOOT_PWD")

USED_PWD=""
LOGIN_JSON=""
for cand in "${CANDIDATES[@]}"; do
  [ -z "$cand" ] && continue
  resp="$(login_with "$cand")"
  token="$(printf '%s' "$resp" | jq -r '.data.access_token // empty' 2>/dev/null)"
  if [ -n "$token" ]; then
    USED_PWD="$cand"
    LOGIN_JSON="$resp"
    break
  fi
done

if [ -z "$USED_PWD" ]; then
  bad "登录失败（试了 ${#CANDIDATES[@]} 个候选口令）"
  echo "===== 汇总：通过 $OK_N 条 / 失败 $BAD_N 条 =====	"
  exit 1
fi
TOKEN="$(printf '%s' "$LOGIN_JSON" | jq -r '.data.access_token')"
ok "登录成功（$USERNAME）"

# 首登强制改密：改密 → 存口令 → 用新口令重登（旧令牌会因 token_version 递增失效）
if [ "$(printf '%s' "$LOGIN_JSON" | jq -r '.data.must_change_password // false')" = "true" ]; then
  chg=$(curl -s -m 10 -X PUT "$API/system/auth/password" \
    -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    -d "$(jq -nc --arg o "$USED_PWD" --arg n "$NEW_PASSWORD" '{old_password:$o,new_password:$n}')")
  if [ "$(printf '%s' "$chg" | jq -r '.code')" = "0" ]; then
    printf '%s\n' "$NEW_PASSWORD" > "$PWD_FILE"
    USED_PWD="$NEW_PASSWORD"
    LOGIN_JSON="$(login_with "$NEW_PASSWORD")"
    TOKEN="$(printf '%s' "$LOGIN_JSON" | jq -r '.data.access_token // empty')"
    ok "首登强制改密完成并重登"
  else
    bad "首登强制改密失败：$chg"
  fi
fi

# 鉴权在位：无令牌 401
SAVED="$TOKEN"
TOKEN=""
call "GET" "$API/business/task/page?page=1&size=1"
expect_eq "无令牌访问 /business/task/page 必须 401" "401" "$REQ_CODE"
TOKEN="$SAVED"

# ------------------------------------------------------------------- 夹具：父 + 3 子
STAMP="$(date +%Y%m%d-%H%M%S)"
call "POST" "$API/business/task" \
  "$(jq -nc --arg t "T11-取消级联-$STAMP-父" '{title:$t,description:"T11 验收：父任务取消级联"}')"
expect_eq "建父任务 200" "200" "$REQ_CODE" "$BODY"
PARENT_ID="$(printf '%s' "$BODY" | jq -r '.data.id // empty')"
PARENT_NO="$(printf '%s' "$BODY" | jq -r '.data.task_no // empty')"
PARENT_PROGRESS_BEFORE="$(printf '%s' "$BODY" | jq -r '.data.progress')"
echo "      父任务 id=$PARENT_ID no=$PARENT_NO progress=$PARENT_PROGRESS_BEFORE"

CHILD_IDS=()
CHILD_NOS=()
for idx in 1 2 3; do
  call "POST" "$API/business/task/$PARENT_ID/subtask" \
    "$(jq -nc --arg t "T11-取消级联-$STAMP-子$idx" '{title:$t}')"
  expect_eq "建子任务 $idx 200" "200" "$REQ_CODE" "$BODY"
  CHILD_IDS+=("$(printf '%s' "$BODY" | jq -r '.data.id // empty')")
  CHILD_NOS+=("$(printf '%s' "$BODY" | jq -r '.data.task_no // empty')")
done
echo "      子任务 id=${CHILD_IDS[*]}"

# ① 三个子任务推到不同状态：子1 DOING / 子2 DOING→REVIEW / 子3 保持 TODO
call "PATCH" "$API/business/task/${CHILD_IDS[0]}/status" '{"to":"DOING"}'
expect_eq "子1 TODO→DOING 200" "200" "$REQ_CODE" "$BODY"
call "PATCH" "$API/business/task/${CHILD_IDS[1]}/status" '{"to":"DOING"}'
expect_eq "子2 TODO→DOING 200" "200" "$REQ_CODE" "$BODY"
call "PATCH" "$API/business/task/${CHILD_IDS[1]}/status" '{"to":"REVIEW"}'
expect_eq "子2 DOING→REVIEW 200" "200" "$REQ_CODE" "$BODY"
call "GET" "$API/business/task/$PARENT_ID"
expect_eq "父任务被 T10 级联进 DOING（前置条件）" "DOING" \
  "$(printf '%s' "$BODY" | jq -r '.data.status')"
PARENT_PROGRESS_BEFORE="$(printf '%s' "$BODY" | jq -r '.data.progress')"
echo "      取消前：父=$(printf '%s' "$BODY" | jq -r '.data.status') progress=$PARENT_PROGRESS_BEFORE"

# ② 父任务推到 CANCELLED
call "PATCH" "$API/business/task/$PARENT_ID/status" \
  '{"to":"CANCELLED","reason":"T11 验收：整条批次作废"}'
expect_eq "父任务 → CANCELLED 200" "200" "$REQ_CODE" "$BODY"
expect_eq "父任务状态已是 CANCELLED" "CANCELLED" "$(printf '%s' "$BODY" | jq -r '.data.status')"

# ③ 三个子任务全部 CANCELLED（不同状态都要被取消）
EXPECT_STATES=(DOING REVIEW CANCELLED)
for idx in 0 1 2; do
  call "GET" "$API/business/task/${CHILD_IDS[$idx]}"
  expect_eq "子$((idx + 1)) 被级联取消（原状态 ${EXPECT_STATES[$idx]}）" "CANCELLED" \
    "$(printf '%s' "$BODY" | jq -r '.data.status')"
done

# ④ 库里的 cancel_reason 写清「级联取消」
for idx in 0 1 2; do
  reason="$(psql_q "SELECT cancel_reason FROM biz_task WHERE id = '${CHILD_IDS[$idx]}'::uuid")"
  expect_eq "子$((idx + 1)) 的 cancel_reason 是级联原因" "父任务已取消，级联取消" "$reason"
done

# 库里一致性：父 CANCELLED 且没有任何非终态子孙
left="$(psql_q "SELECT count(*) FROM biz_task WHERE parent_id = '$PARENT_ID'::uuid AND deleted_at IS NULL AND status NOT IN ('DONE','CANCELLED')")"
expect_eq "父任务下已无「未结束」子任务（不变式）" "0" "$left"

# ⑤ 父任务 progress 不变（红线：级联不改 progress 公式）
call "GET" "$API/business/task/$PARENT_ID"
expect_eq "父任务 progress 不变（$PARENT_PROGRESS_BEFORE）" "$PARENT_PROGRESS_BEFORE" \
  "$(printf '%s' "$BODY" | jq -r '.data.progress')"

# ⑥ 父已取消 + 子被回填成 TODO → 子推 DOING 被拒 409 + 63021
psql_exec "UPDATE biz_task SET status = 'TODO', cancel_reason = NULL WHERE id = '${CHILD_IDS[2]}'::uuid"
call "PATCH" "$API/business/task/${CHILD_IDS[2]}/status" '{"to":"DOING"}'
expect_eq "父已取消时子任务推 DOING 被拒（HTTP 409）" "409" "$REQ_CODE" "$BODY"
expect_eq "父已取消时子任务推 DOING 的业务码" "63021" "$(printf '%s' "$BODY" | jq -r '.code')"
printf '      文案：%s\n' "$(printf '%s' "$BODY" | jq -r '.msg')"

# 父已取消时推 REVIEW 同样被拒
call "PATCH" "$API/business/task/${CHILD_IDS[2]}/status" '{"to":"REVIEW"}'
expect_eq "父已取消时子任务推 REVIEW 被拒（HTTP 409）" "409" "$REQ_CODE" "$BODY"
expect_eq "父已取消时子任务推 REVIEW 的业务码" "63021" "$(printf '%s' "$BODY" | jq -r '.code')"

# ⑦ 父已取消时子任务自己被取消 → 允许（幂等收敛）
call "PATCH" "$API/business/task/${CHILD_IDS[2]}/status" '{"to":"CANCELLED","reason":"T11 验收：幂等收敛"}'
expect_eq "父已取消时子任务自己取消 → 200（幂等放行）" "200" "$REQ_CODE" "$BODY"
expect_eq "子3 状态回升为 CANCELLED" "CANCELLED" "$(printf '%s' "$BODY" | jq -r '.data.status')"

# ⑧ 变更日志：父 1 条 + 每个被取消的子任务各 1 条
call "GET" "$API/business/changelog/page?resource=biz_task&resource_id=$PARENT_ID&page=1&page_size=20"
if [ "$REQ_CODE" = "200" ]; then
  n="$(printf '%s' "$BODY" | jq -r '[.data.list[]?] | length' 2>/dev/null)"
  action="$(printf '%s' "$BODY" | jq -r '.data.list[0].action // empty' 2>/dev/null)"
  if [ "${n:-0}" -ge 1 ] && [ "$action" = "STATUS" ]; then
    ok "父任务有 $n 条变更日志（action=$action）"
  else
    bad "父任务变更日志不符：n=${n:-0} action=${action:-none}"
  fi
else
  skip "变更日志接口不可用（HTTP $REQ_CODE）"
fi
for idx in 0 1; do
  call "GET" "$API/business/changelog/page?resource=biz_task&resource_id=${CHILD_IDS[$idx]}&page=1&page_size=20"
  if [ "$REQ_CODE" = "200" ]; then
    n="$(printf '%s' "$BODY" | jq -r '[.data.list[]?] | length' 2>/dev/null)"
    action="$(printf '%s' "$BODY" | jq -r '.data.list[0].action // empty' 2>/dev/null)"
    expect_eq "子$((idx + 1)) 有 1 条 STATUS 变更日志（级联留痕）" "STATUS" "${action:-none}" "(共 ${n:-0} 条)"
  else
    skip "子$((idx + 1)) 变更日志接口不可用（HTTP $REQ_CODE）"
  fi
done

# ⑨ 收尾：删任务（子先父后），并终止本次验收造出的审批单（不污染库）
for id in "${CHILD_IDS[@]}"; do
  call "DELETE" "$API/business/task/$id"
  expect_eq "清理：删子任务 $id" "200" "$REQ_CODE" "$BODY"
done
call "DELETE" "$API/business/task/$PARENT_ID"
expect_eq "清理：删父任务" "200" "$REQ_CODE" "$BODY"
for no in "${CHILD_NOS[@]}"; do
  psql_exec "UPDATE biz_flow_instance SET status = 'CANCELLED', ended_at = now(), updated_at = now(), result_comment = 'T11 验收清理' WHERE biz_no = '$no' AND status = 'PENDING'"
done
left="$(psql_q "SELECT count(*) FROM biz_task WHERE title LIKE 'T11-取消级联-%' AND deleted_at IS NULL")"
expect_eq "收尾：库里没有残留的未删任务" "0" "$left"

echo
echo "===== 汇总：通过 $OK_N 条 / 失败 $BAD_N 条 / 跳过 $SKIP_N 条 ====="
[ "$BAD_N" -eq 0 ] || exit 1
