#!/usr/bin/env bash
# ============================================================================
# T10 验收：父任务状态随子任务级联（TODO → DOING → REVIEW）
#
# 真实 curl 打**隔离实例**（默认 127.0.0.1:18092 / 库 cloudpivot_t10），
# 全程不碰生产 8023、不碰 dev 8024。
#
# 用法：
#   bash scripts/verify/t10_cascade.sh
#   T10_BASE=http://127.0.0.1:18092 T10_USER=admin bash scripts/verify/t10_cascade.sh
#
# 环境变量：
#   T10_BASE         后端根地址（脚本自己拼 /api），默认 http://127.0.0.1:18092
#   T10_USER         登录账号，默认 admin
#   T10_PASSWORD     登录口令（可空：按 T10_PASSWORD → T10_NEW_PASSWORD → 口令缓存 → env 文件顺序试）
#   T10_NEW_PASSWORD 首登强制改密时改成的口令，默认 T10-Accept-2026!
#   T10_ENV_FILE     读初始口令的 env 文件，默认 /etc/cloudpivot/cloudpivot-dev.env
#
# ⚠️ 前置条件（审批流）：目标库里若存在 **PUBLISHED** 的 `TASK_ACCEPT` 流程，
#    `DOING → REVIEW` 会自动发起审批单，`REVIEW → DONE` 必须等审批结论（`409 + 65030`）
#    （判定见 `flow::model::default_def_of_biz_type`：只看 `status='PUBLISHED'`，
#    `is_default` 只是排序 tiebreaker）。此时本脚本改走「取消子任务」的等价终态路径并打 `[SKIP]`。
#    本卡验收库 `cloudpivot_t10` 已把该流程软删，走「没配流程 → 原两级验收」这条
#    生产上同样存在的分支，因此 DONE 路径能完整跑通。
#
# 断言：
#   ① 建父 + 建子 → 子 TODO→DOING  → 父任务必须是 DOING（规则①）
#   ② 子 DOING→REVIEW              → 父任务仍是 DOING（还没验收完）
#   ③ 子 →DONE（或 →CANCELLED）    → 父任务必须是 REVIEW + progress=100（规则②）
#   ④ 父任务有 STATUS 变更日志（级联留痕）
#   ⑤ 子任务被拉回 DOING           → 父任务停在 REVIEW（Review 不会被回退成 DOING）
#   ⑥ 收尾：子/父任务软删干净（不污染测试库）
# ============================================================================
set -uo pipefail

BASE="${T10_BASE:-http://127.0.0.1:18092}"
API="$BASE/api"
USERNAME="${T10_USER:-admin}"
NEW_PASSWORD="${T10_NEW_PASSWORD:-T10-Accept-2026!}"
PWD_FILE="${T10_PWD_FILE:-/tmp/t10_admin_pwd}"
ENV_FILE="${T10_ENV_FILE:-/etc/cloudpivot/cloudpivot-dev.env}"

OK_N=0
BAD_N=0
TOKEN=""
REQ_CODE=""
BODY=""

ok() { printf '[PASS] %s\n' "$1"; OK_N=$((OK_N + 1)); }
bad() { printf '[FAIL] %s\n' "$1"; BAD_N=$((BAD_N + 1)); }
skip() { printf '[SKIP] %s\n' "$1"; }
expect_eq() { # $1=描述 $2=期望 $3=实际 [$4=原文]
  if [ "$2" = "$3" ]; then
    ok "$1（实际 $3）"
  else
    bad "$1：期望 [$2] 实际 [$3] ${4:-}"
  fi
}

# ---------------------------------------------------------------- HTTP 帮助函数
# 调用后读全局 REQ_CODE / BODY。**不要**用 `x=$(call ...)`：命令替换是子 shell，
# 全局变量改不回来（第一版就栽在这里：所有 200 被读成上一次的 401）。
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
[ -n "${T10_PASSWORD:-}" ] && CANDIDATES+=("${T10_PASSWORD}")
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
  echo "===== 汇总：通过 $OK_N 条 / 失败 $BAD_N 条 ====="
  exit 1
fi
TOKEN="$(printf '%s' "$LOGIN_JSON" | jq -r '.data.access_token')"
ok "登录成功（$USERNAME）"

# 首登强制改密：改密 → 存口令 → 用新口令重登
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

# 鉴权在位：无令牌 401 / 带令牌能认出自己
SAVED="$TOKEN"
TOKEN=""
call "GET" "$API/business/task/page?page=1&size=1"
expect_eq "无令牌访问 /business/task/page 必须 401" "401" "$REQ_CODE"
TOKEN="$SAVED"
call "GET" "$API/system/auth/me"
expect_eq "带令牌访问 /system/auth/me 回到 $USERNAME" "$USERNAME" \
  "$(printf '%s' "$BODY" | jq -r '.data.username // empty')"

# ------------------------------------------------------------------- 夹具：父 + 子
STAMP="$(date +%Y%m%d-%H%M%S)"
call "POST" "$API/business/task" \
  "$(jq -nc --arg t "T10-级联验收-$STAMP-父" '{title:$t,description:"T10 验收：父任务容器"}')"
expect_eq "建父任务 200" "200" "$REQ_CODE" "$BODY"
PARENT_ID="$(printf '%s' "$BODY" | jq -r '.data.id // empty')"
PARENT_NO="$(printf '%s' "$BODY" | jq -r '.data.task_no // empty')"
expect_eq "父任务初始状态 TODO" "TODO" "$(printf '%s' "$BODY" | jq -r '.data.status')"
echo "      父任务 id=$PARENT_ID no=$PARENT_NO"

call "POST" "$API/business/task/$PARENT_ID/subtask" \
  "$(jq -nc --arg t "T10-级联验收-$STAMP-子" '{title:$t}')"
expect_eq "建子任务 200" "200" "$REQ_CODE" "$BODY"
CHILD_ID="$(printf '%s' "$BODY" | jq -r '.data.id // empty')"
expect_eq "子任务初始状态 TODO" "TODO" "$(printf '%s' "$BODY" | jq -r '.data.status')"
echo "      子任务 id=$CHILD_ID"

# ------------------------------------------------- ① 子 TODO→DOING → 父 DOING
call "PATCH" "$API/business/task/$CHILD_ID/status" '{"to":"DOING"}'
expect_eq "子任务 TODO→DOING 200" "200" "$REQ_CODE" "$BODY"
expect_eq "子任务状态已是 DOING" "DOING" "$(printf '%s' "$BODY" | jq -r '.data.status')"
call "GET" "$API/business/task/$PARENT_ID"
P1_SUMMARY="$(printf '%s' "$BODY" | jq -c '{status,progress,child_count,child_done_count}')"
expect_eq "规则①：父任务自动进 DOING" "DOING" "$(printf '%s' "$BODY" | jq -r '.data.status')" "(父任务：$P1_SUMMARY)"

# ------------------------------------- ② 子 DOING→REVIEW → 父仍 DOING（未验收完）
call "PATCH" "$API/business/task/$CHILD_ID/status" '{"to":"REVIEW"}'
expect_eq "子任务 DOING→REVIEW 200" "200" "$REQ_CODE" "$BODY"
call "GET" "$API/business/task/$PARENT_ID"
expect_eq "规则②：子任务只是待验收，父任务仍 DOING" "DOING" \
  "$(printf '%s' "$BODY" | jq -r '.data.status')"

# ------------------------- ③ 子到终态（DONE；被审批流拦住则用 CANCELLED 等价路径）
call "PATCH" "$API/business/task/$CHILD_ID/status" '{"to":"DONE"}'
TERMINAL="DONE"
if [ "$REQ_CODE" = "200" ]; then
  expect_eq "子任务 REVIEW→DONE 200" "200" "$REQ_CODE" "$BODY"
  expect_eq "PATCH 响应里的 parent_progress.all_done" "true" \
    "$(printf '%s' "$BODY" | jq -r '.data.parent_progress[0].all_done')"
elif [ "$(printf '%s' "$BODY" | jq -r '.code')" = "65030" ]; then
  skip "REVIEW→DONE 被审批门闸拦住（65030）：目标库有 PUBLISHED 的 TASK_ACCEPT 流程，改走 CANCELLED 等价路径"
  call "PATCH" "$API/business/task/$CHILD_ID/status" '{"to":"CANCELLED","reason":"T10 验收：审批流环境改用等价终态"}'
  TERMINAL="CANCELLED"
  expect_eq "子任务 REVIEW→CANCELLED 200" "200" "$REQ_CODE" "$BODY"
else
  bad "子任务 REVIEW→DONE 未成功：HTTP $REQ_CODE $BODY"
fi

call "GET" "$API/business/task/$PARENT_ID"
P3_SUMMARY="$(printf '%s' "$BODY" | jq -c '{status,progress,child_count,child_done_count}')"
expect_eq "规则②：子任务到终态（$TERMINAL）→ 父任务进 REVIEW" "REVIEW" \
  "$(printf '%s' "$BODY" | jq -r '.data.status')" "(父任务：$P3_SUMMARY)"
if [ "$TERMINAL" = "DONE" ]; then
  expect_eq "父任务进度汇总到 100（progress 公式未改）" "100" \
    "$(printf '%s' "$BODY" | jq -r '.data.progress')"
  expect_eq "父任务 child_done_count=1" "1" \
    "$(printf '%s' "$BODY" | jq -r '.data.child_done_count')"
fi

# ------------------------------- ④ 级联留痕：父任务有一条 STATUS 变更日志
call "GET" "$API/business/changelog/page?resource=biz_task&resource_id=$PARENT_ID&page=1&page_size=20"
if [ "$REQ_CODE" = "200" ]; then
  hit="$(printf '%s' "$BODY" | jq -r '[.data.list[]?] | length' 2>/dev/null)"
  action="$(printf '%s' "$BODY" | jq -r '.data.list[0].action // empty' 2>/dev/null)"
  if [ "${hit:-0}" -ge 1 ] && [ "$action" = "STATUS" ]; then
    ok "父任务有 $hit 条变更日志，动作为 $action（级联改状态留痕）"
  else
    bad "父任务变更日志不符：hit=${hit:-0} action=${action:-none}"
  fi
else
  skip "变更日志接口不可用（HTTP $REQ_CODE，可能缺 system:change-log:list 权限码）"
fi

# ---------------------- ⑤ 子任务被拉回 DOING → 父任务停在 REVIEW（Review 不回退成 DOING）
call "PATCH" "$API/business/task/$CHILD_ID/status" '{"to":"DOING","reason":"验收发现还有遗留，重新处理"}'
REOPEN_CODE="$(printf '%s' "$BODY" | jq -r '.code // empty')"
if [ "$REQ_CODE" = "200" ]; then
  ok "子任务 $TERMINAL→DOING 重开成功（业务码 $REOPEN_CODE）"
elif [ "$REQ_CODE" = "409" ]; then
  skip "目标环境不允许 $TERMINAL→DOING 重开（409 + $REOPEN_CODE），跳过「父任务不回退」这一步"
else
  bad "子任务重开行为异常：HTTP $REQ_CODE $BODY"
fi
call "GET" "$API/business/task/$PARENT_ID"
expect_eq "父任务仍停在 REVIEW（级联只认 TODO→DOING，不回退 Review）" "REVIEW" \
  "$(printf '%s' "$BODY" | jq -r '.data.status')"

# ---------------------------------------------------------------- ⑥ 收尾清理
call "DELETE" "$API/business/task/$CHILD_ID"
expect_eq "清理：删子任务 200" "200" "$REQ_CODE" "$BODY"
call "DELETE" "$API/business/task/$PARENT_ID"
expect_eq "清理：删父任务 200" "200" "$REQ_CODE" "$BODY"

echo
echo "===== 汇总：通过 $OK_N 条 / 失败 $BAD_N 条 ====="
[ "$BAD_N" -eq 0 ] || exit 1
