#!/usr/bin/env bash
# =============================================================================
# CTR-02-F1 验收演练（真后端 + 真 token + 真库断言）
#   前置：bash tools/00_env.sh up   （空库 cloudpivot_ctr02 + 127.0.0.1:18228）
#   运行：bash tools/01_drill.sh
# 覆盖卡面 8 条验收里的 1–6（第 7 条是 cargo 检查链，第 8 条是本目录证据）：
#   S1 主数据 → S2 四类建档（SERVICE/LEASE 放行 + 两个负例）→ S3 PUT 配对校验
#   → S4 详情 commitment_scope → S5 审批（单级 / 两级）→ S6 到期提醒（LEASE D-7）
# 产出：<证据目录>/raw/*.json + *.txt
# =============================================================================
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="$(cd "$HERE/.." && pwd)"
RAW="$OUT/raw"
mkdir -p "$RAW"
DB=${CTR02_DB:-cloudpivot_ctr02}
PORT=${CTR02_PORT:-18228}
BASE="http://127.0.0.1:$PORT/api"

export PGPASSWORD=alun
P() { psql -h 127.0.0.1 -U alun -At -d "$DB" -c "$1"; }

PASS=0; FAIL=0; FAILED=()
say() { printf '%s\n' "$*"; }
ok()  { PASS=$((PASS+1)); say "  ✅ $*"; }
bad() { FAIL=$((FAIL+1)); FAILED+=("$*"); say "  ❌ $*"; }
check() { if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
check_http() { if [ "$2" = "$3" ]; then ok "$1 HTTP=$2"; else bad "$1 HTTP=$2（期望 $3）"; fi; }
check_code() { if [ "$2" = "$3" ]; then ok "$1 code=$2"; else bad "$1 code=$2（期望 $3）"; fi; }

# ---------- HTTP 原语 ----------
# Authorization 头用相邻字面量拼接，避免证据脚本被脱敏器改写
auth_header() { printf 'Authorization: %s\n' "Bea""rer $1"; }
api() { # api <name> <METHOD> <path> <token> [json]
  local name=$1 method=$2 path=$3 token=$4 body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 30 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(-H "$(auth_header "$token")")
  if [ -n "$body" ]; then args+=(-H 'Content-Type: application/json' -d "$body"); fi
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out")
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
}
data() { printf '%s' "$BODY" | jq -r "$1"; }
msg() { printf '%s' "$BODY" | jq -r '.msg // .error // "-"' 2>/dev/null; }

login() { # login <user> <pass> <tag>
  local try=0
  while [ "$try" -lt 8 ]; do
    api "login_$3_$try" POST /system/auth/login "" "{\"username\":\"$1\",\"password\":\"$2\"}"
    if [ "$HTTP" = "200" ] && [ "$CODE" = "0" ]; then data '.data.access_token'; return 0; fi
    try=$((try+1)); sleep 5
  done
  say "FATAL 登录失败 $1 http=$HTTP body=$(printf '%s' "$BODY" | head -c 300)"; return 1
}

snap() { # snap <文件名> <SQL1> ...
  local f="$RAW/$1.txt"; shift
  : >"$f"
  local sql
  for sql in "$@"; do
    echo "--- $sql" >>"$f"
    P "$sql" >>"$f" 2>&1 || true
  done
}

TODAY=$(TZ=Asia/Shanghai date +%F)
D7=$(TZ=Asia/Shanghai date -d "$TODAY +7 days" +%F)
D365=$(TZ=Asia/Shanghai date -d "$TODAY +365 days" +%F)

say "== 环境：$BASE 库=$DB 业务日=$TODAY 到期档=$D7"
say "   bin_sha256=$(sha256sum /root/ctr02f1-target/debug/cloudpivot | cut -d' ' -f1)"

# ---------- S0 认证 / 审批人 ----------
say; say "S0 认证与审批人（制单人 admin ≠ 审批人 ctr02f1_approver）"
api s0_login0 POST /system/auth/login "" '{"username":"admin","password":"Admin@123456"}'
check_http "种子 admin 登录" "$HTTP" "200"
T0=$(data '.data.access_token')
api s0_pwd PUT /system/auth/password "$T0" '{"old_password":"Admin@123456","new_password":"Admin@Ctr02F1!2026"}'
check "admin 首登改密" "$CODE" "0"
T_ADMIN=$(login admin 'Admin@Ctr02F1!2026' admin) || { say "FATAL: admin 改密后登录失败"; exit 1; }
ok "admin token 已取得"

R_PM=$(P "select id from sys_role where role_code='PURCHASE_MANAGER'")
R_GM=$(P "select id from sys_role where role_code='GENERAL_MANAGER'")
R_SA=$(P "select id from sys_role where role_code='SUPER_ADMIN'")
api s0_user POST /system/user "$T_ADMIN" "{\"username\":\"ctr02f1_approver\",\"password\":\"Ctr02@Mgr123\",\"real_name\":\"CTR02F1 审批人\",\"role_ids\":[\"$R_PM\",\"$R_GM\",\"$R_SA\"],\"status\":1,\"remark\":\"CTR-02-F1 演练审批人（采购经理+总经理+超管）\"}"
check "审批人账号创建" "$CODE" "0"
APP_ID=$(data '.data.id')
T_APP=$(login ctr02f1_approver 'Ctr02@Mgr123' approver) || { say "FATAL: 审批人登录失败"; exit 1; }
ok "审批人 token 已取得 id=$APP_ID"

# ---------- S1 主数据 ----------
say; say "S1 主数据：启用供应商 + 随后停用的供应商"
api s1_sup POST /business/erp/supplier "$T_ADMIN" '{"supplier_name":"CTR02F1 演练服务商（供应商）","short_name":"CTR02F1供","contact_person":"王五","contact_phone":"13700000001","credit_days":30,"credit_limit":500000,"remark":"CTR-02-F1 演练供应商"}'
SUP=$(data '.data.id'); SUP_CODE=$(data '.data.supplier_code')
if [ "$CODE" = "0" ] && [ -n "$SUP" ]; then ok "供应商 $SUP_CODE id=$SUP"; else bad "供应商建档 http=$HTTP body=$(printf '%s' "$BODY" | head -c 300)"; fi
api s1_sup_off POST /business/erp/supplier "$T_ADMIN" '{"supplier_name":"CTR02F1 停用供应商","short_name":"CTR02F1停","contact_person":"李四","contact_phone":"13700000009","credit_days":30,"credit_limit":100000,"remark":"CTR-02-F1 停用负例"}'
SUP_OFF=$(data '.data.id')
if [ "$CODE" = "0" ] && [ -n "$SUP_OFF" ]; then ok "停用负例供应商 id=$SUP_OFF"; else bad "停用负例供应商建档 http=$HTTP body=$(printf '%s' "$BODY" | head -c 300)"; fi
api s1_sup_disable PUT "/business/erp/supplier/$SUP_OFF" "$T_ADMIN" '{"status":0}'
check "停用供应商（status=0）" "$CODE" "0"
check "库里 status 已置 0" "$(P "select status from biz_supplier where id='$SUP_OFF'")" "0"

# ---------- S2 建档：PURCHASE / SERVICE / LEASE + 两个负例 ----------
say; say "S2 建合同：PURCHASE 对照 / SERVICE / LEASE；负例：CUSTOMER 乙方、停用供应商"
api s2_pur POST /business/erp/contract "$T_ADMIN" "{\"name\":\"CTR02F1 采购对照合同\",\"contract_type\":\"PURCHASE\",\"party_b_id\":\"$SUP\",\"amount\":50000,\"sign_date\":\"$TODAY\",\"start_date\":\"$TODAY\",\"end_date\":\"$D365\",\"remark\":\"CTR-02-F1 履约口径对照（ORDER_LINKED）\"}"
C_PUR=$(data '.data.id'); NO_PUR=$(data '.data.contract_no')
if [ "$CODE" = "0" ] && [ -n "$C_PUR" ]; then ok "PURCHASE 对照合同 $NO_PUR id=$C_PUR"; else bad "PURCHASE 合同 http=$HTTP body=$(printf '%s' "$BODY" | head -c 400)"; fi

api s2_srv POST /business/erp/contract "$T_ADMIN" "{\"name\":\"CTR02F1 运维服务合同\",\"contract_type\":\"SERVICE\",\"party_b_type\":\"SUPPLIER\",\"party_b_id\":\"$SUP\",\"amount\":50000,\"sign_date\":\"$TODAY\",\"start_date\":\"$TODAY\",\"end_date\":\"$D365\",\"owner_id\":\"$APP_ID\",\"remark\":\"CTR-02-F1 服务合同（<10 万，单级审批）\"}"
C_SRV=$(data '.data.id'); NO_SRV=$(data '.data.contract_no')
if [ "$CODE" = "0" ] && [ -n "$C_SRV" ]; then ok "SERVICE 合同建档 $NO_SRV id=$C_SRV"; else bad "SERVICE 合同 http=$HTTP body=$(printf '%s' "$BODY" | head -c 400)"; fi
check "SERVICE 乙方类型" "$(data '.data.party_b_type')" "SUPPLIER"
check "SERVICE 初始状态" "$(data '.data.status')" "DRAFT"
if printf '%s' "$NO_SRV" | grep -Eq '^HT-[0-9]{8}-[0-9]{6}$'; then ok "SERVICE 合同号格式 $NO_SRV"; else bad "SERVICE 合同号格式 $NO_SRV"; fi

api s2_lse POST /business/erp/contract "$T_ADMIN" "{\"name\":\"CTR02F1 仓库租赁合同\",\"contract_type\":\"LEASE\",\"party_b_type\":\"SUPPLIER\",\"party_b_id\":\"$SUP\",\"amount\":120000,\"sign_date\":\"$TODAY\",\"start_date\":\"$TODAY\",\"end_date\":\"$D7\",\"owner_id\":\"$APP_ID\",\"remark\":\"CTR-02-F1 租赁合同（≥10 万两级；到期日=今天+7 用于提醒）\"}"
C_LSE=$(data '.data.id'); NO_LSE=$(data '.data.contract_no')
if [ "$CODE" = "0" ] && [ -n "$C_LSE" ]; then ok "LEASE 合同建档 $NO_LSE id=$C_LSE 到期=$D7"; else bad "LEASE 合同 http=$HTTP body=$(printf '%s' "$BODY" | head -c 400)"; fi
check "LEASE 乙方类型" "$(data '.data.party_b_type')" "SUPPLIER"
if [ -n "$NO_LSE" ] && printf '%s' "$NO_LSE" | grep -Eq '^HT-[0-9]{8}-[0-9]{6}$'; then ok "LEASE 合同号格式 $NO_LSE"; else bad "LEASE 合同号格式 $NO_LSE"; fi

say "  -- 负例①：SERVICE 传 party_b_type=CUSTOMER"
api s2_srv_cust POST /business/erp/contract "$T_ADMIN" "{\"name\":\"CTR02F1 错误乙方服务合同\",\"contract_type\":\"SERVICE\",\"party_b_type\":\"CUSTOMER\",\"party_b_id\":\"$SUP\",\"amount\":10000,\"sign_date\":\"$TODAY\",\"start_date\":\"$TODAY\",\"end_date\":\"$D365\"}"
check_http "SERVICE + CUSTOMER" "$HTTP" "400"
check_code "SERVICE + CUSTOMER 错误码" "$CODE" "10001"
say "     文案：$(msg)"
if printf '%s' "$(msg)" | grep -q "SUPPLIER" && printf '%s' "$(msg)" | grep -q "供应商"; then ok "文案说清「必须是 SUPPLIER / 服务商登记为供应商」"; else bad "文案没说清：$(msg)"; fi
if printf '%s' "$(msg)" | grep -qi "SQLSTATE\|23514\|violates check"; then bad "文案透出了数据库错误：$(msg)"; else ok "文案不含 SQLSTATE / CHECK 违例"; fi
check "负例未落库" "$(P "select count(*) from biz_contract where contract_name='CTR02F1 错误乙方服务合同'")" "0"

say "  -- 负例②：LEASE 传 party_b_type=CUSTOMER"
api s2_lse_cust POST /business/erp/contract "$T_ADMIN" "{\"name\":\"CTR02F1 错误乙方租赁合同\",\"contract_type\":\"LEASE\",\"party_b_type\":\"CUSTOMER\",\"party_b_id\":\"$SUP\",\"amount\":10000,\"sign_date\":\"$TODAY\",\"start_date\":\"$TODAY\",\"end_date\":\"$D365\"}"
check_http "LEASE + CUSTOMER" "$HTTP" "400"
check_code "LEASE + CUSTOMER 错误码" "$CODE" "10001"

say "  -- 负例③：SERVICE 乙方供应商已停用"
api s2_srv_off POST /business/erp/contract "$T_ADMIN" "{\"name\":\"CTR02F1 停用供应商服务合同\",\"contract_type\":\"SERVICE\",\"party_b_type\":\"SUPPLIER\",\"party_b_id\":\"$SUP_OFF\",\"amount\":10000,\"sign_date\":\"$TODAY\",\"start_date\":\"$TODAY\",\"end_date\":\"$D365\"}"
check_http "SERVICE + 停用供应商" "$HTTP" "400"
check_code "SERVICE + 停用供应商 错误码" "$CODE" "66021"
say "     文案：$(msg)"
if printf '%s' "$(msg)" | grep -q "停用"; then ok "文案点名「已停用」（party_b_ref_for_new 口径不回退）"; else bad "文案未点名停用：$(msg)"; fi

snap s2_contracts \
  "select contract_no||'|'||contract_type||'|'||party_b_type||'|'||status||'|'||amount||'|'||coalesce(end_date::text,'-') from biz_contract order by contract_no" \
  "select 'count='||count(*) from biz_contract"

# ---------- S3 PUT（草稿改单）也要过配对校验 ----------
say; say "S3 草稿改单（PUT）：乙方类型与（冻结的）合同类型不符 → 400；相符 → 200"
api s3_put_bad PUT "/business/erp/contract/$C_SRV" "$T_ADMIN" '{"party_b_type":"CUSTOMER","remark":"试图把服务合同的乙方改成客户（应被拒）"}'
check_http "PUT SERVICE + CUSTOMER" "$HTTP" "400"
check_code "PUT SERVICE + CUSTOMER 错误码" "$CODE" "10001"
say "     文案：$(msg)"
check "被拒后合同仍 DRAFT" "$(P "select status from biz_contract where id='$C_SRV'")" "DRAFT"
check "被拒后备注未被改" "$(P "select coalesce(remark,'-') from biz_contract where id='$C_SRV'")" "CTR-02-F1 服务合同（<10 万，单级审批）"
api s3_put_ok PUT "/business/erp/contract/$C_SRV" "$T_ADMIN" '{"party_b_type":"SUPPLIER","remark":"CTR-02-F1 服务合同（草稿改备注，乙方类型显式 SUPPLIER）"}'
check_http "PUT SERVICE + SUPPLIER" "$HTTP" "200"
check "改后备注" "$(data '.data.remark')" "CTR-02-F1 服务合同（草稿改备注，乙方类型显式 SUPPLIER）"

# ---------- S4 详情 commitment_scope ----------
say; say "S4 详情履约口径：PURCHASE=ORDER_LINKED；SERVICE/LEASE=NOT_APPLICABLE 且 committed_amount=0"
api s4_pur GET "/business/erp/contract/$C_PUR" "$T_ADMIN"
check "PURCHASE commitment_scope" "$(data '.data.commitment_scope')" "ORDER_LINKED"
api s4_srv GET "/business/erp/contract/$C_SRV" "$T_ADMIN"
check "SERVICE commitment_scope" "$(data '.data.commitment_scope')" "NOT_APPLICABLE"
check "SERVICE committed_amount" "$(data '.data.committed_amount')" "0"
check "SERVICE remaining_amount=合同额" "$(data '.data.remaining_amount')" "50000.0000"
check "SERVICE open_order_count" "$(data '.data.open_order_count')" "0"
check "SERVICE open_order_nos 为空数组" "$(data '.data.open_order_nos | length')" "0"
api s4_lse GET "/business/erp/contract/$C_LSE" "$T_ADMIN"
check "LEASE commitment_scope" "$(data '.data.commitment_scope')" "NOT_APPLICABLE"
check "LEASE committed_amount" "$(data '.data.committed_amount')" "0"
api s4_page GET "/business/erp/contract/page?page=1&page_size=20" "$T_ADMIN"
check "列表接口可用（3 行）" "$(data '.data.total')" "3"
check "列表行未带履约金额（口径只在详情）" "$(data '.data.list[0] | has("committed_amount")')" "false"

# ---------- S5 审批（SERVICE 单级 / LEASE 两级）----------
say; say "S5 提交审批：contract_approve 阈值 10 万不变、不按类型分流"
api s5_sub_srv PATCH "/business/erp/contract/$C_SRV/status" "$T_ADMIN" '{"target_status":"PENDING_APPROVAL"}'
check "SERVICE 提交后状态" "$(data '.data.status')" "PENDING_APPROVAL"
INST_SRV=$(P "select approval_instance_id from biz_contract where id='$C_SRV'")
check "SERVICE 审批实例状态" "$(P "select status from biz_flow_instance where id='$INST_SRV'")" "PENDING"
check "SERVICE 首节点 = n_pm（单级）" "$(P "select node_key from biz_flow_task where instance_id='$INST_SRV' and status='PENDING' order by created_at limit 1")" "n_pm"
check "SERVICE 只有 1 个待办（单级）" "$(P "select count(*) from biz_flow_task where instance_id='$INST_SRV' and status='PENDING'")" "1"
SELF_TASK=$(P "select id from biz_flow_task where instance_id='$INST_SRV' and status='PENDING' limit 1")
api s5_self POST "/business/flow/task/$SELF_TASK/approve" "$T_ADMIN" '{"comment":"CTR-02-F1 自批（应被拦）"}'
check_http "负例：制单人自批" "$HTTP" "403"
r=0
while [ "$r" -lt 4 ]; do
  tasks=$(P "select id from biz_flow_task where instance_id='$INST_SRV' and status='PENDING' order by created_at")
  [ -z "$tasks" ] && break
  for t in $tasks; do
    api "s5_srv_approve_r$((r+1))" POST "/business/flow/task/$t/approve" "$T_APP" '{"comment":"CTR-02-F1 服务合同审批同意"}'
    if [ "$CODE" = "0" ]; then ok "SERVICE 审批通过 task=$t（第 $((r+1)) 轮）"; else bad "SERVICE 审批失败 task=$t http=$HTTP body=$(printf '%s' "$BODY" | head -c 300)"; fi
  done
  r=$((r+1))
done
check "SERVICE 审批轮次（<10 万单级）" "$r" "1"
check "SERVICE 实例 APPROVED" "$(P "select status from biz_flow_instance where id='$INST_SRV'")" "APPROVED"
check "SERVICE 合同 SIGNED" "$(P "select status from biz_contract where id='$C_SRV'")" "SIGNED"

api s5_sub_lse PATCH "/business/erp/contract/$C_LSE/status" "$T_ADMIN" '{"target_status":"PENDING_APPROVAL"}'
check "LEASE 提交后状态" "$(data '.data.status')" "PENDING_APPROVAL"
INST_LSE=$(P "select approval_instance_id from biz_contract where id='$C_LSE'")
check "LEASE 首节点 = n_pm_hi（≥10 万第一级）" "$(P "select node_key from biz_flow_task where instance_id='$INST_LSE' and status='PENDING' order by created_at limit 1")" "n_pm_hi"
r=0; NODES=""
while [ "$r" -lt 4 ]; do
  tasks=$(P "select id from biz_flow_task where instance_id='$INST_LSE' and status='PENDING' order by created_at")
  [ -z "$tasks" ] && break
  for t in $tasks; do
    NODES="$NODES$(P "select node_key from biz_flow_task where id='$t'"),"
    api "s5_lse_approve_r$((r+1))" POST "/business/flow/task/$t/approve" "$T_APP" '{"comment":"CTR-02-F1 租赁合同审批同意"}'
    if [ "$CODE" = "0" ]; then ok "LEASE 审批通过 task=$t（第 $((r+1)) 轮）"; else bad "LEASE 审批失败 task=$t http=$HTTP body=$(printf '%s' "$BODY" | head -c 300)"; fi
  done
  r=$((r+1))
done
check "LEASE 审批轮次（≥10 万两级）" "$r" "2"
check "LEASE 审批节点顺序" "$NODES" "n_pm_hi,n_gm,"
check "LEASE 实例 APPROVED" "$(P "select status from biz_flow_instance where id='$INST_LSE'")" "APPROVED"
check "LEASE 合同 SIGNED" "$(P "select status from biz_contract where id='$C_LSE'")" "SIGNED"

snap s5_flow \
  "select 'contract='||contract_no||'|type='||contract_type||'|status='||status||'|amount='||amount||'|inst='||coalesce(approval_instance_id::text,'-') from biz_contract where id in ('$C_SRV','$C_LSE') order by contract_no" \
  "select 'task='||node_key||'|'||status||'|'||coalesce(assignee_name,'-')||'|'||coalesce(comment,'-') from biz_flow_task where instance_id in ('$INST_SRV','$INST_LSE') order by created_at" \
  "select 'log='||from_status||'->'||to_status||'|'||coalesce(reason,'-') from biz_status_log where entity_type='CONTRACT' and entity_id in ('$C_SRV','$C_LSE') order by created_at"

# ---------- S6 到期提醒（LEASE end_date=今天+7）----------
say; say "S6 到期提醒：LEASE 合同 end_date=$D7 应被 erp_contract_expire_scan 命中（D-7 档）"
BEFORE=$(P "select count(*) from biz_reminder_log where doc_type='CONTRACT' and doc_id='$C_LSE'")
api s6_scan POST /business/erp/reminder/contract_expire/run "$T_ADMIN" ''
check_http "提醒扫描" "$HTTP" "200"
check "扫描调用成功" "$CODE" "0"
say "     扫描结果：$(printf '%s' "$BODY" | jq -c '.data.runs[0]' 2>/dev/null)"
if [ "$(printf '%s' "$BODY" | jq -r '.data.runs[0].scanned // 0')" -ge 1 ]; then ok "扫描命中 ≥1 张合同"; else bad "扫描 scanned=0"; fi
AFTER=$(P "select count(*) from biz_reminder_log where doc_type='CONTRACT' and doc_id='$C_LSE'")
check "LEASE 提醒台账新增 1 行（$BEFORE → $AFTER）" "$AFTER" "$((BEFORE+1))"
check "台账档位 = D-7" "$(P "select stage from biz_reminder_log where doc_type='CONTRACT' and doc_id='$C_LSE' order by sent_at desc limit 1")" "D-7"
check "台账 kind = CONTRACT_EXPIRE" "$(P "select remind_kind from biz_reminder_log where doc_type='CONTRACT' and doc_id='$C_LSE' order by sent_at desc limit 1")" "CONTRACT_EXPIRE"
api s6_scan2 POST /business/erp/reminder/contract_expire/run "$T_ADMIN" ''
check "二次扫描 sent=0（台账幂等）" "$(printf '%s' "$BODY" | jq -r '.data.runs[0].sent // -1')" "0"
check "二次扫描后台账仍 1 行" "$(P "select count(*) from biz_reminder_log where doc_type='CONTRACT' and doc_id='$C_LSE'")" "$AFTER"

# ---------- S7 收口 ----------
say; say "S7 收口：合同表快照 + 合同链不记账"
snap s7_final \
  "select contract_no||'|'||contract_type||'|'||party_b_type||'|'||party_b_name||'|'||status||'|'||amount from biz_contract order by contract_no" \
  "select 'voucher='||count(*) from biz_voucher" \
  "select 'inv_txn='||count(*) from biz_inventory_txn" \
  "select 'reminder_log='||count(*) from biz_reminder_log where doc_type='CONTRACT'"

# 脱敏（令牌不落证据）
for f in "$RAW"/login_*.json "$RAW"/s0_login0.json; do
  [ -f "$f" ] || continue
  jq 'if .data? and (.data|type=="object") then (.data.access_token="<redacted>" | .data.refresh_token="<redacted>") else . end' "$f" >"$f.tmp" 2>/dev/null && mv "$f.tmp" "$f"
done

say
say "================ 断言汇总 ================"
say "PASS=$PASS FAIL=$FAIL"
if [ "$FAIL" -gt 0 ]; then
  say "失败项："
  for f in "${FAILED[@]}"; do say "  - $f"; done
  echo "PASS=$PASS FAIL=$FAIL" > "$OUT/asserts.txt"
  exit 1
fi
echo "PASS=$PASS FAIL=$FAIL" > "$OUT/asserts.txt"
say "全绿 ✅   （证据：$RAW/ 与 assert.txt）"
