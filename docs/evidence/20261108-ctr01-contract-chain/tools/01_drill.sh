#!/usr/bin/env bash
# =============================================================================
# CTR-01 合同链演练（真后端 + 真 token curl + 真库断言）
#   前置：bash 00_env.sh（库 cloudpivot_ctr01 + 实例 127.0.0.1:18226）
#   运行：bash 01_drill.sh
# 链路：主数据 → 4 类合同建档 → 附件 → 提交审批（含金额两级）→ 审批通过
#       → 生效 → 变更（DRAFT 可改 / 签后冻结）→ 履约（采购单 / 销售单挂合同 + 额度勾稽）
#       → 到期提醒（D-30 手动触发 / 二次幂等 / DRAFT 不提醒）→ 归档 COMPLETED / 终止 TERMINATED
# 产出：/tmp/ctr01_raw/*.json（每步响应）+ *.txt（每步 DB 快照）
# =============================================================================
set -uo pipefail
DB=${CTR01_DB:-cloudpivot_ctr01}
PORT=${CTR01_PORT:-18226}
BASE="http://127.0.0.1:$PORT/api"
RAW=/tmp/ctr01_raw
rm -rf "$RAW"; mkdir -p "$RAW"

set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
P() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }

PASS=0; FAIL=0; FAILED=()
say() { printf '%s\n' "$*"; }
ok()  { PASS=$((PASS+1)); say "  ✅ $*"; }
bad() { FAIL=$((FAIL+1)); FAILED+=("$*"); say "  ❌ $*"; }
check() { if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
check_http() { if [ "$2" = "$3" ]; then ok "$1 HTTP=$2"; else bad "$1 HTTP=$2（期望 $3）"; fi; }
check_code() { if [ "$2" = "$3" ]; then ok "$1 code=$2"; else bad "$1 code=$2（期望 $3）"; fi; }

# ---------- HTTP 原语 ----------
# Authorization 头用两段拼接，避免证据脚本被脱敏器改写成 ***
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

snap() { # snap <文件名> <SQL1> <SQL2> ...
  local f="$RAW/$1.txt"; shift
  : >"$f"
  local sql
  for sql in "$@"; do
    echo "--- $sql" >>"$f"
    P "$sql" >>"$f" 2>&1 || true
  done
  if grep -q '^ERROR' "$f"; then say "  ⚠ 快照有 SQL 报错：$f"; fi
}

TODAY=$(TZ=Asia/Shanghai date +%F)
D30=$(TZ=Asia/Shanghai date -d "$TODAY +30 days" +%F)
MAT=d1370003-0000-4000-8000-000000000002   # M-W1A-002 原料B（种子）
WH=d1370001-0000-4000-8000-000000000001    # WH01 总仓（种子）

say "== 环境：$BASE 库=$DB 业务日=$TODAY 到期档=$D30"
say "   git_head=$(cd /opt/cloudpivot && git rev-parse HEAD)"
say "   binary_sha256=$(sha256sum /opt/cloudpivot/backend/target/debug/cloudpivot | cut -d' ' -f1)"

# ---------- S0 认证 / 审批人 ----------
say; say "S0 认证与审批人（制单人 admin ≠ 审批人 ctr01_approver）"
api s0_login0 POST /system/auth/login "" '{"username":"admin","password":"Admin@123456"}'
check_http "种子 admin 登录" "$HTTP" "200"
T0=$(data '.data.access_token')
api s0_pwd PUT /system/auth/password "$T0" '{"old_password":"Admin@123456","new_password":"Admin@Ctr01!2026"}'
check "admin 首登改密" "$CODE" "0"
T_ADMIN=$(login admin 'Admin@Ctr01!2026' admin) || { say "FATAL: admin 改密后登录失败，检查上面的 login_admin_*.json"; exit 1; }
ok "admin token 已取得（改密后重新登录）"

R_PM=$(P "select id from sys_role where role_code='PURCHASE_MANAGER'")
R_GM=$(P "select id from sys_role where role_code='GENERAL_MANAGER'")
R_SA=$(P "select id from sys_role where role_code='SUPER_ADMIN'")
api s0_user POST /system/user "$T_ADMIN" "{\"username\":\"ctr01_approver\",\"password\":\"Ctr01@Mgr123\",\"real_name\":\"CTR01 审批人\",\"role_ids\":[\"$R_PM\",\"$R_GM\",\"$R_SA\"],\"status\":1,\"remark\":\"CTR-01 演练审批人（采购经理+总经理+超管）\"}"
check "审批人账号创建" "$CODE" "0"
APP_ID=$(data '.data.id')
T_APP=$(login ctr01_approver 'Ctr01@Mgr123' approver) || { say "FATAL: 审批人登录失败（用户没建成？看 s0_user.json）"; exit 1; }
ok "审批人 token 已取得 id=$APP_ID"

# ---------- S1 主数据 ----------
say; say "S1 主数据：供应商（采购合同乙方）/ 客户（销售合同乙方）"
api s1_sup POST /business/erp/supplier "$T_ADMIN" '{"supplier_name":"CTR01 演练供应商","short_name":"CTR01供","contact_person":"王五","contact_phone":"13700000001","credit_days":30,"credit_limit":500000,"remark":"CTR-01 合同演练供应商"}'
SUP=$(data '.data.id'); SUP_CODE=$(data '.data.supplier_code')
if [ "$CODE" = "0" ] && [ -n "$SUP" ]; then ok "供应商 $SUP_CODE id=$SUP"; else bad "供应商建档 http=$HTTP body=$(printf '%s' "$BODY" | head -c 300)"; fi
api s1_cust POST /business/erp/customer "$T_ADMIN" '{"code":"C-CTR01-001","name":"CTR01 演练客户","tax_no":"91110000CTR01TESTX","contact_name":"赵六","contact_phone":"13700000002","address":"北京市朝阳区演练路 9 号","credit_days":30,"credit_limit":200000,"remark":"CTR-01 合同演练客户"}'
CUST=$(data '.data.id')
if [ "$CODE" = "0" ] && [ -n "$CUST" ]; then ok "客户 C-CTR01-001 id=$CUST"; else bad "客户建档 http=$HTTP body=$(printf '%s' "$BODY" | head -c 300)"; fi

# ---------- S2 四类合同 -------------
say; say "S2 建合同：PURCHASE / SALES 各一个；SERVICE / LEASE 各试一次（查类型闸）"
api s2_pur POST /business/erp/contract "$T_ADMIN" "{\"name\":\"CTR01 年度原料采购框架协议\",\"contract_type\":\"PURCHASE\",\"party_b_id\":\"$SUP\",\"amount\":100000,\"sign_date\":\"2026-11-01\",\"start_date\":\"2026-11-01\",\"end_date\":\"2027-11-01\",\"owner_id\":\"$APP_ID\",\"remark\":\"CTR-01 采购合同（≥10 万，走两级审批）\"}"
C_PUR=$(data '.data.id'); NO_PUR=$(data '.data.contract_no')
if [ "$CODE" = "0" ] && [ -n "$C_PUR" ]; then ok "采购合同 $NO_PUR id=$C_PUR 金额=$(data '.data.amount') 乙方快照=$(data '.data.party_b_name')"; else bad "采购合同建档 http=$HTTP body=$(printf '%s' "$BODY" | head -c 400)"; fi
check "采购合同初始状态" "$(data '.data.status')" "DRAFT"
check "采购合同可编辑" "$(data '.data.can_edit')" "true"
check "采购合同可用于下单（草稿=false）" "$(data '.data.usable_for_order')" "false"
check "采购合同 next_statuses" "$(data '.data.next_statuses | join(",")')" "PENDING_APPROVAL"
if printf '%s' "$NO_PUR" | grep -Eq '^HT-[0-9]{8}-[0-9]{6}$'; then ok "合同号格式 $NO_PUR（HT-yyyyMMdd-000001）"; else bad "合同号格式 $NO_PUR"; fi

api s2_sal POST /business/erp/contract "$T_ADMIN" "{\"name\":\"CTR01 产品销售框架合同\",\"contract_type\":\"SALES\",\"party_b_id\":\"$CUST\",\"amount\":50000,\"sign_date\":\"2026-11-01\",\"start_date\":\"2026-11-01\",\"end_date\":\"2027-06-30\",\"owner_id\":\"$APP_ID\",\"remark\":\"CTR-01 销售合同\"}"
C_SAL=$(data '.data.id'); NO_SAL=$(data '.data.contract_no')
if [ "$CODE" = "0" ] && [ -n "$C_SAL" ]; then ok "销售合同 $NO_SAL id=$C_SAL 乙方类型=$(data '.data.party_b_type') 乙方快照=$(data '.data.party_b_name')"; else bad "销售合同建档 http=$HTTP body=$(printf '%s' "$BODY" | head -c 400)"; fi

api s2_srv POST /business/erp/contract "$T_ADMIN" "{\"name\":\"CTR01 运维服务合同\",\"contract_type\":\"SERVICE\",\"party_b_id\":\"$SUP\",\"amount\":30000,\"sign_date\":\"2026-11-01\",\"start_date\":\"2026-11-01\",\"end_date\":\"2027-11-01\"}"
check_http "SERVICE 合同建档（类型闸）" "$HTTP" "400"
check_code "SERVICE 合同错误码" "$CODE" "10001"
say "     SERVICE 文案：$(msg)"
if printf '%s' "$(msg)" | grep -q "PURCHASE" && printf '%s' "$(msg)" | grep -q "SALES"; then ok "SERVICE 拒绝文案点名放行集（PURCHASE / SALES）"; else bad "SERVICE 拒绝文案未点名放行集：$(msg)"; fi

api s2_lse POST /business/erp/contract "$T_ADMIN" "{\"name\":\"CTR01 仓库租赁合同\",\"contract_type\":\"LEASE\",\"party_b_id\":\"$SUP\",\"amount\":200000,\"sign_date\":\"2026-11-01\",\"start_date\":\"2026-11-01\",\"end_date\":\"2027-11-01\"}"
check_http "LEASE 合同建档（类型闸）" "$HTTP" "400"
check_code "LEASE 合同错误码" "$CODE" "10001"

# ---------- S2b 附件（合同扫描件） ----------
say; say "S2b 合同附件（CONTRACT / CONTRACT_SCAN）"
printf '\x89PNG\r\n\x1a\n' >/tmp/ctr01_scan.png
printf 'CTR-01 contract scan evidence (fake png payload)\n' >>/tmp/ctr01_scan.png
UP=$(curl -s -m 30 -X POST "$BASE/business/attachment/upload" -H "$(auth_header "$T_ADMIN")" \
  -F "doc_type=CONTRACT" -F "doc_id=$C_PUR" -F "file_role=CONTRACT_SCAN" \
  -F "file=@/tmp/ctr01_scan.png;type=image/png")
printf '%s' "$UP" >"$RAW/s2b_upload.json"
UCODE=$(printf '%s' "$UP" | jq -r '.code // empty')
if [ "$UCODE" = "0" ]; then ok "合同扫描件已上传"; else bad "合同扫描件上传失败 $(printf '%s' "$UP" | head -c 200)"; fi
check "附件行数（biz_doc_attachment）" "$(P "select count(*) from biz_doc_attachment where doc_type='CONTRACT' and doc_id='$C_PUR' and deleted_at is null")" "1"

snap s2_contracts \
  "select 'contract='||contract_no||'|'||contract_type||'|'||status||'|amt='||amount||'|party='||party_b_type||':'||party_b_name||'|sign='||coalesce(sign_date::text,'-')||'|start='||coalesce(start_date::text,'-')||'|end='||coalesce(end_date::text,'-') from biz_contract order by contract_no" \
  "select 'attach='||doc_type||'|'||file_role||'|file='||file_id from biz_doc_attachment where doc_id='$C_PUR'"

# ---------- S3 审批 ----------
say; say "S3 提交审批：先打两条负例（DRAFT 直签 / 直生效），再走真实审批"
api s3_draft_sign PATCH "/business/erp/contract/$C_SAL/status" "$T_ADMIN" '{"target_status":"SIGNED"}'
check_http "负例：DRAFT → SIGNED（跳过审批）" "$HTTP" "409"
check_code "负例：DRAFT → SIGNED 错误码" "$CODE" "66015"
if printf '%s' "$(msg)" | grep -q "提交审批"; then ok "拒绝文案指向「先提交审批」"; else bad "拒绝文案未指向提交审批：$(msg)"; fi
api s3_draft_active PATCH "/business/erp/contract/$C_SAL/status" "$T_ADMIN" '{"target_status":"ACTIVE"}'
check_code "负例：DRAFT → ACTIVE 错误码" "$CODE" "66015"

api s3_submit PATCH "/business/erp/contract/$C_PUR/status" "$T_ADMIN" '{"target_status":"PENDING_APPROVAL"}'
check "采购合同提交后状态" "$(data '.data.status')" "PENDING_APPROVAL"
INST_PUR=$(P "select approval_instance_id from biz_contract where id='$C_PUR'")
if [ -n "$INST_PUR" ]; then ok "审批实例已挂 approval_instance_id=$INST_PUR"; else bad "审批实例为空"; fi
check "流程实例状态" "$(P "select status from biz_flow_instance where id='$INST_PUR'")" "PENDING"
snap s3_submitted \
  "select 'contract='||contract_no||'|'||status||'|inst='||coalesce(approval_instance_id::text,'-') from biz_contract where id='$C_PUR'" \
  "select 'task='||node_key||'|'||status||'|assignee='||coalesce(assignee_name,'-') from biz_flow_task where instance_id='$INST_PUR' order by created_at"

say "  -- 负例：制单人（admin）自己批自己的合同"
SELF_TASK=$(P "select id from biz_flow_task where instance_id='$INST_PUR' and status='PENDING' order by created_at limit 1")
api s3_self POST "/business/flow/task/$SELF_TASK/approve" "$T_ADMIN" '{"comment":"CTR-01 自批（应被拦）"}'
check_http "负例：自批自己的合同" "$HTTP" "403"
say "     自批结果：code=$CODE msg=$(msg)"

say "  -- 真实审批（审批人 ctr01_approver，最多 4 轮）"
r=0
while [ "$r" -lt 4 ]; do
  tasks=$(P "select t.id from biz_flow_task t where t.status='PENDING' and t.instance_id='$INST_PUR' order by t.created_at")
  [ -z "$tasks" ] && break
  for t in $tasks; do
    api "s3_approve_r$((r+1))" POST "/business/flow/task/$t/approve" "$T_APP" '{"comment":"CTR-01 演练审批同意"}'
    if [ "$CODE" = "0" ]; then ok "审批通过 task=$t（第 $((r+1)) 轮）"; else bad "审批失败 task=$t http=$HTTP body=$(printf '%s' "$BODY" | head -c 300)"; fi
  done
  r=$((r+1))
done
check "审批轮次（≥10 万两级）" "$r" "2"
check "审批结果实例状态" "$(P "select status from biz_flow_instance where id='$INST_PUR'")" "APPROVED"
check "采购合同审批后状态" "$(P "select status from biz_contract where id='$C_PUR'")" "SIGNED"
check "SIGNED 保留实例 id（审计）" "$(P "select approval_instance_id is not null from biz_contract where id='$C_PUR'")" "t"
check "状态日志：DRAFT → PENDING_APPROVAL" "$(P "select count(*) from biz_status_log where entity_type='CONTRACT' and entity_id='$C_PUR' and from_status='DRAFT' and to_status='PENDING_APPROVAL'")" "1"
check "状态日志：PENDING_APPROVAL → SIGNED" "$(P "select count(*) from biz_status_log where entity_type='CONTRACT' and entity_id='$C_PUR' and from_status='PENDING_APPROVAL' and to_status='SIGNED'")" "1"

api s3_active PATCH "/business/erp/contract/$C_PUR/status" "$T_ADMIN" '{"target_status":"ACTIVE"}'
check "采购合同生效后状态" "$(data '.data.status')" "ACTIVE"
check "生效后可用于下单" "$(data '.data.usable_for_order')" "true"
check "生效后不可编辑" "$(data '.data.can_edit')" "false"
snap s3_after_approval \
  "select 'contract='||contract_no||'|'||status||'|inst='||coalesce(approval_instance_id::text,'-') from biz_contract where id='$C_PUR'" \
  "select 'log='||from_status||'->'||to_status||'|'||coalesce(reason,'-')||'|'||created_at from biz_status_log where entity_type='CONTRACT' and entity_id='$C_PUR' order by created_at" \
  "select 'task='||node_key||'|'||status||'|'||coalesce(assignee_name,'-')||'|'||coalesce(comment,'-') from biz_flow_task where instance_id='$INST_PUR' order by created_at"

# ---------- S4 合同变更 ----------
say; say "S4 变更（金额 / 期限）：草稿可改、签后冻结、有没有变更记录表"
LOG_BEFORE=$(P "select count(*) from biz_status_log where entity_type='CONTRACT' and entity_id='$C_SAL'")
api s4_update_draft PUT "/business/erp/contract/$C_SAL" "$T_ADMIN" '{"amount":55000,"end_date":"2027-07-31","remark":"CTR-01 销售合同：草稿阶段改金额 5 万→5.5 万，到期日延到 2027-07-31"}'
check "草稿改金额" "$(data '.data.amount')" "55000.0000"
check "草稿改到期日" "$(data '.data.end_date' | cut -c1-10)" "2027-07-31"
check "改单后状态仍是 DRAFT" "$(data '.data.status')" "DRAFT"
LOG_AFTER=$(P "select count(*) from biz_status_log where entity_type='CONTRACT' and entity_id='$C_SAL'")
check "改单不新增状态日志（字段变更不是状态变更）" "$LOG_AFTER" "$LOG_BEFORE"
CHG=$(P "select count(*) from sys_change_log where resource like '%contract%' or resource_id='$C_SAL'")
check "sys_change_log 里的合同变更留痕" "$CHG" "0"
TABLES=$(P "select count(*) from information_schema.tables where table_schema='public' and table_name like '%contract%' and table_name not like 'biz_contract%'")
check "独立变更记录表数量（biz_contract* 之外）" "$TABLES" "0"
snap s4_update \
  "select 'contract='||contract_no||'|amt='||amount||'|end='||end_date||'|status='||status||'|updated_by='||coalesce(update_by::text,'-') from biz_contract where id='$C_SAL'" \
  "select 'change_log_rows='||count(*) from sys_change_log where resource_id='$C_SAL' or resource like '%contract%'"

say "  -- 销售合同走审批到 SIGNED，再试改金额（应被字段冻结拦）"
api s4_sal_submit PATCH "/business/erp/contract/$C_SAL/status" "$T_ADMIN" '{"target_status":"PENDING_APPROVAL"}'
check "销售合同提交审批" "$(data '.data.status')" "PENDING_APPROVAL"
api s4_update_pending PUT "/business/erp/contract/$C_SAL" "$T_ADMIN" '{"amount":88000}'
check_http "负例：审批中改金额（审 A 改 B 洞）" "$HTTP" "409"
check_code "负例：审批中改金额错误码" "$CODE" "66016"
check "审批中金额未被改动" "$(P "select amount from biz_contract where id='$C_SAL'")" "55000.0000"
INST_SAL=$(P "select approval_instance_id from biz_contract where id='$C_SAL'")
r=0
while [ "$r" -lt 4 ]; do
  tasks=$(P "select t.id from biz_flow_task t where t.status='PENDING' and t.instance_id='$INST_SAL' order by t.created_at")
  [ -z "$tasks" ] && break
  for t in $tasks; do
    api "s4_approve_r$((r+1))" POST "/business/flow/task/$t/approve" "$T_APP" '{"comment":"CTR-01 销售合同审批同意"}'
    if [ "$CODE" = "0" ]; then ok "销售合同审批通过 task=$t（第 $((r+1)) 轮）"; else bad "销售合同审批失败 task=$t http=$HTTP body=$(printf '%s' "$BODY" | head -c 300)"; fi
  done
  r=$((r+1))
done
check "销售合同审批后状态" "$(P "select status from biz_contract where id='$C_SAL'")" "SIGNED"
say "     审批人链路：$(P "select string_agg(node_key||':'||coalesce(assignee_name,'-'),' → ' order by created_at) from biz_flow_task where instance_id='$INST_SAL'")"

api s4_update_signed PUT "/business/erp/contract/$C_SAL" "$T_ADMIN" '{"amount":60000}'
check_http "负例：已签订改金额" "$HTTP" "409"
check_code "负例：已签订改金额错误码" "$CODE" "66016"
say "     冻结文案：$(msg)"
api s4_active_sal PATCH "/business/erp/contract/$C_SAL/status" "$T_ADMIN" '{"target_status":"ACTIVE"}'
check "销售合同生效" "$(data '.data.status')" "ACTIVE"

# ---------- S5 履约：关联订单 + 金额勾稽 ----------
say; say "S5 履约：采购单挂合同（供应商派生 + 累计额度勾稽）"
api s5_po1 POST /business/erp/purchase-order "$T_ADMIN" "{\"purchase_type\":\"CONTRACT\",\"contract_id\":\"$C_PUR\",\"warehouse_id\":\"$WH\",\"order_date\":\"2026-11-09\",\"expect_date\":\"2026-11-20\",\"remark\":\"CTR-01 履约采购单 1\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":100,\"price_excl\":300,\"tax_rate\":13}]}"
PO1=$(data '.data.order.id')
if [ "$CODE" = "0" ] && [ -n "$PO1" ]; then ok "采购单1 $(data '.data.order.order_no') id=$PO1 价税合计=$(data '.data.order.total_with_tax')"; else bad "采购单1 http=$HTTP body=$(printf '%s' "$BODY" | head -c 400)"; fi
check "采购单1 采购类型" "$(data '.data.order.purchase_type')" "CONTRACT"
check "采购单1 供应商=合同乙方（派生）" "$(data '.data.order.supplier_id')" "$SUP"
check "采购单1 合同号快照" "$(data '.data.order.contract_no_snapshot')" "$NO_PUR"
check "采购单1 价税合计 33900" "$(P "select total_with_tax from biz_purchase_order where id='$PO1'")" "33900.0000"

api s5_po2_bad POST /business/erp/purchase-order "$T_ADMIN" "{\"purchase_type\":\"CONTRACT\",\"contract_id\":\"$C_PUR\",\"warehouse_id\":\"$WH\",\"order_date\":\"2026-11-09\",\"remark\":\"CTR-01 超额单（应拒）\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":200,\"price_excl\":300,\"tax_rate\":13}]}"
check_http "负例：累计超合同金额" "$HTTP" "409"
check_code "负例：超额错误码" "$CODE" "66019"
say "     超额文案：$(msg)"

api s5_po3 POST /business/erp/purchase-order "$T_ADMIN" "{\"purchase_type\":\"CONTRACT\",\"contract_id\":\"$C_PUR\",\"warehouse_id\":\"$WH\",\"order_date\":\"2026-11-09\",\"remark\":\"CTR-01 履约采购单 2（把额度刚好用满）\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":661,\"price_excl\":100,\"tax_rate\":0}]}"
PO3=$(data '.data.order.id')
if [ "$CODE" = "0" ] && [ -n "$PO3" ]; then ok "采购单2 id=$PO3 价税合计=$(data '.data.order.total_with_tax')"; else bad "采购单2 http=$HTTP body=$(printf '%s' "$BODY" | head -c 400)"; fi
check "累计已下单额 = 合同金额（边界相等放行）" "$(P "select sum(total_with_tax) from biz_purchase_order where contract_id='$C_PUR' and status<>'CANCELLED' and deleted_at is null")" "100000.0000"
api s5_po4_bad POST /business/erp/purchase-order "$T_ADMIN" "{\"purchase_type\":\"CONTRACT\",\"contract_id\":\"$C_PUR\",\"warehouse_id\":\"$WH\",\"order_date\":\"2026-11-09\",\"remark\":\"CTR-01 超 1 元（应拒）\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":1,\"price_excl\":1,\"tax_rate\":0}]}"
check_code "负例：超出 1 元也拒（66019）" "$CODE" "66019"

say "  -- 销售单挂销售合同"
api s5_so1 POST /business/erp/sales-order "$T_ADMIN" "{\"customer_id\":\"$CUST\",\"contract_id\":\"$C_SAL\",\"order_date\":\"2026-11-09\",\"delivery_date\":\"2026-11-20\",\"remark\":\"CTR-01 履约销售单\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":100,\"price\":300,\"tax_rate\":13}]}"
SO1=$(data '.data.id')
if [ "$CODE" = "0" ] && [ -n "$SO1" ]; then ok "销售单 $(data '.data.so_no') id=$SO1 价税合计=$(data '.data.total_with_tax')"; else bad "销售单 http=$HTTP body=$(printf '%s' "$BODY" | head -c 400)"; fi
api s5_so1_detail GET "/business/erp/sales-order/$SO1" "$T_ADMIN"
check "销售单详情带合同号（SO-27）" "$(data '.data.contract_no')" "$NO_SAL"
api s5_so2_bad POST /business/erp/sales-order "$T_ADMIN" "{\"customer_id\":\"$CUST\",\"contract_id\":\"$C_SAL\",\"order_date\":\"2026-11-09\",\"remark\":\"CTR-01 销售超额（应拒）\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":100,\"price\":300,\"tax_rate\":13}]}"
check_code "负例：销售单累计超合同金额" "$CODE" "66019"

say "  -- 跨类型 / 跨乙方的合同引用负例"
api s5_po_expired POST /business/erp/purchase-order "$T_ADMIN" "{\"purchase_type\":\"CONTRACT\",\"contract_id\":\"$C_PUR\",\"warehouse_id\":\"$WH\",\"order_date\":\"2028-01-01\",\"remark\":\"CTR-01 下单日超出合同有效期（应拒）\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":1,\"price_excl\":1,\"tax_rate\":0}]}"
check_http "负例：下单日超出合同到期日" "$HTTP" "400"
check_code "负例：超出有效期错误码" "$CODE" "66014"
say "     文案：$(msg)"
api s5_po_cross POST /business/erp/purchase-order "$T_ADMIN" "{\"purchase_type\":\"CONTRACT\",\"contract_id\":\"$C_SAL\",\"warehouse_id\":\"$WH\",\"order_date\":\"2026-11-09\",\"remark\":\"CTR-01 采购单挂销售合同（应拒）\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":1,\"price_excl\":1,\"tax_rate\":0}]}"
check_http "负例：采购单挂销售合同" "$HTTP" "400"
say "     结果：code=$CODE msg=$(msg)"
api s5_so_cross POST /business/erp/sales-order "$T_ADMIN" "{\"customer_id\":\"$CUST\",\"contract_id\":\"$C_PUR\",\"order_date\":\"2026-11-09\",\"remark\":\"CTR-01 销售单挂采购合同（应拒）\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":1,\"price\":100,\"tax_rate\":13}]}"
check_http "负例：销售单挂采购合同" "$HTTP" "400"
say "     结果：code=$CODE msg=$(msg)"

api s5_detail GET "/business/erp/contract/$C_PUR" "$T_ADMIN"
HAS_EXEC=$(printf '%s' "$BODY" | jq -r '.data | has("executed_amount") or has("used_amount") or has("committed_amount")')
check "合同详情是否下发已执行金额字段" "$HAS_EXEC" "false"

snap s5_performance \
  "select 'contract='||contract_no||'|amt='||amount from biz_contract where id in ('$C_PUR','$C_SAL') order by contract_no" \
  "select 'po='||order_no||'|'||status||'|type='||purchase_type||'|total='||total_with_tax||'|contract='||coalesce(contract_no_snapshot,'-') from biz_purchase_order order by created_at" \
  "select 'po_committed='||coalesce(sum(total_with_tax),0) from biz_purchase_order where contract_id='$C_PUR' and status<>'CANCELLED' and deleted_at is null" \
  "select 'so='||so_no||'|'||status||'|total='||total_with_tax||'|contract='||coalesce(contract_id::text,'-') from biz_sales_order order by created_at" \
  "select 'so_committed='||coalesce(sum(total_with_tax),0) from biz_sales_order where contract_id='$C_SAL' and status<>'CANCELLED' and deleted_at is null"

# ---------- S6 到期提醒 ----------
say; say "S6 到期提醒：建一张 ${D30} 到期（D-30 档）的生效合同 + 一张同日期草稿，手动触发扫描"
api s6_exp POST /business/erp/contract "$T_ADMIN" "{\"name\":\"CTR01 到期提醒演练合同\",\"contract_type\":\"PURCHASE\",\"party_b_id\":\"$SUP\",\"amount\":20000,\"sign_date\":\"2026-11-09\",\"start_date\":\"2026-11-09\",\"end_date\":\"$D30\",\"owner_id\":\"$APP_ID\",\"remark\":\"CTR-01 到期提醒（D-30）\"}"
C_EXP=$(data '.data.id'); NO_EXP=$(data '.data.contract_no')
if [ "$CODE" = "0" ] && [ -n "$C_EXP" ]; then ok "提醒用合同 $NO_EXP 到期日=$D30"; else bad "提醒用合同建档 http=$HTTP body=$(printf '%s' "$BODY" | head -c 300)"; fi
api s6_exp_draft POST /business/erp/contract "$T_ADMIN" "{\"name\":\"CTR01 草稿合同（同到期日）\",\"contract_type\":\"PURCHASE\",\"party_b_id\":\"$SUP\",\"amount\":20000,\"sign_date\":\"2026-11-09\",\"start_date\":\"2026-11-09\",\"end_date\":\"$D30\",\"remark\":\"CTR-01 草稿不提醒\"}"
C_EXPD=$(data '.data.id')
api s6_exp_submit PATCH "/business/erp/contract/$C_EXP/status" "$T_ADMIN" '{"target_status":"PENDING_APPROVAL"}'
INST_EXP=$(P "select approval_instance_id from biz_contract where id='$C_EXP'")
r=0
while [ "$r" -lt 4 ]; do
  tasks=$(P "select t.id from biz_flow_task t where t.status='PENDING' and t.instance_id='$INST_EXP' order by t.created_at")
  [ -z "$tasks" ] && break
  for t in $tasks; do api "s6_approve_r$((r+1))" POST "/business/flow/task/$t/approve" "$T_APP" '{"comment":"CTR-01 提醒合同审批同意"}'; done
  r=$((r+1))
done
api s6_exp_active PATCH "/business/erp/contract/$C_EXP/status" "$T_ADMIN" '{"target_status":"ACTIVE"}'
check "提醒用合同生效" "$(data '.data.status')" "ACTIVE"

say "  -- ① 定时任务页面有没有这个任务（sys_job）"
check "sys_job 有 erp_contract_expire_scan" "$(P "select count(*) from sys_job where job_name='erp_contract_expire_scan' and deleted_at is null")" "1"
say "     $(P "select job_name||' status='||status||' cron='||cron_expr||' target='||invoke_target from sys_job where job_name='erp_contract_expire_scan'")"
JOB_ID=$(P "select id from sys_job where job_name='erp_contract_expire_scan' and deleted_at is null")

say "  -- ② 手动触发（业务提醒接口 contract_expire）"
api s6_run POST /business/erp/reminder/contract_expire/run "$T_ADMIN" '{}'
check "提醒扫描接口返回" "$CODE" "0"
say "     runs=$(printf '%s' "$BODY" | jq -c '.data.runs')"
SENT=$(printf '%s' "$BODY" | jq -r '.data.runs[0].sent // 0')
if [ "${SENT:-0}" -ge 1 ]; then ok "本轮发出提醒 sent=$SENT"; else bad "本轮未发出提醒 sent=$SENT"; fi
check "台账行（biz_reminder_log CONTRACT_EXPIRE / D-30）" "$(P "select count(*) from biz_reminder_log where doc_id='$C_EXP' and remind_kind='CONTRACT_EXPIRE' and stage='D-30'")" "1"
check "提醒通知（sys_notice biz_type=contract_expire）" "$(P "select count(*) from sys_notice where biz_type='contract_expire' and biz_id='$C_EXP'")" "1"
RCV=$(P "select count(*) from sys_notice_receiver r join sys_notice n on n.id=r.notice_id where n.biz_type='contract_expire' and n.biz_id='$C_EXP'")
if [ "${RCV:-0}" -ge 1 ]; then ok "提醒收件人数=$RCV（制单人/负责人/采购经理）"; else bad "提醒没有收件人"; fi
check "草稿合同不提醒（同到期日也无台账）" "$(P "select count(*) from biz_reminder_log where doc_id='$C_EXPD'")" "0"

say "  -- ③ 同一天再触发一次（幂等）"
api s6_run2 POST /business/erp/reminder/contract_expire/run "$T_ADMIN" '{}'
SENT2=$(printf '%s' "$BODY" | jq -r '.data.runs[0].sent // 0')
SKIP2=$(printf '%s' "$BODY" | jq -r '.data.runs[0].skipped_reminded // 0')
check "第二次触发 sent=0" "$SENT2" "0"
if [ "${SKIP2:-0}" -ge 1 ]; then ok "第二次触发 skipped_reminded=$SKIP2"; else bad "第二次触发没有被台账拦住 skipped_reminded=$SKIP2"; fi
check "台账仍只有 1 行（同日不重复）" "$(P "select count(*) from biz_reminder_log where doc_id='$C_EXP' and remind_kind='CONTRACT_EXPIRE'")" "1"

say "  -- ④ 定时任务页「立即执行」也跑一次"
api s6_job_trigger POST "/support/job/$JOB_ID/trigger" "$T_ADMIN" '{}'
check "sys_job 立即执行返回" "$CODE" "0"
say "     $(printf '%s' "$BODY" | jq -c '.data' | head -c 300)"

snap s6_reminder \
  "select 'log='||doc_type||'|'||remind_kind||'|'||stage||'|'||remind_date||'|'||coalesce(sent_at::text,'-') from biz_reminder_log order by sent_at" \
  "select 'notice='||biz_type||'|'||title||'|'||created_at from sys_notice where biz_type='contract_expire' order by created_at" \
  "select 'receiver='||u.username from sys_notice_receiver r join sys_notice n on n.id=r.notice_id join sys_user u on u.id=r.user_id where n.biz_type='contract_expire'"

# ---------- S7 归档 / 终止 ----------
say; say "S7 归档关闭：ACTIVE → COMPLETED（有在途采购单也允许？），之后还能挂新单吗"
api s7_complete PATCH "/business/erp/contract/$C_PUR/status" "$T_ADMIN" '{"target_status":"COMPLETED"}'
if [ "$CODE" = "0" ]; then ok "合同归档（COMPLETED）成功"; else bad "合同归档失败 http=$HTTP code=$CODE msg=$(msg)"; fi
check "归档后状态" "$(P "select status from biz_contract where id='$C_PUR'")" "COMPLETED"
api s7_po_after POST /business/erp/purchase-order "$T_ADMIN" "{\"purchase_type\":\"CONTRACT\",\"contract_id\":\"$C_PUR\",\"warehouse_id\":\"$WH\",\"order_date\":\"2026-11-09\",\"remark\":\"CTR-01 归档后再挂单（应拒）\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":1,\"price_excl\":1,\"tax_rate\":0}]}"
check_http "负例：归档后再关联新采购单" "$HTTP" "400"
check_code "负例：归档后挂单错误码" "$CODE" "66014"
say "     文案：$(msg)"
api s7_del DELETE "/business/erp/contract/$C_PUR" "$T_ADMIN"
check_http "负例：被采购单引用的合同不可删" "$HTTP" "409"
check_code "负例：不可删错误码" "$CODE" "66018"
say "     文案：$(msg)"

say "  -- 终止：无原因 vs 有原因"
api s7_term_noreason PATCH "/business/erp/contract/$C_SAL/status" "$T_ADMIN" '{"target_status":"TERMINATED"}'
check_http "负例：终止未填原因" "$HTTP" "400"
say "     结果：code=$CODE msg=$(msg)"
api s7_term PATCH "/business/erp/contract/$C_SAL/status" "$T_ADMIN" '{"target_status":"TERMINATED","reason":"CTR-01 演练：客户要求提前终止框架合同"}'
if [ "$CODE" = "0" ]; then ok "终止成功（带原因）"; else bad "终止失败 http=$HTTP code=$CODE msg=$(msg)"; fi
check "终止后状态" "$(P "select status from biz_contract where id='$C_SAL'")" "TERMINATED"
check "终止原因落 biz_status_log.reason" "$(P "select reason from biz_status_log where entity_type='CONTRACT' and entity_id='$C_SAL' and to_status='TERMINATED'")" "CTR-01 演练：客户要求提前终止框架合同"
REMARK_OK=$(P "select case when remark like '%【终止】CTR-01 演练：客户要求提前终止框架合同%' and remark like '%草稿阶段改金额%' then 'yes' else 'no' end from biz_contract where id='$C_SAL'")
check "终止原因回写 remark（V-18：追加且保留原备注）" "$REMARK_OK" "yes"
api s7_so_after POST /business/erp/sales-order "$T_ADMIN" "{\"customer_id\":\"$CUST\",\"contract_id\":\"$C_SAL\",\"order_date\":\"2026-11-09\",\"remark\":\"CTR-01 终止后再挂销售单（应拒）\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":1,\"price\":100,\"tax_rate\":13}]}"
check_http "负例：终止后再关联新销售单" "$HTTP" "400"
check_code "负例：终止后挂销售单错误码" "$CODE" "66014"

snap s7_final \
  "select 'contract='||contract_no||'|'||contract_type||'|'||status||'|amt='||amount||'|end='||coalesce(end_date::text,'-') from biz_contract order by contract_no" \
  "select 'log='||entity_id||'|'||from_status||'->'||to_status||'|'||coalesce(reason,'-') from biz_status_log order by created_at" \
  "select 'po='||order_no||'|'||status||'|'||total_with_tax from biz_purchase_order order by created_at" \
  "select 'so='||so_no||'|'||status||'|'||total_with_tax from biz_sales_order order by created_at"

# ---------- S8 收口断言 ----------
say; say "S8 收口：合同链不产生任何凭证（合同不记账）"
check "凭证张数（合同 / 订单都不记账）" "$(P "select count(*) from biz_voucher")" "0"
check "库存流水条数（合同链不碰库存）" "$(P "select count(*) from biz_inventory_txn")" "0"

# ---------- S9 汇总 ----------
say
say "===== 汇总：PASS=$PASS FAIL=$FAIL ====="
if [ "$FAIL" -gt 0 ]; then
  say "失败项："
  for f in "${FAILED[@]}"; do say "  - $f"; done
fi
say "raw 目录：$RAW（$(ls "$RAW" | wc -l) 个文件）"
[ "$FAIL" -eq 0 ]
