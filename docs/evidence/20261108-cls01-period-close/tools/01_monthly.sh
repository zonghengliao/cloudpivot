#!/usr/bin/env bash
# =============================================================================
# CLS-01 ① 建 1-11 月业务数据 + 逐月结账（真后端 + 真 token curl + 真库断言）
#   前置：bash 00_env.sh（空库 cloudpivot_cls01 + 实例 127.0.0.1:18228）
#   运行：bash 01_monthly.sh
# 每月口径：
#   · 采购：无源入库 20 件 × 100（13%）→ 过账审批（stock_in_post）→ 库存/进项税/应付
#   · 折旧：2026-02 起每月计提 760（家具 48000 / 60 期 / 残值 5%）
#   · 费用：1-10 月手工凭证（借 5602.03 + 借 2221.01 / 贷 2241.01）；
#           11 月走真实报销链（建单 → 提交 → 直属上级审批 → 计提凭证）
#   · 销售：2/4/6/8/10 月各 1 单：SO 10 × 600（13%）→ 审批 → 发货过账 → 开票（收入凭证）
#   · 每月末：结账（POST /period/{id}/close）
# 产出：/tmp/cls01_raw/*.json（每步响应）+ *.txt（库快照）
# =============================================================================
set -uo pipefail
DB=${CLS01_DB:-cloudpivot_cls01}
PORT=${CLS01_PORT:-18228}
BASE="http://127.0.0.1:$PORT/api"
RAW=${CLS01_RAW:-/tmp/cls01_raw}
mkdir -p "$RAW"

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
check_in() {
  local name=$1 actual=$2; shift 2
  local s
  for s in "$@"; do
    if printf '%s' "$actual" | grep -q -- "$s"; then ok "$name 含「$s」"; else bad "$name 不含「$s」：$(printf '%s' "$actual" | head -c 200)"; fi
  done
}
check_num() {
  local name=$1 a=${2:-0} b=${3:-0}
  if awk "BEGIN{exit !( ($a)-($b) < 0.0001 && ($b)-($a) < 0.0001 )}"; then ok "$name = $a"; else bad "$name = $a（期望 $b）"; fi
}

# ---------- HTTP 原语 ----------
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
    try=$((try+1)); sleep 4
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
}

TODAY=$(TZ=Asia/Shanghai date +%F)
MAT=d1370003-0000-4000-8000-000000000002   # M-W1A-002 原料B（种子，13%，非一物一码）
WH=d1370001-0000-4000-8000-000000000001    # WH01 总仓（种子）
CUS=d0260000-0000-4000-8000-000000000002   # CUS-0002 上海云枢贸易有限公司（种子）
SUP=$(P "select id from biz_supplier order by supplier_code limit 1")
ACC_OFFICE=$(P "select id from biz_account where account_code='5602.03' and deleted_at is null")
ACC_ITAX=$(P "select id from biz_account where account_code='2221.01' and deleted_at is null")
ACC_EMP=$(P "select id from biz_account where account_code='2241.01' and deleted_at is null")
ETYPE_OFFICE=$(P "select id from biz_expense_type where type_code='OFFICE'")
CAT_FURN=$(P "select id from biz_asset_category where category_code='FURNITURE'")
pid_of() { P "select id from biz_accounting_period where period_code='$1'"; }

say "== 环境：$BASE 库=$DB 今天=$TODAY"
say "   git_head=$(cd /opt/cloudpivot && git rev-parse HEAD)"
say "   binary_sha256=$(sha256sum /opt/cloudpivot/backend/target/debug/cloudpivot | cut -d' ' -f1)"

# ---------- S0 认证 / 组织 ----------
say; say "S0 认证与组织（admin 制单 + cls01_approver 审批 + cls01_clerk 报销人 + 财务部）"
api s0_login POST /system/auth/login "" '{"username":"admin","password":"Admin@123456"}'
check_http "种子 admin 登录" "$HTTP" "200"
T0=$(data '.data.access_token')
api s0_pwd PUT /system/auth/password "$T0" '{"old_password":"Admin@123456","new_password":"Admin@Cls01!2026"}'
check "admin 首登改密" "$CODE" "0"
T_ADMIN=$(login admin 'Admin@Cls01!2026' admin) || { say "FATAL: admin 改密后登录失败"; exit 1; }
ok "admin token 已取得"

R_FIN=$(P "select id from sys_role where role_code='FINANCE_MANAGER'")
R_GM=$(P "select id from sys_role where role_code='GENERAL_MANAGER'")
R_SALES=$(P "select id from sys_role where role_code='SALES_SUPERVISOR'")
R_WH=$(P "select id from sys_role where role_code='WAREHOUSE_MANAGER'")
R_DEPT=$(P "select id from sys_role where role_code='DEPT_LEADER'")
R_EMP=$(P "select id from sys_role where role_code='EMPLOYEE'")
R_SA=$(P "select id from sys_role where role_code='SUPER_ADMIN'")

api s0_dept POST /system/dept "$T_ADMIN" '{"dept_code":"CLS01-FIN","dept_name":"财务部（CLS01）","status":1,"sort_order":10}'
DEPT=$(data '.data.id')
check "财务部创建" "$CODE" "0"
check "部门 id 非空" "$([ -n "$DEPT" ] && echo yes || echo no)" "yes"

api s0_app POST /system/user "$T_ADMIN" "{\"username\":\"cls01_approver\",\"password\":\"Cls01@Mgr123\",\"real_name\":\"CLS01 审批人\",\"dept_id\":\"$DEPT\",\"role_ids\":[\"$R_FIN\",\"$R_GM\",\"$R_SALES\",\"$R_WH\",\"$R_DEPT\",\"$R_SA\"],\"status\":1,\"remark\":\"CLS-01 演练审批人\"}"
check "审批人账号创建" "$CODE" "0"
APP_ID=$(data '.data.id')
T_APP=$(login cls01_approver 'Cls01@Mgr123' approver) || { say "FATAL: 审批人登录失败"; exit 1; }

api s0_clerk POST /system/user "$T_ADMIN" "{\"username\":\"cls01_clerk\",\"password\":\"Cls01@Staff123\",\"real_name\":\"CLS01 报销人\",\"dept_id\":\"$DEPT\",\"role_ids\":[\"$R_EMP\"],\"status\":1,\"remark\":\"CLS-01 演练报销人\"}"
check "报销人账号创建" "$CODE" "0"
CLERK_ID=$(data '.data.id')
T_CLERK=$(login cls01_clerk 'Cls01@Staff123' clerk) || { say "FATAL: 报销人登录失败"; exit 1; }

api s0_dept_upd PUT "/system/dept/$DEPT" "$T_ADMIN" "{\"dept_code\":\"CLS01-FIN\",\"dept_name\":\"财务部（CLS01）\",\"leader_id\":\"$APP_ID\",\"status\":1,\"sort_order\":10}"
check "部门负责人 = 审批人（直属上级链路）" "$CODE" "0"
ok "T_ADMIN id=$(P "select id from sys_user where username='admin'") / T_APP id=$APP_ID / T_CLERK id=$CLERK_ID"

# ---------- S1 期间 ----------
say; say "S1 期间：2026 全部 OPEN（迁移种子）；批量开账 2027（年结前置 ⑤）"
check "2026 期间数" "$(P "select count(*) from biz_accounting_period where period_year=2026")" "12"
check "2026 OPEN 数" "$(P "select count(*) from biz_accounting_period where period_year=2026 and status='OPEN'")" "12"
api s1_open POST /business/erp/period/batch-open "$T_ADMIN" '{"from":"2027-01","to":"2027-12","remark":"CLS-01 演练：年结前置，提前开好 2027 全年"}'
check "批量开账 2027 全年" "$CODE" "0"
check "2027 期间数" "$(P "select count(*) from biz_accounting_period where period_year=2027")" "12"
check "2027 OPEN 数" "$(P "select count(*) from biz_accounting_period where period_year=2027 and status='OPEN'")" "12"

# ---------- S2 固定资产建卡（折旧源） ----------
say; say "S2 固定资产建卡：办公家具 48000 / 60 期 / 残值 5% → 月折旧 760，2026-02 起提"
api s2_asset POST /business/erp/asset "$T_ADMIN" "{\"asset_name\":\"CLS01 办公家具\",\"category_id\":\"$CAT_FURN\",\"use_dept_id\":\"$DEPT\",\"original_value\":48000,\"purchase_date\":\"2026-01-10\",\"source_type\":\"MANUAL\",\"buy_mode\":\"DIRECT_BUY\",\"pay_mode\":\"CREDIT\",\"supplier_id\":\"$SUP\",\"remark\":\"CLS-01 演练固定资产\"}"
ASSET=$(data '.data.id'); ASSETNO=$(data '.data.asset_no')
if [ "$CODE" = "0" ] && [ -n "$ASSET" ]; then ok "资产建卡 $ASSETNO id=$ASSET"; else bad "资产建卡 http=$HTTP $(printf '%s' "$BODY" | head -c 300)"; fi
check_num "固定资产原价落账（借 1601）" "$(P "select coalesce(sum(e.debit_amount),0) from biz_voucher_entry e where e.account_code='1601'")" "48000"
check_num "购置凭证贷 2202 应付账款" "$(P "select coalesce(sum(e.credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where e.account_code='2202' and v.source_type='FIXED_ASSET_ACQ'")" "48000"
snap s2_asset_dep "select period_code||' '||status||' '||total_amount from biz_depreciation_batch order by period_code"

# ---------- S3 逐月业务 + 结账 ----------
mk_purchase() { # mk_purchase <m> <date>
  local m=$1 d=$2
  api "s3_${m}_si" POST /business/erp/stock-in "$T_ADMIN" "{\"supplier_id\":\"$SUP\",\"is_sourceless\":true,\"warehouse_id\":\"$WH\",\"in_date\":\"$d\",\"remark\":\"CLS-01 $m 月采购入库\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":20,\"price_excl\":100,\"tax_rate\":13}]}"
  local si; si=$(data '.data.header.id')
  if [ "$CODE" != "0" ] || [ -z "$si" ] || [ "$si" = "null" ]; then bad "$m 月入库建单 http=$HTTP $(printf '%s' "$BODY" | head -c 200)"; return 1; fi
  api "s3_${m}_si_post" PATCH "/business/erp/stock-in/$si/status" "$T_ADMIN" '{"status":"POSTED"}'
  local inst; inst=$(P "select approval_instance_id from biz_stock_in where id='$si'")
  if [ -n "$inst" ]; then
    local task; task=$(P "select id from biz_flow_task where instance_id='$inst' and status='PENDING' order by created_at limit 1")
    api "s3_${m}_si_appr" POST "/business/flow/task/$task/approve" "$T_APP" "{\"comment\":\"CLS-01 $m 月入库过账批准\"}"
    [ "$CODE" = "0" ] || bad "$m 月入库过账审批 code=$CODE $(msg)"
  fi
  check "$m 月入库单已过账" "$(P "select status from biz_stock_in where id='$si'")" "POSTED"
}

mk_dep() { # mk_dep <m>
  local m=$1
  api "s3_${m}_dep" POST /business/erp/depreciation/run "$T_ADMIN" "{\"period_code\":\"2026-$m\",\"remark\":\"CLS-01 $m 月折旧\"}"
  if [ "$CODE" = "0" ]; then
    check "$m 月折旧批次金额" "$(data '.data.total_amount')" "760.0000"
    check "$m 月折旧凭证已出" "$([ -n "$(data '.data.voucher_id')" ] && echo yes)" "yes"
  else
    bad "$m 月折旧计提 code=$CODE $(msg)"
  fi
}

mk_expense_manual() { # mk_expense_manual <m> <date>  （1-10 月：手工凭证等价于报销计提）
  local m=$1 d=$2
  api "s3_${m}_mv" POST /business/erp/voucher "$T_ADMIN" "{\"voucher_date\":\"$d\",\"summary\":\"CLS-01 $m 月办公费报销计提\",\"entries\":[{\"account_id\":\"$ACC_OFFICE\",\"debit\":471.70,\"summary\":\"办公费\"},{\"account_id\":\"$ACC_ITAX\",\"debit\":28.30,\"summary\":\"进项税\"},{\"account_id\":\"$ACC_EMP\",\"credit\":500,\"summary\":\"应付员工垫付\"}]}"
  local vid; vid=$(data '.data.id')
  if [ "$CODE" != "0" ] || [ -z "$vid" ]; then bad "$m 月费用凭证建单 http=$HTTP $(printf '%s' "$BODY" | head -c 200)"; return 1; fi
  check "$m 月费用凭证初始 DRAFT" "$(data '.data.status')" "DRAFT"
  api "s3_${m}_mv_audit" POST "/business/erp/voucher/$vid/audit" "$T_APP" '{"remark":"CLS-01 复核"}'
  [ "$CODE" = "0" ] || bad "$m 月费用凭证审核 code=$CODE $(msg)"
  api "s3_${m}_mv_post" POST "/business/erp/voucher/$vid/post" "$T_APP" '{}'
  check "$m 月费用凭证已记账" "$(P "select status from biz_voucher where id='$vid'")" "POSTED"
}

mk_sale() { # mk_sale <m> <date>
  local m=$1 d=$2
  api "s3_${m}_so" POST /business/erp/sales-order "$T_ADMIN" "{\"customer_id\":\"$CUS\",\"order_date\":\"$d\",\"remark\":\"CLS-01 $m 月销售\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":10,\"price\":600,\"tax_rate\":13}]}"
  local so; so=$(data '.data.id')
  if [ "$CODE" != "0" ] || [ -z "$so" ]; then bad "$m 月销售订单建单 http=$HTTP $(printf '%s' "$BODY" | head -c 200)"; return 1; fi
  check_num "$m 月订单价税合计" "$(data '.data.total_with_tax')" "6780"
  api "s3_${m}_so_sub" POST "/business/erp/sales-order/$so/submit-approval" "$T_ADMIN" "{\"reason\":\"CLS-01 $m 月销售送审\"}"
  local inst; inst=$(P "select approval_instance_id from biz_sales_order where id='$so'")
  local task; task=$(P "select id from biz_flow_task where instance_id='$inst' and status='PENDING' order by created_at limit 1")
  api "s3_${m}_so_appr" POST "/business/flow/task/$task/approve" "$T_APP" "{\"comment\":\"CLS-01 $m 月销售批准\"}"
  check "$m 月订单 APPROVED" "$(P "select status from biz_sales_order where id='$so'")" "APPROVED"

  api "s3_${m}_sh" POST /business/erp/shipment "$T_ADMIN" "{\"so_id\":\"$so\",\"warehouse_id\":\"$WH\",\"ship_date\":\"$d\"}"
  local sh; sh=$(data '.data.id')
  api "s3_${m}_sh_p1" PATCH "/business/erp/shipment/$sh/status" "$T_ADMIN" '{"target_status":"PENDING_POST"}'
  api "s3_${m}_sh_p2" PATCH "/business/erp/shipment/$sh/status" "$T_ADMIN" '{"target_status":"POSTED"}'
  check "$m 月发货已过账" "$(P "select status from biz_shipment where id='$sh'")" "POSTED"
  local ar; ar=$(P "select id from biz_receivable where source_type='SALES_OUT' and source_id=(select stock_out_id from biz_shipment where id='$sh')")
  check_num "$m 月自动挂应收" "$(P "select amount from biz_receivable where id='$ar'")" "6780.0000"

  api "s3_${m}_inv" POST /business/erp/invoice "$T_ADMIN" "{\"receivable_id\":\"$ar\",\"invoice_date\":\"$d\",\"total_with_tax\":6780,\"tax_rate\":13,\"remark\":\"CLS-01 $m 月销售发票\"}"
  local inv; inv=$(data '.data.id')
  upload "s3_${m}_up" "$T_ADMIN" INVOICE "$inv" INVOICE_SCAN /tmp/cls01_fake.pdf
  check "$m 月发票影像上传" "$ACCODE" "0"
  api "s3_${m}_inv_issue" PATCH "/business/erp/invoice/$inv/status" "$T_ADMIN" '{"status":"ISSUED"}'
  check "$m 月开票 ISSUED" "$CODE" "0"
  api "s3_${m}_inv_post" PATCH "/business/erp/invoice/$inv/status" "$T_ADMIN" '{"status":"POSTED"}'
  check "$m 月发票归档" "$CODE" "0"
  check_num "$m 月收入凭证 5001" "$(P "select coalesce(sum(e.credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='INVOICE' and v.period='2026-$m' and e.account_code='5001'")" "6000"
}

# 11 月走真实报销链
mk_expense_module() {
  api s3_11_exp POST /business/erp/expense "$T_CLERK" "{\"title\":\"CLS-01 11 月办公费\",\"expense_date\":\"2026-11-05\",\"dept_id\":\"$DEPT\",\"remark\":\"CLS-01 报销链实证\",\"items\":[{\"expense_type_id\":\"$ETYPE_OFFICE\",\"with_tax_amount\":500,\"tax_rate\":0.06,\"invoice_no\":\"CLS01-2026-11\",\"invoice_date\":\"2026-11-05\",\"invoice_type\":\"VAT_SPECIAL\",\"summary\":\"11 月办公用品\"}]}"
  local eid; eid=$(data '.data.report.id')
  if [ "$CODE" != "0" ] || [ -z "$eid" ] || [ "$eid" = "null" ]; then bad "11 月报销单建单 http=$HTTP $(printf '%s' "$BODY" | head -c 300)"; return 1; fi
  ok "11 月报销单 $(data '.data.report.report_no') id=$eid 价税合计=$(data '.data.report.total_with_tax') 档位=$(data '.data.tier_label')"
  # 附件闸（ATT/V0347 规则 EXPENSE_REPORT/SUBMIT ≥1 个 pdf/jpg/png/ofd）：先裸提交验证闸门
  api s3_11_exp_sub0 POST "/business/erp/expense/$eid/submit" "$T_CLERK" '{}'
  check_http "无发票影像提交报销（附件闸）" "$HTTP" "400"
  check_in "缺附件文案" "$(msg)" "发票影像" "费用凭证"
  check "提交失败后仍是 DRAFT" "$(P "select status from biz_expense_report where id='$eid'")" "DRAFT"
  # 上传人只能是具备 business:attachment:upload 的角色（EMPLOYEE 没有 → 见证据 README §4 F2）
  upload s3_11_up "$T_APP" EXPENSE_REPORT "$eid" INVOICE_SCAN /tmp/cls01_fake.pdf
  check "报销发票影像上传（EXPENSE_REPORT，财务代传）" "$ACCODE" "0"
  api s3_11_exp_sub POST "/business/erp/expense/$eid/submit" "$T_CLERK" '{}'
  check "11 月报销提交审批" "$CODE" "0"
  local inst; inst=$(P "select approval_instance_id from biz_expense_report where id='$eid'")
  local task; task=$(P "select id from biz_flow_task where instance_id='$inst' and status='PENDING' order by created_at limit 1")
  api s3_11_exp_appr POST "/business/flow/task/$task/approve" "$T_APP" '{"comment":"CLS-01 11 月报销批准"}'
  check "11 月报销审批通过" "$CODE" "0"
  check "11 月报销 APPROVED" "$(P "select status from biz_expense_report where id='$eid'")" "APPROVED"
  check_num "11 月报销计提凭证 借 5602.03" "$(P "select coalesce(sum(e.debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='EXPENSE_REPORT' and e.account_code='5602.03'")" "471.7000"
  check_num "11 月报销计提凭证 借 2221.01 进项税" "$(P "select coalesce(sum(e.debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='EXPENSE_REPORT' and e.account_code='2221.01'")" "28.3000"
  check_num "11 月报销计提凭证 贷 2241.01" "$(P "select coalesce(sum(e.credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='EXPENSE_REPORT' and e.account_code='2241.01'")" "500.0000"
}

close_month() { # close_month <m>
  local m=$1 pid; pid=$(pid_of "2026-$m")
  api "s4_${m}_check" GET "/business/erp/period/$pid/close-check" "$T_ADMIN"
  local blockers; blockers=$(data '.data.blockers | length')
  check "$m 月结账预检 blocker 数" "${blockers:-x}" "0"
  api "s4_${m}_close" POST "/business/erp/period/$pid/close" "$T_ADMIN" "{\"reason\":\"CLS-01 $m 月结账（演练）\"}"
  if [ "$CODE" != "0" ]; then bad "$m 月结账 code=$CODE $(printf '%s' "$BODY" | head -c 300)"; return 1; fi
  check "$m 月状态" "$(P "select status from biz_accounting_period where period_code='2026-$m'")" "CLOSED"
}

printf '%%PDF-1.4\n%% CLS-01 fake invoice pdf\n' >/tmp/cls01_fake.pdf

say; say "S3 逐月：业务数据 → 折旧 → 结账（1-11 月）"
for m in 01 02 03 04 05 06 07 08 09 10 11; do
  say "-- 2026-$m --"
  d="2026-$m-05"
  mk_purchase "$m" "$d"
  case "$m" in
    02|04|06|08|10) mk_sale "$m" "$d" ;;
  esac
  if [ "$m" = "11" ]; then
    mk_expense_module
  else
    mk_expense_manual "$m" "$d"
  fi
  if [ "$m" != "01" ]; then mk_dep "$m"; fi
  close_month "$m"
done

# ---------- S5 1-11 月汇总快照 ----------
say; say "S5 1-11 月凭证与损益汇总"
snap s5_month_vouchers "select v.period||' '||v.voucher_type||' '||v.voucher_no||' '||v.summary||' 借='||v.total_debit from biz_voucher v where v.deleted_at is null order by v.period, v.voucher_no"
snap s5_pl_by_month "select v.period||' 损益发生额(借-贷)='||coalesce(sum(e.debit_amount-e.credit_amount),0) from biz_voucher v join biz_voucher_entry e on e.voucher_id=v.id join biz_account a on a.account_code=e.account_code where a.account_category='PROFIT_LOSS' group by v.period order by v.period"
snap s5_balances "select account_code||' 借='||coalesce(sum(debit_amount),0)||' 贷='||coalesce(sum(credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.status='POSTED' group by account_code order by account_code"
check "1-11 月全部 CLOSED" "$(P "select count(*) from biz_accounting_period where period_year=2026 and status='CLOSED'")" "11"
check "12 月仍 OPEN（待结）" "$(P "select status from biz_accounting_period where period_code='2026-12'")" "OPEN"
check_num "库存商品 1405 余额（11×20 - 5×10 = 170 件 × 100）" "$(P "select coalesce(sum(debit_amount-credit_amount),0) from biz_voucher_entry e where e.account_code='1405'")" "17000"
check_num "固定资产原价 1601" "$(P "select coalesce(sum(debit_amount-credit_amount),0) from biz_voucher_entry e where e.account_code='1601'")" "48000"
check_num "累计折旧 1602（贷 10 期 × 760）" "$(P "select coalesce(sum(credit_amount-debit_amount),0) from biz_voucher_entry e where e.account_code='1602'")" "7600"
check_num "主营业务收入 5001（5 单 × 6000）" "$(P "select coalesce(sum(credit_amount-debit_amount),0) from biz_voucher_entry e where e.account_code='5001'")" "30000"
check_num "主营业务成本 6401（5 单 × 1000）" "$(P "select coalesce(sum(debit_amount-credit_amount),0) from biz_voucher_entry e where e.account_code='6401'")" "5000"
check_num "管理费用-办公费 5602.03（11 × 471.70）" "$(P "select coalesce(sum(debit_amount-credit_amount),0) from biz_voucher_entry e where e.account_code='5602.03'")" "5188.7000"
check_num "管理费用-折旧费 5602.01（10 × 760）" "$(P "select coalesce(sum(debit_amount-credit_amount),0) from biz_voucher_entry e where e.account_code='5602.01'")" "7600"
check_num "进项税 2221.01（2860 + 311.30）" "$(P "select coalesce(sum(debit_amount-credit_amount),0) from biz_voucher_entry e where e.account_code='2221.01'")" "3171.3000"
check_num "销项税 2221.02（5 × 780）" "$(P "select coalesce(sum(credit_amount-debit_amount),0) from biz_voucher_entry e where e.account_code='2221.02'")" "3900"
check_num "应付账款 2202（11×2260 + 48000）" "$(P "select coalesce(sum(credit_amount-debit_amount),0) from biz_voucher_entry e where e.account_code='2202'")" "72860"
check "结转前 3103 无发生额（月结不做损益结转）" "$(P "select count(*) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where e.account_code='3103'")" "0"
check "结转前 3104 无发生额" "$(P "select count(*) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where e.account_code='3104'")" "0"

say
say "===== 01_monthly 小计：PASS=$PASS FAIL=$FAIL ====="
if [ "$FAIL" -gt 0 ]; then printf '  FAILED: %s\n' "${FAILED[@]}"; fi
