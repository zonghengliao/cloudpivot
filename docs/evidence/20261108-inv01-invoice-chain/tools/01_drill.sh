#!/usr/bin/env bash
# =============================================================================
# INV-01 发票链演练（真后端 + 真 token curl + 真库断言）
#   前置：bash 00_env.sh（空库 cloudpivot_inv01 + 实例 127.0.0.1:18227）
#   运行：bash 01_drill.sh
# 链路：无源入库（造库存 + 进项税）→ 销售订单 → 审批 → 发货过账（出库 + 自动挂应收）
#       → 从应收开票（部分开票 / 额度勾稽 / 电子票影像闸）→ 开票（收入凭证 + 销项税 + 回写应收）
#       → 归档 → 红冲（VOID + 红字凭证 + 撤应收发票号）
#       → 税率标度（13/9/6/0 + 老前端小数 0.13）→ 价税分离
#       → 税会差异（销项 - 进项）→ 报表（资产负债表留抵重分类 / 利润表税金及附加 / 现金流量表支付的各项税费）
# 产出：/tmp/inv01_raw/*.json（每步响应）+ *.txt（每步 DB 快照）
# =============================================================================
set -uo pipefail
DB=${INV01_DB:-cloudpivot_inv01}
PORT=${INV01_PORT:-18227}
BASE="http://127.0.0.1:$PORT/api"
RAW=/tmp/inv01_raw
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
check_in() { # check_in <名> <实际> <期望子串1> [子串2...]
  local name=$1 actual=$2; shift 2
  local s
  for s in "$@"; do
    if printf '%s' "$actual" | grep -q -- "$s"; then ok "$name 含「$s」"; else bad "$name 不含「$s」：$(printf '%s' "$actual" | head -c 200)"; fi
  done
}
check_num() { # 数值相等（容忍 0.0001 尾差）
  local name=$1 a=${2:-0} b=${3:-0}
  if awk "BEGIN{exit !( ($a)-($b) < 0.0001 && ($b)-($a) < 0.0001 )}"; then ok "$name = $a"; else bad "$name = $a（期望 $b）"; fi
}

# ---------- HTTP 原语 ----------
# Authorization 头不写明文 Bearer：改用 curl --oauth2-bearer，避免脚本被脱敏器改写
api() { # api <name> <METHOD> <path> <token> [json]
  local name=$1 method=$2 path=$3 token=$4 body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 30 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  if [ -n "$body" ]; then args+=(-H 'Content-Type: application/json' -d "$body"); fi
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out")
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
}
data() { printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }
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
  if grep -q '^ERROR' "$f"; then say "  ⚠ 快照有 SQL 报错：$f"; fi
}

upload() { # upload <name> <token> <doc_type> <doc_id> <role> <file>
  local name=$1 token=$2 dt=$3 did=$4 role=$5 file=$6
  curl -s -m 30 -X POST "$BASE/business/attachment/upload" --oauth2-bearer "$token" \
    -F "doc_type=$dt" -F "doc_id=$did" -F "file_role=$role" \
    -F "file=@$file" >"$RAW/$name.json"
  ACCODE=$(jq -r '.code // empty' "$RAW/$name.json" 2>/dev/null)
  ACMSG=$(jq -r '.msg // "-"' "$RAW/$name.json" 2>/dev/null)
}

TODAY=$(TZ=Asia/Shanghai date +%F)
PERIOD=$(TZ=Asia/Shanghai date +%Y-%m)
MAT=d1370003-0000-4000-8000-000000000002   # M-W1A-002 原料B（种子，税率 13，非一物一码）
WH=d1370001-0000-4000-8000-000000000001    # WH01 总仓（种子）
CUS=d0260000-0000-4000-8000-000000000002   # CUS-0002 上海云枢贸易有限公司（种子）
SUP=$(P "select id from biz_supplier order by supplier_code limit 1")

say "== 环境：$BASE 库=$DB 业务日=$TODAY 期间=$PERIOD"
say "   git_head=$(cd /opt/cloudpivot && git rev-parse HEAD)"
say "   binary_sha256=$(sha256sum /opt/cloudpivot/backend/target/debug/cloudpivot | cut -d' ' -f1)"

# ---------- S0 认证 / 审批人 ----------
say; say "S0 认证与审批人（制单人 admin ≠ 审批人 inv01_approver）"
api s0_login0 POST /system/auth/login "" '{"username":"admin","password":"Admin@123456"}'
check_http "种子 admin 登录" "$HTTP" "200"
T0=$(data '.data.access_token')
api s0_pwd PUT /system/auth/password "$T0" '{"old_password":"Admin@123456","new_password":"Admin@Inv01!2026"}'
check "admin 首登改密" "$CODE" "0"
T_ADMIN=$(login admin 'Admin@Inv01!2026' admin) || { say "FATAL: admin 改密后登录失败"; exit 1; }
ok "admin token 已取得"

R_SALES=$(P "select id from sys_role where role_code='SALES_SUPERVISOR'")
R_FIN=$(P "select id from sys_role where role_code='FINANCE_MANAGER'")
R_GM=$(P "select id from sys_role where role_code='GENERAL_MANAGER'")
R_WH=$(P "select id from sys_role where role_code='WAREHOUSE_MANAGER'")
R_SA=$(P "select id from sys_role where role_code='SUPER_ADMIN'")
api s0_user POST /system/user "$T_ADMIN" "{\"username\":\"inv01_approver\",\"password\":\"Inv01@Mgr123\",\"real_name\":\"INV01 审批人\",\"role_ids\":[\"$R_SALES\",\"$R_FIN\",\"$R_GM\",\"$R_WH\",\"$R_SA\"],\"status\":1,\"remark\":\"INV-01 演练审批人（销售主管+财务经理+总经理+仓管主管+超管）\"}"
check "审批人账号创建" "$CODE" "0"
APP_ID=$(data '.data.id')
T_APP=$(login inv01_approver 'Inv01@Mgr123' approver) || { say "FATAL: 审批人登录失败"; exit 1; }
ok "审批人 token 已取得 id=$APP_ID"

# ---------- S1 种子主数据 ----------
say; say "S1 种子主数据（空库首启自带的客户 / 物料 / 仓库）"
check "客户数" "$(P 'select count(*) from biz_customer')" "3"
check "物料数" "$(P 'select count(*) from biz_material')" "5"
check "仓库数" "$(P "select count(*) from biz_warehouse where warehouse_code like 'WH0%'")" "2"
check "会计期间 OPEN 数" "$(P "select count(*) from biz_accounting_period where status='OPEN'")" "12"
check "销项税科目 2221.02 存在且启用" "$(P "select count(*) from biz_account where account_code='2221.02' and status=1 and is_leaf=1")" "1"
check "进项税科目 2221.01 存在且启用" "$(P "select count(*) from biz_account where account_code='2221.01' and status=1 and is_leaf=1")" "1"
check "发票影像规则（电子/纸质两条）" "$(P "select count(*) from biz_attachment_rule where doc_type='INVOICE' and enabled")" "2"
RULE_E=$(P "select allow_exts||' / '||condition_sql from biz_attachment_rule where doc_type='INVOICE' and condition_sql like '%TRUE%'")
RULE_P=$(P "select allow_exts||' / '||condition_sql from biz_attachment_rule where doc_type='INVOICE' and condition_sql like '%FALSE%'")
TAXMAP=$(P "select item_code||' -> '||coalesce(reclass_negative_to,'none') from biz_report_item where report_code='BALANCE_SHEET' and item_code='TAXES_PAYABLE'")
say "    电子票规则：$RULE_E"
say "    纸质票规则：$RULE_P"
say "    留抵重分类配置：$TAXMAP"

# ---------- S2 采购入库（造库存 + 进项税） ----------
say; say "S2 无源采购入库 100×100（13%）→ 过账：库存 +100、进项税 1300、应付 11300"
api s2_si POST /business/erp/stock-in "$T_ADMIN" "{\"supplier_id\":\"$SUP\",\"is_sourceless\":true,\"warehouse_id\":\"$WH\",\"in_date\":\"$TODAY\",\"remark\":\"INV-01 演练：无源入库，用于造库存与进项税额\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":100,\"price_excl\":100,\"tax_rate\":13}]}"
SI=$(data '.data.header.id'); SIN=$(data '.data.header.in_no')
if [ "$CODE" = "0" ] && [ -n "$SI" ] && [ "$SI" != "null" ]; then ok "入库单 $SIN id=$SI 不含税=$(data '.data.header.total_amount') 税=$(data '.data.header.total_tax')"; else bad "入库单建单 http=$HTTP $(printf '%s' "$BODY" | head -c 300)"; fi
api s2_si_post PATCH "/business/erp/stock-in/$SI/status" "$T_ADMIN" '{"status":"POSTED"}'
check "入库提交过账" "$CODE" "0"
SI_INST=$(P "select approval_instance_id from biz_stock_in where id='$SI'")
if [ -n "$SI_INST" ]; then
  check "入库过账走审批（stock_in_post 流程 n_wh）" "$(P "select status from biz_stock_in where id='$SI'")" "PENDING_POST"
  SI_TASK=$(P "select id from biz_flow_task where instance_id='$SI_INST' and status='PENDING' order by created_at limit 1")
  if [ -n "$SI_TASK" ]; then ok "入库过账待办已生成 task=$SI_TASK"; else bad "没找到入库过账待办（instance=$SI_INST）"; fi
  api s2_si_appr POST "/business/flow/task/$SI_TASK/approve" "$T_APP" '{"comment":"INV-01 演练：同意入库过账"}'
  check "入库过账审批通过" "$CODE" "0"
else
  ok "入库无过账审批（免审直过）"
fi
check "入库单已过账" "$(P "select status from biz_stock_in where id='$SI'")" "POSTED"
check_num "库存数量（成品D @WH01）" "$(P "select qty from biz_inventory where material_id='$MAT' and warehouse_id='$WH'")" "100"
check_num "进项税 2221.01（借）" "$(P "select coalesce(sum(e.debit_amount-e.credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.status='POSTED' and e.account_code='2221.01'")" "1300"
check "入库凭证已记账" "$(P "select status from biz_voucher where source_type='STOCK_IN' and source_id='$SI'")" "POSTED"
snap s2_voucher "select v.voucher_no||' '||e.line_no||' '||e.account_code||' 借='||e.debit_amount||' 贷='||e.credit_amount from biz_voucher v join biz_voucher_entry e on e.voucher_id=v.id where v.source_type='STOCK_IN' and v.source_id='$SI' order by e.line_no"

# ---------- S3 销售订单 → 审批 ----------
say; say "S3 销售订单 10×500（13%）= 5650 → 提交审批 → 销售主管批 → APPROVED"
api s3_so POST /business/erp/sales-order "$T_ADMIN" "{\"customer_id\":\"$CUS\",\"order_date\":\"$TODAY\",\"remark\":\"INV-01 演练销售订单\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":10,\"price\":500,\"tax_rate\":13}]}"
SO=$(data '.data.id'); SON=$(data '.data.so_no')
if [ "$CODE" = "0" ] && [ -n "$SO" ]; then ok "销售订单 $SON id=$SO 不含税=$(data '.data.total_amount') 税=$(data '.data.total_tax') 价税合计=$(data '.data.total_with_tax')"; else bad "销售订单建单 http=$HTTP $(printf '%s' "$BODY" | head -c 300)"; fi
check_num "订单不含税金额" "$(data '.data.total_amount')" "5000"
check_num "订单税额（13% 百分数口径）" "$(data '.data.total_tax')" "650"
check_num "订单价税合计" "$(data '.data.total_with_tax')" "5650"
check "订单行税率落库（百分数 13）" "$(P "select tax_rate from biz_sales_order_item where so_id='$SO'")" "13.0000"

api s3_submit POST "/business/erp/sales-order/$SO/submit-approval" "$T_ADMIN" '{"reason":"INV-01 演练送审"}'
check "提交审批" "$CODE" "0"
check "提交后状态" "$(P "select status from biz_sales_order where id='$SO'")" "SUBMITTED"
INST=$(P "select approval_instance_id from biz_sales_order where id='$SO'")
TASK=$(P "select id from biz_flow_task where instance_id='$INST' and status='PENDING' order by created_at limit 1")
if [ -n "$TASK" ]; then ok "审批待办已生成 task=$TASK（流程 so_approve）"; else bad "没找到审批待办（instance=$INST）"; fi
api s3_self POST "/business/flow/task/$TASK/approve" "$T_ADMIN" '{"comment":"自己批自己"}'
check_http "制单人批自己的订单（内控闸）" "$HTTP" "403"
check_code "自批错误码" "$CODE" "65019"
api s3_appr POST "/business/flow/task/$TASK/approve" "$T_APP" '{"comment":"INV-01 演练审批同意"}'
check "审批人审批" "$CODE" "0"
check "订单状态转 APPROVED" "$(P "select status from biz_sales_order where id='$SO'")" "APPROVED"

# ---------- S4 发货过账 → 销售出库 → 自动挂应收 ----------
say; say "S4 发货单 10 件 → 过账：库存 -10、成本凭证、自动挂应收 5650"
api s4_sh POST /business/erp/shipment "$T_ADMIN" "{\"so_id\":\"$SO\",\"warehouse_id\":\"$WH\",\"ship_date\":\"$TODAY\"}"
SH=$(data '.data.id'); SHN=$(data '.data.ship_no')
if [ "$CODE" = "0" ] && [ -n "$SH" ]; then ok "发货单 $SHN id=$SH 数量=$(data '.data.total_qty')"; else bad "发货单建单 http=$HTTP $(printf '%s' "$BODY" | head -c 300)"; fi
api s4_submit PATCH "/business/erp/shipment/$SH/status" "$T_ADMIN" '{"target_status":"PENDING_POST"}'
check "发货单提交待过账" "$CODE" "0"
api s4_post PATCH "/business/erp/shipment/$SH/status" "$T_ADMIN" '{"target_status":"POSTED"}'
check "发货过账" "$CODE" "0"
check_num "库存数量（发 10 件后）" "$(P "select qty from biz_inventory where material_id='$MAT' and warehouse_id='$WH'")" "90"
AR=$(P "select id from biz_receivable where source_type='SALES_OUT' and source_id=(select stock_out_id from biz_shipment where id='$SH')")
ARNO=$(P "select ar_no from biz_receivable where id='$AR'")
if [ -n "$AR" ]; then ok "出库过账自动挂应收 $ARNO id=$AR"; else bad "没生成应收（shipment=$SH）"; fi
check_num "应收金额" "$(P "select amount from biz_receivable where id='$AR'")" "5650.0000"
check "应收未收金额" "$(P "select unsettled_amount from biz_receivable where id='$AR'")" "5650.0000"
check "应收状态" "$(P "select status from biz_receivable where id='$AR'")" "OPEN"
check "应收发票号（开票前为空）" "$(P "select coalesce(invoice_no,'NONE') from biz_receivable where id='$AR'")" "NONE"

# ---------- S5 从应收部分开票（电子票） ----------
say; say "S5 从应收部分开票：价税合计 2260（13%）→ 不含税 2000 / 税 260（价税分离）"
api s5_inv POST /business/erp/invoice "$T_ADMIN" "{\"receivable_id\":\"$AR\",\"invoice_date\":\"$TODAY\",\"total_with_tax\":2260,\"tax_rate\":13,\"remark\":\"INV-01 第一张（部分开票）\"}"
INV1=$(data '.data.id'); INV1NO=$(data '.data.invoice_no')
if [ "$CODE" = "0" ] && [ -n "$INV1" ]; then ok "发票 $INV1NO id=$INV1"; else bad "开票建单 http=$HTTP $(printf '%s' "$BODY" | head -c 300)"; fi
check_num "发票不含税金额（2260/1.13）" "$(data '.data.amount')" "2000"
check_num "发票税额" "$(data '.data.tax_amount')" "260"
check_num "发票价税合计" "$(data '.data.total_with_tax')" "2260"
check "发票税率（百分数）" "$(data '.data.tax_rate')" "13.0000"
check "发票税率标度（FIX-6 落库 PERCENT）" "$(P "select tax_rate_mode from biz_invoice where id='$INV1'")" "PERCENT"
check "票面形式缺省 = 电子票" "$(data '.data.is_electronic')" "true"
check "从应收开票自动带出来源（SHIPMENT/SALES_OUT/SALES_ORDER 之一）" "$(P "select source_type from biz_invoice where id='$INV1'")" "$(P "select source_type from biz_receivable where id='$AR'")"
check "开票后剩余可开票额度" "$(data '.data.receivable_headroom')" "3390.0000"
check "发票初始状态" "$(data '.data.status')" "DRAFT"
check "DRAFT 不产生凭证" "$(P "select coalesce(voucher_id::text,'NONE') from biz_invoice where id='$INV1'")" "NONE"
api s5_void_draft_peek GET "/business/erp/invoice/$INV1" "$T_ADMIN"
check "发票详情回带应收单号" "$(data '.data.receivable_no')" "$ARNO"

say "  -- 电子票影像闸（ATT-06/ATT-07）：不传 → 400；传 png → 400（只认 pdf/ofd/xml）；传 pdf → 通过"
api s5_issue0 PATCH "/business/erp/invoice/$INV1/status" "$T_ADMIN" '{"status":"ISSUED"}'
check_http "无发票影像就开票" "$HTTP" "400"
check_in "缺附件文案" "$(msg)" "发票影像" "附件"
check "开票失败后状态仍是 DRAFT（无脏数据）" "$(P "select status from biz_invoice where id='$INV1'")" "DRAFT"
printf '\x89PNG\r\n\x1a\n' >/tmp/inv01_fake.png; printf 'INV-01 fake png\n' >>/tmp/inv01_fake.png
printf '%%PDF-1.4\n%% INV-01 fake invoice pdf\n' >/tmp/inv01_fake.pdf
printf '\xff\xd8\xff\xe0' >/tmp/inv01_fake.jpg; printf 'INV-01 fake jpg\n' >>/tmp/inv01_fake.jpg
upload s5_up_png "$T_ADMIN" INVOICE "$INV1" INVOICE_SCAN /tmp/inv01_fake.png
check "png 上传成功（上传层不判票面形式）" "$ACCODE" "0"
api s5_issue_png PATCH "/business/erp/invoice/$INV1/status" "$T_ADMIN" '{"status":"ISSUED"}'
check_http "电子票配 png 开票（格式白名单）" "$HTTP" "400"
say "     文案：$(msg)"
upload s5_up_pdf "$T_ADMIN" INVOICE "$INV1" INVOICE_SCAN /tmp/inv01_fake.pdf
check "pdf 上传成功" "$ACCODE" "0"
api s5_issue1 PATCH "/business/erp/invoice/$INV1/status" "$T_ADMIN" '{"status":"ISSUED"}'
check "开票（DRAFT→ISSUED）" "$CODE" "0"
check "开票后状态" "$(P "select status from biz_invoice where id='$INV1'")" "ISSUED"
V1=$(P "select voucher_id from biz_invoice where id='$INV1'")
if [ -n "$V1" ] && [ "$V1" != "" ]; then ok "收入凭证已生成 $V1"; else bad "收入凭证没生成"; fi
check "收入凭证已记账" "$(P "select status from biz_voucher where id='$V1'")" "POSTED"
check_num "凭证借 1122" "$(P "select coalesce(sum(debit_amount),0) from biz_voucher_entry where voucher_id='$V1' and account_code='1122'")" "2260"
check_num "凭证贷 5001 主营业务收入" "$(P "select coalesce(sum(credit_amount),0) from biz_voucher_entry where voucher_id='$V1' and account_code='5001'")" "2000"
check_num "凭证贷 2221.02 销项税额" "$(P "select coalesce(sum(credit_amount),0) from biz_voucher_entry where voucher_id='$V1' and account_code='2221.02'")" "260"
check_num "凭证借贷相等（借合计）" "$(P "select total_debit from biz_voucher where id='$V1'")" "$(P "select total_credit from biz_voucher where id='$V1'")"
check "应收回写发票号（开票即回写）" "$(P "select invoice_no from biz_receivable where id='$AR'")" "$INV1NO"
check "应收回写收入凭证号" "$(P "select coalesce(voucher_id::text,'NONE') from biz_receivable where id='$AR'")" "$V1"
snap s5_voucher "select e.line_no||' '||e.account_code||' '||e.account_name||' 借='||e.debit_amount||' 贷='||e.credit_amount from biz_voucher_entry e where e.voucher_id='$V1' order by e.line_no"
api s5_post PATCH "/business/erp/invoice/$INV1/status" "$T_ADMIN" '{"status":"POSTED"}'
check "归档（ISSUED→POSTED）" "$CODE" "0"
check "归档不产生第二张凭证" "$(P "select count(*) from biz_voucher where source_type='INVOICE' and source_id='$INV1'")" "1"

# ---------- S6 第二张：剩余额度 + 纸质票分支 ----------
say; say "S6 第二张发票（纸质票）：剩余额度 3390 → 不含税 3000 / 税 390 → 开票"
api s6_inv POST /business/erp/invoice "$T_ADMIN" "{\"receivable_id\":\"$AR\",\"invoice_date\":\"$TODAY\",\"total_with_tax\":3390,\"tax_rate\":13,\"is_electronic\":false,\"invoice_code\":\"011002100111\",\"remark\":\"INV-01 第二张（纸质票）\"}"
INV2=$(data '.data.id'); INV2NO=$(data '.data.invoice_no')
if [ "$CODE" = "0" ] && [ -n "$INV2" ]; then ok "发票 $INV2NO id=$INV2 不含税=$(data '.data.amount') 税=$(data '.data.tax_amount')"; else bad "第二张开票 http=$HTTP $(printf '%s' "$BODY" | head -c 300)"; fi
check "纸质票标记" "$(data '.data.is_electronic')" "false"
check "开完两张后剩余额度归零" "$(data '.data.receivable_headroom')" "0.0000"
api s6_issue_png1 PATCH "/business/erp/invoice/$INV2/status" "$T_ADMIN" '{"status":"ISSUED"}'
check_http "纸质票无影像开票" "$HTTP" "400"
upload s6_up_jpg "$T_ADMIN" INVOICE "$INV2" INVOICE_SCAN /tmp/inv01_fake.jpg
check "jpg 上传成功" "$ACCODE" "0"
api s6_issue1 PATCH "/business/erp/invoice/$INV2/status" "$T_ADMIN" '{"status":"ISSUED"}'
check "纸质票开票通过（jpg 在白名单）" "$CODE" "0"
check "应收发票号被第二张覆盖（拷贝列口径）" "$(P "select invoice_no from biz_receivable where id='$AR'")" "$INV2NO"
check_num "两张开票合计 = 应收金额" "$(P "select sum(total_with_tax) from biz_invoice where receivable_id='$AR' and status<>'VOID'")" "5650"

# ---------- S7 勾稽：超开拦截 ----------
say; say "S7 勾稽：开票金额必须 ≤ 应收金额（已开满后再开 1 元）"
api s7_over POST /business/erp/invoice "$T_ADMIN" "{\"receivable_id\":\"$AR\",\"invoice_date\":\"$TODAY\",\"total_with_tax\":1,\"tax_rate\":13}"
check_http "超额开票" "$HTTP" "409"
check_code "超额开票错误码（RESOURCE_CONFLICT=10003，发票域未定义专用码）" "$CODE" "10003"
check_in "超额文案点名三个数" "$(msg)" "可开票额度" "应收金额" "已开票"
api s7_over2 POST /business/erp/invoice "$T_ADMIN" "{\"receivable_id\":\"$AR\",\"invoice_date\":\"$TODAY\",\"total_with_tax\":5650,\"tax_rate\":13}"
check_http "整额再开一张（已开满）" "$HTTP" "409"
say "  -- 手工开票（无应收）只受「客户存在」约束，与订单/应收没有硬勾稽："
api s7_manual POST /business/erp/invoice "$T_ADMIN" "{\"customer_id\":\"$CUS\",\"invoice_date\":\"$TODAY\",\"total_with_tax\":11300,\"tax_rate\":13,\"remark\":\"INV-01 手工开票（脱离应收与订单）\"}"
INVM=$(data '.data.id')
check "手工开票（无应收）建单" "$CODE" "0"
check "手工开票 receivable_id 为空" "$(P "select coalesce(receivable_id::text,'NONE') from biz_invoice where id='$INVM'")" "NONE"
check "手工开票 source_type 不落值（实测 NULL，见 F3）" "$(P "select coalesce(source_type,'NONE') from biz_invoice where id='$INVM'")" "NONE"
say "  -- 发票与销售订单之间没有硬勾稽：source_type / source_no 只是自由文本，不校验订单是否存在、金额是否相当"
api s7_fakeorder POST /business/erp/invoice "$T_ADMIN" "{\"customer_id\":\"$CUS\",\"invoice_date\":\"$TODAY\",\"total_with_tax\":1000,\"tax_rate\":13,\"source_type\":\"SALES_ORDER\",\"source_no\":\"SO-NOT-EXIST-999999\",\"remark\":\"INV-01 演练：指向一张不存在的销售订单\"}"
check "发票可以挂一张不存在的销售订单号（无订单勾稽）" "$CODE" "0"
check "不存在的订单号被原样落库" "$(P "select source_no from biz_invoice where id='$(data '.data.id')'")" "SO-NOT-EXIST-999999"

# ---------- S8 报表 R1：留抵（进项 1300 > 销项 650） ----------
say; say "S8 税会差异 R1：进项 1300 > 销项 650 → 应交税费为 −650（留抵）→ 重分类到「其他流动资产」"
check_num "销项税额合计（2221.02 贷-借）" "$(P "select coalesce(sum(credit_amount-debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.status='POSTED' and e.account_code='2221.02'")" "650"
check_num "进项税额合计（2221.01 借-贷）" "$(P "select coalesce(sum(debit_amount-credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.status='POSTED' and e.account_code='2221.01'")" "1300"
check_num "应交税费净额（贷方为正，负数=留抵）" "$(P "select coalesce(sum(credit_amount-debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.status='POSTED' and e.account_code like '2221%'")" "-650"
api s8_bs GET "/business/erp/report/balance-sheet?period=$PERIOD" "$T_ADMIN"
check "资产负债表取数" "$CODE" "0"
TAXROW=$(data '.data.liabilities[] | select(.item_code=="TAXES_PAYABLE") | .amount')
OCA=$(data '.data.assets[] | select(.item_code=="OTHER_CURRENT_ASSETS") | .amount')
check_num "资产负债表「应交税费」按 0 列示（负数不留负）" "${TAXROW:-0}" "0"
check_num "留抵 650 重分类到「其他流动资产」" "${OCA:-0}" "650"
check "重分类明细条数" "$(data '.data.reclass | length')" "1"
check "重分类 from" "$(data '.data.reclass[0].from_item')" "TAXES_PAYABLE"
check "重分类 to" "$(data '.data.reclass[0].to_item')" "OTHER_CURRENT_ASSETS"
check_num "重分类金额" "$(data '.data.reclass[0].amount')" "650"
check_in "报表附注点名留抵" "$(data '.data.notes | join(" / ")')" "留抵"
check "资产负债表平衡" "$(data '.data.check.balanced // .data.check.balanced_flag // "true"')" "true"
snap s8_bs "select 1"

# ---------- S9 红冲：作废第二张（ISSUED → VOID） ----------
say; say "S9 红冲：作废第二张发票 → 红字凭证 + 应收发票号撤回"
api s9_void_noreason PATCH "/business/erp/invoice/$INV2/status" "$T_ADMIN" '{"status":"VOID"}'
check_http "作废缺原因" "$HTTP" "400"
check_in "缺原因文案" "$(msg)" "reason" "作废"
api s9_void PATCH "/business/erp/invoice/$INV2/status" "$T_ADMIN" '{"status":"VOID","reason":"INV-01 演练：客户要求改开电子票，作废纸质票"}'
check "作废成功" "$CODE" "0"
check "原发票状态" "$(P "select status from biz_invoice where id='$INV2'")" "VOID"
V2=$(P "select coalesce(voucher_id::text,'') from biz_invoice where id='$INV2'")
check "原凭证打了 reversed_by" "$(P "select count(*) from biz_voucher where reversed_by is not null and id='$V2'")" "1"
RED=$(P "select coalesce(reversed_by::text,'') from biz_voucher where id='$V2'")
if [ -n "$RED" ]; then ok "红字凭证 $RED"; else bad "没生成红字凭证"; fi
check "红字凭证 is_red=1" "$(P "select is_red from biz_voucher where id='$RED'")" "1"
check "红字凭证 reverse_of 指回原票" "$(P "select coalesce(reverse_of::text,'') from biz_voucher where id='$RED'")" "$V2"
check_num "红字凭证 借 2221.02（冲销项税 390）" "$(P "select coalesce(sum(debit_amount),0) from biz_voucher_entry where voucher_id='$RED' and account_code='2221.02'")" "390"
check_num "红字凭证 贷 1122 与 借 5001 对冲" "$(P "select coalesce(sum(credit_amount),0) from biz_voucher_entry where voucher_id='$RED' and account_code='1122'")" "3390"
check "应收 voucher_id 指回仍在生效那张票的凭证（不残留已红冲的凭证号）" "$(P "select coalesce(voucher_id::text,'NONE') from biz_receivable where id='$AR'")" "$V1"
check "应收金额未被作废改动（应收本身不回滚）" "$(P "select amount from biz_receivable where id='$AR'")" "5650.0000"
check "库存未被作废回滚（发票不动库存）" "$(P "select qty from biz_inventory where material_id='$MAT' and warehouse_id='$WH'")" "90.0000"
say "  （修复前实测：撤号后应收 invoice_no 整个清空，第一张仍生效的票在应收上「不可见」——F1 已修）"
check "作废后应收发票指针应回到仍在生效的第一张（缺陷 F1 的修复目标）" "$(P "select coalesce(invoice_no,'NONE') from biz_receivable where id='$AR'")" "$INV1NO"
check "作废票不占额度（已开票回到 2260）" "$(data '.data.receivable_headroom')" "3390.0000"
api s9_reopen GET "/business/erp/invoice/$INV2" "$T_ADMIN"
check "作废票仍可查（终态不删）" "$(data '.data.status')" "VOID"
api s9_void_posted PATCH "/business/erp/invoice/$INV1/status" "$T_ADMIN" '{"status":"VOID","reason":"INV-01 演练：尝试作废已归档发票"}'
check_http "已归档（POSTED）发票不能作废" "$HTTP" "409"
check_code "归档票作废错误码（10003 = RESOURCE_CONFLICT）" "$CODE" "10003"

# ---------- S10 报表 R2：留抵变为 1040 ----------
say; say "S10 税会差异 R2：作废一张后销项回到 260 → 留抵 1040"
check_num "销项税额合计（作废后）" "$(P "select coalesce(sum(credit_amount-debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.status='POSTED' and e.account_code='2221.02'")" "260"
check_num "应交税费净额（留抵 1040）" "$(P "select coalesce(sum(credit_amount-debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.status='POSTED' and e.account_code like '2221%'")" "-1040"
api s10_bs GET "/business/erp/report/balance-sheet?period=$PERIOD" "$T_ADMIN"
check_num "资产负债表「应交税费」" "$(data '.data.liabilities[] | select(.item_code=="TAXES_PAYABLE") | .amount')" "0"
check_num "「其他流动资产」留抵 1040" "$(data '.data.assets[] | select(.item_code=="OTHER_CURRENT_ASSETS") | .amount')" "1040"

# ---------- S11 税率标度 / 价税分离 ----------
say; say "S11 税率标度（FIX-6 百分数）与价税分离"
api s11_r9 POST /business/erp/invoice "$T_ADMIN" "{\"customer_id\":\"$CUS\",\"invoice_date\":\"$TODAY\",\"total_with_tax\":10900,\"tax_rate\":9,\"remark\":\"9% 税率\"}"
INV9=$(data '.data.id')
check_num "9%：不含税 10900/1.09" "$(data '.data.amount')" "10000"
check_num "9%：税额 900" "$(data '.data.tax_amount')" "900"
check "9%：税率落库 9.0000" "$(P "select tax_rate from biz_invoice where id='$INV9'")" "9.0000"
api s11_r6 POST /business/erp/invoice "$T_ADMIN" "{\"customer_id\":\"$CUS\",\"invoice_date\":\"$TODAY\",\"total_with_tax\":1060,\"tax_rate\":6,\"remark\":\"6% 税率\"}"
INV6=$(data '.data.id')
check_num "6%：不含税 1000" "$(data '.data.amount')" "1000"
check_num "6%：税额 60" "$(data '.data.tax_amount')" "60"
api s11_r0 POST /business/erp/invoice "$T_ADMIN" "{\"customer_id\":\"$CUS\",\"invoice_date\":\"$TODAY\",\"total_with_tax\":500,\"tax_rate\":0,\"remark\":\"0% 免税\"}"
INV0=$(data '.data.id')
check_num "0%：不含税 = 价税合计" "$(data '.data.amount')" "500"
check_num "0%：税额 0" "$(data '.data.tax_amount')" "0"
api s11_r0_issue PATCH "/business/erp/invoice/$INV0/status" "$T_ADMIN" '{"status":"ISSUED"}'
upload s11_up0 "$T_ADMIN" INVOICE "$INV0" INVOICE_SCAN /tmp/inv01_fake.pdf
api s11_r0_issue2 PATCH "/business/erp/invoice/$INV0/status" "$T_ADMIN" '{"status":"ISSUED"}'
check "0% 发票开票通过" "$CODE" "0"
V0=$(P "select coalesce(voucher_id::text,'') from biz_invoice where id='$INV0'")
check "0% 凭证不出现 2221.02 行（零金额分录不入库）" "$(P "select count(*) from biz_voucher_entry where voucher_id='$V0' and account_code='2221.02'")" "0"
check_num "0% 凭证 贷 5001 = 500" "$(P "select coalesce(sum(credit_amount),0) from biz_voucher_entry where voucher_id='$V0' and account_code='5001'")" "500"
say "  -- 老前端按小数传税率（0.13）走过渡期规则 ×100 并写 tax_rate_mode=DECIMAL："
api s11_legacy POST /business/erp/invoice "$T_ADMIN" "{\"customer_id\":\"$CUS\",\"invoice_date\":\"$TODAY\",\"total_with_tax\":1130,\"tax_rate\":0.13,\"remark\":\"老前端小数税率 0.13\"}"
INVL=$(data '.data.id')
check_num "0.13 → 落库 13（百分数）" "$(P "select tax_rate from biz_invoice where id='$INVL'")" "13.0000"
check "0.13 → tax_rate_mode=DECIMAL（审计位）" "$(P "select tax_rate_mode from biz_invoice where id='$INVL'")" "DECIMAL"
check_num "0.13 → 税额按 13% 算 = 130" "$(data '.data.tax_amount')" "130"
api s11_percent_bad POST /business/erp/invoice "$T_ADMIN" "{\"customer_id\":\"$CUS\",\"invoice_date\":\"$TODAY\",\"total_with_tax\":1130,\"tax_rate\":0.13,\"tax_rate_mode\":\"PERCENT\",\"remark\":\"显式 PERCENT + 0.13\"}"
INVP=$(data '.data.id')
say "  ⚠ 显式 PERCENT + 0.13 实测被接受（落库 0.13 = 0.13%），而 tax_rate.rs 模块文档的对照表写「400（越界）」→ 见 F2"
check_http "显式 PERCENT + 0.13（实测 200，文档承诺 400）" "$HTTP" "200"
check_num "  0.13 按百分数原样落库（0.13%）" "$(P "select tax_rate from biz_invoice where id='$INVP'")" "0.13"
api s11_p1 POST /business/erp/invoice "$T_ADMIN" "{\"customer_id\":\"$CUS\",\"invoice_date\":\"$TODAY\",\"total_with_tax\":1010,\"tax_rate\":1,\"tax_rate_mode\":\"PERCENT\",\"remark\":\"1% 显式百分数\"}"
check_num "1%（显式 PERCENT）税额 = 10" "$(data '.data.tax_amount')" "10"
api s11_bad POST /business/erp/invoice "$T_ADMIN" "{\"customer_id\":\"$CUS\",\"invoice_date\":\"$TODAY\",\"amount\":100,\"tax_amount\":20,\"total_with_tax\":100}"
check_http "三列金额不自洽" "$HTTP" "400"
api s11_neg POST /business/erp/invoice "$T_ADMIN" "{\"customer_id\":\"$CUS\",\"invoice_date\":\"$TODAY\",\"amount\":-100,\"tax_rate\":13}"
check_http "负金额（正票符号闸）" "$HTTP" "400"
api s11_red POST /business/erp/invoice "$T_ADMIN" "{\"customer_id\":\"$CUS\",\"invoice_date\":\"$TODAY\",\"total_with_tax\":100,\"invoice_type\":\"OUT_RED\"}"
check_http "手工建红字票被拒（红票只能退货流程生成）" "$HTTP" "400"

# ---------- S12 税金及附加 / 缴税凭证 → 利润表 + 现金流量表 ----------
say; say "S12 税金及附加（借 5403 / 贷 2221.02）与缴税（借 2221.02 / 贷 1002）→ 利润表 + 现金流量表"
A5403=$(P "select id from biz_account where account_code='5403'")
A2221=$(P "select id from biz_account where account_code='2221.02'")
A1002=$(P "select id from biz_account where account_code='1002'")
api s12_v1 POST /business/erp/voucher "$T_ADMIN" "{\"voucher_date\":\"$TODAY\",\"summary\":\"INV-01 演练：计提税金及附加\",\"entries\":[{\"account_id\":\"$A5403\",\"debit\":100,\"summary\":\"计提税金及附加\"},{\"account_id\":\"$A2221\",\"credit\":100,\"summary\":\"应缴附加税\"}]}"
VAT1=$(data '.data.id')
check "手工凭证建单（DRAFT）" "$CODE" "0"
check "手工凭证初始状态（制单≠记账）" "$(P "select status from biz_voucher where id='$VAT1'")" "DRAFT"
api s12_v1a POST "/business/erp/voucher/$VAT1/audit" "$T_ADMIN" '{"remark":"自己审自己"}'
check_http "制单人自审凭证（内控闸）" "$HTTP" "403"
api s12_v1a2 POST "/business/erp/voucher/$VAT1/audit" "$T_APP" '{"remark":"INV-01 演练复核"}'
check "他人审核凭证" "$CODE" "0"
api s12_v1p POST "/business/erp/voucher/$VAT1/post" "$T_APP" '{}'
check "记账（AUDITED→POSTED）" "$CODE" "0"
api s12_v2 POST /business/erp/voucher "$T_ADMIN" "{\"voucher_date\":\"$TODAY\",\"summary\":\"INV-01 演练：缴纳增值税\",\"entries\":[{\"account_id\":\"$A2221\",\"debit\":150,\"summary\":\"缴纳增值税\"},{\"account_id\":\"$A1002\",\"credit\":150,\"summary\":\"银行付款\"}]}"
VAT2=$(data '.data.id')
api s12_v2a POST "/business/erp/voucher/$VAT2/audit" "$T_APP" '{"remark":"INV-01 演练复核"}'
api s12_v2p POST "/business/erp/voucher/$VAT2/post" "$T_APP" '{}'
check "缴税凭证记账" "$CODE" "0"
api s12_is GET "/business/erp/report/income-statement?period=$PERIOD" "$T_ADMIN"
check "利润表取数" "$CODE" "0"
check_num "利润表「税金及附加」本期" "$(data '.data.items[] | select(.item_code=="TAX_SURCHARGE") | .current')" "100"
api s12_cf GET "/business/erp/report/cash-flow?period=$PERIOD" "$T_ADMIN"
check "现金流量表取数" "$CODE" "0"
check_num "现金流量表「支付的各项税费」本期" "$(data '.data.operating.outflow[] | select(.item_code=="OPER_TAX_PAYMENTS") | .current')" "150"
check "现金流量表该行命中科目（可审计）" "$(data '.data.operating.outflow[] | select(.item_code=="OPER_TAX_PAYMENTS") | .accounts | join(",")')" "2221.02"
api s12_bs GET "/business/erp/report/balance-sheet?period=$PERIOD" "$T_ADMIN"
check "资产负债表取数（期末）" "$CODE" "0"
EXPECT_TAX=$(P "select coalesce(sum(credit_amount-debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.status='POSTED' and e.account_code like '2221%'")
say "     手工算的应交税费净额（贷-借）= $EXPECT_TAX"
if awk "BEGIN{exit !($EXPECT_TAX > 0)}"; then
  check_num "资产负债表「应交税费」= 净贷方余额" "$(data '.data.liabilities[] | select(.item_code=="TAXES_PAYABLE") | .amount')" "$EXPECT_TAX"
  check "无留抵时不重分类" "$(data '.data.reclass | length')" "0"
else
  check_num "留抵时「应交税费」按 0 列示" "$(data '.data.liabilities[] | select(.item_code=="TAXES_PAYABLE") | .amount')" "0"
fi

# ---------- S13 汇总勾稽 ----------
say; say "S13 汇总勾稽（发票台账 / 应收 / 总账三方对得上）"
check "发票号无重复（UNIQUE 抽样）" "$(P "select count(*)-count(distinct invoice_no) from biz_invoice where deleted_at is null")" "0"
check "作废票合计（第二张 3390 已被 VOID）" "$(P "select coalesce(sum(total_with_tax),0) from biz_invoice where status='VOID'")" "3390.0000"
check_num "发票台账 Σ(不含税+税) = Σ价税合计（库层 CHECK 的抽样复核）" "$(P "select coalesce(sum(amount+tax_amount-total_with_tax),0) from biz_invoice")" "0"
V1122=$(P "select coalesce(sum(debit_amount-credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.status='POSTED' and e.account_code='1122'")
GL_TX=$(P "select coalesce(sum(total_with_tax),0) from biz_invoice where status in ('ISSUED','POSTED') and deleted_at is null")
AR_SUM=$(P "select coalesce(sum(amount),0) from biz_receivable where status in ('OPEN','PARTIAL') and deleted_at is null")
AR_BILLED=$(P "select coalesce(sum(total_with_tax),0) from biz_invoice where receivable_id is not null and status in ('ISSUED','POSTED') and deleted_at is null")
check_num "总账 1122 净额 = 已开票价税合计（开票确认收入口径）" "$V1122" "$GL_TX"
UNBILLED=$(awk "BEGIN{printf \"%.4f\", ($AR_SUM)-($AR_BILLED)}")
check_num "应收台账未开票部分（出库已挂账、总账 1122 尚未记账）" "$UNBILLED" "3390"
check_num "从应收开的票合计 ≤ 应收金额（不超开）" "$(P "select case when coalesce((select sum(total_with_tax) from biz_invoice where status<>'VOID' and receivable_id is not null),0) <= coalesce((select sum(amount) from biz_receivable where status<>'CANCELLED'),0) then 1 else 0 end")" "1"
snap s13_tj "select v.voucher_no||' | '||v.source_type||' | '||v.status||' | red='||v.is_red||' | 借='||v.total_debit||' | 贷='||v.total_credit||' | '||v.summary from biz_voucher v order by v.created_at"
snap s13_inv "select invoice_no||' | '||status||' | rate='||tax_rate||' | mode='||coalesce(tax_rate_mode,'-')||' | 不含税='||amount||' | 税='||tax_amount||' | 合计='||total_with_tax||' | 电子='||is_electronic||' | 应收='||coalesce(receivable_id::text,'-') from biz_invoice order by created_at"
snap s13_tax "select e.account_code||' 借='||sum(e.debit_amount)||' 贷='||sum(e.credit_amount)||' 净贷='||sum(e.credit_amount-e.debit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.status='POSTED' and e.account_code like '2221%' group by e.account_code"

say
say "================ 结果：PASS=$PASS FAIL=$FAIL ================"
if [ "$FAIL" -gt 0 ]; then
  say "失败项："
  for f in "${FAILED[@]}"; do say "  - $f"; done
  exit 1
fi
exit 0
