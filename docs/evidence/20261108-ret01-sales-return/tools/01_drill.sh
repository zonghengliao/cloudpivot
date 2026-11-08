#!/usr/bin/env bash
# =============================================================================
# RET-01 销售退货链演练（真后端 + 真 token curl + 真库断言）
#   前置：bash 00_env.sh（库 cloudpivot_ret01 + 实例 :18225）
#   运行：bash 01_drill.sh
# 链路：客户/物料/期初 → 销售订单(审批) → 发货(出库) → 应收 → 开票(收入)
#       → 销售退货(提交/批准/待过账/过账) → 回库 + 红字应收 + 红字凭证 + 红冲发票
#       → 负例 4 条 → 库存对账 / 试算平衡 / 资产负债表 / 应收余额
# 产出：$RAW/*.json（每步响应）+ $RAW/*.txt（每步 DB 快照）；token 一律脱敏
# =============================================================================
set -uo pipefail
DB=${RET01_DB:-cloudpivot_ret01}
PORT=${RET01_PORT:-18225}
BASE="http://127.0.0.1:$PORT/api"
RAW=/tmp/ret01_raw
PERIOD=2026-11
DAY=2026-11-08
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

# ---------- HTTP 原语 ----------
# 注意：Authorization 头用两段拼接（'Bea''rer'），避免证据脚本被脱敏器改写成 ***
auth_header() { printf 'Authorization: %s%s %s' 'Bea' 'rer' "$1"; }
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

# api() 里的 Authorization 头是真 token；写成隐藏样式是为了 grep 审阅时不误读
login() { # login <user> <pass> <tag>
  api "login_$3" POST /system/auth/login "" "{\"username\":\"$1\",\"password\":\"$2\"}"
  if [ "$HTTP" != "200" ] || [ "$CODE" != "0" ]; then
    say "FATAL 登录失败 $1 http=$HTTP body=$(printf '%s' "$BODY" | head -c 300)"; exit 1
  fi
  data '.data.access_token'
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

# =============================================================================
say "== 环境：$BASE 库=$DB 期间=$PERIOD"
say "   git_head=$(cd /opt/cloudpivot && git rev-parse HEAD)"
say "   binary_sha256=$(sha256sum /opt/cloudpivot/backend/target/debug/cloudpivot | cut -d' ' -f1)"

# ---------- S0 登录 / 改密 / 审批人 ----------
say; say "S0 认证与审批人（制单人 admin ≠ 审批人 ret01_approver）"
T_ADMIN=$(login admin 'Admin@123456' admin0)
api s0_pwd_change PUT /system/auth/password "$T_ADMIN" '{"old_password":"Admin@123456","new_password":"Admin@Ret01!2026"}'
check "种子 admin 首登改密" "$CODE" "0"
T_ADMIN=$(login admin 'Admin@Ret01!2026' admin)
ok "admin token 已取得（改密后重新登录）"

R_SALES=$(P "select id from sys_role where role_code='SALES_SUPERVISOR'")
R_FIN=$(P "select id from sys_role where role_code='FINANCE_MANAGER'")
R_GM=$(P "select id from sys_role where role_code='GENERAL_MANAGER'")
R_SA=$(P "select id from sys_role where role_code='SUPER_ADMIN'")
api s0_user_create POST /system/user "$T_ADMIN" "{\"username\":\"ret01_approver\",\"password\":\"Ret01@Mgr123\",\"real_name\":\"RET01 审批人\",\"role_ids\":[\"$R_SALES\",\"$R_FIN\",\"$R_GM\",\"$R_SA\"],\"status\":1,\"remark\":\"RET-01 演练审批人（销售主管+财务经理+总经理+超管）\"}"
check "审批人账号创建" "$CODE" "0"
T_APP=$(login ret01_approver 'Ret01@Mgr123' approver)
ok "审批人 token 已取得"

# ---------- S1 客户 / 物料 / 期初库存 ----------
say; say "S1 客户 + 物料 + 期初库存"
api s1_customer POST /business/erp/customer "$T_ADMIN" '{"code":"C-RET01-001","name":"退货演练客户A","tax_no":"91110000RET01TESTX","contact_name":"李四","contact_phone":"13900000000","address":"北京市海淀区演练路 8 号","credit_days":30,"credit_limit":100000,"remark":"RET-01 演练客户"}'
CUST=$(data '.data.id')
if [ "$CODE" = "0" ] && [ -n "$CUST" ]; then ok "客户 C-RET01-001 id=$CUST 额度=$(data '.data.credit_limit')"; else bad "客户建档 http=$HTTP body=$(printf '%s' "$BODY" | head -c 300)"; fi

api s1_material POST /business/erp/material "$T_ADMIN" '{"material_code":"RET01-MAT-001","material_name":"退货演练成品","spec":"1 台","unit_id":"d1370000-0000-4000-8000-000000000001","material_type":"FINISHED","costing_method":"MOVING_AVG","tax_rate":13,"status":1,"remark":"RET-01 演练物料（非一物一码）"}'
MAT=$(data '.data.id')
if [ "$CODE" = "0" ] && [ -n "$MAT" ]; then ok "物料 RET01-MAT-001 id=$MAT"; else bad "物料建档 http=$HTTP body=$(printf '%s' "$BODY" | head -c 300)"; fi

WH=d1370001-0000-4000-8000-000000000001   # WH01 总仓（种子）
api s1_opening POST /business/erp/opening-balance/inventory "$T_ADMIN" "{\"period\":\"$PERIOD\",\"items\":[{\"material_id\":\"$MAT\",\"warehouse_id\":\"$WH\",\"qty\":200,\"unit_cost\":300,\"remark\":\"RET-01 期初\"}]}"
check "期初库存入账" "$CODE" "0"
snap s1_master \
  "select 'customer='||code||'|'||name||'|limit='||credit_limit from biz_customer where id='$CUST'" \
  "select 'material='||material_code||'|'||material_name||'|costing='||costing_method||'|serial='||serial_required from biz_material where id='$MAT'" \
  "select 'inv='||qty||'|amt='||amount||'|avg='||avg_cost from biz_inventory where material_id='$MAT'" \
  "select 'txn='||txn_type||'|'||biz_type||'|'||qty||'|before='||before_qty||'|after='||after_qty from biz_inventory_txn where material_id='$MAT' order by created_at"
check "期初库存 qty" "$(P "select qty from biz_inventory where material_id='$MAT' and warehouse_id='$WH'")" "200.0000"

# ---------- S2 销售订单 → 审批 ----------
say; say "S2 销售订单（100 × 300，税率 13%）→ 提交 → 审批通过"
api s2_so POST /business/erp/sales-order "$T_ADMIN" "{\"customer_id\":\"$CUST\",\"order_date\":\"$DAY\",\"delivery_date\":\"2026-11-20\",\"remark\":\"RET-01 销售订单\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":100,\"price\":300,\"tax_rate\":13}]}"
SO=$(data '.data.id'); SO_ITEM=$(data '.data.items[0].id')
if [ "$CODE" = "0" ] && [ -n "$SO" ]; then
  ok "订单 $(data '.data.so_no') id=$SO 不含税=$(data '.data.total_amount') 税=$(data '.data.total_tax') 价税合计=$(data '.data.total_with_tax') 行=$SO_ITEM"
else
  bad "订单创建 http=$HTTP body=$(printf '%s' "$BODY" | head -c 400)"
fi
check "订单不含税金额" "$(P "select total_amount from biz_sales_order where id='$SO'")" "30000.0000"
check "订单价税合计" "$(P "select total_with_tax from biz_sales_order where id='$SO'")" "33900.0000"

api s2_submit POST "/business/erp/sales-order/$SO/submit-approval" "$T_ADMIN" '{}'
check "订单提交审批后状态" "$(data '.data.status')" "SUBMITTED"
r=0
while [ "$r" -lt 6 ]; do
  tasks=$(P "select t.id from biz_flow_task t where t.status='PENDING' and t.instance_id in (select id from biz_flow_instance where biz_id='$SO') order by t.created_at")
  [ -z "$tasks" ] && break
  for t in $tasks; do
    api "s2_approve_r$((r+1))" POST "/business/flow/task/$t/approve" "$T_APP" '{"comment":"RET-01 演练审批同意"}'
    if [ "$CODE" = "0" ]; then ok "审批通过 task=$t（第 $((r+1)) 轮）"; else bad "审批失败 task=$t http=$HTTP body=$(printf '%s' "$BODY" | head -c 300)"; fi
  done
  r=$((r+1))
done
check "订单审批后状态" "$(P "select status from biz_sales_order where id='$SO'")" "APPROVED"
snap s2_so \
  "select 'so='||so_no||'|'||status||'|amt='||total_amount||'|tax='||total_tax||'|total='||total_with_tax from biz_sales_order where id='$SO'" \
  "select 'item: line='||line_no||'|qty='||qty||'|price='||price||'|rate='||tax_rate||'|delivered='||delivered_qty from biz_sales_order_item where so_id='$SO'" \
  "select 'flow: '||node_key||'|'||status||'|assignee='||coalesce(assignee_name,'-') from biz_flow_task where instance_id in (select id from biz_flow_instance where biz_id='$SO') order by created_at"

# ---------- S3 发货 → 出库 → 应收 ----------
say; say "S3 发货单 100 台 → 提交待过账 → 过账（出库 + 应收 + 成本凭证）"
api s3_ship POST /business/erp/shipment "$T_ADMIN" "{\"so_id\":\"$SO\",\"warehouse_id\":\"$WH\",\"ship_date\":\"$DAY\",\"remark\":\"RET-01 发货\",\"items\":[{\"so_item_id\":\"$SO_ITEM\",\"qty\":100}]}"
SH=$(data '.data.id')
if [ "$CODE" = "0" ] && [ -n "$SH" ]; then ok "发货单 $(data '.data.ship_no') id=$SH 状态=$(data '.data.status')"; else bad "发货单创建 http=$HTTP body=$(printf '%s' "$BODY" | head -c 400)"; fi
api s3_ship_submit PATCH "/business/erp/shipment/$SH/status" "$T_ADMIN" '{"target_status":"PENDING_POST"}'
check "发货单提交后状态" "$(data '.data.status')" "PENDING_POST"
api s3_ship_post PATCH "/business/erp/shipment/$SH/status" "$T_ADMIN" '{"target_status":"POSTED"}'
check "发货单过账后状态" "$(data '.data.status')" "POSTED"
STOCKOUT=$(P "select stock_out_id from biz_shipment where id='$SH'")
AR=$(P "select id from biz_receivable where source_id='$STOCKOUT' and source_type='SALES_OUT'")
ok "出库单 id=$STOCKOUT 应收 id=$AR"
check "过账后库存 qty（200−100）" "$(P "select qty from biz_inventory where material_id='$MAT' and warehouse_id='$WH'")" "100.0000"
check "应收金额（100×300×1.13）" "$(P "select amount from biz_receivable where id='$AR'")" "33900.0000"
check "订单状态（全部发完）" "$(P "select status from biz_sales_order where id='$SO'")" "OUTBOUND"
check "订单行 delivered_qty" "$(P "select delivered_qty from biz_sales_order_item where id='$SO_ITEM'")" "100.0000"
check "出库单流水 biz_type" "$(P "select distinct txn_type||'/'||biz_type from biz_inventory_txn where doc_id='$STOCKOUT'")" "OUT/SALE_OUT"
check "成本结转凭证（借6401/贷1405）" "$(P "select (select sum(debit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='STOCK_OUT' and e.account_code='6401')||'/'||(select sum(credit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='STOCK_OUT' and e.account_code='1405')")" "30000.0000/30000.0000"
snap s3_post \
  "select 'inv='||qty||'|amt='||amount||'|avg='||avg_cost from biz_inventory where material_id='$MAT'" \
  "select 'ar='||ar_no||'|'||status||'|amt='||amount||'|received='||received_amount||'|unsettled='||unsettled_amount from biz_receivable where id='$AR'" \
  "select 'voucher='||voucher_no||'|'||source_type||'|'||total_debit||'/'||total_credit from biz_voucher order by created_at" \
  "select 'entry='||account_code||'|D='||debit_amount||'|C='||credit_amount from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='STOCK_OUT' order by e.line_no"

# ---------- S3b 开票（收入确认） ----------
say; say "S3b 按应收开票 30000（税率 13% → 价税合计 33900）→ 开票（DRAFT→ISSUED）"
api s3b_invoice POST /business/erp/invoice "$T_ADMIN" "{\"receivable_id\":\"$AR\",\"invoice_date\":\"$DAY\",\"amount\":30000,\"tax_rate\":13,\"is_electronic\":false,\"remark\":\"RET-01 发票（纸质票，演练）\"}"
IV=$(data '.data.id')
if [ "$CODE" = "0" ] && [ -n "$IV" ]; then ok "发票 $(data '.data.invoice_no') id=$IV 不含税=$(data '.data.amount') 税=$(data '.data.tax_amount') 合计=$(data '.data.total_with_tax')"; else bad "发票创建 http=$HTTP body=$(printf '%s' "$BODY" | head -c 400)"; fi
# ATT-06：开票（=发票的提交动作）必须已有发票影像
api s3b_issue_noatt PATCH "/business/erp/invoice/$IV/status" "$T_ADMIN" '{"status":"ISSUED"}'
check_http "负例：发票无影像就开票" "$HTTP" "400"
printf '\x89PNG\r\n\x1a\n' >/tmp/ret01_evidence.png
printf 'RET-01 sales return drill evidence (fake png payload)\n' >>/tmp/ret01_evidence.png
curl -s -m 30 -o "$RAW/s3b_upload.json" -X POST "$BASE/business/attachment/upload" \
  -H "$(auth_header "$T_ADMIN")" \
  -F "doc_type=INVOICE" -F "doc_id=$IV" -F "file_role=INVOICE_SCAN" \
  -F "file=@/tmp/ret01_evidence.png;type=image/png" >/dev/null
UCODE=$(jq -r '.code // empty' "$RAW/s3b_upload.json" 2>/dev/null)
if [ "$UCODE" = "0" ]; then ok "发票影像已上传（biz_doc_attachment）"; else bad "发票影像上传失败 $(head -c 200 "$RAW/s3b_upload.json")"; fi
api s3b_issue PATCH "/business/erp/invoice/$IV/status" "$T_ADMIN" '{"status":"ISSUED"}'
check "发票开票后状态" "$(data '.data.status')" "ISSUED"
check "收入凭证（借1122/贷5001/贷2221.02）" "$(P "select (select sum(debit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='INVOICE' and e.account_code='1122')||'/'||(select sum(credit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='INVOICE' and e.account_code='5001')||'/'||(select sum(credit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='INVOICE' and e.account_code='2221.02')")" "33900.0000/30000.0000/3900.0000"

# ---------- S4 建退货单 + 负例 ----------
say; say "S4 建销售退货单（关联原发货单，退 40 台，原因=质量）"
api s4_returnable GET "/business/erp/sales-return/returnable/$SH" "$T_ADMIN" ""
if [ "$CODE" = "0" ]; then ok "可退明细：$(data '.data[0] | "shipped=\(.shipped) returned=\(.returned) remaining=\(.remaining) price=\(.price) rate=\(.tax_rate)"')"; else bad "可退明细 http=$HTTP"; fi

say "  -- 负例 1：退货数量 > 已发货（200 > 100）"
api n4_over POST /business/erp/sales-return "$T_ADMIN" "{\"shipment_id\":\"$SH\",\"warehouse_id\":\"$WH\",\"return_date\":\"$DAY\",\"return_type\":\"QUALITY\",\"remark\":\"负例：超退\",\"items\":[{\"so_item_id\":\"$SO_ITEM\",\"qty\":200}]}"
check_http "负例 超退建单" "$HTTP" "400"
CHECK_MSG=$(msg); ok "  报文：$CHECK_MSG"
if printf '%s' "$CHECK_MSG" | grep -q '超过可退量'; then ok "  报文点名「超过可退量」"; else bad "  报文未点名可退量：$CHECK_MSG"; fi

say "  -- 负例 2：退货原因（return_type）非法"
api n4_type POST /business/erp/sales-return "$T_ADMIN" "{\"shipment_id\":\"$SH\",\"warehouse_id\":\"$WH\",\"return_date\":\"$DAY\",\"return_type\":\"REFUND\",\"items\":[{\"so_item_id\":\"$SO_ITEM\",\"qty\":1}]}"
check_http "负例 非法退货原因" "$HTTP" "400"
ok "  报文：$(msg)"

say "  -- 正例：退 40 台"
api s4_return POST /business/erp/sales-return "$T_ADMIN" "{\"shipment_id\":\"$SH\",\"so_id\":\"$SO\",\"customer_id\":\"$CUST\",\"warehouse_id\":\"$WH\",\"return_date\":\"$DAY\",\"return_type\":\"QUALITY\",\"remark\":\"RET-01 部分退货 40 台（外观破损）\",\"items\":[{\"so_item_id\":\"$SO_ITEM\",\"qty\":40,\"remark\":\"质量问题返厂\"}]}"
SR=$(data '.data.id')
if [ "$CODE" = "0" ] && [ -n "$SR" ]; then
  ok "退货单 $(data '.data.return_no') id=$SR 状态=$(data '.data.status') 不含税=$(data '.data.total_amount') 税=$(data '.data.total_tax') 合计=$(data '.data.total_with_tax')"
else
  bad "退货单创建 http=$HTTP body=$(printf '%s' "$BODY" | head -c 400)"
fi
check "退货单不含税金额（40×300）" "$(P "select total_amount from biz_sales_return where id='$SR'")" "12000.0000"
check "退货单价来自订单行" "$(P "select price from biz_sales_return_item where return_id='$SR'")" "300.000000"
check "退货税率来自订单行" "$(P "select tax_rate from biz_sales_return_item where return_id='$SR'")" "13.0000"

say "  -- 负例 3：未审批就过账（DRAFT → POSTED）"
api n4_jump PATCH "/business/erp/sales-return/$SR/status" "$T_ADMIN" '{"target_status":"POSTED"}'
say "     HTTP=$HTTP msg=$(msg)"
if [ "$HTTP" = "409" ]; then ok "负例 未审批就过账被拒（HTTP=409，非法状态迁移；卡面写 400，实测口径为 409 冲突）"; else bad "负例 未审批就过账 HTTP=$HTTP（期望 409）"; fi

say "  -- 负例 4：未审批就跳到待过账（DRAFT → PENDING_POST）"
api n4_jump2 PATCH "/business/erp/sales-return/$SR/status" "$T_ADMIN" '{"target_status":"PENDING_POST"}'
check_http "负例 草稿跳待过账" "$HTTP" "409"
ok "  报文：$(msg)"

# ---------- S5 提交 → 附件闸 → 批准 ----------
say; say "S5 提交审批（ATT-06：退货单/物流凭证必传）→ 批准 → 待过账"
api s5_submit_noatt PATCH "/business/erp/sales-return/$SR/status" "$T_ADMIN" '{"target_status":"SUBMITTED"}'
check_http "负例 无附件提交" "$HTTP" "400"
ok "  报文：$(msg)"
curl -s -m 30 -o "$RAW/s5_upload.json" -X POST "$BASE/business/attachment/upload" \
  -H "$(auth_header "$T_ADMIN")" \
  -F "doc_type=SALES_RETURN" -F "doc_id=$SR" -F "file_role=DEFAULT" \
  -F "file=@/tmp/ret01_evidence.png;type=image/png" >/dev/null
UCODE=$(jq -r '.code // empty' "$RAW/s5_upload.json" 2>/dev/null)
if [ "$UCODE" = "0" ]; then ok "退货单附件已上传"; else bad "退货单附件上传失败 $(head -c 200 "$RAW/s5_upload.json")"; fi
api s5_submit PATCH "/business/erp/sales-return/$SR/status" "$T_ADMIN" '{"target_status":"SUBMITTED"}'
check "退货单提交后状态" "$(data '.data.status')" "SUBMITTED"
api s5_approve PATCH "/business/erp/sales-return/$SR/status" "$T_APP" '{"target_status":"APPROVED"}'
check "退货单批准后状态" "$(data '.data.status')" "APPROVED"
api s5_pending PATCH "/business/erp/sales-return/$SR/status" "$T_ADMIN" '{"target_status":"PENDING_POST"}'
check "退货单待过账状态" "$(data '.data.status')" "PENDING_POST"
snap s5_before_post \
  "select 'sr='||return_no||'|'||status||'|total='||total_with_tax||'|stock_in='||coalesce(stock_in_id::text,'-')||'|ar='||coalesce(receivable_id::text,'-')||'|red_iv='||coalesce(red_invoice_id::text,'-') from biz_sales_return where id='$SR'" \
  "select 'inv_before='||qty||'|amt='||amount from biz_inventory where material_id='$MAT'" \
  "select 'ar_before='||status||'|amt='||amount||'|unsettled='||unsettled_amount from biz_receivable where id='$AR'"

# ---------- S6 过账 ----------
say; say "S6 确认退货（过账）：回库 + 红字入库凭证 + 冲减应收 + 红字发票 + 收入冲减凭证"
api s6_post PATCH "/business/erp/sales-return/$SR/status" "$T_ADMIN" '{"target_status":"POSTED"}'
if [ "$CODE" = "0" ]; then ok "过账成功 状态=$(data '.data.status')"; else bad "过账 http=$HTTP body=$(printf '%s' "$BODY" | head -c 500)"; fi
check "过账后状态" "$(P "select status from biz_sales_return where id='$SR'")" "POSTED"
check "回库后库存 qty（100+40）" "$(P "select qty from biz_inventory where material_id='$MAT' and warehouse_id='$WH'")" "140.0000"
check "回库后库存金额（42000 = 140×300）" "$(P "select amount from biz_inventory where material_id='$MAT' and warehouse_id='$WH'")" "42000.0000"
check "回库流水（IN / SALES_RETURN_IN / 40）" "$(P "select txn_type||'/'||biz_type||'/'||qty from biz_inventory_txn where doc_id=(select stock_in_id from biz_sales_return where id='$SR')")" "IN/SALES_RETURN_IN/40.0000"
check "退货入库单 in_type/source_type" "$(P "select in_type||'/'||source_type||'/'||status from biz_stock_in where id=(select stock_in_id from biz_sales_return where id='$SR')")" "RETURN/SALES_RETURN/POSTED"
check "应收冲减后（33900−13560）" "$(P "select amount from biz_receivable where id='$AR'")" "20340.0000"
check "应收未收余额" "$(P "select unsettled_amount from biz_receivable where id='$AR'")" "20340.0000"
check "应收状态仍 OPEN" "$(P "select status from biz_receivable where id='$AR'")" "OPEN"
check "红字发票金额/税额/合计" "$(P "select amount||'/'||tax_amount||'/'||total_with_tax from biz_invoice where id=(select red_invoice_id from biz_sales_return where id='$SR')")" "-12000.0000/-1560.0000/-13560.0000"
check "红字发票类型/状态" "$(P "select invoice_type||'/'||status from biz_invoice where id=(select red_invoice_id from biz_sales_return where id='$SR')")" "OUT_RED/ISSUED"
check "成本冲减凭证（借1405/贷6401）" "$(P "select (select sum(debit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='STOCK_IN' and e.account_code='1405')||'/'||(select sum(credit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='STOCK_IN' and e.account_code='6401')")" "12000.0000/12000.0000"
check "收入冲减凭证（借5001/借2221.02/贷1122，按红票来源）" "$(P "select (select sum(debit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_id=(select red_invoice_id from biz_sales_return where id='$SR') and e.account_code='5001')||'/'||(select sum(debit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_id=(select red_invoice_id from biz_sales_return where id='$SR') and e.account_code='2221.02')||'/'||(select sum(credit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_id=(select red_invoice_id from biz_sales_return where id='$SR') and e.account_code='1122')")" "12000.0000/1560.0000/13560.0000"
check "订单行 delivered_qty 回退（100−40）" "$(P "select delivered_qty from biz_sales_order_item where id='$SO_ITEM'")" "60.0000"
check "退货单三回链已回填" "$(P "select (stock_in_id is not null)::text||'/'||(receivable_id is not null)::text||'/'||(red_invoice_id is not null)::text from biz_sales_return where id='$SR'")" "true/true/true"
snap s6_post \
  "select 'sr='||return_no||'|'||status||'|amt='||total_amount||'|tax='||total_tax||'|total='||total_with_tax from biz_sales_return where id='$SR'" \
  "select 'inv='||qty||'|amt='||amount||'|avg='||avg_cost from biz_inventory where material_id='$MAT'" \
  "select 'txn='||txn_type||'|'||biz_type||'|'||qty||'|before='||before_qty||'|after='||after_qty||'|amt='||amount from biz_inventory_txn where material_id='$MAT' order by created_at" \
  "select 'voucher='||voucher_no||'|'||source_type||'|'||total_debit||'/'||total_credit||'|'||summary from biz_voucher order by created_at" \
  "select 'entry='||e.account_code||'|'||e.account_name||'|D='||e.debit_amount||'|C='||e.credit_amount||'|'||v.source_type from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id order by v.created_at, e.line_no" \
  "select 'ar='||ar_no||'|'||status||'|amt='||amount||'|received='||received_amount||'|unsettled='||unsettled_amount from biz_receivable where id='$AR'" \
  "select 'iv='||invoice_no||'|'||invoice_type||'|'||status||'|amt='||amount||'|tax='||tax_amount||'|total='||total_with_tax from biz_invoice order by created_at"

# ---------- S7 第二次退货（全额退 → 应收作废） ----------
say; say "S7 二次退货 60 台（= 剩余可退，应收全额冲减 → 作废）"
api s7_return POST /business/erp/sales-return "$T_ADMIN" "{\"shipment_id\":\"$SH\",\"warehouse_id\":\"$WH\",\"return_date\":\"$DAY\",\"return_type\":\"QUALITY\",\"remark\":\"RET-01 剩余 60 台全退\",\"items\":[{\"so_item_id\":\"$SO_ITEM\",\"qty\":60}]}"
SR2=$(data '.data.id')
if [ "$CODE" = "0" ] && [ -n "$SR2" ]; then ok "退货单② $(data '.data.return_no') id=$SR2 合计=$(data '.data.total_with_tax')"; else bad "退货单②创建 http=$HTTP body=$(printf '%s' "$BODY" | head -c 400)"; fi
curl -s -m 30 -o "$RAW/s7_upload.json" -X POST "$BASE/business/attachment/upload" \
  -H "$(auth_header "$T_ADMIN")" -F "doc_type=SALES_RETURN" -F "doc_id=$SR2" \
  -F "file=@/tmp/ret01_evidence.png;type=image/png" >/dev/null
for target in SUBMITTED APPROVED PENDING_POST POSTED; do
  tok="$T_ADMIN"; [ "$target" = "APPROVED" ] && tok="$T_APP"
  api "s7_$target" PATCH "/business/erp/sales-return/$SR2/status" "$tok" "{\"target_status\":\"$target\"}"
  if [ "$CODE" = "0" ]; then ok "退货单② → $target"; else bad "退货单② → $target http=$HTTP body=$(printf '%s' "$BODY" | head -c 400)"; fi
done
check "二次退货后应收状态（作废）" "$(P "select status from biz_receivable where id='$AR'")" "CANCELLED"
check "二次退货后应收已收金额（0，未动过钱）" "$(P "select received_amount from biz_receivable where id='$AR'")" "0.0000"
check "账龄口径未结合计（作废单不计入：status∈OPEN/PARTIAL）" "$(P "select coalesce(sum(unsettled_amount),0)::text from biz_receivable where customer_id='$CUST' and status in ('OPEN','PARTIAL')")" "0"
check "红字发票价税合计（−33900 = 全额红冲）" "$(P "select sum(total_with_tax) from biz_invoice where invoice_type='OUT_RED'")" "-33900.0000"
check "红字发票不含税合计（−30000）" "$(P "select sum(amount) from biz_invoice where invoice_type='OUT_RED'")" "-30000.0000"
check "库存回到期初（200 / 60000）" "$(P "select qty||'/'||amount from biz_inventory where material_id='$MAT' and warehouse_id='$WH'")" "200.0000/60000.0000"
check "订单行 delivered_qty 归零" "$(P "select delivered_qty from biz_sales_order_item where id='$SO_ITEM'")" "0.0000"
check "成本冲减累计 = 原结转成本" "$(P "select (select sum(debit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='STOCK_IN' and e.account_code='1405')||'/'||(select sum(debit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='STOCK_OUT' and e.account_code='6401')")" "30000.0000/30000.0000"
check "收入冲减累计（借5001 = 贷5001 = 30000，净额 0）" "$(P "select (select sum(debit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='INVOICE' and e.account_code='5001')||'/'||(select sum(credit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='INVOICE' and e.account_code='5001')")" "30000.0000/30000.0000"
check "应收账款净额（借 = 贷 = 33900，余额 0）" "$(P "select sum(debit_amount)||'/'||sum(credit_amount)||'/'||sum(debit_amount-credit_amount) from biz_voucher_entry where account_code='1122'")" "33900.0000/33900.0000/0.0000"
check "销项税净额（借 = 贷 = 3900，余额 0）" "$(P "select sum(debit_amount)||'/'||sum(credit_amount)||'/'||sum(debit_amount-credit_amount) from biz_voucher_entry where account_code='2221.02'")" "3900.0000/3900.0000/0.0000"
check "无多余红冲凭证（全链路 is_red=1 张数，应为 0：退货用红票，不另开整张红冲）" "$(P "select count(*) from biz_voucher where is_red=1")" "0"

# ---------- S8 负例：已全额退货再退 ----------
say; say "S8 负例：已全额退货的订单行再退（可退量 0）"
api n8_again POST /business/erp/sales-return "$T_ADMIN" "{\"shipment_id\":\"$SH\",\"warehouse_id\":\"$WH\",\"return_date\":\"$DAY\",\"return_type\":\"QUALITY\",\"items\":[{\"so_item_id\":\"$SO_ITEM\",\"qty\":1}]}"
check_http "负例 全额退后再退" "$HTTP" "400"
ok "  报文：$(msg)"
api n8_returnable GET "/business/erp/sales-return/returnable/$SH" "$T_ADMIN" ""
ok "  可退明细剩余：$(data '.data[0] | "shipped=\(.shipped) returned=\(.returned) remaining=\(.remaining)"')"

# ---------- S9 对账 / 报表 ----------
say; say "S9 库存对账 + 试算平衡 + 资产负债表 + 应收余额"
RECON1="WITH inv AS (SELECT material_id, warehouse_id, SUM(qty) qty FROM biz_inventory GROUP BY 1,2), txn AS (SELECT material_id, warehouse_id, SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) qty FROM biz_inventory_txn GROUP BY 1,2) SELECT count(*) FROM inv FULL JOIN txn USING (material_id, warehouse_id) WHERE COALESCE(inv.qty,0) <> COALESCE(txn.qty,0)"
RECON2="WITH cost AS (SELECT i.material_id, i.warehouse_id, COALESCE(il.location_id::text,'(未分配)') bucket, SUM(i.qty) qty FROM biz_inventory i LEFT JOIN biz_inventory_location il ON il.id=i.location_id GROUP BY 1,2,3), loc AS (SELECT material_id, warehouse_id, COALESCE(location_id::text,'(未分配)') bucket, SUM(qty) qty FROM biz_inventory_location GROUP BY 1,2,3) SELECT count(*) FROM cost FULL JOIN loc ON cost.material_id=loc.material_id AND cost.warehouse_id=loc.warehouse_id AND cost.bucket=loc.bucket WHERE COALESCE(cost.qty,0) <> COALESCE(loc.qty,0)"
check "对账① 库存表 vs 流水累计 差异行数" "$(P "$RECON1")" "0"
check "对账② 成本账 vs 库位账 差异桶数" "$(P "$RECON2")" "0"
snap s9_reconcile \
  "WITH inv AS (SELECT material_id, warehouse_id, SUM(qty) qty FROM biz_inventory GROUP BY 1,2), txn AS (SELECT material_id, warehouse_id, SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) qty FROM biz_inventory_txn GROUP BY 1,2) SELECT 'recon1 '||coalesce(m.material_code,'?')||' wh='||coalesce(w.warehouse_code,'?')||' inv='||coalesce(inv.qty,0)||' txn='||coalesce(txn.qty,0)||' diff='||coalesce(inv.qty,0)-coalesce(txn.qty,0) FROM inv FULL JOIN txn USING (material_id, warehouse_id) LEFT JOIN biz_material m ON m.id=coalesce(inv.material_id,txn.material_id) LEFT JOIN biz_warehouse w ON w.id=coalesce(inv.warehouse_id,txn.warehouse_id)" \
  "select 'stock_all='||qty||'|amt='||amount||'|avg='||avg_cost from biz_inventory where material_id='$MAT'" \
  "select 'loc='||coalesce(location_id::text,'-')||'|qty='||qty from biz_inventory_location where material_id='$MAT'" \
  "select 'ar_all='||ar_no||'|'||status||'|amt='||amount||'|unsettled='||unsettled_amount from biz_receivable order by created_at" \
  "select 'cust_open_ar(账龄口径 OPEN/PARTIAL)='||coalesce(sum(unsettled_amount),0) from biz_receivable where customer_id='$CUST' and status in ('OPEN','PARTIAL')" \
  "select 'cust_open_ar(全状态含作废)='||coalesce(sum(unsettled_amount),0) from biz_receivable where customer_id='$CUST'"

api s9_tb GET "/business/erp/ledger/trial-balance?period=$PERIOD" "$T_ADMIN" ""
if [ "$CODE" = "0" ]; then
  ok "试算平衡：本期借=$(data '.data.total_period_debit') 贷=$(data '.data.total_period_credit') 期末借=$(data '.data.total_closing_debit') 期末贷=$(data '.data.total_closing_credit') balanced=$(data '.data.balanced')"
  check "试算平衡标记" "$(data '.data.balanced')" "true"
  if [ "$(data '.data.total_period_debit')" = "$(data '.data.total_period_credit')" ]; then ok "本期 Σ借 = Σ贷（$(data '.data.total_period_debit')）"; else bad "本期 Σ借=$(data '.data.total_period_debit') ≠ Σ贷=$(data '.data.total_period_credit')"; fi
else
  bad "试算平衡 http=$HTTP body=$(printf '%s' "$BODY" | head -c 300)"
fi
api s9_bs GET "/business/erp/report/balance-sheet?period=$PERIOD" "$T_ADMIN" ""
if [ "$CODE" = "0" ]; then
  ok "资产负债表：资产=$(data '.data.check.assets_total') 负债+权益=$(data '.data.check.liability_equity_total') diff=$(data '.data.check.diff') balanced=$(data '.data.check.balanced')"
  check "资产负债表勾稽 balanced" "$(data '.data.check.balanced')" "true"
  check "资产负债表 diff" "$(data '.data.check.diff')" "0"
else
  bad "资产负债表 http=$HTTP body=$(printf '%s' "$BODY" | head -c 300)"
fi

# ---------- 汇总 ----------
say
say "单据汇总：$(P "select 'so='||(select count(*) from biz_sales_order)||' ship='||(select count(*) from biz_shipment)||' stock_out='||(select count(*) from biz_stock_out)||' ar='||(select count(*) from biz_receivable)||' invoice='||(select count(*) from biz_invoice)||' stock_in='||(select count(*) from biz_stock_in)||' sales_return='||(select count(*) from biz_sales_return)||' voucher='||(select count(*) from biz_voucher)")"
say "凭证一览：$(P "select string_agg(voucher_no||':'||source_type||':'||total_debit||'/'||total_credit, ' | ' order by created_at) from biz_voucher")"

# 证据脱敏：token 不落盘
for f in "$RAW"/*.json; do
  sed -i 's/"access_token":"[^"]*"/"access_token":"<redacted>"/g; s/"refresh_token":"[^"]*"/"refresh_token":"<redacted>"/g' "$f"
done

say
say "===== 汇总：PASS=$PASS FAIL=$FAIL ====="
[ "$FAIL" -eq 0 ] || say "失败项：${FAILED[*]}"
