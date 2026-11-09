#!/usr/bin/env bash
# =============================================================================
# e2e07_fix2_contract_notice_probe.sh —— 合同状态变更**真的发出通知**的真机验收（145）
#
# 卡：t_a7152992 / E2E07-FIX-2 ③（CONTRACT 零联动规则 → V0360 补齐后的实测）
#
# 验什么（真 token、真 HTTP、真库，逐条 PASS/FAIL）
#   1. 库内 CONTRACT 的预置规则已生效：SIGNED / TERMINATED 两条 enabled + TAKEOVER
#   2. 建单（制单人 = admin）+ 提交审批 → 合同 PENDING_APPROVAL
#   3. 审批通过（e2e01_appr 批 n_pm 那张待办；V0359 之后该节点是或签）
#        → 合同 SIGNED
#        → **sys_notice 落 1 条 `LNK:CONTRACT:SIGNED:<合同 id>`，收件人 = 制单人**
#          （这是「LNK-07 接线了但规则表 0 条」在默认配置下的反证）
#   4. 重复审批同一待办（应 409）→ SIGNED 通知仍是 1 条（联动只触发一次）
#   5. 合同 SIGNED → TERMINATED（人工 PATCH）→ 再落 1 条 `LNK:CONTRACT:TERMINATED:<合同 id>`
#   6. 合同名下 0 张凭证（CONTRACT 的排除清单有牙：合同不产生凭证）
#
# 期望：PASS=N FAIL=0，退出码 0。任何一条 FAIL 退出码 1。
# 运行环境：145（BASE 默认 http://127.0.0.1:8033/api），库 cloudpivot_dev。
# 夹具：合同 / 通知由本脚本自建；合同留作证据不删除（与 FIX-1 同口径）。
# =============================================================================
set -uo pipefail
ENV_FILE="${ENV_FILE:-/etc/cloudpivot/cloudpivot-dev.env}"
set -a; . "$ENV_FILE"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE="${BASE:-http://127.0.0.1:8033/api}"
WORK="${WORK:-/tmp/e2e07fix2}"; mkdir -p "$WORK/raw"
PASS=0; FAIL=0
ok()   { PASS=$((PASS+1)); printf '  ✅ %s\n' "$*"; }
bad()  { FAIL=$((FAIL+1)); printf '  ❌ %s\n' "$*"; }
check(){ if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
say()  { printf '%s\n' "$*"; }
SQ()   { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -X -q -tA -F'|' -c "$1"; }

api() { # api <名> <METHOD> <path> <token> [json]
  local name=$1 method=$2 path=$3 token=$4 body=${5:-}
  local out="$WORK/raw/$name.json"
  local -a args=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
}
data() { printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }

login() { # login <用户名> <口令>
  api "login_$1" POST /system/auth/login "" "{\"username\":\"$1\",\"password\":\"$2\"}"
  [ "$HTTP" = 200 ] && [ "$CODE" = 0 ] && data '.data.access_token'
}

RUN=$(date +%m%d%H%M%S)
say "### 0) 登录"
T_ADMIN=$(login admin "$NIK_ADMIN_PASSWORD"); [ -n "$T_ADMIN" ] && ok "admin 登录" || { bad "admin 登录失败"; exit 1; }
T_PM=$(login e2e01_appr 'E2e01@Appr123'); [ -n "$T_PM" ] && ok "e2e01_appr（PURCHASE_MANAGER 成员）登录" || { bad "e2e01_appr 登录失败"; exit 1; }
ADMIN_ID=$(SQ "select id from sys_user where username='admin'")
PM_ID=$(SQ "select id from sys_user where username='e2e01_appr' and deleted_at is null")

say "### 1) V0360 预置规则在库内生效（CONTRACT/SIGNED + CONTRACT/TERMINATED）"
for st in SIGNED TERMINATED; do
  ROW=$(SQ "select enabled || '|' || (action_params ->> 'mode') from biz_flow_linkage_rule where trigger_entity='CONTRACT' and trigger_status='$st' and action_code='SEND_NOTICE' and deleted_at is null")
  check "CONTRACT/$st 规则（enabled|mode）" "$ROW" "true|TAKEOVER"
done

say "### 2) 建单 + 提交审批（5 万 → 命中 n_pm 单级）"
SUP_ID=$(SQ "select id from biz_supplier where deleted_at is null and status=1 order by supplier_code limit 1")
[ -z "$SUP_ID" ] && { bad "库里没有可用供应商"; exit 1; }
api create POST /business/erp/contract "$T_ADMIN" \
  "{\"name\":\"E2E07FIX2-NOTICE-$RUN\",\"contract_type\":\"PURCHASE\",\"party_b_id\":\"$SUP_ID\",\"amount\":50000,\"sign_date\":\"2026-11-09\",\"start_date\":\"2026-11-09\",\"end_date\":\"2027-11-08\",\"owner_id\":\"$ADMIN_ID\"}"
C1=$(data '.data.id')
check "建单 HTTP/CODE" "$HTTP/c$CODE" "200/c0"
[ -z "$C1" ] || [ "$C1" = "null" ] && { bad "建单没拿到 id"; exit 1; }
check "制单人（biz_contract.create_by）" "$(SQ "select create_by from biz_contract where id='$C1'")" "$ADMIN_ID"
check "建单阶段 CONTRACT 联动通知条数（提交前应为 0）" \
  "$(SQ "select count(*) from sys_notice where idempotency_key like 'LNK:CONTRACT:%:$C1' and deleted_at is null")" "0"
api submit PATCH "/business/erp/contract/$C1/status" "$T_ADMIN" '{"target_status":"PENDING_APPROVAL"}'
check "提交审批 HTTP/CODE" "$HTTP/c$CODE" "200/c0"
check "合同状态" "$(SQ "select status from biz_contract where id='$C1'")" "PENDING_APPROVAL"

say "### 3) 审批通过（n_pm 或签：只批一票）→ 合同 SIGNED 并发通知"
I1=$(SQ "select id from biz_flow_instance where flow_code='contract_approve' and biz_id='$C1' order by created_at desc limit 1")
[ -z "$I1" ] && { bad "没找到流程实例"; exit 1; }
TID=$(SQ "select id from biz_flow_task where instance_id='$I1' and node_key='n_pm' and status='PENDING' and assignee_id='$PM_ID' limit 1")
[ -z "$TID" ] && { bad "e2e01_appr 名下没有待办"; exit 1; }
api approve POST "/business/flow/task/$TID/approve" "$T_PM" '{"comment":"E2E07-FIX-2 联动通知验证：合同签订应发通知"}'
check "审批 HTTP/CODE" "$HTTP/c$CODE" "200/c0"
check "流程实例状态" "$(SQ "select status from biz_flow_instance where id='$I1'")" "APPROVED"
check "合同状态" "$(SQ "select status from biz_contract where id='$C1'")" "SIGNED"
KEY_SIGNED="LNK:CONTRACT:SIGNED:$C1"
check "SIGNED 通知条数（幂等键 $KEY_SIGNED）" \
  "$(SQ "select count(*) from sys_notice where idempotency_key='$KEY_SIGNED' and deleted_at is null")" "1"
say "     · 通知正文：$(SQ "select title || ' / ' || content from sys_notice where idempotency_key='$KEY_SIGNED' and deleted_at is null")"
check "SIGNED 通知收件人 = 制单人（sys_notice_receiver）" \
  "$(SQ "select count(*) from sys_notice_receiver r join sys_notice n on n.id=r.notice_id where n.idempotency_key='$KEY_SIGNED' and r.user_id='$ADMIN_ID'")" "1"
check "合同名下凭证张数（合同不产生凭证）" \
  "$(SQ "select count(*) from biz_voucher where source_id='$C1' and deleted_at is null")" "0"

say "### 4) 重复审批同一待办 → 409，通知不增（联动只触发一次）"
api approve_again POST "/business/flow/task/$TID/approve" "$T_PM" '{"comment":"重复审批"}'
case "$HTTP" in 400|409) ok "重复审批被挡（HTTP $HTTP / code $CODE）";; *) bad "重复审批应被挡，实际 HTTP $HTTP code $CODE";; esac
check "SIGNED 通知条数（重复触发被抑制）" \
  "$(SQ "select count(*) from sys_notice where idempotency_key='$KEY_SIGNED' and deleted_at is null")" "1"

say "### 5) 合同终止（SIGNED → TERMINATED）→ 再发一条终止通知"
api terminate PATCH "/business/erp/contract/$C1/status" "$T_ADMIN" '{"target_status":"TERMINATED"}'
check "终止 HTTP/CODE" "$HTTP/c$CODE" "200/c0"
check "合同状态" "$(SQ "select status from biz_contract where id='$C1'")" "TERMINATED"
KEY_TERM="LNK:CONTRACT:TERMINATED:$C1"
check "TERMINATED 通知条数" \
  "$(SQ "select count(*) from sys_notice where idempotency_key='$KEY_TERM' and deleted_at is null")" "1"
say "     · 通知正文：$(SQ "select title || ' / ' || content from sys_notice where idempotency_key='$KEY_TERM' and deleted_at is null")"
say "     · 合同 $C1（E2E07FIX2-NOTICE-$RUN）与其通知留作演练证据，不删除"

say ""
say "== PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
