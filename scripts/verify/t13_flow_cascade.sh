#!/usr/bin/env bash
# ============================================================================
# T13 验收：审批通过后的父任务级联（settle 后独立事务 + 巡检兜底）
#
# 真实 curl 打**隔离实例**（默认 127.0.0.1:18093 / 库 cloudpivot_t13），
# 全程不碰生产 8023、不碰 dev 8024。
#
# 用法：
#   bash scripts/verify/t13_flow_cascade.sh
#   T13_BASE=http://127.0.0.1:18093 T13_DB=cloudpivot_t13 bash scripts/verify/t13_flow_cascade.sh
#
# 环境变量：
#   T13_BASE         后端根地址（脚本自己拼 /api），默认 http://127.0.0.1:18093
#   T13_DB           断言用的库名，默认 cloudpivot_t13（必须与实例指向同一个库）
#   T13_USER         登录账号，默认 admin
#   T13_PASSWORD     登录口令（可空：按 T13_PASSWORD → T13_NEW_PASSWORD → 口令缓存 → env 文件顺序试）
#   T13_NEW_PASSWORD 首登强制改密时改成的口令，默认 T13-Accept-2026!
#   T13_ENV_FILE     读库连接 / 初始口令的 env 文件，默认 /etc/cloudpivot/cloudpivot-dev.env
#
# 复跑前置（隔离实例，全程不碰 8023 prod / 8024 dev）：
#   su - postgres -c 'createdb -O cloudpivot cloudpivot_t13'
#   set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
#   export NIK_PROFILE=test NIK_DATABASE_NAME=cloudpivot_t13 \
#          NIK_SERVER_LISTEN=127.0.0.1:18093 NIK_REDIS_URL=redis://127.0.0.1:6379/9
#   cd backend && cargo build && setsid nohup ./target/debug/cloudpivot \
#          > /tmp/t13/server.log 2>&1 < /dev/null &
#   （等日志出现「数据库迁移完成: 应用 N 个迁移」→ curl .../health = 200）
#
# ⚠️ 本卡必须走**真审批**：目标库要有 PUBLISHED 的 `TASK_ACCEPT` 流程（V0127 种子），
#    否则 `DOING → REVIEW` 不会生成审批单，`REVIEW → DONE` 也不会被 65030 拦住，
#    测出来的就不是本卡的路径了。脚本第一步就会断言这一点。
#
# 断言：
#   ① 无令牌访问受保护路由 → 401
#   ② 建父 + 2 子（父 TODO）
#   ③ 子1 DOING → REVIEW → 真审批通过 → DONE；父任务仍是 DOING（还有子2没完成）
#   ④ 子2 DOING → REVIEW → 真审批通过 → DONE；**父任务必须是 REVIEW**（本卡核心）
#   ⑤ 父任务进度 = 100（progress 公式未改）
#   ⑥ 巡检接口 dry_run=true：造一条不一致 → 只报不修（库里状态不变）
#   ⑦ 巡检接口 dry_run=false：实修 → 父任务回到 REVIEW
#   ⑧ 修完再 dry_run：该父任务不再出现在不一致清单里
#   ⑨ 巡检接口无令牌 401（不是 404 —— 路由确实注册了）
#   ⑩ 收尾：任务软删干净，库里无本次残留的不一致
# ============================================================================
set -uo pipefail

BASE="${T13_BASE:-http://127.0.0.1:18093}"
API="$BASE/api"
DB="${T13_DB:-cloudpivot_t13}"
USERNAME="${T13_USER:-admin}"
NEW_PASSWORD="${T13_NEW_PASSWORD:-T13-Accept-2026!}"
PWD_FILE="${T13_PWD_FILE:-/tmp/t13_admin_pwd}"
ENV_FILE="${T13_ENV_FILE:-/etc/cloudpivot/cloudpivot-dev.env}"

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

# ---------------------------------------------------------------- HTTP 帮助函数
# 调用后读全局 REQ_CODE / BODY。**不要**用 `x=$(call ...)`：命令替换是子 shell，
# 全局变量改不回来（T10 实测：所有 200 被读成上一次的 401）。
call() { # $1=方法 $2=url [$3=JSON body]
  local method="$1" url="$2" body="${3:-}" raw
  if [ -n "$body" ]; then
    raw=$(curl -s -m 20 -w $'\n%{http_code}' -X "$method" "$url" \
      -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d "$body")
  else
    raw=$(curl -s -m 20 -w $'\n%{http_code}' -X "$method" "$url" \
      -H "Authorization: Bearer $TOKEN")
  fi
  REQ_CODE=$(printf '%s\n' "$raw" | tail -1)
  BODY=$(printf '%s\n' "$raw" | sed '$d')
}

login_with() { # $1=口令 → 打印登录响应
  curl -s -m 10 -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d "$(jq -nc --arg u "$USERNAME" --arg p "$1" '{username:$u,password:$p}')"
}

# psql：一律用**应用账号**直连（su - postgres 读不到 /root，且用错 owner 会 permission denied）
psql_q() { # $1=SQL → 打印结果（-At 单列）；失败返回空
  PGPASSWORD="$DB_PASSWORD" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB" \
    -At -v ON_ERROR_STOP=1 -c "$1" 2>/dev/null | head -1
}

# ---------------------------------------------------------------------- 登录
DB_HOST="$(sed -n 's/^NIK_DATABASE_HOST=//p' "$ENV_FILE" | head -1)"
DB_PORT="$(sed -n 's/^NIK_DATABASE_PORT=//p' "$ENV_FILE" | head -1)"
DB_USER="$(sed -n 's/^NIK_DATABASE_USER=//p' "$ENV_FILE" | head -1)"
DB_PASSWORD="$(sed -n 's/^NIK_DATABASE_PASSWORD=//p' "$ENV_FILE" | head -1)"
DB_HOST="${DB_HOST:-127.0.0.1}"
DB_PORT="${DB_PORT:-5432}"
DB_USER="${DB_USER:-cloudpivot}"

BOOT_PWD="$(sed -n 's/^NIK_ADMIN_PASSWORD=//p' "$ENV_FILE" 2>/dev/null | head -1)"
CANDIDATES=()
[ -n "${T13_PASSWORD:-}" ] && CANDIDATES+=("${T13_PASSWORD}")
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
  echo "===== 汇总：通过 $OK_N 条 / 失败 $BAD_N 条 / 跳过 $SKIP_N 条 ====="
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

# --------------------------------------------------- ① 鉴权在位（无令牌 401）
SAVED="$TOKEN"
TOKEN=""
call "GET" "$API/business/task/page?page=1&size=1"
expect_eq "① 无令牌访问 /business/task/page 必须 401" "401" "$REQ_CODE"
call "GET" "$API/business/task/admin/repair-parent-status?dry_run=true"
expect_eq "⑨ 巡检接口无令牌必须 401（不是 404：路由确实注册了）" "401" "$REQ_CODE"
TOKEN="$SAVED"

# ------------------------------- 前置：目标库必须有 PUBLISHED 的 TASK_ACCEPT 流程
FLOW_N="$(psql_q "SELECT count(*) FROM biz_flow_def d JOIN biz_flow_def_version v ON v.def_id = d.id WHERE d.biz_type = 'TASK_ACCEPT' AND v.version_status = 'PUBLISHED' AND d.deleted_at IS NULL")"
if [ "${FLOW_N:-0}" -ge 1 ] 2>/dev/null; then
  ok "前置：目标库有 PUBLISHED 的 TASK_ACCEPT 流程（$FLOW_N 个），本卡走真审批"
else
  bad "前置不满足：$DB 里没有 PUBLISHED 的 TASK_ACCEPT 流程（$FLOW_N），测不出真审批路径"
  echo "===== 汇总：通过 $OK_N 条 / 失败 $BAD_N 条 / 跳过 $SKIP_N 条 ====="
  exit 1
fi

# ------------------------------------------------------------- ② 夹具：父 + 2 子
STAMP="$(date +%Y%m%d-%H%M%S)"
call "POST" "$API/business/task" \
  "$(jq -nc --arg t "T13-审批级联-$STAMP-父" '{title:$t,description:"T13 验收：父任务容器"}')"
expect_eq "② 建父任务 200" "200" "$REQ_CODE" "$BODY"
PARENT_ID="$(printf '%s' "$BODY" | jq -r '.data.id // empty')"
expect_eq "② 父任务初始状态 TODO" "TODO" "$(printf '%s' "$BODY" | jq -r '.data.status')"
echo "      父任务 id=$PARENT_ID"

CHILD_IDS=()
for idx in 1 2; do
  call "POST" "$API/business/task/$PARENT_ID/subtask" \
    "$(jq -nc --arg t "T13-审批级联-$STAMP-子$idx" '{title:$t}')"
  expect_eq "② 建子任务$idx 200" "200" "$REQ_CODE" "$BODY"
  CHILD_IDS+=("$(printf '%s' "$BODY" | jq -r '.data.id // empty')")
  echo "      子任务$idx id=${CHILD_IDS[$((idx - 1))]}"
done

# 审批通过一个子任务（真审批：待办列表 → 同意）
approve_child() { # $1=子任务 id $2=任务标题关键字 → 打印结论
  call "PATCH" "$API/business/task/$1/status" '{"to":"DOING"}'
  if [ "$REQ_CODE" != "200" ]; then
    bad "子任务 $1 DOING 失败：HTTP $REQ_CODE $BODY"
    return 1
  fi
  call "PATCH" "$API/business/task/$1/status" '{"to":"REVIEW"}'
  if [ "$REQ_CODE" != "200" ]; then
    bad "子任务 $1 提交验收失败：HTTP $REQ_CODE $BODY"
    return 1
  fi
  local todo_id todo_no
  # 审批待办：拿「我的待办」里属于这次提交的那一条（按子任务标题关键字匹配）
  call "GET" "$API/business/flow/task/todo?page=1&size=100"
  todo_id="$(printf '%s' "$BODY" | jq -r --arg key "$2" \
    '.data.list[] | select((.title|contains($key))) | .id' 2>/dev/null | head -1)"
  todo_no="$(printf '%s' "$BODY" | jq -r --arg key "$2" \
    '.data.list[] | select((.title|contains($key))) | .instance_no' 2>/dev/null | head -1)"
  if [ -z "$todo_id" ]; then
    bad "子任务 $1 没有生成审批待办：$(printf '%s' "$BODY" | jq -c '{total:.data.total}')"
    return 1
  fi
  call "POST" "$API/business/flow/task/$todo_id/approve" '{"comment":"T13 验收：同意"}'
  if [ "$REQ_CODE" != "200" ]; then
    bad "审批同意失败（待办 $todo_id）：HTTP $REQ_CODE $BODY"
    return 1
  fi
  echo "      审批单 $todo_no 状态 → $(printf '%s' "$BODY" | jq -r '.data.instance_status')"
  return 0
}

child_title_key() { printf 'T13-审批级联-%s-子%s' "$STAMP" "$1"; }

# ---------------------- ③ 子1 审批通过 → DONE，父任务仍 DOING（还有子2没完成）
if approve_child "${CHILD_IDS[0]}" "$(child_title_key 1)"; then
  call "GET" "$API/business/task/${CHILD_IDS[0]}"
  expect_eq "③ 子1 审批通过后 → DONE（settle 写的）" "DONE" "$(printf '%s' "$BODY" | jq -r '.data.status')"
  call "GET" "$API/business/task/$PARENT_ID"
  P_SUMMARY="$(printf '%s' "$BODY" | jq -c '{status,progress,child_count,child_done_count}')"
  expect_eq "③ 子1 完成后父任务仍是 DOING（子2还没完成）" "DOING" \
    "$(printf '%s' "$BODY" | jq -r '.data.status')" "(父任务：$P_SUMMARY)"
else
  skip "③ 子1 审批链路未跑通，跳过父任务断言"
fi

# ---------------------- ④ 子2 审批通过 → DONE，父任务必须进 REVIEW（本卡核心）
if approve_child "${CHILD_IDS[1]}" "$(child_title_key 2)"; then
  call "GET" "$API/business/task/${CHILD_IDS[1]}"
  expect_eq "④ 子2 审批通过后 → DONE" "DONE" "$(printf '%s' "$BODY" | jq -r '.data.status')"
  call "GET" "$API/business/task/$PARENT_ID"
  P_SUMMARY="$(printf '%s' "$BODY" | jq -c '{status,progress,child_count,child_done_count}')"
  expect_eq "④ 子任务全部终态 → 父任务必须级联到 REVIEW（本卡核心验收点）" "REVIEW" \
    "$(printf '%s' "$BODY" | jq -r '.data.status')" "(父任务：$P_SUMMARY)"
  expect_eq "⑤ 父任务进度 = 100（progress 公式未改）" "100" \
    "$(printf '%s' "$BODY" | jq -r '.data.progress')"
else
  skip "④ 子2 审批链路未跑通，跳过核心断言"
fi

# -------------------------------- ⑥⑦⑧ 巡检接口：造一条不一致 → 干跑 → 实修 → 复查
# 造不一致：直接改库把父任务按回 DOING（模拟「settle 的独立事务级联失败」留下的形态）。
psql_q "UPDATE biz_task SET status = 'DOING', updated_at = now() WHERE id = '$PARENT_ID'" > /dev/null
GAP_RAW="$(psql_q "SELECT status FROM biz_task WHERE id = '$PARENT_ID'")"
expect_eq "⑥ 造数据：父任务被按回 DOING（模拟级联失败）" "DOING" "$GAP_RAW"

call "GET" "$API/business/task/admin/repair-parent-status?dry_run=true"
DRY_TOTAL="$(printf '%s' "$BODY" | jq -r '.data.total // -1')"
DRY_HIT="$(printf '%s' "$BODY" | jq -r --arg id "$PARENT_ID" '[.data.items[] | select(.id == $id)] | length')"
expect_eq "⑥ 干跑：命中本卡父任务 1 条" "1" "$DRY_HIT" "($(printf '%s' "$BODY" | jq -c '{total,fixed,failed}'))"
expect_eq "⑥ 干跑：items 里 target=REVIEW" "REVIEW" \
  "$(printf '%s' "$BODY" | jq -r --arg id "$PARENT_ID" '.data.items[] | select(.id == $id) | .target')"
expect_eq "⑥ 干跑：status_after 为 null（没修）" "null" \
  "$(printf '%s' "$BODY" | jq -r --arg id "$PARENT_ID" '.data.items[] | select(.id == $id) | .status_after')"
expect_eq "⑥ 干跑不动库：父任务仍是 DOING" "DOING" \
  "$(psql_q "SELECT status FROM biz_task WHERE id = '$PARENT_ID'")"

call "GET" "$API/business/task/admin/repair-parent-status?dry_run=false&limit=200"
FIX_HIT="$(printf '%s' "$BODY" | jq -r --arg id "$PARENT_ID" '[.data.items[] | select(.id == $id and .status_after == "REVIEW")] | length')"
expect_eq "⑦ 实修：本卡父任务被修成 REVIEW" "1" "$FIX_HIT" \
  "($(printf '%s' "$BODY" | jq -c '{total,fixed,failed}'))"
expect_eq "⑦ 实修：库里父任务已是 REVIEW" "REVIEW" \
  "$(psql_q "SELECT status FROM biz_task WHERE id = '$PARENT_ID'")"
FIXED_N="$(printf '%s' "$BODY" | jq -r '.data.fixed // 0')"
if [ "${FIXED_N:-0}" -ge 1 ] 2>/dev/null; then
  ok "⑦ 实修：fixed=$FIXED_N（≥1）"
else
  bad "⑦ 实修：fixed 应 ≥1，实际 [$FIXED_N]"
fi

call "GET" "$API/business/task/admin/repair-parent-status?dry_run=true"
AFTER_HIT="$(printf '%s' "$BODY" | jq -r --arg id "$PARENT_ID" '[.data.items[] | select(.id == $id)] | length')"
expect_eq "⑧ 复查：修完后该父任务不再出现在不一致清单" "0" "$AFTER_HIT" \
  "(全库还剩 $(printf '%s' "$BODY" | jq -r '.data.total // -1') 条，属别的夹具)"

# ---------------------------------------------------------------------- ⑩ 收尾
for id in "${CHILD_IDS[0]}" "${CHILD_IDS[1]}" "$PARENT_ID"; do
  call "DELETE" "$API/business/task/$id"
  if [ "$REQ_CODE" != "200" ]; then
    bad "收尾：删任务 $id 失败 HTTP $REQ_CODE $BODY"
  fi
done
LEFT="$(psql_q "SELECT count(*) FROM biz_task WHERE id IN ('$PARENT_ID','${CHILD_IDS[0]}','${CHILD_IDS[1]}') AND deleted_at IS NULL")"
expect_eq "⑩ 收尾：本卡造的 3 条任务全部软删" "0" "$LEFT"
STALE="$(psql_q "SELECT count(*) FROM biz_task WHERE task_no LIKE 'T13-%' AND deleted_at IS NULL")"
echo "      库内未删除的 T13- 任务残留：${STALE:-?}（本次脚本造的已清零；单测夹具不在本库）"

echo "===== 汇总：通过 $OK_N 条 / 失败 $BAD_N 条 / 跳过 $SKIP_N 条 ====="
[ "$BAD_N" -eq 0 ] || exit 1
