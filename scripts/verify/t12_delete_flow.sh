#!/usr/bin/env bash
# ============================================================================
# T12 验收：任务删除 → 在跑审批单自动终止（同事务 / ended_at 约束 / 精确匹配）
#
# 真实 curl 打**隔离实例**（默认 127.0.0.1:18092 / 库 cloudpivot_t12），
# 全程不碰生产 8023、不碰 dev 8024。
#
# 用法：
#   bash scripts/verify/t12_delete_flow.sh
#   T12_BASE=http://127.0.0.1:18092 T12_DB=cloudpivot_t12 bash scripts/verify/t12_delete_flow.sh
#
# 环境变量：
#   T12_BASE         后端根地址（脚本自己拼 /api），默认 http://127.0.0.1:18092
#   T12_USER         登录账号，默认 admin
#   T12_PASSWORD     登录口令（可空：按 T12_PASSWORD → T12_NEW_PASSWORD → 口令缓存 → env 文件顺序试）
#   T12_NEW_PASSWORD 首登强制改密时改成的口令，默认 T12-Accept-2026!
#   T12_ENV_FILE     读库连接 / 初始口令的 env 文件，默认 /etc/cloudpivot/cloudpivot-dev.env
#   T12_DB           断言用的库名，默认 cloudpivot_t12（必须与实例指向同一个库）
#
# 断言（①–⑳）：
#   ① 无令牌 401
#   ② 建任务 A（need_review=true）→ DOING → REVIEW 生成审批单（PENDING / ended_at 空）
#   ③ A 的审批待办 PENDING ≥1（说明「审批人的待办」里确实有这条）
#   ④ 建任务 B（对照组）→ REVIEW 生成审批单，保持 PENDING
#   ⑤ 建任务 C（已完成对照）→ REVIEW 生成审批单后置为 APPROVED + 原审批意见
#   ⑥ DELETE A → 200 / deleted=true
#   ⑦ A 的实例 → CANCELLED + ended_at 非空 + result_comment = 「任务已删除，审批单自动终止」
#   ⑧ A 的实例 revision +1（与 bump_instance 同口径）
#   ⑨ A 的审批待办全部非 PENDING（CANCELLED）且 completed_at 非空
#   ⑩ A 的活动节点全部 CANCELLED
#   ⑪ B 的实例仍 PENDING（biz_no / biz_id 精确匹配，不误伤）
#   ⑫ C 的实例仍 APPROVED，result_comment 不被覆盖（已完成的事实保留审计）
#   ⑬ 全库「PENDING 实例指向已软删任务」= 0（孤儿清零）
#   ⑭ 重复 DELETE A → 404 + 63001（既有语义：已删任务 require_task 就 404），
#      实例仍是 CANCELLED、revision 不再涨（不会重复 bump）
#   ⑮ 收尾：DELETE B / C → 它们的在跑审批单也被终止（同一修复的第二次验证）
#   ⑯ 收尾后库里没有本次造出的 PENDING 实例
# ============================================================================
set -uo pipefail

BASE="${T12_BASE:-http://127.0.0.1:18092}"
API="$BASE/api"
USERNAME="${T12_USER:-admin}"
NEW_PASSWORD="${T12_NEW_PASSWORD:-T12-Accept-2026!}"
PWD_FILE="${T12_PWD_FILE:-/tmp/t12_admin_pwd}"
ENV_FILE="${T12_ENV_FILE:-/etc/cloudpivot/cloudpivot-dev.env}"
DB_NAME="${T12_DB:-cloudpivot_t12}"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
STAMP="$(date +%Y%m%d-%H%M%S)"
TITLE_PREFIX="T12-DeleteFlow-$STAMP"
COMMENT="任务已删除，审批单自动终止"

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
set -a
# shellcheck disable=SC1090
. "$ENV_FILE" 2>/dev/null
set +a
psql_q() { # $1=SQL → stdout（-At，只取第一行）
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
[ -n "${T12_PASSWORD:-}" ] && CANDIDATES+=("${T12_PASSWORD}")
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

# 首登强制改密：改密 → 存口令 → 用新口令重登（旧令牌会因 token_version 递增失效）
if [ "$(printf '%s' "$LOGIN_JSON" | jq -r '.data.must_change_password // false')" = "true" ]; then
  chg=$(curl -s -m 10 -X PUT "$API/system/auth/password" \
    -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    -d "$(jq -nc --arg o "$USED_PWD" --arg n "$NEW_PASSWORD" '{old_password:$o,new_password:$n}')")
  if [ "$(printf '%s' "$chg" | jq -r '.code')" = "0" ]; then
    printf '%s\n' "$NEW_PASSWORD" >"$PWD_FILE"
    USED_PWD="$NEW_PASSWORD"
    LOGIN_JSON="$(login_with "$NEW_PASSWORD")"
    TOKEN="$(printf '%s' "$LOGIN_JSON" | jq -r '.data.access_token // empty')"
    ok "首登强制改密完成并重登"
  else
    bad "首登强制改密失败：$chg"
  fi
fi

# ① 鉴权在位：无令牌 401
SAVED="$TOKEN"
TOKEN=""
call "GET" "$API/business/task/page?page=1&size=1"
expect_eq "① 无令牌访问 /business/task/page 必须 401" "401" "$REQ_CODE"
TOKEN="$SAVED"

# ------------------------------------------------------------ 造任务并推 REVIEW 生成审批单
# need_review=true：REVIEW→DONE 要走审批（D3b 提交验收即发起 TASK_ACCEPT 审批单）
create_task() { # $1=标题后缀 → 打印 id
  call "POST" "$API/business/task" \
    "$(jq -nc --arg t "$TITLE_PREFIX-$1" '{title:$t,description:"T12 验收：删除任务即终止审批单",need_review:true}')"
  printf '%s' "$BODY" | jq -r '.data.id // empty'
}

review_task() { # $1=任务 id $2=标签
  call "PATCH" "$API/business/task/$1/status" '{"to":"DOING"}'
  expect_eq "$2 TODO→DOING 200" "200" "$REQ_CODE" "$BODY"
  call "PATCH" "$API/business/task/$1/status" '{"to":"REVIEW"}'
  expect_eq "$2 DOING→REVIEW 200（提交验收 → 发起审批单）" "200" "$REQ_CODE" "$BODY"
}

instance_field() { # $1=task_no $2=列 → 值
  psql_q "SELECT $2 FROM biz_flow_instance WHERE biz_no = '$1' AND deleted_at IS NULL ORDER BY started_at DESC LIMIT 1"
}

# ② 任务 A：生成审批单
TASK_A="$(create_task A)"
expect_eq "② 建任务 A 200（need_review=true）" "1" "$([ -n "$TASK_A" ] && echo 1 || echo 0)" "$BODY"
NO_A="$(psql_q "SELECT task_no FROM biz_task WHERE id = '$TASK_A'::uuid")"
echo "      任务 A id=$TASK_A no=$NO_A"
review_task "$TASK_A" "② 任务 A"
INST_A="$(instance_field "$NO_A" "id")"
if [ -z "$INST_A" ]; then
  bad "② 任务 A 提交验收后没有生成审批单（TASK_ACCEPT 默认流程缺失？）"
else
  ok "② 任务 A 生成了审批单 ${INST_A:0:8}…（instance_no=$(instance_field "$NO_A" instance_no)）"
fi
expect_eq "② A 的审批单状态" "PENDING" "$(instance_field "$NO_A" status)"
expect_eq "② A 的审批单 ended_at 为空（PENDING 约束）" "" \
  "$(psql_q "SELECT coalesce(ended_at::text,'') FROM biz_flow_instance WHERE id = '$INST_A'::uuid")"

# ③ 审批待办
FT_A_PENDING="$(psql_q "SELECT count(*) FROM biz_flow_task WHERE instance_id = '$INST_A'::uuid AND status = 'PENDING'")"
if [ "${FT_A_PENDING:-0}" -ge 1 ]; then
  ok "③ A 的审批待办 PENDING 共 $FT_A_PENDING 条（审批人待办里能看到）"
else
  bad "③ A 的审批待办 PENDING 为 0（没有待办就算不上「污染待办列表」）"
fi

# ④ 任务 B（对照组：删 A 不许动 B）
TASK_B="$(create_task B)"
NO_B="$(psql_q "SELECT task_no FROM biz_task WHERE id = '$TASK_B'::uuid")"
review_task "$TASK_B" "④ 任务 B"
INST_B="$(instance_field "$NO_B" "id")"
expect_eq "④ B 的审批单状态" "PENDING" "$(instance_field "$NO_B" status)"

# ⑤ 任务 C（已完成对照：APPROVED 实例不许被动）
TASK_C="$(create_task C)"
NO_C="$(psql_q "SELECT task_no FROM biz_task WHERE id = '$TASK_C'::uuid")"
review_task "$TASK_C" "⑤ 任务 C"
INST_C="$(instance_field "$NO_C" "id")"
psql_exec "UPDATE biz_flow_task SET status='APPROVED', completed_at=now(), comment='T12 人工通过' WHERE instance_id = '$INST_C'::uuid AND status='PENDING'"
psql_exec "UPDATE biz_flow_instance_node SET status='APPROVED', ended_at=now() WHERE instance_id = '$INST_C'::uuid AND status IN ('ACTIVE','PENDING')"
psql_exec "UPDATE biz_flow_instance SET status='APPROVED', ended_at=now(), result_comment='T12 人工通过，保留审计' WHERE id = '$INST_C'::uuid"
expect_eq "⑤ C 的审批单已置为 APPROVED（前置条件）" "APPROVED" "$(instance_field "$NO_C" status)"

# ⑥ 删任务 A
REV_A_BEFORE="$(psql_q "SELECT revision FROM biz_flow_instance WHERE id = '$INST_A'::uuid")"
call "DELETE" "$API/business/task/$TASK_A"
expect_eq "⑥ DELETE 任务 A → 200" "200" "$REQ_CODE" "$BODY"
expect_eq "⑥ 响应 deleted=true" "true" "$(printf '%s' "$BODY" | jq -r '.data.deleted')"

# ⑦ 实例被终止 + ended_at + 文案
expect_eq "⑦ A 的审批单状态 → CANCELLED" "CANCELLED" "$(instance_field "$NO_A" status)"
expect_eq "⑦ A 的审批单 ended_at 非空" "1" \
  "$(psql_q "SELECT (ended_at IS NOT NULL)::int::text FROM biz_flow_instance WHERE id = '$INST_A'::uuid")"
expect_eq "⑦ A 的审批单 result_comment = 删除文案" "$COMMENT" "$(instance_field "$NO_A" result_comment)"

# ⑧ revision +1
REV_A_AFTER="$(psql_q "SELECT revision FROM biz_flow_instance WHERE id = '$INST_A'::uuid")"
expect_eq "⑧ 实例 revision +1（$REV_A_BEFORE → $REV_A_AFTER）" \
  "$((REV_A_BEFORE + 1))" "$REV_A_AFTER"

# ⑨ 待办全部终止 + completed_at 非空
expect_eq "⑨ A 的实例下已无 PENDING 待办" "0" \
  "$(psql_q "SELECT count(*) FROM biz_flow_task WHERE instance_id = '$INST_A'::uuid AND status = 'PENDING'")"
expect_eq "⑨ A 的待办全部 CANCELLED" "0" \
  "$(psql_q "SELECT count(*) FROM biz_flow_task WHERE instance_id = '$INST_A'::uuid AND status <> 'CANCELLED'")"
expect_eq "⑨ A 的待办 completed_at 全部非空（ck_biz_flow_task_done）" "0" \
  "$(psql_q "SELECT count(*) FROM biz_flow_task WHERE instance_id = '$INST_A'::uuid AND completed_at IS NULL")"

# ⑩ 节点全部作废
expect_eq "⑩ A 的实例下已无 ACTIVE/PENDING 节点" "0" \
  "$(psql_q "SELECT count(*) FROM biz_flow_instance_node WHERE instance_id = '$INST_A'::uuid AND status IN ('ACTIVE','PENDING')")"

# ⑪ 精确匹配：B 不受影响
expect_eq "⑪ B 的审批单仍 PENDING（不误伤别的任务）" "PENDING" "$(instance_field "$NO_B" status)"
expect_eq "⑪ B 的审批待办仍 PENDING" "1" \
  "$([ "$(psql_q "SELECT count(*) FROM biz_flow_task WHERE instance_id = '$INST_B'::uuid AND status = 'PENDING'")" -ge 1 ] && echo 1 || echo 0)"

# ⑫ C 已完成的事实保留
expect_eq "⑫ C 的审批单仍 APPROVED" "APPROVED" "$(instance_field "$NO_C" status)"
expect_eq "⑫ C 的 result_comment 未被覆盖" "T12 人工通过，保留审计" "$(instance_field "$NO_C" result_comment)"

# ⑬ 全库孤儿清零
ORPHANS="$(psql_q "SELECT count(*) FROM biz_flow_instance i JOIN biz_task t ON t.id = i.biz_id WHERE i.status = 'PENDING' AND i.deleted_at IS NULL AND t.deleted_at IS NOT NULL")"
expect_eq "⑬ 全库「PENDING 实例指向已软删任务」= 0（孤儿清零）" "0" "$ORPHANS"

# ⑭ 重复删除：既有语义是 404 + 63001（`require_task` 过滤 deleted_at），
# 重点是「没有重复动审批单」—— 状态仍是 CANCELLED、revision 不再涨。
call "DELETE" "$API/business/task/$TASK_A"
expect_eq "⑭ 重复 DELETE 任务 A → 404（既有语义：已删任务不可见）" "404" "$REQ_CODE" "$BODY"
expect_eq "⑭ 重复删除的业务码 63001" "63001" "$(printf '%s' "$BODY" | jq -r '.code')"
expect_eq "⑭ 实例仍是 CANCELLED" "CANCELLED" "$(instance_field "$NO_A" status)"
expect_eq "⑭ 重复删除不再 bump revision" "$REV_A_AFTER" \
  "$(psql_q "SELECT revision FROM biz_flow_instance WHERE id = '$INST_A'::uuid")"

# ⑮ 收尾：DELETE B / C → 在跑审批单同样被终止
call "DELETE" "$API/business/task/$TASK_B"
expect_eq "⑮ DELETE 任务 B → 200" "200" "$REQ_CODE" "$BODY"
expect_eq "⑮ B 的审批单 → CANCELLED" "CANCELLED" "$(instance_field "$NO_B" status)"
call "DELETE" "$API/business/task/$TASK_C"
expect_eq "⑮ DELETE 任务 C → 200" "200" "$REQ_CODE" "$BODY"
expect_eq "⑮ C 的 APPROVED 审批单不动" "APPROVED" "$(instance_field "$NO_C" status)"
expect_eq "⑮ C 的原审批意见仍保留" "T12 人工通过，保留审计" "$(instance_field "$NO_C" result_comment)"

# ⑯ 库里不留本次的 PENDING 实例
expect_eq "⑯ 本次验收造出的实例已无 PENDING" "0" \
  "$(psql_q "SELECT count(*) FROM biz_flow_instance i JOIN biz_task t ON t.id = i.biz_id WHERE i.status = 'PENDING' AND t.title LIKE '$TITLE_PREFIX%'")"
# 物理清掉本次造出的实例与任务行（隔离库整洁；任务已软删）
psql_exec "DELETE FROM biz_flow_instance WHERE biz_id IN (SELECT id FROM biz_task WHERE title LIKE '$TITLE_PREFIX%')"
psql_exec "DELETE FROM biz_task WHERE title LIKE '$TITLE_PREFIX%'"
LEFT="$(psql_q "SELECT count(*) FROM biz_task WHERE title LIKE '$TITLE_PREFIX%'")"
expect_eq "⑯ 收尾：库里没有残留的本次任务行" "0" "$LEFT"

# ⑰ 历史清理脚本 `scripts/maintenance/clean_orphan_flow.sh`：
#    造一个「任务已软删、实例仍 PENDING」的孤儿（绕过 API 直接写 deleted_at，
#    模拟 T12 上线前的存量），并造一个「任务已删但审批已通过」的对照 ——
#    前者必须被清、后者必须原样保留。
TASK_D="$(create_task D)"
NO_D="$(psql_q "SELECT task_no FROM biz_task WHERE id = '$TASK_D'::uuid")"
review_task "$TASK_D" "⑰ 任务 D"
INST_D="$(instance_field "$NO_D" id)"
INO_D="$(instance_field "$NO_D" instance_no)"
psql_exec "UPDATE biz_task SET deleted_at = now() WHERE id = '$TASK_D'::uuid"
expect_eq "⑰ 造出孤儿：任务 D 已软删、实例仍 PENDING" "PENDING|1" \
  "$(instance_field "$NO_D" status)|$(psql_q "SELECT (deleted_at IS NOT NULL)::int::text FROM biz_task WHERE id = '$TASK_D'::uuid")"

TASK_E="$(create_task E)"
NO_E="$(psql_q "SELECT task_no FROM biz_task WHERE id = '$TASK_E'::uuid")"
review_task "$TASK_E" "⑰ 任务 E"
INST_E="$(instance_field "$NO_E" id)"
INO_E="$(instance_field "$NO_E" instance_no)"
psql_exec "UPDATE biz_flow_task SET status='APPROVED', completed_at=now() WHERE instance_id = '$INST_E'::uuid AND status='PENDING'"
psql_exec "UPDATE biz_flow_instance_node SET status='APPROVED', ended_at=now() WHERE instance_id = '$INST_E'::uuid AND status IN ('ACTIVE','PENDING')"
psql_exec "UPDATE biz_flow_instance SET status='APPROVED', ended_at=now(), result_comment='T12 已完成，保留审计' WHERE id = '$INST_E'::uuid"
psql_exec "UPDATE biz_task SET deleted_at = now() WHERE id = '$TASK_E'::uuid"

DRY_OUT="$(T12_DB="$DB_NAME" bash "$REPO/scripts/maintenance/clean_orphan_flow.sh" 2>&1)"
expect_eq "⑰ dry-run 列出孤儿 D（$INO_D）" "1" "$(printf '%s' "$DRY_OUT" | grep -c "$INO_D")"
expect_eq "⑰ dry-run 不列已完成的 E（$INO_E）" "0" "$(printf '%s' "$DRY_OUT" | grep -c "$INO_E")"
expect_eq "⑰ dry-run 不动库（D 仍 PENDING）" "PENDING" "$(instance_field "$NO_D" status)"

APPLY_OUT="$(T12_DB="$DB_NAME" bash "$REPO/scripts/maintenance/clean_orphan_flow.sh" --apply 2>&1)"
expect_eq "⑱ --apply 清理后：D 的实例 → CANCELLED" "CANCELLED" "$(instance_field "$NO_D" status)"
expect_eq "⑱ D 的实例 ended_at 非空" "1" \
  "$(psql_q "SELECT (ended_at IS NOT NULL)::int::text FROM biz_flow_instance WHERE id = '$INST_D'::uuid")"
expect_eq "⑱ D 的实例 result_comment = 删除文案" "$COMMENT" "$(instance_field "$NO_D" result_comment)"
expect_eq "⑱ 已完成的 E 的实例一条都没动（APPROVED + 原意见）" "APPROVED|T12 已完成，保留审计" \
  "$(instance_field "$NO_E" status)|$(instance_field "$NO_E" result_comment)"
expect_eq "⑱ 清理脚本自检「剩余 PENDING 孤儿 0」" "1" \
  "$(printf '%s' "$APPLY_OUT" | grep -c '剩余 PENDING 孤儿：0')"

# 收尾：D / E 的任务行与实例行（任务已软删、实例已 CANCELLED/APPROVED）物理清掉，隔离库保持整洁
psql_exec "DELETE FROM biz_flow_instance WHERE biz_id IN (SELECT id FROM biz_task WHERE title LIKE '$TITLE_PREFIX%')"
psql_exec "DELETE FROM biz_task WHERE title LIKE '$TITLE_PREFIX%'"
expect_eq "⑱ 收尾：库里没有残留的本次任务行" "0" \
  "$(psql_q "SELECT count(*) FROM biz_task WHERE title LIKE '$TITLE_PREFIX%'")"

echo
echo "===== 汇总：通过 $OK_N 条 / 失败 $BAD_N 条 / 跳过 $SKIP_N 条 ====="
[ "$BAD_N" -eq 0 ] || exit 1

