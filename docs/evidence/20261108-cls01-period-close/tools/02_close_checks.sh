#!/usr/bin/env bash
# =============================================================================
# CLS-01 ② 11 月结账验收：试算平衡 / 资产负债表平衡 / 已结账不可记账 / 反结账再结
#   前置：bash 01_monthly.sh（1-11 月已结账）
#   运行：bash 02_close_checks.sh
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

api() {
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
login() {
  local try=0
  while [ "$try" -lt 8 ]; do
    api "login_$3_$try" POST /system/auth/login "" "{\"username\":\"$1\",\"password\":\"$2\"}"
    if [ "$HTTP" = "200" ] && [ "$CODE" = "0" ]; then data '.data.access_token'; return 0; fi
    try=$((try+1)); sleep 4
  done
  say "FATAL 登录失败 $1"; return 1
}
snap() {
  local f="$RAW/$1.txt"; shift
  : >"$f"
  local sql
  for sql in "$@"; do echo "--- $sql" >>"$f"; P "$sql" >>"$f" 2>&1 || true; done
}
pid_of() { P "select id from biz_accounting_period where period_code='$1'"; }
ACC_1002=$(P "select id from biz_account where account_code='1002'")
ACC_1122=$(P "select id from biz_account where account_code='1122'")

T_ADMIN=$(login admin 'Admin@Cls01!2026' admin) || exit 1
T_APP=$(login cls01_approver 'Cls01@Mgr123' approver2) || exit 1

say "== 环境：$BASE 库=$DB"

# ---------- T1 试算平衡（2026-11） ----------
say; say "T1 试算平衡（GET /business/erp/ledger/trial-balance?period=2026-11）"
api t1_tb GET "/business/erp/ledger/trial-balance?period=2026-11" "$T_ADMIN"
check "科目余额表取数" "$CODE" "0"
check "借贷平衡标记 balanced" "$(data '.data.balanced')" "true"
check_num "本期借方合计 = 本期贷方合计" "$(data '.data.total_period_debit')" "$(data '.data.total_period_credit')"
check_num "期末借方合计 = 期末贷方合计" "$(data '.data.total_closing_debit')" "$(data '.data.total_closing_credit')"
say "     本期发生额 借=$(data '.data.total_period_debit') 贷=$(data '.data.total_period_credit')；期末余额 借=$(data '.data.total_closing_debit') 贷=$(data '.data.total_closing_credit')"
printf '%s' "$BODY" | jq -r '.data.rows[] | select((.opening_debit+.opening_credit+.period_debit+.period_credit+.closing_debit+.closing_credit) != 0) | [.account_code, .account_name, .opening_debit, .opening_credit, .period_debit, .period_credit, .closing_debit, .closing_credit] | @tsv' >"$RAW/t1_tb_rows.txt" 2>/dev/null || true

# 期末余额逐科目（SQL 直算，与接口口径同源表）
snap t1_pl_balances "select a.account_code||' '||a.account_name||' 余额(借正贷负)='||coalesce(sum(e.debit_amount-e.credit_amount),0) from biz_account a left join biz_voucher_entry e on e.account_code=a.account_code left join biz_voucher v on v.id=e.voucher_id and v.status='POSTED' and v.voucher_date<='2026-11-30' where a.account_category='PROFIT_LOSS' and a.deleted_at is null group by a.account_code, a.account_name having coalesce(sum(e.debit_amount-e.credit_amount),0)<>0 order by a.account_code"
PL_NET=$(P "select coalesce(sum(e.credit_amount-e.debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id join biz_account a on a.account_code=e.account_code where v.status='POSTED' and a.account_category='PROFIT_LOSS' and v.voucher_date<='2026-11-30'")
check_num "11 月末损益类科目净额（贷-借；月结不结转 ⇒ 不为 0，见 README §4 F1）" "$PL_NET" "12211.30"
check_num "11 月末 3103 本年利润余额（月结不落 3103）" "$(P "select coalesce(sum(debit_amount-credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where e.account_code='3103' and v.status='POSTED'")" "0"

# ---------- T2 资产负债表平衡（2026-11） ----------
say; say "T2 资产负债表（GET /business/erp/report/balance-sheet?period=2026-11）"
api t2_bs GET "/business/erp/report/balance-sheet?period=2026-11" "$T_ADMIN"
check "资产负债表取数" "$CODE" "0"
check "资产 = 负债 + 权益（check.balanced）" "$(data '.data.check.balanced | tostring')" "true"
TOTAL_ASSETS=$(data '.data.assets[] | select(.item_code=="TOTAL_ASSETS") | .amount')
TOTAL_LE=$(data '.data.equity[] | select(.item_code=="TOTAL_LIABILITIES_AND_EQUITY") | .amount')
check_num "资产总计" "$TOTAL_ASSETS" "91300"
check_num "负债和所有者权益总计" "$TOTAL_LE" "91300"
check_num "未分配利润（3103 未结转也归并到这里）" "$(data '.data.equity[] | select(.item_code=="UNDISTRIBUTED_PROFIT") | .amount')" "12211.30"
snap t2_bs_lines "select 1"
say "     资产总计=$(data '.data.assets[] | select(.item_code=="TOTAL_ASSETS") | .amount') 负债合计=$(data '.data.liabilities[] | select(.item_code=="TOTAL_LIABILITIES") | .amount') 权益合计=$(data '.data.equity[] | select(.item_code=="TOTAL_EQUITY") | .amount')"

# ---------- T3 利润表（2026-11 本年累计） ----------
say; say "T3 利润表（GET /business/erp/report/income-statement?period=2026-11）"
api t3_is GET "/business/erp/report/income-statement?period=2026-11" "$T_ADMIN"
check "利润表取数" "$CODE" "0"
check_num "营业收入（本年累计）" "$(data '.data.items[] | select(.item_code=="REVENUE") | .ytd')" "30000"
check_num "营业成本（本年累计）" "$(data '.data.items[] | select(.item_code=="COST_OF_SALES") | .ytd')" "5000"
check_num "管理费用（本年累计）" "$(data '.data.items[] | select(.item_code=="ADMIN_EXPENSE") | .ytd')" "12788.70"
check_num "净利润（本年累计）" "$(data '.data.items[] | select(.item_code=="NET_PROFIT") | .ytd')" "12211.30"
check "未归类损益科目数（check.unmapped）" "$(data '.data.check.unmapped | length')" "0"

# ---------- T4 已结账期间不能再记账 ----------
say; say "T4 已结账期间不可记账（2026-06 已封账）"
api t4_closed POST /business/erp/voucher "$T_ADMIN" "{\"voucher_date\":\"2026-06-15\",\"summary\":\"CLS-01 违规补记（应被拒）\",\"entries\":[{\"account_id\":\"$ACC_1002\",\"debit\":100},{\"account_id\":\"$ACC_1122\",\"credit\":100}]}"
check_http "已结账期间建手工凭证" "$HTTP" "400"
check_in "拦截文案" "$(msg)" "已结账" "反结账"
check "没有落库脏凭证" "$(P "select count(*) from biz_voucher where summary like '%违规补记%'")" "0"

# ---------- T5 反结账 → 补记 → 再结账 ----------
say; say "T5 反结账 2026-11 → 补记一笔 → 重新结账（唯一合法的改账路径）"
P11=$(pid_of "2026-11")
api t5_reopen_bad POST "/business/erp/period/$P11/reopen" "$T_ADMIN" '{"reason":"太短"}'
check_http "反结账原因不足 10 字" "$HTTP" "400"
check_in "原因字数文案" "$(msg)" "10"
api t5_reopen POST "/business/erp/period/$P11/reopen" "$T_ADMIN" '{"reason":"CLS-01 演练：反结账验证改账路径"}'
check "反结账 2026-11" "$CODE" "0"
check "2026-11 状态回 OPEN" "$(P "select status from biz_accounting_period where period_code='2026-11'")" "OPEN"
api t5_mv POST /business/erp/voucher "$T_ADMIN" "{\"voucher_date\":\"2026-11-20\",\"summary\":\"CLS-01 反结账后补记（客户回款）\",\"entries\":[{\"account_id\":\"$ACC_1002\",\"debit\":200,\"summary\":\"客户回款\"},{\"account_id\":\"$ACC_1122\",\"credit\":200,\"summary\":\"客户回款\"}]}"
check "反结账后补记凭证建单" "$CODE" "0"
V=$(data '.data.id')
api t5_audit POST "/business/erp/voucher/$V/audit" "$T_APP" '{"remark":"CLS-01 复核"}'
check "补记凭证审核（不同人）" "$CODE" "0"
api t5_post POST "/business/erp/voucher/$V/post" "$T_APP" '{}'
check "补记凭证记账" "$CODE" "0"
check "补记凭证已 POSTED" "$(P "select status from biz_voucher where id='$V'")" "POSTED"
api t5_tb GET "/business/erp/ledger/trial-balance?period=2026-11" "$T_ADMIN"
check "补记后 11 月仍试算平衡" "$(data '.data.balanced')" "true"
check_num "1122 期末余额（33900 - 200）" "$(P "select coalesce(sum(debit_amount-credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where e.account_code='1122' and v.status='POSTED'")" "33700"
api t5_reclose POST "/business/erp/period/$P11/close" "$T_ADMIN" '{"reason":"CLS-01 演练：补记完成重新结账"}'
check "重新结账 2026-11" "$CODE" "0"
check "2026-11 状态" "$(P "select status from biz_accounting_period where period_code='2026-11'")" "CLOSED"
api t5_closed_again POST /business/erp/voucher "$T_ADMIN" "{\"voucher_date\":\"2026-11-25\",\"summary\":\"CLS-01 违规补记2（应被拒）\",\"entries\":[{\"account_id\":\"$ACC_1002\",\"debit\":1},{\"account_id\":\"$ACC_1122\",\"credit\":1}]}"
check_http "重新结账后再补记" "$HTTP" "400"

# ---------- T6 每月凭证/损益台账（证据用） ----------
say; say "T6 1-11 月逐月台账（每月凭证张数 / 损益发生额 / 月末损益余额）"
snap t6_monthly "select p.period_code||' | '||p.status||' | 凭证='||(select count(*) from biz_voucher v where v.period=p.period_code and v.deleted_at is null)||' | 损益发生额(借-贷)='||coalesce((select sum(e.debit_amount-e.credit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id join biz_account a on a.account_code=e.account_code where v.period=p.period_code and v.status='POSTED' and a.account_category='PROFIT_LOSS'),0) from biz_accounting_period p where p.period_year=2026 order by p.period_code"
snap t6_vouchers "select v.period||' | '||v.voucher_type||' | '||v.voucher_no||' | '||v.source_type||' | '||v.summary||' | 借='||v.total_debit from biz_voucher v where v.period like '2026-%' and v.deleted_at is null order by v.period, v.voucher_no"
snap t6_status_log "select entity_type||' | '||entity_no||' | '||coalesce(from_status,'NULL')||'->'||to_status||' | '||coalesce(operator_name,'-')||' | '||coalesce(reason,'-') from biz_status_log where entity_type in ('ACCOUNTING_PERIOD','ACCOUNTING_YEAR') order by created_at"

say
say "===== 02_close_checks 小计：PASS=$PASS FAIL=$FAIL ====="
if [ "$FAIL" -gt 0 ]; then printf '  FAILED: %s\n' "${FAILED[@]}"; fi
