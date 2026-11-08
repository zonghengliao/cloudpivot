#!/usr/bin/env bash
# CTR-02 合同域缺口收口 —— 隔离实例实测（:18228 / 库 cloudpivot_ctr02）
# 覆盖：F2 变更留痕、F6 已执行/剩余、F3 未关闭单据口径（不硬拦）
set -uo pipefail
BASE=http://127.0.0.1:18228
DB=cloudpivot_ctr02
PSQL="psql postgres://alun:alun@127.0.0.1:5432/${DB} -At"
RAW=/opt/cloudpivot/docs/evidence/20261109-ctr02-contract-gaps/raw
mkdir -p "$RAW"
PASS=0; FAIL=0
ok(){ PASS=$((PASS+1)); echo "PASS $*"; }
bad(){ FAIL=$((FAIL+1)); echo "FAIL $*"; }
check(){ if [ "$2" = "$3" ]; then ok "$1 = $3"; else bad "$1 期望 [$3] 实际 [$2]"; fi; }

HTTP=""; BODY=""; CODE=""
api(){ # api <tag> <method> <path> <token> [json]
  local tag=$1 method=$2 path=$3 token=$4 body=${5:-}
  # X-Real-IP：模拟 nginx 覆写真实客户端 IP（生产必带）；直连回环会被 utils::client_ip 归零
  local args=(-s -m 15 -o "$RAW/$tag.json" -w '%{http_code}' -X "$method" "$BASE$path" -H 'X-Real-IP: 203.0.113.9')
  [ -n "$token" ] && args+=(-H "Authorization: Bearer $token")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$RAW/$tag.json" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
}
data(){ printf '%s' "$BODY" | jq -r ".data$1 // empty" 2>/dev/null; }

echo "== S0 登录（种子 admin，首登强制改密；重跑时直接用改后的口令） =="
RUN=$(date +%H%M%S)   # 可重复执行：主数据/合同名带时间戳，避免唯一约束撞车
api s0_login POST /api/system/auth/login "" '{"username":"admin","password":"Admin@Ctr02!2026"}'
if [ "$HTTP" != "200" ]; then
  api s0_login_seed POST /api/system/auth/login "" '{"username":"admin","password":"Admin@123456"}'
  check "种子 admin 登录" "$HTTP" "200"
  T0=$(data '.access_token')
  api s0_pwd PUT /api/system/auth/password "$T0" '{"old_password":"Admin@123456","new_password":"Admin@Ctr02!2026"}'
  check "首登改密" "$CODE" "0"
  api s0_relogin POST /api/system/auth/login "" '{"username":"admin","password":"Admin@Ctr02!2026"}'
fi
check "改密后登录" "$HTTP" "200"
T=$(data '.access_token')
[ -z "$T" ] && { echo "FATAL 拿不到 token"; exit 1; }

echo; echo "== S1 主数据 + 建两张草稿合同（PURCHASE 50000 / 8000） =="
api s1_sup POST /api/business/erp/supplier "$T" "{\"supplier_name\":\"CTR02 演练供应商 $RUN\",\"short_name\":\"CTR02供$RUN\",\"contact_person\":\"王五\",\"contact_phone\":\"13700000011\",\"credit_days\":30,\"credit_limit\":500000,\"remark\":\"CTR-02 变更留痕演练\"}"
SUP=$(data '.id'); check "供应商建档" "$CODE" "0"
mk_contract(){ # mk_contract <tag> <name> <amount>
  api "$1" POST /api/business/erp/contract "$T" "{\"name\":\"$2\",\"contract_type\":\"PURCHASE\",\"party_b_id\":\"$SUP\",\"amount\":\"$3\",\"sign_date\":\"2026-01-01\",\"start_date\":\"2026-01-02\",\"end_date\":\"2026-12-31\",\"remark\":\"CTR-02 初始备注\"}"
  data '.id'
}
C_MAIN=$(mk_contract s1_c1 "CTR02 变更留痕合同 $RUN" 50000); check "建合同 A(50000) 草稿" "$CODE" "0"
C_IDLE=$(mk_contract s1_c2 "CTR02 无订单合同 $RUN" 8000);   check "建合同 B(8000) 草稿" "$CODE" "0"
echo "   C_MAIN=$C_MAIN  C_IDLE=$C_IDLE"

echo; echo "== S2 F6：无订单时 已执行 0 / 剩余=合同额 / 未关闭 0 =="
api s2_detail GET "/api/business/erp/contract/$C_IDLE" "$T"
check "详情 HTTP" "$HTTP" "200"
check "无订单 committed_amount" "$(data '.committed_amount')" "0"
check "无订单 remaining_amount（NUMERIC(18,4) 原样下发）" "$(data '.remaining_amount')" "8000.0000"
check "无订单 open_order_count" "$(data '.open_order_count')" "0"

echo; echo "== S3 F2：草稿改金额 50000 → 55000（改前值必须留痕） =="
BEFORE_LOG=$($PSQL -c "select count(*) from sys_change_log where resource='biz_contract' and resource_id='$C_MAIN'")
api s3_update PUT "/api/business/erp/contract/$C_MAIN" "$T" '{"amount":"55000","end_date":"2027-06-30","remark":"CTR-02 改后备注：金额与到期日都改了"}'
check "改草稿 HTTP" "$HTTP" "200"
check "改后 amount" "$(data '.amount')" "55000.0000"
check "改后 end_date" "$(data '.end_date')" "2027-06-30"
AFTER_LOG=$($PSQL -c "select count(*) from sys_change_log where resource='biz_contract' and resource_id='$C_MAIN'")
echo "   sys_change_log 行数：改前 $BEFORE_LOG → 改后 $AFTER_LOG"
check "sys_change_log 新增恰好 1 行" "$((AFTER_LOG - BEFORE_LOG))" "1"
$PSQL -c "select 'row: operator='||coalesce(operator_name,'-')||' action='||action||'('||action_label||') changed='||array_to_string(changed_fields,',')||' ip='||coalesce(client_ip::text,'-')||' at='||to_char(created_at,'YYYY-MM-DD HH24:MI:SS') from sys_change_log where resource='biz_contract' and resource_id='$C_MAIN'" | tee "$RAW/s3_log_row.txt"
$PSQL -c "select diff::text from sys_change_log where resource='biz_contract' and resource_id='$C_MAIN'" > "$RAW/s3_diff.json"
check "diff.amount.b（改前值）" "$(jq -r '.fields.amount.b' "$RAW/s3_diff.json")" "50000.0000"
check "diff.amount.a（改后值）" "$(jq -r '.fields.amount.a' "$RAW/s3_diff.json")" "55000.0000"
check "diff.end_date.b" "$(jq -r '.fields.end_date.b' "$RAW/s3_diff.json")" "2026-12-31"
check "diff.end_date.a" "$(jq -r '.fields.end_date.a' "$RAW/s3_diff.json")" "2027-06-30"
check "diff.remark.b" "$(jq -r '.fields.remark.b' "$RAW/s3_diff.json")" "CTR-02 初始备注"
check "changed_fields 含 amount/end_date/remark" "$(jq -r '.fields|keys|sort|join(",")' "$RAW/s3_diff.json")" "amount,end_date,remark"

echo; echo "== S4 F2 反例：原样回传（同值）不得写假日志 =="
api s4_noop PUT "/api/business/erp/contract/$C_MAIN" "$T" '{"amount":"55000","end_date":"2027-06-30","remark":"CTR-02 改后备注：金额与到期日都改了"}'
check "同值改单 HTTP" "$HTTP" "200"
NOOP_LOG=$($PSQL -c "select count(*) from sys_change_log where resource='biz_contract' and resource_id='$C_MAIN'")
check "同值改单后仍是 1 行（Skipped，不写空日志）" "$NOOP_LOG" "1"

echo; echo "== S5 F6/F3：挂 2 张未关闭采购单（33900 + 66100） =="
C_NO=$($PSQL -c "select contract_no from biz_contract where id='$C_MAIN'")
po_ins(){ # po_ins <单号> <价税合计> <不含税> <税额> <状态>
  $PSQL -c "insert into biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status, purchase_type, contract_id, contract_no_snapshot, total_qty, total_amount, total_tax, total_with_tax) values (gen_random_uuid(),'$1','$SUP',date '2026-03-01',gen_random_uuid(),'$5','CONTRACT','$C_MAIN','$C_NO',1,$3,$4,$2)" >/dev/null
}
po_ins "PO-CTR02-$RUN-1" 33900 30000 3900 DRAFT
po_ins "PO-CTR02-$RUN-2" 66100 66100 0 APPROVED
api s5_detail GET "/api/business/erp/contract/$C_MAIN" "$T"
check "已执行金额 = 33900+66100" "$(data '.committed_amount')" "100000.0000"
check "剩余额度 = 55000-100000 = -45000（超签，不夹 0）" "$(data '.remaining_amount')" "-45000.0000"
check "未关闭单据数" "$(data '.open_order_count')" "2"
check "未关闭单号" "$(printf '%s' "$BODY" | jq -r '.data.open_order_nos|join(",")')" "PO-CTR02-$RUN-1,PO-CTR02-$RUN-2"

echo; echo "== S6 F3：ACTIVE 状态下带 2 张未关闭单归档 —— 仍放行（ARCH-2 §4.1 E4 不硬拦） =="
$PSQL -c "update biz_contract set status='ACTIVE' where id='$C_MAIN'" >/dev/null
api s6_archive PATCH "/api/business/erp/contract/$C_MAIN/status" "$T" '{"target_status":"COMPLETED"}'
check "归档 HTTP" "$HTTP" "200"
check "归档后状态" "$(data '.status')" "COMPLETED"
api s6_detail GET "/api/business/erp/contract/$C_MAIN" "$T"
check "归档后未关闭单据数（口径可查）" "$(data '.open_order_count')" "2"
check "归档后已执行金额仍可查" "$(data '.committed_amount')" "100000.0000"
$PSQL -c "select 'status_log: '||from_status||'->'||to_status||' at '||to_char(created_at,'YYYY-MM-DD HH24:MI:SS') from biz_status_log where entity_type='CONTRACT' and entity_id='$C_MAIN' order by created_at" | tee "$RAW/s6_status_log.txt"
# 归档后不能再挂新单（CTR-01 §1.5 已实测 400/66014；本卡只验状态变成 COMPLETED 后
# 「未关闭单据数」这个口径仍然可查，不复测下单守卫）

echo; echo "== S7 合同明细的变更留痕（改后备注保留 + 状态日志不受影响） =="
$PSQL -c "select 'contract: '||contract_name||' amount='||amount||' end_date='||end_date||' status='||status||' remark='||coalesce(remark,'-') from biz_contract where id='$C_MAIN'" | tee "$RAW/s7_contract.txt"
$PSQL -c "select 'status_log_rows='||count(*) from biz_status_log where entity_type='CONTRACT' and entity_id='$C_MAIN'" | tee "$RAW/s7_counts.txt"

echo; echo "===== 断言汇总 PASS=$PASS FAIL=$FAIL ====="
