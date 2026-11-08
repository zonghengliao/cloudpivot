#!/usr/bin/env bash
# =============================================================================
# CLS-01 ③ 12 月结账 → 年结（2026→2027）→ 下年期初 → 次年开账 → 跨年查询
#   前置：bash 01_monthly.sh + bash 02_close_checks.sh
#   运行：bash 03_year_end.sh
# 覆盖：结转凭证科目/借贷平衡 / 结转后损益清零 / 3103→3104 / 下年期初逐科目
#       反年结 + 重新年结 / 2027-01 记账 / 2026 报表仍可查 / 2027 期初接上年期末
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
bal() { # 科目在指定日期前的余额（借正贷负）
  P "select coalesce(sum(e.debit_amount-e.credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.status='POSTED' and v.deleted_at is null and v.voucher_date<='$2' and e.account_code='$1'"
}
ACC_1002=$(P "select id from biz_account where account_code='1002'")
ACC_5001=$(P "select id from biz_account where account_code='5001'")
ACC_2221_02=$(P "select id from biz_account where account_code='2221.02'")

T_ADMIN=$(login admin 'Admin@Cls01!2026' admin) || exit 1
T_APP=$(login cls01_approver 'Cls01@Mgr123' approver3) || exit 1

say "== 环境：$BASE 库=$DB"

# ---------- U1 12 月结账（空月） ----------
say; say "U1 12 月结账（本年最后一个月，无业务数据：验证空月可结）"
P12=$(pid_of "2026-12")
api u1_check GET "/business/erp/period/$P12/close-check" "$T_ADMIN"
check "12 月结账预检 blocker" "$(data '.data.blockers | length')" "0"
api u1_close POST "/business/erp/period/$P12/close" "$T_ADMIN" '{"reason":"CLS-01 演练：12 月封账，进入年结"}'
check "12 月结账" "$CODE" "0"
check "2026 全年 12 个月 CLOSED" "$(P "select count(*) from biz_accounting_period where period_year=2026 and status='CLOSED'")" "12"
check "结转前 12 月期末损益余额（应为全年累计 12211.30 贷方）" "$(P "select coalesce(sum(credit_amount-debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id join biz_account a on a.account_code=e.account_code where v.status='POSTED' and a.account_category='PROFIT_LOSS' and v.voucher_date<='2026-12-31'")" "12211.3000"

# ---------- U2 年结前置检查 ----------
say; say "U2 年结前置检查（GET /business/erp/year-end/check?year=2026）"
api u2_check GET "/business/erp/year-end/check?year=2026" "$T_ADMIN"
check "前置检查取数" "$CODE" "0"
check "can_close" "$(data '.data.can_close | tostring')" "true"
check "blockers 条数" "$(data '.data.blockers | length')" "0"
check "年度状态 NOT_CLOSED" "$(data '.data.state.status')" "NOT_CLOSED"
check "2027 期间就绪" "$(data '.data.state.next_year_periods_ready | tostring')" "true"
check_in "12 个月全结账计数" "$(data '.data.state.periods_closed | tostring')" "12"

# ---------- U3 年结前余额基线 ----------
say; say "U3 年结前余额基线（2026-12-31 逐科目，用于与下年期初对照）"
snap u3_pre_close "select account_code||' | '||account_category||' | 余额(借正贷负)='||coalesce(sum(debit_amount-credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id join biz_account a on a.account_code=e.account_code where v.status='POSTED' and v.voucher_date<='2026-12-31' group by account_code, account_category having coalesce(sum(debit_amount-credit_amount),0)<>0 order by account_code"
B_1002=$(bal 1002 2026-12-31); B_1122=$(bal 1122 2026-12-31); B_1405=$(bal 1405 2026-12-31)
B_1601=$(bal 1601 2026-12-31); B_1602=$(bal 1602 2026-12-31); B_2202=$(bal 2202 2026-12-31)
B_ITAX=$(bal 2221.01 2026-12-31); B_OTAX=$(bal 2221.02 2026-12-31); B_EMP=$(bal 2241.01 2026-12-31)
B_3104=$(bal 3104 2026-12-31)
check_num "1002 银行存款" "$B_1002" "200"
check_num "1122 应收账款" "$B_1122" "33700"
check_num "1405 库存商品" "$B_1405" "17000"
check_num "1601 固定资产" "$B_1601" "48000"
check_num "1602 累计折旧（贷）" "$B_1602" "-7600"
check_num "2202 应付账款（贷）" "$B_2202" "-72860"
check_num "2221.01 进项税" "$B_ITAX" "3171.30"
check_num "2221.02 销项税（贷）" "$B_OTAX" "-3900"
check_num "2241.01 员工垫付（贷）" "$B_EMP" "-5500"
check_num "3104 利润分配（年结前应为 0）" "$B_3104" "0"

# ---------- U4 年结 ----------
say; say "U4 年结 2026 → 2027（POST /business/erp/year-end/close）"
api u4_close POST /business/erp/year-end/close "$T_ADMIN" '{"year":2026,"reason":"CLS-01 演练：2026 年度结转"}'
if [ "$CODE" != "0" ]; then
  bad "年结失败 http=$HTTP code=$CODE $(printf '%s' "$BODY" | head -c 400)"
else
  ok "年结成功 凭证=$(data '.data.carry_forward_voucher_no') 净利润=$(data '.data.net_profit') 分录=$(data '.data.entry_count') 期初行=$(data '.data.opening_rows') 已结月=$(data '.data.periods_closed')"
fi
check_num "年结净利润 = 利润表 12211.30" "$(data '.data.net_profit')" "12211.30"
check "年结后状态 CLOSED" "$(data '.data.state.status')" "CLOSED"
check "2026-12 仍 CLOSED（结转凭证靠类型白名单落进已封账期间）" "$(P "select status from biz_accounting_period where period_code='2026-12'")" "CLOSED"
YE_VNO=$(data '.data.carry_forward_voucher_no')
YE_ID=$(P "select id from biz_voucher where voucher_type='YEAR_END' and period='2026-12' and deleted_at is null")

# ---------- U5 结转凭证科目与借贷 ----------
say; say "U5 结转凭证核验（科目 5001/6401/5602 结平 + 3103 本年利润 + 3104 未分配利润）"
check "结转凭证类型" "$(P "select voucher_type from biz_voucher where id='$YE_ID'")" "YEAR_END"
check "结转凭证期间" "$(P "select period from biz_voucher where id='$YE_ID'")" "2026-12"
check "结转凭证日期" "$(P "select voucher_date from biz_voucher where id='$YE_ID'")" "2026-12-31"
check "结转凭证状态" "$(P "select status from biz_voucher where id='$YE_ID'")" "POSTED"
check_num "结转凭证 Σ借 = Σ贷（单头 total_debit）" "$(P "select total_debit from biz_voucher where id='$YE_ID'")" "$(P "select total_credit from biz_voucher where id='$YE_ID'")"
check_num "结转凭证 Σ借" "$(P "select total_debit from biz_voucher where id='$YE_ID'")" "60000"
check_num "分录行 Σ借 = Σ贷（明细侧）" "$(P "select coalesce(sum(debit_amount),0) from biz_voucher_entry where voucher_id='$YE_ID'")" "$(P "select coalesce(sum(credit_amount),0) from biz_voucher_entry where voucher_id='$YE_ID'")"
check_num "借 5001 主营业务收入（结平）" "$(P "select coalesce(sum(debit_amount),0) from biz_voucher_entry where voucher_id='$YE_ID' and account_code='5001'")" "30000"
check_num "贷 6401 主营业务成本（结平）" "$(P "select coalesce(sum(credit_amount),0) from biz_voucher_entry where voucher_id='$YE_ID' and account_code='6401'")" "5000"
check_num "贷 5602.03 管理费用-办公费（结平）" "$(P "select coalesce(sum(credit_amount),0) from biz_voucher_entry where voucher_id='$YE_ID' and account_code='5602.03'")" "5188.7000"
check_num "贷 5602.01 管理费用-折旧费（结平）" "$(P "select coalesce(sum(credit_amount),0) from biz_voucher_entry where voucher_id='$YE_ID' and account_code='5602.01'")" "7600"
check_num "借 3103 本年利润（费用转入）" "$(P "select coalesce(sum(debit_amount),0) from biz_voucher_entry where voucher_id='$YE_ID' and account_code='3103' and summary like '%费用转入%'")" "17788.7000"
check_num "贷 3103 本年利润（收入转入）" "$(P "select coalesce(sum(credit_amount),0) from biz_voucher_entry where voucher_id='$YE_ID' and account_code='3103' and summary like '%收入转入%'")" "30000"
check_num "借 3103 合计（费用转入 17788.70 + 转利润分配 12211.30）" "$(P "select coalesce(sum(debit_amount),0) from biz_voucher_entry where voucher_id='$YE_ID' and account_code='3103'")" "30000"
check_num "3103 在结转凭证内的净额（借-贷）= 0（借贷同额，余额归零靠两腿相抵）" "$(P "select coalesce(sum(debit_amount-credit_amount),0) from biz_voucher_entry where voucher_id='$YE_ID' and account_code='3103'")" "0"
check_num "贷 3104 利润分配（本年利润转入）" "$(P "select coalesce(sum(credit_amount),0) from biz_voucher_entry where voucher_id='$YE_ID' and account_code='3104'")" "12211.3000"
check "结转凭证只有一张 3104 行" "$(P "select count(*) from biz_voucher_entry where voucher_id='$YE_ID' and account_code='3104'")" "1"
snap u5_ye_entries "select line_no||' | '||account_code||' '||account_name||' | 借='||debit_amount||' 贷='||credit_amount||' | '||summary from biz_voucher_entry where voucher_id='$YE_ID' order by line_no"

# ---------- U6 结转后损益清零 ----------
say; say "U6 结转后损益类科目余额 = 0 / 3103 归零 / 3104 = 净利润"
check_num "损益类科目期末余额合计（2026-12-31）" "$(P "select coalesce(sum(credit_amount-debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id join biz_account a on a.account_code=e.account_code where v.status='POSTED' and a.account_category='PROFIT_LOSS' and v.voucher_date<='2026-12-31'")" "0"
check "有余额的损益类科目数（应为 0）" "$(P "select count(*) from (select a.account_code from biz_account a left join biz_voucher_entry e on e.account_code=a.account_code left join biz_voucher v on v.id=e.voucher_id and v.status='POSTED' and v.voucher_date<='2026-12-31' where a.account_category='PROFIT_LOSS' and a.deleted_at is null group by a.account_code having coalesce(sum(e.debit_amount-e.credit_amount),0)<>0) t")" "0"
check_num "3103 本年利润余额（结转后归零）" "$(bal 3103 2026-12-31)" "0"
check_num "3104 利润分配余额（= 净利润）" "$(bal 3104 2026-12-31)" "-12211.3000"
api u6_tb GET "/business/erp/ledger/trial-balance?period=2026-12" "$T_ADMIN"
check "2026-12 试算平衡（含结转凭证）" "$(data '.data.balanced')" "true"
check_num "2026-12 期末借 = 期末贷" "$(data '.data.total_closing_debit')" "$(data '.data.total_closing_credit')"
printf '%s' "$BODY" | jq -r '.data.rows[] | select((.closing_debit+.closing_credit) != 0) | [.account_code, .account_name, .closing_debit, .closing_credit] | @tsv' >"$RAW/u6_tb_2026_12_rows.txt" 2>/dev/null || true

# ---------- U7 下年期初台账 ----------
say; say "U7 下年期初台账（biz_opening_balance 2027-01 / source=YEAR_END）"
check "2027-01 期初行数 = 非零科目数 10" "$(P "select count(*) from biz_opening_balance where period_code='2027-01'")" "10"
check "期初来源全部 YEAR_END" "$(P "select count(*) from biz_opening_balance where period_code='2027-01' and source='YEAR_END'")" "10"
check "期初每行只有单边（借或贷其一）" "$(P "select count(*) from biz_opening_balance where period_code='2027-01' and debit_opening>0 and credit_opening>0")" "0"
OB_ROW() { P "select coalesce(sum(debit_opening),0)||'/'||coalesce(sum(credit_opening),0) from biz_opening_balance where period_code='2027-01' and account_code='$1'"; }
say "  逐科目抽查（期初台账 借/贷）"
check "1002 银行存款 期初" "$(OB_ROW 1002)" "200.0000/0.0000"
check "1122 应收账款 期初" "$(OB_ROW 1122)" "33700.0000/0.0000"
check "1405 库存商品 期初" "$(OB_ROW 1405)" "17000.0000/0.0000"
check "1601 固定资产 期初" "$(OB_ROW 1601)" "48000.0000/0.0000"
check "1602 累计折旧 期初" "$(OB_ROW 1602)" "0.0000/7600.0000"
check "2202 应付账款 期初" "$(OB_ROW 2202)" "0.0000/72860.0000"
check "2221.01 进项税 期初" "$(OB_ROW 2221.01)" "3171.3000/0.0000"
check "2221.02 销项税 期初" "$(OB_ROW 2221.02)" "0.0000/3900.0000"
check "2241.01 员工垫付 期初" "$(OB_ROW 2241.01)" "0.0000/5500.0000"
check "3104 利润分配 期初" "$(OB_ROW 3104)" "0.0000/12211.3000"
check "损益类科目没有期初行" "$(P "select count(*) from biz_opening_balance ob join biz_account a on a.account_code=ob.account_code where ob.period_code='2027-01' and a.account_category='PROFIT_LOSS'")" "0"
check_num "期初台账合计 借 = 贷（复式平衡）" "$(P "select coalesce(sum(debit_opening),0) from biz_opening_balance where period_code='2027-01'")" "$(P "select coalesce(sum(credit_opening),0) from biz_opening_balance where period_code='2027-01'")"
# 期初台账 vs 报表期初（同一个 SQL 两边各算一次）
check_num "期初台账借合计 200+33700+17000+48000+3171.30" "$(P "select coalesce(sum(debit_opening),0) from biz_opening_balance where period_code='2027-01'")" "102071.3000"
check_num "期初台账贷合计 7600+72860+3900+5500+12211.30" "$(P "select coalesce(sum(credit_opening),0) from biz_opening_balance where period_code='2027-01'")" "102071.3000"
check_num "手工期初接口把 YEAR_END 行视为不可覆盖（source 计数）" "$(P "select count(*) from biz_opening_balance where period_code='2027-01' and source<>'YEAR_END'")" "0"
snap u7_opening "select account_code||' | 借='||debit_opening||' 贷='||credit_opening||' | '||source from biz_opening_balance where period_code='2027-01' order by account_code"

# ---------- U8 2027 报表期初接上年期末 ----------
say; say "U8 跨年报表：2027-01 报表「上期」= 2026-12 期末；2026 报表仍可查"
api u8_bs26 GET "/business/erp/report/balance-sheet?period=2026-12" "$T_ADMIN"
check "2026-12 资产负债表仍可查" "$CODE" "0"
check "2026-12 资产 = 负债 + 权益" "$(data '.data.check.balanced | tostring')" "true"
A26=$(data '.data.assets[] | select(.item_code=="TOTAL_ASSETS") | .amount')
check_num "2026-12 资产总计" "$A26" "91300"
api u8_bs27 GET "/business/erp/report/balance-sheet?period=2027-01" "$T_ADMIN"
check "2027-01 资产负债表取数" "$CODE" "0"
check "2027-01 资产 = 负债 + 权益" "$(data '.data.check.balanced | tostring')" "true"
check_num "2027-01 上期（previous_period）" "$(data '.data.previous_period')" "2026-12"
P27=$(data '.data.assets[] | select(.item_code=="TOTAL_ASSETS") | .previous_amount')
check_num "2027-01 资产总计上期 = 2026-12 期末 91300" "$P27" "91300"
check_num "2027-01 未分配利润上期 = 12211.30" "$(data '.data.equity[] | select(.item_code=="UNDISTRIBUTED_PROFIT") | .previous_amount')" "12211.30"
check_num "2027-01 存货上期 = 2026-12 期末 17000" "$(data '.data.assets[] | select(.item_code=="INVENTORY") | .previous_amount')" "17000"
api u8_is26 GET "/business/erp/report/income-statement?period=2026-12" "$T_ADMIN"
check "2026 年利润表仍可查" "$CODE" "0"
check_num "2026 净利润（本年累计，结转凭证已排除出本期发生额）" "$(data '.data.items[] | select(.item_code=="NET_PROFIT") | .ytd')" "12211.30"
check "利润表勾稽（净利润 ≡ 未分配利润变动）" "$(data '.data.check.balanced | tostring')" "true"
snap u8_reports "select 1"

# ---------- U9 反年结 + 重新年结 ----------
say; say "U9 反年结（2027 尚无业务凭证）→ 状态回滚 → 重新年结"
api u9_reopen_bad POST /business/erp/year-end/reopen "$T_ADMIN" '{"year":2026,"reason":"太短"}'
check_http "反年结原因不足 10 字" "$HTTP" "400"
api u9_reopen POST /business/erp/year-end/reopen "$T_ADMIN" '{"year":2026,"reason":"CLS-01 演练：反年结验证回滚链路"}'
check "反年结 2026" "$CODE" "0"
check "结转凭证已删除" "$(P "select count(*) from biz_voucher where voucher_type='YEAR_END' and period='2026-12' and deleted_at is null")" "0"
check "2027-01 期初台账已清空" "$(P "select count(*) from biz_opening_balance where period_code='2027-01'")" "0"
check "2026-12 解封为 OPEN" "$(P "select status from biz_accounting_period where period_code='2026-12'")" "OPEN"
api u9_check GET "/business/erp/year-end/check?year=2026" "$T_ADMIN"
check "反年结后不能直接再年结（12 月已被解封）" "$(data '.data.can_close | tostring')" "false"
check "阻塞项类型 PERIOD_NOT_CLOSED" "$(data '.data.blockers[0].type')" "PERIOD_NOT_CLOSED"
check_in "阻塞项点名 2026-12" "$(data '.data.blockers[0].periods | join(",")')" "2026-12"
say "  -- 反年结的设计把 2026-12 解封（capability：让 12 月能改），所以再年结前必须先重新结 12 月"
api u9_reclose_dec POST "/business/erp/period/$P12/close" "$T_ADMIN" '{"reason":"CLS-01 演练：反年结后重新结 12 月"}'
check "重新结 2026-12" "$CODE" "0"
api u9_check2 GET "/business/erp/year-end/check?year=2026" "$T_ADMIN"
check "重新结账后可以再年结" "$(data '.data.can_close | tostring')" "true"
api u9_reclose POST /business/erp/year-end/close "$T_ADMIN" '{"year":2026,"reason":"CLS-01 演练：回滚后重新年结"}'
check "重新年结" "$CODE" "0"
check_num "重新年结净利润一致" "$(data '.data.net_profit')" "12211.30"
check "结转凭证恢复（1 张）" "$(P "select count(*) from biz_voucher where voucher_type='YEAR_END' and period='2026-12' and deleted_at is null")" "1"
check "2027-01 期初台账恢复（10 行）" "$(P "select count(*) from biz_opening_balance where period_code='2027-01'")" "10"
check "2026-12 回到 CLOSED" "$(P "select status from biz_accounting_period where period_code='2026-12'")" "CLOSED"
check_num "重结后损益余额仍为 0" "$(P "select coalesce(sum(credit_amount-debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id join biz_account a on a.account_code=e.account_code where v.status='POSTED' and a.account_category='PROFIT_LOSS' and v.voucher_date<='2026-12-31'")" "0"

# ---------- U10 次年开账：2027-01 记账 ----------
say; say "U10 次年开账：2027-01 正常记账（手工凭证 借1002 1130 / 贷5001 1000 / 贷2221.02 130）"
api u10_mv POST /business/erp/voucher "$T_ADMIN" "{\"voucher_date\":\"2027-01-15\",\"summary\":\"CLS-01 2027 年 1 月销售（手工）\",\"entries\":[{\"account_id\":\"$ACC_1002\",\"debit\":1130,\"summary\":\"收款\"},{\"account_id\":\"$ACC_5001\",\"credit\":1000,\"summary\":\"收入\"},{\"account_id\":\"$ACC_2221_02\",\"credit\":130,\"summary\":\"销项税\"}]}"
check "2027 记账建单" "$CODE" "0"
V27=$(data '.data.id')
check "凭证归期 2027-01" "$(P "select period from biz_voucher where id='$V27'")" "2027-01"
api u10_audit POST "/business/erp/voucher/$V27/audit" "$T_APP" '{"remark":"CLS-01 复核"}'
check "2027 凭证审核" "$CODE" "0"
api u10_post POST "/business/erp/voucher/$V27/post" "$T_APP" '{}'
check "2027 凭证记账" "$CODE" "0"
check "2027 凭证已 POSTED" "$(P "select status from biz_voucher where id='$V27'")" "POSTED"
api u10_tb GET "/business/erp/ledger/trial-balance?period=2027-01" "$T_ADMIN"
check "2027-01 试算平衡" "$(data '.data.balanced')" "true"
check_num "2027-01 期初借方合计 = 2026-12 期末借方合计" "$(data '.data.total_opening_debit')" "102071.3000"
check_num "2027-01 期初贷方合计 = 2026-12 期末贷方合计" "$(data '.data.total_opening_credit')" "102071.3000"
api u10_is GET "/business/erp/report/income-statement?period=2027-01" "$T_ADMIN"
check "2027-01 利润表取数" "$CODE" "0"
check_num "2027 本期营业收入（不含 2026 的 30000）" "$(data '.data.items[] | select(.item_code=="REVENUE") | .current')" "1000"
check_num "2027 本年累计营业收入" "$(data '.data.items[] | select(.item_code=="REVENUE") | .ytd')" "1000"
check_num "2027 本期净利润" "$(data '.data.items[] | select(.item_code=="NET_PROFIT") | .current')" "1000"
api u10_bs GET "/business/erp/report/balance-sheet?period=2027-01" "$T_ADMIN"
check "2027-01 资产负债表期末平衡" "$(data '.data.check.balanced | tostring')" "true"
check_num "2027-01 资产总计（91300 + 1130）" "$(data '.data.assets[] | select(.item_code=="TOTAL_ASSETS") | .amount')" "92430"
check_num "2027-01 资产上期（= 2026 期末）" "$(data '.data.assets[] | select(.item_code=="TOTAL_ASSETS") | .previous_amount')" "91300"

# ---------- U11 跨年护栏 ----------
say; say "U11 跨年护栏：2027 已有业务凭证 ⇒ 反年结必须被拦住"
api u11_reopen POST /business/erp/year-end/reopen "$T_ADMIN" '{"year":2026,"reason":"CLS-01 演练：明年已记账后试图反年结"}'
check_http "明年已记账后反年结" "$HTTP" "409"
check_in "拦截文案" "$(msg)" "凭证"
api u11_closed2026 POST /business/erp/voucher "$T_ADMIN" "{\"voucher_date\":\"2026-12-31\",\"summary\":\"CLS-01 违规补记 2026（应被拒）\",\"entries\":[{\"account_id\":\"$ACC_1002\",\"debit\":1},{\"account_id\":\"$ACC_5001\",\"credit\":1}]}"
check_http "年结后向 2026-12 补记" "$HTTP" "400"
# 年结护栏（本次演练补的代码）：已年结的年度不许单独反结账。
# 没有这道闸时，path 是「反结账 2026-12（此时它是 2026 最后一个 CLOSED 期间，能过）→ 补记一笔
# → 历史累计变了、biz_opening_balance(2027-01) 快照没变 → 2027 报表期初与期初台账对不上」。
api u11_reopen_period POST "/business/erp/period/$P12/reopen" "$T_ADMIN" '{"reason":"CLS-01 演练：年结后试图反结账 12 月"}'
check_http "年结后反结账 2026-12（年结护栏）" "$HTTP" "409"
check_in "护栏文案" "$(msg)" "已经年结" "反年结"
check "2026-12 仍 CLOSED（护栏没改状态）" "$(P "select status from biz_accounting_period where period_code='2026-12'")" "CLOSED"
check "护栏不落状态日志（2026-12 最新一条期间日志仍是 CLOSED）" "$(P "select to_status from biz_status_log where entity_type='ACCOUNTING_PERIOD' and entity_no='2026-12' order by created_at desc limit 1")" "CLOSED"
api u11_tb2027 GET "/business/erp/ledger/trial-balance?period=2027-01" "$T_ADMIN"
check "2027-01 仍试算平衡（反年结被拒后账未被破坏）" "$(data '.data.balanced')" "true"

# ---------- U12 最终台账 ----------
say; say "U12 最终台账（跨年对照表）"
snap u12_cross_year_balances "select a.account_code||' | '||a.account_name||' | 2026期末(借正贷负)='||coalesce((select sum(e.debit_amount-e.credit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.status='POSTED' and v.voucher_date<='2026-12-31' and e.account_code=a.account_code),0)||' | 2027期初台账借='||coalesce((select sum(debit_opening) from biz_opening_balance ob where ob.period_code='2027-01' and ob.account_code=a.account_code),0)||' 贷='||coalesce((select sum(credit_opening) from biz_opening_balance ob where ob.period_code='2027-01' and ob.account_code=a.account_code),0) from biz_account a where a.deleted_at is null and (coalesce((select sum(e.debit_amount-e.credit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.status='POSTED' and v.voucher_date<='2026-12-31' and e.account_code=a.account_code),0)<>0 or exists (select 1 from biz_opening_balance ob where ob.period_code='2027-01' and ob.account_code=a.account_code)) order by a.account_code"
snap u12_year_state "select entity_type||' | '||entity_no||' | '||coalesce(from_status,'NULL')||'->'||to_status||' | '||coalesce(reason,'-') from biz_status_log where entity_type='ACCOUNTING_YEAR' order by created_at"
snap u12_period_state "select period_code||' | '||status from biz_accounting_period where period_year in (2026,2027) order by period_code"

say
say "===== 03_year_end 小计：PASS=$PASS FAIL=$FAIL ====="
if [ "$FAIL" -gt 0 ]; then printf '  FAILED: %s\n' "${FAILED[@]}"; fi
