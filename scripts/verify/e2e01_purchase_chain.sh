#!/usr/bin/env bash
# E2E-01 采购域全流程演练（145 cloudpivot_dev，真后端 :8033 + 真 token curl）
#   bash e2e01_chain.sh step0   前置：审批人账号 / 部门 / 仓库+默认库位
#   bash e2e01_chain.sh step1   ① 供应商档案（建/改/停用）
#   bash e2e01_chain.sh step2   ② 询比价（询价→3 家报价→比价→定标→转采购订单）
# 状态落 /tmp/e2e01/state.sh，证据落 /tmp/e2e01/raw/
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE="${BASE:-http://127.0.0.1:8033/api}"
WORK=/tmp/e2e01; RAW=$WORK/raw; STATE=$WORK/state.sh
mkdir -p "$RAW"
[ -f "$STATE" ] && . "$STATE"
PSQLQ() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off "$@"; }
# 固定夹具：沿用库内既有物料（只读使用，不修改）
MAT1=${MAT1:-01a1b5d2-5061-75c1-9845-fd0a8a41512b}   # DEMO-P101 采购演练物料P1
MAT2=${MAT2:-01a1b5d2-506e-73e0-96e9-0533e4f4e791}   # DEMO-P102 采购演练物料P2
T_ADMIN=${T_ADMIN:-}; T_APP=${T_APP:-}; WH_ID=${WH_ID:-}; LOC_ID=${LOC_ID:-}; DEPT_ID=${DEPT_ID:-}
SUP_A=${SUP_A:-}; SUP_B=${SUP_B:-}; SUP_C=${SUP_C:-}; SUP_D=${SUP_D:-}; APP_USER_ID=${APP_USER_ID:-}
SI1=${SI1:-}; SI1_ITEM=${SI1_ITEM:-}; AP1=${AP1:-}; RT1=${RT1:-}; PO_MERGED=${PO_MERGED:-}; PO_FROM_PIQ1=${PO_FROM_PIQ1:-}; PO_FROM_PIQ2=${PO_FROM_PIQ2:-}
PASS=0; FAIL=0
say()  { printf '%s\n' "$*"; }
ok()   { PASS=$((PASS+1)); say "  ✅ $*"; }
bad()  { FAIL=$((FAIL+1)); say "  ❌ $*"; }
check(){ if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
summary(){ say; say "== PASS=$PASS FAIL=$FAIL"; }

api() { # api <名> <METHOD> <path> <token> [json]
  local name=$1 method=$2 path=$3 token=$4 body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 40 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(-H "Authorization: Bea""rer $token")
  if [ -n "$body" ]; then args+=(-H 'Content-Type: application/json' -d "$body"); fi
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
  printf '%s' "$BODY" | jq -e . >/dev/null 2>&1 || say "  ⚠ 非 JSON($name) http=$HTTP: $(printf '%s' "$BODY" | head -c 200)"
}
data() { printf '%s' "$BODY" | jq -r "$1"; }
save() { printf '%s=%q\n' "$1" "$2" >> "$STATE"; export "$1=$2"; }
login() { # login <user> <pass> <tag>
  api "login_$3" POST /system/auth/login "" "{\"username\":\"$1\",\"password\":\"$2\"}"
  if [ "$HTTP" = 200 ] && [ "$CODE" = 0 ]; then data '.data.access_token'; else
    say "FATAL: 登录失败 $1 http=$HTTP body=$(printf '%s' "$BODY" | head -c 300)" >&2; return 1; fi
}
show() { printf '%s' "$BODY" | jq -c '{code,msg}' 2>/dev/null; }

# 最小 PDF 夹具（附件必传规则用）
mkpdf() {
  local f=$WORK/e2e01-return-slip.pdf
  printf '%%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 4 0 R>>endobj\n4 0 obj<</Length 44>>stream\nBT /F1 12 Tf 20 100 Td (E2E-01 return slip) Tj ET\nendstream endobj\ntrailer<</Root 1 0 R>>\n%%%%EOF\n' > "$f"
  printf '%s' "$f"
}
attach_upload() { # attach_upload <name> <doc_type> <doc_id> <file>
  local name=$1 dtype=$2 did=$3 file=$4 out="$RAW/$1.json"
  HTTP=$(curl -s -m 60 -o "$out" -w '%{http_code}' -X POST "$BASE/business/attachment/upload" \
    -H "Authorization: Bea""rer $T_ADMIN" \
    -F "doc_type=$dtype" -F "doc_id=$did" -F "file=@$file;type=application/pdf")
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
}

# 审批：把某实例下当前登录人的待办全部批掉（多节点流程跑到结束）
approve_instance() { # approve_instance <token> <instance_id> <tag前缀>
  local t=$1 iid=$2 tag=$3 n=0 tid node
  while [ $n -lt 8 ]; do
    api "${tag}_todo$n" GET "/business/flow/task/todo?page=1&page_size=200" "$t"
    [ "$CODE" = 0 ] || { say "  ⚠ $tag 待办查询 code=$CODE"; return 1; }
    tid=$(data ".data.list[]? | select(.instance_id==\"$iid\") | .id" | head -1)
    [ -z "$tid" ] && return 0
    node=$(data ".data.list[]? | select(.id==\"$tid\") | .current_node_name")
    api "${tag}_ap$n" POST "/business/flow/task/$tid/approve" "$t" '{"comment":"E2E-01 演练审批通过"}'
    if [ "$CODE" = 0 ]; then ok "审批通过节点 [$node]"; else bad "审批节点 [$node] 失败 http=$HTTP $(show)"; return 1; fi
    n=$((n+1))
  done
}

flow_state() { # flow_state <instance_id> -> instance status
  PSQLQ -c "select status from biz_flow_instance where id='$1';"
}

# ---- 审批角色收成单成员（照 p22b_credit_branch_e2e.py 的既有做法：临时移出他人绑定，用完恢复）----
ROLE_IDS="20000000-0000-4000-8000-0000000f0001 20000000-0000-4000-8000-0000000f0002 20000000-0000-4000-8000-0000000f0003 20000000-0000-4000-8000-0000000f0004 20000000-0000-4000-8000-0000000f0005 20000000-0000-4000-8000-0000000f0006"
solo() {
  say "== 审批角色收成单成员（DEPT_LEADER / PURCHASE_MANAGER / GM / WAREHOUSE_MANAGER / FINANCE_MANAGER / FINANCE_DIRECTOR）"
  : > "$WORK/role_backup.txt"
  local r uid
  for r in $ROLE_IDS; do
    for uid in $(PSQLQ -c "select user_id from sys_user_role where role_id='$r' and user_id<>'$APP_USER_ID';"); do
      printf '%s|%s\n' "$uid" "$r" >> "$WORK/role_backup.txt"
      PSQLQ -c "delete from sys_user_role where user_id='$uid' and role_id='$r';" >/dev/null
      say "  移出 $uid ← $r"
    done
  done
  say "  备份 $(wc -l < "$WORK/role_backup.txt") 条绑定 -> $WORK/role_backup.txt"
}
unsolo() {
  say "== 恢复被临时移出的角色绑定"
  [ -f "$WORK/role_backup.txt" ] || { say "  无备份"; return 0; }
  local uid r
  while IFS='|' read -r uid r; do
    [ -z "${uid:-}" ] && continue
    PSQLQ -c "insert into sys_user_role (user_id, role_id) values ('$uid','$r') on conflict do nothing;" >/dev/null
    say "  恢复 $uid ← $r"
  done < "$WORK/role_backup.txt"
  mv "$WORK/role_backup.txt" "$WORK/role_backup.done.txt"
}

# ===========================================================================
step0() {
  say "== step0 前置"
  T_ADMIN=$(login admin "$NIK_ADMIN_PASSWORD" admin0) || exit 1
  save T_ADMIN "$T_ADMIN"; ok "admin 登录"
  # 部门
  api s0_dept POST /system/dept "$T_ADMIN" '{"dept_code":"E2E01-DEPT","dept_name":"采购演练部","status":1,"sort_order":910}'
  if [ "$CODE" = 0 ]; then save DEPT_ID "$(data '.data.id')"; ok "部门 E2E01-DEPT $(data '.data.id')";
  else say "  部门已存在或失败 http=$HTTP $(show)"; DEPT_ID=$(PSQLQ -c "select id from sys_dept where dept_code='E2E01-DEPT' and deleted_at is null;"); save DEPT_ID "$DEPT_ID"; fi
  # 审批人（覆盖 po/pr/stock_in/payable/return 全部节点角色）
  api s0_user POST /system/user "$T_ADMIN" "{\"username\":\"e2e01_appr\",\"password\":\"E2e01@Appr123\",\"real_name\":\"E2E01 采购演练审批人\",\"dept_id\":\"$DEPT_ID\",\"status\":1,\"role_ids\":[\"20000000-0000-4000-8000-0000000f0001\",\"20000000-0000-4000-8000-0000000f0002\",\"20000000-0000-4000-8000-0000000f0003\",\"20000000-0000-4000-8000-0000000f0004\",\"20000000-0000-4000-8000-0000000f0005\",\"20000000-0000-4000-8000-0000000f0006\"],\"remark\":\"E2E-01 演练审批人（DEPT_LEADER/PURCHASE_MANAGER/GM/WAREHOUSE_MANAGER/FINANCE_MANAGER/FINANCE_DIRECTOR）\"}"
  if [ "$CODE" = 0 ]; then APP_USER_ID=$(data '.data.id'); ok "审批人 e2e01_appr 建号 $APP_USER_ID"
  else say "  审批人已存在或失败 http=$HTTP $(show)"; APP_USER_ID=$(PSQLQ -c "select id from sys_user where username='e2e01_appr' and deleted_at is null;")
       api s0_user_pwd PATCH "/system/user/$APP_USER_ID/password" "$T_ADMIN" '{"password":"E2e01@Appr123"}'; [ "$CODE" = 0 ] && ok "审批人密码重置" || bad "审批人密码重置 http=$HTTP $(show)"; fi
  save APP_USER_ID "$APP_USER_ID"
  T_APP=$(login e2e01_appr 'E2e01@Appr123' appr0) || exit 1; save T_APP "$T_APP"; ok "审批人登录"
  # 仓库 + 默认库位
  api s0_wh POST /business/erp/warehouse "$T_ADMIN" '{"warehouse_code":"E2E01-WH","warehouse_name":"E2E-01 采购演练仓","warehouse_type":"NORMAL","level":1,"status":1,"remark":"E2E-01 演练仓"}'
  if [ "$CODE" = 0 ]; then WH_ID=$(data '.data.id'); ok "仓库 E2E01-WH $WH_ID"
  else WH_ID=$(PSQLQ -c "select id from biz_warehouse where warehouse_code='E2E01-WH' and deleted_at is null;"); say "  仓库已存在 http=$HTTP $(show) -> $WH_ID"; fi
  save WH_ID "$WH_ID"
  api s0_loc POST /business/erp/location "$T_ADMIN" "{\"location_code\":\"E2E01-L01\",\"warehouse_id\":\"$WH_ID\",\"location_name\":\"默认库位\",\"location_type\":\"STORAGE\",\"is_default\":true,\"status\":1}"
  if [ "$CODE" = 0 ]; then LOC_ID=$(data '.data.id'); ok "默认库位 E2E01-L01 $LOC_ID"
  else LOC_ID=$(PSQLQ -c "select id from biz_location where location_code='E2E01-L01' and warehouse_id='$WH_ID' and deleted_at is null;"); say "  库位已存在 http=$HTTP $(show) -> $LOC_ID"; fi
  save LOC_ID "$LOC_ID"
  # 记录起始库存（本仓）
  say "  起始库存(本仓): $(PSQLQ -c "select count(*), coalesce(sum(qty),0) from biz_inventory where warehouse_id='$WH_ID';")"
  summary
}

# ===========================================================================
step1() {
  say "== step1 ① 供应商档案（建→改→停用→启用）"
  mk() { # mk <code> <name>  —— 只把 id 打到 stdout
    api "s1_${1}" POST /business/erp/supplier "$T_ADMIN" "{\"supplier_code\":\"$1\",\"supplier_name\":\"$2\",\"contact_person\":\"演练联系人\",\"contact_phone\":\"01000000000\",\"credit_days\":30,\"credit_limit\":500000,\"status\":1,\"remark\":\"E2E-01 演练供应商\"}"
    local id
    if [ "$CODE" = 0 ]; then id=$(data '.data.id'); say "  ✅ 建供应商 $1 -> $id" >&2
    else id=$(PSQLQ -c "select id from biz_supplier where supplier_code='$1' and deleted_at is null;"); say "  · $1 已存在 http=$HTTP $(show) -> $id" >&2; fi
    printf '%s' "$id"
  }
  SUPA=$(mk E2E01-SUPA "E2E-01 供应商甲"); save SUP_A "$SUPA"
  SUPB=$(mk E2E01-SUPB "E2E-01 供应商乙"); save SUP_B "$SUPB"
  SUPC=$(mk E2E01-SUPC "E2E-01 供应商丙"); save SUP_C "$SUPC"
  SUPD=$(mk E2E01-SUPD "E2E-01 供应商丁"); save SUP_D "$SUPD"
  # 改
  api s1_supd_update PUT "/business/erp/supplier/$SUPD" "$T_ADMIN" '{"supplier_name":"E2E-01 供应商丁（改名）","contact_person":"新联系人","credit_days":45}'
  [ "$CODE" = 0 ] && ok "改供应商丁名称/账期" || bad "改供应商丁 http=$HTTP $(show)"
  check "DB supplier_name" "$(PSQLQ -c "select supplier_name||'/'||credit_days from biz_supplier where id='$SUPD';")" "E2E-01 供应商丁（改名）/45"
  # 停用
  api s1_supd_disable PUT "/business/erp/supplier/$SUPD" "$T_ADMIN" '{"status":0}'
  [ "$CODE" = 0 ] && ok "停用供应商丁" || bad "停用供应商丁 http=$HTTP $(show)"
  check "DB status(停用)" "$(PSQLQ -c "select status from biz_supplier where id='$SUPD';")" "0"
  # 停用后再启用（验证 status 双向）
  api s1_supd_enable PUT "/business/erp/supplier/$SUPD" "$T_ADMIN" '{"status":1}'
  check "DB status(启用)" "$(PSQLQ -c "select status from biz_supplier where id='$SUPD';")" "1"
  summary
}

# ===========================================================================
step2() {
  say "== step2 ② 询比价"
  local M1=$MAT1 M2=$MAT2 body id iid1 iid2
  body="{\"warehouse_id\":\"$WH_ID\",\"remark\":\"E2E-01 询价单\",\"items\":[{\"material_id\":\"$M1\",\"qty\":100},{\"material_id\":\"$M2\",\"qty\":50}]}"
  api s2_create POST /business/erp/price-inquiry "$T_ADMIN" "$body"
  [ "$CODE" = 0 ] || { bad "建询价单 http=$HTTP $(show)"; return; }
  PIQ_ID=$(data '.data.head.id'); save PIQ_ID "$PIQ_ID"
  say "  ✅ 建询价单 $(data '.data.head.inquiry_no') 状态=$(data '.data.head.status') id=$PIQ_ID"
  api s2_detail GET "/business/erp/price-inquiry/$PIQ_ID" "$T_ADMIN"
  iid1=$(data '.data.items[0].id'); iid2=$(data '.data.items[1].id'); save PIQ_I1 "$iid1"; save PIQ_I2 "$iid2"
  ok "询价明细 2 行 ($iid1 / $iid2)"
  # 3 家报价（甲 10/20，乙 9.5/21，丙 11/19 —— 行1 乙中标、行2 丙中标）
  quote() { # quote <tag> <sup_id> <p1> <p2>
    api "s2_quote_$1" POST "/business/erp/price-inquiry/$PIQ_ID/quote" "$T_ADMIN" "{\"supplier_id\":\"$2\",\"items\":[{\"item_id\":\"$iid1\",\"price_excl\":$3,\"tax_rate\":13,\"delivery_days\":7},{\"item_id\":\"$iid2\",\"price_excl\":$4,\"tax_rate\":13,\"delivery_days\":5}]}"
    [ "$CODE" = 0 ] && ok "报价[$1] 行1=$3 行2=$4" || bad "报价[$1] http=$HTTP $(show)"
  }
  quote A "$SUP_A" 10 20
  quote B "$SUP_B" 9.5 21
  quote C "$SUP_C" 11 19
  api s2_detail2 GET "/business/erp/price-inquiry/$PIQ_ID" "$T_ADMIN"
  say "  详情字段: $(data '.data | keys | tostring')"
  say "  比价表: $(data '.data.compare // .data.compare_rows // .data.quotes | tostring' | head -c 500)"
  check "报价行数(DB)" "$(PSQLQ -c "select count(*) from biz_price_quote q join biz_price_inquiry_item i on i.id=q.item_id where i.inquiry_id='$PIQ_ID';")" "6"
  check "询价状态" "$(data '.data.head.status')" "QUOTING"
  # 定标
  api s2_award POST "/business/erp/price-inquiry/$PIQ_ID/award" "$T_ADMIN" "{\"items\":[{\"item_id\":\"$iid1\",\"supplier_id\":\"$SUP_B\"},{\"item_id\":\"$iid2\",\"supplier_id\":\"$SUP_C\"}]}"
  [ "$CODE" = 0 ] && ok "定标: 行1→乙(9.5) 行2→丙(19) 状态 $(data '.data.head.status')" || bad "定标 http=$HTTP $(show)"
  # 转单
  api s2_topo POST "/business/erp/price-inquiry/$PIQ_ID/to-po" "$T_ADMIN" '{}'
  [ "$CODE" = 0 ] && ok "转采购订单" || bad "转单 http=$HTTP $(show)"
  say "  转单返回: $(data '.data | tostring' | head -c 700)"
  save PO_FROM_PIQ1 "$(data '.data.purchase_orders[0].po_id // empty')"
  save PO_FROM_PIQ2 "$(data '.data.purchase_orders[1].po_id // empty')"
  # 报价单价带入校验
  local n pid
  for n in 0 1; do
    pid=$(data ".data.purchase_orders[$n].po_id // empty"); [ -z "$pid" ] && continue
    api "s2_po$n" GET "/business/erp/purchase-order/$pid" "$T_ADMIN"
    say "  PO[$n] $(data '.data.order_no') 供应商=$(data '.data.supplier_name') 明细=$(data '.data.items | map(.material_code+":"+(.qty|tostring)+"@"+(.price_excl|tostring)+"/tax"+(.tax_rate|tostring)) | join(",")') 状态=$(data '.data.status') 价税合计=$(data '.data.total_with_tax')"
  done
  check "询价单已转单(CONVERTED)" "$(PSQLQ -c "select status from biz_price_inquiry where id='$PIQ_ID';")" "CONVERTED"
  check "PO1 中标单价(乙,9.5)" "$(PSQLQ -c "select price from biz_purchase_order_item where order_id='$PO_FROM_PIQ1';")" "9.500000"
  check "PO2 中标单价(丙,19)" "$(PSQLQ -c "select price from biz_purchase_order_item where order_id='$PO_FROM_PIQ2';")" "19.000000"
  summary
}

# ===========================================================================
step3() {
  say "== step3 ③ 采购申请（建→提交→审批→合并转单）"
  mkreq() { # mkreq <tag> <qty>
    api "s3_${1}_create" POST /business/erp/purchase-requisition "$T_ADMIN" "{\"dept_id\":\"$DEPT_ID\",\"warehouse_id\":\"$WH_ID\",\"demand_date\":\"2026-12-20\",\"remark\":\"E2E-01 采购申请$1\",\"items\":[{\"material_id\":\"$MAT1\",\"qty\":$2,\"est_price\":5,\"tax_rate\":13,\"suggest_supplier_id\":\"$SUP_A\"}]}"
    if [ "$CODE" != 0 ]; then say "  ❌ 建申请$1 http=$HTTP $(show)" >&2; printf 'ERR'; return; fi
    data '.data.requisition.id // .data.id'
  }
  RQ1=$(mkreq A 10); save PRQ1 "$RQ1"
  RQ2=$(mkreq B 20); save PRQ2 "$RQ2"
  say "  申请单 A=$RQ1 B=$RQ2"
  api s3_rq1_detail GET "/business/erp/purchase-requisition/$RQ1" "$T_ADMIN"
  say "  申请单A $(data '.data.requisition.req_no') 状态=$(data '.data.requisition.status')"
  api s3_rq2_detail GET "/business/erp/purchase-requisition/$RQ2" "$T_ADMIN"
  say "  申请单B $(data '.data.requisition.req_no') 状态=$(data '.data.requisition.status')"
  api s3_sub1 POST "/business/erp/purchase-requisition/$RQ1/submit" "$T_ADMIN" '{}'
  [ "$CODE" = 0 ] && ok "申请A 提交审批 状态=$(data '.data.requisition.status')" || bad "申请A 提交 http=$HTTP $(show)"
  api s3_sub2 POST "/business/erp/purchase-requisition/$RQ2/submit" "$T_ADMIN" '{}'
  [ "$CODE" = 0 ] && ok "申请B 提交审批 状态=$(data '.data.requisition.status')" || bad "申请B 提交 http=$HTTP $(show)"
  local x rid iid
  for x in 1 2; do
    rid=$(eval echo \$RQ$x)
    iid=$(PSQLQ -c "select approval_instance_id from biz_purchase_requisition where id='$rid';")
    say "  申请$x 实例=$iid"; approve_instance "$T_APP" "$iid" "s3_ap$x"
    check "申请$x 审批后状态" "$(PSQLQ -c "select status from biz_purchase_requisition where id='$rid';")" "APPROVED"
  done
  api s3_topo POST "/business/erp/purchase-requisition/$RQ1/to-po" "$T_ADMIN" "{\"requisition_ids\":[\"$RQ1\",\"$RQ2\"],\"warehouse_id\":\"$WH_ID\",\"supplier_id\":\"$SUP_A\"}"
  [ "$CODE" = 0 ] || bad "合并转单 http=$HTTP $(show)"
  say "  转单返回: $(data '.data | tostring' | head -c 500)"
  PO_MERGED=$(data '.data.requisition.po_id // .data.po_id // .data.id // empty')
  [ -z "$PO_MERGED" ] && PO_MERGED=$(PSQLQ -c "select po_id from biz_purchase_requisition where id='$RQ1';")
  save PO_MERGED "$PO_MERGED"
  check "申请A 已转单(CONVERTED)" "$(PSQLQ -c "select status from biz_purchase_requisition where id='$RQ1';")" "CONVERTED"
  check "申请B 已转单(CONVERTED)" "$(PSQLQ -c "select status from biz_purchase_requisition where id='$RQ2';")" "CONVERTED"
  check "两申请指向同一 PO" "$(PSQLQ -c "select count(distinct po_id) from biz_purchase_requisition where id in ('$RQ1','$RQ2');")" "1"
  say "  合并 PO 明细: $(PSQLQ -c "select string_agg(m.material_code||':'||i.qty||'@'||i.price,',') from biz_purchase_order_item i join biz_material m on m.id=i.material_id where i.order_id='$PO_MERGED';")"
  check "合并数量 = 30" "$(PSQLQ -c "select sum(qty)::numeric(20,0)::text from biz_purchase_order_item where order_id='$PO_MERGED';")" "30"
  summary
}

# ===========================================================================
step4() {
  say "== step4 ④ 采购订单（提交→审批→查询）+ ⑧ 采购明细"
  local PO iid x
  for x in "$PO_MERGED" "$PO_FROM_PIQ1" "$PO_FROM_PIQ2"; do
    [ -z "$x" ] && continue
    PO=$x
    api "s4_po_$PO" GET "/business/erp/purchase-order/$PO" "$T_ADMIN"
    say "  PO $(data '.data.order_no') 状态=$(data '.data.status') 价税合计=$(data '.data.total_with_tax') 供应商=$(data '.data.supplier_name')"
    api "s4_sub_$PO" POST "/business/erp/purchase-order/$PO/submit-approval" "$T_ADMIN" '{}'
    [ "$CODE" = 0 ] && ok "$(data '.data.order_no') 提交审批 → $(data '.data.status')" || bad "提交审批 http=$HTTP $(show)"
    iid=$(PSQLQ -c "select approval_instance_id from biz_purchase_order where id='$PO';")
    approve_instance "$T_APP" "$iid" "s4_ap_${PO:0:8}"
    check "PO 审批后状态" "$(PSQLQ -c "select status from biz_purchase_order where id='$PO';")" "APPROVED"
    check "审批实例状态" "$(flow_state "$iid")" "APPROVED"
  done
  api s4_items GET "/business/erp/purchase-order-item/page?page=1&page_size=10" "$T_ADMIN"
  [ "$CODE" = 0 ] && ok "采购明细分页 total=$(data '.data.total')" || bad "采购明细分页 http=$HTTP $(show)"
  say "  明细首行: $(data '.data.list[0] | tostring' | head -c 400)"
  summary
}

# ===========================================================================
step5() {
  say "== step5 ⑤ 收货入库（带明细→提交过账审批→过账→六本账）"
  local PO=$PO_FROM_PIQ1 pi qty=60
  pi=$(PSQLQ -c "select id from biz_purchase_order_item where order_id='$PO' limit 1;")
  say "  PO=$(PSQLQ -c "select order_no from biz_purchase_order where id='$PO';") 明细行=$pi 本次收 60"
  api s5_in1 POST /business/erp/stock-in "$T_ADMIN" "{\"source_order_id\":\"$PO\",\"warehouse_id\":\"$WH_ID\",\"in_date\":\"2026-11-09\",\"invoice_received\":true,\"remark\":\"E2E-01 收货单1\",\"items\":[{\"purchase_order_item_id\":\"$pi\",\"material_id\":\"$MAT1\",\"qty\":$qty,\"price_excl\":9.5,\"tax_rate\":13}]}"
  [ "$CODE" = 0 ] || bad "建收货单1 http=$HTTP $(show)"
  SI1=$(data '.data.header.id // .data.id // empty')
  [ -z "$SI1" ] && SI1=$(PSQLQ -c "select id from biz_stock_in where remark='E2E-01 收货单1' order by created_at desc limit 1;")
  save SI1 "$SI1"; say "  收货单1 = $SI1 状态=$(PSQLQ -c "select status from biz_stock_in where id='$SI1';")"
  api s5_in1_detail GET "/business/erp/stock-in/$SI1" "$T_ADMIN"
  say "  单据 $(data '.data.header.in_no') 金额=$(data '.data.header.total_with_tax')"
  save SI1_ITEM "$(data '.data.items[0].id')"
  api s5_post1 POST "/business/erp/stock-in/$SI1/submit-post" "$T_ADMIN" '{}'
  say "  submit-post http=$HTTP $(show) 状态=$(PSQLQ -c "select status from biz_stock_in where id='$SI1';")"
  local st=$(PSQLQ -c "select status from biz_stock_in where id='$SI1';")
  if [ "$st" = "PENDING_POST" ]; then
    local iid=$(PSQLQ -c "select approval_instance_id from biz_stock_in where id='$SI1';")
    approve_instance "$T_APP" "$iid" "s5_ap1"
  fi
  check "收货单1 已过账(POSTED)" "$(PSQLQ -c "select status from biz_stock_in where id='$SI1';")" "POSTED"
  say "  库存: $(PSQLQ -c "select i.qty from biz_inventory i where i.material_id='$MAT1' and i.warehouse_id='$WH_ID';")"
  say "  流水: $(PSQLQ -c "select txn_type||'/'||qty||'/'||biz_type from biz_inventory_txn where material_id='$MAT1' and warehouse_id='$WH_ID' order by created_at desc limit 3;")"
  say "  凭证: $(PSQLQ -c "select voucher_no||' '||summary from biz_voucher where source_id='$SI1';")"
  say "  应付: $(PSQLQ -c "select payable_no||' amt='||amount||' unpaid='||unpaid_amount||' st='||status from biz_payable where source_id='$SI1' and deleted_at is null;")"
  save AP1 "$(PSQLQ -c "select id from biz_payable where source_id='$SI1' and deleted_at is null;")"
  summary
}

# ===========================================================================
step6() {
  say "== step6 ⑥ 应付账款（自动生成→付款申请→审批→付款）"
  local AP=$AP1 iid
  say "  应付单: $(PSQLQ -c "select payable_no||' amt='||amount||' unpaid='||unpaid_amount||' st='||status from biz_payable where id='$AP';")"
  api s6_info GET "/business/erp/payable/$AP/payment-info" "$T_ADMIN"
  say "  可付信息: $(data '.data.payable | tostring' | head -c 400)"
  api s6_pay1 POST "/business/erp/payable/$AP/pay" "$T_ADMIN" '{"amount":300,"pay_method":"银行转账","remark":"E2E-01 部分付款 300"}'
  [ "$CODE" = 0 ] && ok "付款申请300 outcome=$(data '.data.outcome') msg=$(data '.data.message')" || bad "付款申请 http=$HTTP $(show)"
  iid=$(data '.data.approval_instance_id // empty')
  [ -z "$iid" ] && iid=$(PSQLQ -c "select approval_instance_id from biz_payment where payable_id='$AP' order by created_at desc limit 1;")
  if [ -n "$iid" ] && [ "$iid" != "null" ]; then approve_instance "$T_APP" "$iid" "s6_ap1"; fi
  say "  付300后: $(PSQLQ -c "select 'paid='||paid_amount||' unpaid='||unpaid_amount||' st='||status from biz_payable where id='$AP';")"
  check "部分付款后状态" "$(PSQLQ -c "select status from biz_payable where id='$AP';")" "PARTIAL_PAID"
  local rest=$(PSQLQ -c "select unpaid_amount from biz_payable where id='$AP';")
  api s6_pay2 POST "/business/erp/payable/$AP/pay" "$T_ADMIN" "{\"amount\":$rest,\"pay_method\":\"银行转账\",\"remark\":\"E2E-01 付清余额\"}"
  [ "$CODE" = 0 ] && ok "付款申请余额 $rest outcome=$(data '.data.outcome')" || bad "付款余额 http=$HTTP $(show)"
  iid=$(data '.data.approval_instance_id // empty')
  [ -z "$iid" ] && iid=$(PSQLQ -c "select approval_instance_id from biz_payment where payable_id='$AP' order by created_at desc limit 1;")
  if [ -n "$iid" ] && [ "$iid" != "null" ]; then approve_instance "$T_APP" "$iid" "s6_ap2"; fi
  say "  付清后: $(PSQLQ -c "select 'paid='||paid_amount||' unpaid='||unpaid_amount||' st='||status from biz_payable where id='$AP';")"
  check "付清后余额归零" "$(PSQLQ -c "select unpaid_amount from biz_payable where id='$AP';")" "0.0000"
  check "付清后状态已付款" "$(PSQLQ -c "select status from biz_payable where id='$AP';")" "SETTLED"
  say "  付款记录: $(PSQLQ -c "select string_agg(payment_no||':'||amount,',') from biz_payment where payable_id='$AP';")"
  say "  付款凭证: $(PSQLQ -c "select string_agg(v.voucher_no||' '||v.summary,' | ') from biz_voucher v where v.source_id in (select id from biz_payment where payable_id='$AP');")"
  summary
}

# ===========================================================================
step7() {
  say "== step7 ⑦ 采购退货（选入库单→退货→审批→库存回冲+红字应付+凭证+防超退）"
  local SI=$SI1 AP=$AP1
  api s7_source GET "/business/erp/purchase-return/source/$SI" "$T_ADMIN"
  [ "$CODE" = 0 ] && ok "可退明细: $(data '.data | tostring' | head -c 400)" || bad "可退明细 http=$HTTP $(show)"
  local itm=$(data '.data.items[0].id // .data.list[0].id // empty')
  [ -z "$itm" ] && itm=$SI1_ITEM
  say "  退货行=$itm 数量 10"
  if [ -n "$RT1" ] && [ "$(PSQLQ -c "select status from biz_purchase_return where id='$RT1';" 2>/dev/null)" = "DRAFT" ]; then
    say "  复用已有退货单 $RT1（DRAFT）"
  else
    api s7_create POST /business/erp/purchase-return "$T_ADMIN" "{\"stock_in_id\":\"$SI\",\"return_date\":\"2026-11-09\",\"reason\":\"E2E-01 演练退货：质量不符\",\"remark\":\"E2E-01 退货单\",\"items\":[{\"stock_in_item_id\":\"$itm\",\"qty\":10}]}"
    [ "$CODE" = 0 ] || bad "建退货单 http=$HTTP $(show)"
    say "  退货返回: $(data '.data | tostring' | head -c 400)"
    RT1=$(data '.data.header.id // .data.purchase_return.id // .data.id // empty')
    [ -z "$RT1" ] && RT1=$(PSQLQ -c "select id from biz_purchase_return where remark like 'E2E-01 退货单%' order by created_at desc limit 1;")
    save RT1 "$RT1"
  fi
  say "  退货单=$RT1 状态=$(PSQLQ -c "select status from biz_purchase_return where id='$RT1';")"
  # 附件必传规则（V0347：PURCHASE_RETURN / SUBMIT / 1 个 pdf|jpg|png）—— 走真实上传接口
  local pdf=$(mkpdf)
  attach_upload s7_attach PURCHASE_RETURN "$RT1" "$pdf"
  [ "$CODE" = 0 ] && ok "上传退货单附件 $(data '.data.file_name // .data.id // ""')" || bad "上传附件 http=$HTTP $(show)"
  api s7_submit POST "/business/erp/purchase-return/$RT1/submit" "$T_ADMIN" '{}'
  [ "$CODE" = 0 ] && ok "退货提交审批 状态=$(PSQLQ -c "select status from biz_purchase_return where id='$RT1';")" || bad "退货提交 http=$HTTP $(show)"
  local iid=$(PSQLQ -c "select approval_instance_id from biz_purchase_return where id='$RT1';")
  approve_instance "$T_APP" "$iid" "s7_ap"
  check "退货单审批后状态" "$(PSQLQ -c "select status from biz_purchase_return where id='$RT1';")" "APPROVED"
  api s7_over POST /business/erp/purchase-return "$T_ADMIN" "{\"stock_in_id\":\"$SI\",\"return_date\":\"2026-11-09\",\"reason\":\"E2E-01 防超退验证\",\"items\":[{\"stock_in_item_id\":\"$itm\",\"qty\":51}]}"
  if [ "$CODE" != 0 ]; then ok "防超退拦截：退 51 被拒 http=$HTTP $(show)"; else bad "防超退失效：退 51 竟然建单成功"; fi
  api s7_confirm POST "/business/erp/purchase-return/$RT1/confirm" "$T_ADMIN" '{"reason":"E2E-01 仓库已收退货"}'
  [ "$CODE" = 0 ] && ok "确认退货 → 状态 $(PSQLQ -c "select status from biz_purchase_return where id='$RT1';")" || bad "确认退货 http=$HTTP $(show)"
  say "  库存(应 50): $(PSQLQ -c "select qty from biz_inventory where material_id='$MAT1' and warehouse_id='$WH_ID';")"
  say "  退货流水: $(PSQLQ -c "select txn_type||'/'||qty||'/'||biz_type from biz_inventory_txn where material_id='$MAT1' and warehouse_id='$WH_ID' order by created_at desc limit 2;")"
  say "  冲销凭证: $(PSQLQ -c "select voucher_no||' '||summary from biz_voucher where source_id='$RT1';")"
  say "  应付联动: $(PSQLQ -c "select payable_no||' amt='||amount||' paid='||paid_amount||' unpaid='||unpaid_amount||' st='||status from biz_payable where id='$AP';")"
  summary
}

# ===========================================================================
# 把「会签缺人」卡住的实例撤回（只撤回本卡自己造的、状态 SUBMITTED 的采购申请实例）
withdraw_stuck() {
  say "== 撤回本卡被会签卡住的采购申请实例"
  local rid iid
  for rid in $(PSQLQ -c "select id from biz_purchase_requisition where remark like 'E2E-01%' and status='SUBMITTED' and deleted_at is null;"); do
    iid=$(PSQLQ -c "select approval_instance_id from biz_purchase_requisition where id='$rid';")
    api "wd_$rid" POST "/business/flow/instance/$iid/withdraw" "$T_ADMIN" '{"reason":"E2E-01 演练：会签缺人，撤回后换单成员重跑"}'
    say "  撤回实例 $iid http=$HTTP $(show) 单据状态=$(PSQLQ -c "select status from biz_purchase_requisition where id='$rid';")"
  done
}

# ===========================================================================
step8() {
  say "== step8 免审过账 / 审批过账 / BUG-01 死单回归"
  local PO2=$PO_FROM_PIQ2 p2 pi2
  pi2=$(PSQLQ -c "select id from biz_purchase_order_item where order_id='$PO2' limit 1;")
  say "-- 8a 收货单2（PO-055 丙 50@19 价税 1073.50 ≥ 1000 → 走审批）"
  api s8_in2 POST /business/erp/stock-in "$T_ADMIN" "{\"source_order_id\":\"$PO2\",\"warehouse_id\":\"$WH_ID\",\"in_date\":\"2026-11-09\",\"invoice_received\":true,\"remark\":\"E2E-01 收货单2\",\"items\":[{\"purchase_order_item_id\":\"$pi2\",\"material_id\":\"$MAT2\",\"qty\":50,\"price_excl\":19,\"tax_rate\":13}]}"
  [ "$CODE" = 0 ] || bad "建收货单2 http=$HTTP $(show)"
  SI2=$(data '.data.header.id // empty')
  [ -z "$SI2" ] && SI2=$(PSQLQ -c "select id from biz_stock_in where remark='E2E-01 收货单2' order by created_at desc limit 1;")
  save SI2 "$SI2"
  say "  收货单2=$SI2 价税合计=$(PSQLQ -c "select total_with_tax from biz_stock_in where id='$SI2';")"
  api s8_post2 POST "/business/erp/stock-in/$SI2/submit-post" "$T_ADMIN" '{}'
  check "8a 提交后状态（应待审批）" "$(PSQLQ -c "select status from biz_stock_in where id='$SI2';")" "PENDING_POST"
  local iid=$(PSQLQ -c "select approval_instance_id from biz_stock_in where id='$SI2';")
  approve_instance "$T_APP" "$iid" "s8_ap2"
  check "8a 审批后过账(POSTED)" "$(PSQLQ -c "select status from biz_stock_in where id='$SI2';")" "POSTED"
  say "  应付2: $(PSQLQ -c "select payable_no||' amt='||amount from biz_payable where source_id='$SI2' and deleted_at is null;")"
  save AP2 "$(PSQLQ -c "select id from biz_payable where source_id='$SI2' and deleted_at is null;")"

  say "-- 8b BUG-01：采购单已收满 → 免审过账必须打回 DRAFT（不能留死单）"
  local PO1=$PO_FROM_PIQ1 pi1
  pi1=$(PSQLQ -c "select id from biz_purchase_order_item where order_id='$PO1' limit 1;")
  api s8_inA POST /business/erp/stock-in "$T_ADMIN" "{\"source_order_id\":\"$PO1\",\"warehouse_id\":\"$WH_ID\",\"in_date\":\"2026-11-09\",\"invoice_received\":true,\"remark\":\"E2E-01 收货单A(收满前)\",\"items\":[{\"purchase_order_item_id\":\"$pi1\",\"material_id\":\"$MAT1\",\"qty\":40,\"price_excl\":9.5,\"tax_rate\":13}]}"
  SIA=$(data '.data.header.id // empty'); [ -z "$SIA" ] && SIA=$(PSQLQ -c "select id from biz_stock_in where remark='E2E-01 收货单A(收满前)' order by created_at desc limit 1;")
  api s8_inB POST /business/erp/stock-in "$T_ADMIN" "{\"source_order_id\":\"$PO1\",\"warehouse_id\":\"$WH_ID\",\"in_date\":\"2026-11-09\",\"invoice_received\":true,\"remark\":\"E2E-01 收货单B(会超收)\",\"items\":[{\"purchase_order_item_id\":\"$pi1\",\"material_id\":\"$MAT1\",\"qty\":40,\"price_excl\":9.5,\"tax_rate\":13}]}"
  SIB=$(data '.data.header.id // empty'); [ -z "$SIB" ] && SIB=$(PSQLQ -c "select id from biz_stock_in where remark='E2E-01 收货单B(会超收)' order by created_at desc limit 1;")
  save SIA "$SIA"; save SIB "$SIB"
  say "  A=$SIA B=$SIB"
  api s8_postA POST "/business/erp/stock-in/$SIA/submit-post" "$T_ADMIN" '{}'
  say "  A 过账 http=$HTTP $(show) 状态=$(PSQLQ -c "select status from biz_stock_in where id='$SIA';")"
  check "8b A 单已过账" "$(PSQLQ -c "select status from biz_stock_in where id='$SIA';")" "POSTED"
  say "  采购单状态: $(PSQLQ -c "select status from biz_purchase_order where id='$PO1';")（60+40-退货10=90/100 → 只剩 10）"
  api s8_postB POST "/business/erp/stock-in/$SIB/submit-post" "$T_ADMIN" '{}'
  say "  B 过账返回 http=$HTTP $(show)"
  if [ "$HTTP" -ge 400 ] 2>/dev/null; then ok "8b B 单过账被正确拒绝（http=$HTTP）"; else bad "8b B 单过账居然成功"; fi
  check "8b B 单被打回 DRAFT（不是死单 PENDING_POST）" "$(PSQLQ -c "select status from biz_stock_in where id='$SIB';")" "DRAFT"
  say "  状态日志: $(PSQLQ -c "select coalesce(string_agg(coalesce(from_status,'-')||'→'||to_status||':'||coalesce(left(reason,70),''),' | '),'(空)') from biz_status_log where entity_id='$SIB';")"
  say "  B 单应付行数(应 0): $(PSQLQ -c "select count(*) from biz_payable where source_id='$SIB' and deleted_at is null;")"

  say "-- 8c 采购单已 RECEIVED 后再收 → 应被拦"
  api s8_inC POST /business/erp/stock-in "$T_ADMIN" "{\"source_order_id\":\"$PO2\",\"warehouse_id\":\"$WH_ID\",\"in_date\":\"2026-11-09\",\"invoice_received\":true,\"remark\":\"E2E-01 收货单C(收满后再收)\",\"items\":[{\"purchase_order_item_id\":\"$pi2\",\"material_id\":\"$MAT2\",\"qty\":10,\"price_excl\":19,\"tax_rate\":13}]}"
  if [ "$CODE" != 0 ]; then ok "8c 再收被拦 http=$HTTP $(show)"; else bad "8c 收满后仍能建收货单"; fi
  say "-- 8d BUG-02 重新过账接口冒烟（非 PENDING_POST 应 409）"
  api s8_repost POST "/business/erp/stock-in/$SIA/repost" "$T_ADMIN" '{}'
  if [ "$CODE" != 0 ]; then ok "8d repost 对已过账单据返回 http=$HTTP $(show)"; else bad "8d repost 对已过账单据竟然成功"; fi
  summary
}

# 只重跑 8b/8c/8d（8a 已落库，避免重复建单）
step8bc() {
  say "== step8bc 复跑 BUG-01 / 收满再收 / repost 冒烟（复用已建单据）"
  local PO1=$PO_FROM_PIQ1 PO2=$PO_FROM_PIQ2 pi2
  pi2=$(PSQLQ -c "select id from biz_purchase_order_item where order_id='$PO2' limit 1;")
  say "  A=$(PSQLQ -c "select status from biz_stock_in where id='$SIA';") B=$(PSQLQ -c "select status from biz_stock_in where id='$SIB';") PO1=$(PSQLQ -c "select status from biz_purchase_order where id='$PO1';") PO2=$(PSQLQ -c "select status from biz_purchase_order where id='$PO2';")"
  say "  状态日志: $(PSQLQ -c "select coalesce(string_agg(coalesce(from_status,'-')||'→'||to_status||':'||coalesce(left(reason,70),''),' | '),'(空)') from biz_status_log where entity_id='$SIB';")"
  check "8b B 单为 DRAFT（被拒绝后打回，非死单）" "$(PSQLQ -c "select status from biz_stock_in where id='$SIB';")" "DRAFT"
  say "  B 单应付行数(应 0): $(PSQLQ -c "select count(*) from biz_payable where source_id='$SIB' and deleted_at is null;")"
  api s8b_postB POST "/business/erp/stock-in/$SIB/submit-post" "$T_ADMIN" '{}'
  say "  复跑 B 过账 http=$HTTP $(show)"
  check "8b 复跑后仍为 DRAFT" "$(PSQLQ -c "select status from biz_stock_in where id='$SIB';")" "DRAFT"
  say "-- 8c 采购单已 RECEIVED 后再收 → 应被拦"
  api s8c_inC POST /business/erp/stock-in "$T_ADMIN" "{\"source_order_id\":\"$PO2\",\"warehouse_id\":\"$WH_ID\",\"in_date\":\"2026-11-09\",\"invoice_received\":true,\"remark\":\"E2E-01 收货单C(收满后再收)\",\"items\":[{\"purchase_order_item_id\":\"$pi2\",\"material_id\":\"$MAT2\",\"qty\":10,\"price_excl\":19,\"tax_rate\":13}]}"
  if [ "$CODE" != 0 ]; then ok "8c 再收被拦 http=$HTTP $(show)"; else bad "8c 收满后仍能建收货单"; fi
  say "-- 8d BUG-02 重新过账接口冒烟"
  api s8d_repost POST "/business/erp/stock-in/$SIA/repost" "$T_ADMIN" '{}'
  if [ "$CODE" != 0 ]; then ok "8d repost 对已过账单据返回 http=$HTTP $(show)"; else bad "8d repost 竟然成功"; fi
  summary
}

# ===========================================================================
step9() {
  say "== step9 勾稽验证（PO → 入库 → 应付 / 库存 vs 流水 / 凭证平衡）"
  local PO1=$PO_FROM_PIQ1 PO2=$PO_FROM_PIQ2
  say "-- ① 收货单金额 = 应付金额（本仓全部收货单）"
  local bad_ap=$(PSQLQ -c "select count(*) from biz_payable p join biz_stock_in s on s.id=p.source_id
     where p.source_type='STOCK_IN' and p.deleted_at is null and s.deleted_at is null
       and s.warehouse_id='$WH_ID' and p.amount <> s.total_with_tax;")
  check "入库↔应付金额不一致行数" "$bad_ap" "0"
  say "  本仓应付合计: $(PSQLQ -c "select coalesce(sum(p.amount),0) from biz_payable p join biz_stock_in s on s.id=p.source_id where s.warehouse_id='$WH_ID' and p.source_type='STOCK_IN' and p.deleted_at is null;")"
  say "-- ② 采购单行数量 = 已过账收货合计（PO-054 / PO-055）"
  PSQLQ -c "select o.order_no||' | 订单 '||i.qty||' | 已过账收货 '||coalesce((select sum(si.qty) from biz_stock_in_item si join biz_stock_in s on s.id=si.stock_in_id where si.source_item_id=i.id and s.status='POSTED' and s.deleted_at is null),0)||' | 状态 '||o.status
               from biz_purchase_order o join biz_purchase_order_item i on i.order_id=o.id
              where o.id in ('$PO1','$PO2') order by o.order_no;" | sed 's/^/  /'
  say "-- ③ 库存 = 流水累计（按物料+仓库，本仓）"
  PSQLQ -c "with inv as (select material_id, warehouse_id, sum(qty) qty from biz_inventory where warehouse_id='$WH_ID' group by 1,2),
                 txn as (select material_id, warehouse_id, sum(case when txn_type='IN' then qty else -qty end) qty
                           from biz_inventory_txn where warehouse_id='$WH_ID' group by 1,2)
            select m.material_code||' 库存='||coalesce(inv.qty,0)||' 流水='||coalesce(txn.qty,0)||' 差异='||(coalesce(inv.qty,0)-coalesce(txn.qty,0))
              from inv full join txn using (material_id, warehouse_id) join biz_material m on m.id=coalesce(inv.material_id,txn.material_id);" | sed 's/^/  /'
  local diff=$(PSQLQ -c "with inv as (select material_id, warehouse_id, sum(qty) qty from biz_inventory where warehouse_id='$WH_ID' group by 1,2),
                 txn as (select material_id, warehouse_id, sum(case when txn_type='IN' then qty else -qty end) qty
                           from biz_inventory_txn where warehouse_id='$WH_ID' group by 1,2)
            select count(*) from inv full join txn using (material_id, warehouse_id) where coalesce(inv.qty,0)<>coalesce(txn.qty,0);")
  check "库存↔流水差异行数" "$diff" "0"
  say "-- ④ 凭证借贷平衡（本卡单据产生的凭证）"
  local vbad=$(PSQLQ -c "select count(*) from (
       select v.id, sum(e.debit_amount) d, sum(e.credit_amount) c
         from biz_voucher v join biz_voucher_entry e on e.voucher_id=v.id
        where v.source_id in (select id from biz_stock_in where warehouse_id='$WH_ID')
           or v.source_id in (select stock_out_id from biz_purchase_return where id='$RT1')
           or v.source_id in (select id from biz_payment where payable_id='$AP1')
        group by v.id having sum(e.debit_amount) <> sum(e.credit_amount)) t;")
  check "借贷不平凭证数" "$vbad" "0"
  say "-- ⑤ 六本账联动明细"
  PSQLQ -c "select '库存行', count(*) from biz_inventory where warehouse_id='$WH_ID'
   union all select '库存流水', count(*) from biz_inventory_txn where warehouse_id='$WH_ID'
   union all select '凭证(本卡单据)', count(*) from biz_voucher where source_id in (select id from biz_stock_in where warehouse_id='$WH_ID') or source_id in (select stock_out_id from biz_purchase_return where id='$RT1')
   union all select '应付', count(*) from biz_payable p join biz_stock_in s on s.id=p.source_id where s.warehouse_id='$WH_ID' and p.deleted_at is null
   union all select '付款记录', count(*) from biz_payment where payable_id='$AP1'
   union all select '退货单', count(*) from biz_purchase_return where id='$RT1'
   union all select '状态日志(本卡收货单)', count(*) from biz_status_log where entity_id in ('$SI1','$SI2','$SIA','$SIB');" | sed 's/^/  /'
  summary
}

# ===========================================================================
step10() {
  say "== step10 ⑧ 采购明细 / 采购订单列表查询与筛选"
  api s10_items_all GET "/business/erp/purchase-order-item/page?page=1&page_size=5" "$T_ADMIN"
  [ "$CODE" = 0 ] && ok "采购明细分页 total=$(data '.data.total') 本页=$(data '.data.list | length')" || bad "采购明细分页 http=$HTTP $(show)"
  api s10_items_po GET "/business/erp/purchase-order-item/page?page=1&page_size=20&order_id=$PO_MERGED" "$T_ADMIN"
  [ "$CODE" = 0 ] && ok "按订单筛明细 total=$(data '.data.total')" || bad "按订单筛明细 http=$HTTP $(show)"
  api s10_items_mat GET "/business/erp/purchase-order-item/page?page=1&page_size=20&material_id=$MAT1" "$T_ADMIN"
  [ "$CODE" = 0 ] && ok "按物料筛明细 total=$(data '.data.total')" || bad "按物料筛明细 http=$HTTP $(show)"
  api s10_po_kw GET "/business/erp/purchase-order/page?page=1&page_size=10&keyword=E2E-01" "$T_ADMIN"
  [ "$CODE" = 0 ] && ok "采购订单关键词筛 total=$(data '.data.total')" || bad "采购订单关键词 http=$HTTP $(show)"
  api s10_po_status GET "/business/erp/purchase-order/page?page=1&page_size=10&status=RECEIVED" "$T_ADMIN"
  [ "$CODE" = 0 ] && ok "采购订单状态筛(RECEIVED) total=$(data '.data.total')" || bad "采购订单状态筛 http=$HTTP $(show)"
  api s10_po_detail GET "/business/erp/purchase-order/$PO_FROM_PIQ1" "$T_ADMIN"
  say "  PO 详情: $(data '.data.order_no') 状态=$(data '.data.status')"
  summary
}

"$@"
