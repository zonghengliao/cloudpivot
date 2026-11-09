#!/usr/bin/env bash
# 汇总 E2E06-FIX-2 的实测证据到 /tmp/fix2_evidence/（供 docs/evidence/e2e06-fix2/ 收录）
#
# 产出：
#   report_curl.out            真实 token + curl 打报表接口（token 已抹），含两年度报文与关键字段
#   e2e06_run_all.out          145 上 e2e06_run_all.sh 全量输出（A + B + fix2 探针 + 勾稽）
#   asserts.log                PASS/FAIL 明细
#   erp_invariants_budget.out  巡检「会计」「报销」两组（预算占用 ↔ 报销口径）
#   deploy.out                 145 部署实况（commit / 构建 / health）
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
OUT=/tmp/fix2_evidence
mkdir -p "$OUT"
cd /opt/cloudpivot || exit 1

BASE=http://127.0.0.1:8033/api
TOKEN=$(curl -s -m 30 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" | jq -r '.data.access_token // empty')
if [ -z "$TOKEN" ]; then echo "FATAL: 登录取不到 token"; exit 1; fi

{
  echo "== 145 部署实况"
  git log --oneline -1
  systemctl is-active cloudpivot-test
  curl -s -o /dev/null -w 'health=%{http_code}\n' -m 5 "$BASE/../health"
  echo
  echo "== curl 实测：GET /business/erp/budget/report?fiscal_year=2027（真实 token，已抹）"
  curl -s -m 30 "$BASE/business/erp/budget/report?fiscal_year=2027" --oauth2-bearer "$TOKEN" \
    | tee "$OUT/report_2027.json" \
    | jq -c '{code, rows: [.data[]|select(.account_code=="5602.05" or .account_code=="5602.02")
        | {fiscal_year, dept_name, account_code, total_amount, used, available, over,
           effective, effective_from, month_period, month_limit, month_used,
           month_available, month_over, month_over_amount}]}'
  echo
  echo "== curl 实测：GET /business/erp/budget/report?fiscal_year=2023（年度早于生效日）"
  curl -s -m 30 "$BASE/business/erp/budget/report?fiscal_year=2023" --oauth2-bearer "$TOKEN" \
    | tee "$OUT/report_2023.json" \
    | jq -c '{code, rows: [.data[]|select(.account_code=="5602.02")
        | {fiscal_year, account_code, over, available, effective, effective_from,
           month_period, month_over, month_available}]}'
  echo
  echo "== 提交侧同源证据：biz_budget_usage 冻结的 over_amount（5602.05 / FY2027）"
  psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off -c \
    "select u.source_no, u.period_code, u.amount::text, u.over_budget,
            coalesce(u.over_amount::text,'NULL')
       from biz_budget_usage u
       join biz_account a on a.id = u.account_id
      where a.account_code='5602.05' and u.fiscal_year=2027 and u.deleted_at is null
      order by u.created_at"
  echo
  echo "== 报表分月明细（5602.05 / FY2027，2027-02 该月已超）"
  jq -c '[.data[]|select(.account_code=="5602.05")][0]
          | {month_period, month_over, month_over_amount,
             months: [.months[]|select(.period_code=="2027-02")]}' "$OUT/report_2027.json"
} > "$OUT/report_curl.out" 2>&1

cp /tmp/fix2_run_all2.out "$OUT/e2e06_run_all.out" 2>/dev/null || true
cp /tmp/e2e06/asserts.log "$OUT/asserts.log" 2>/dev/null || true

python3 scripts/verify/erp_invariants.py --group 会计 > "$OUT/erp_invariants_acct.out" 2>&1
python3 scripts/verify/erp_invariants.py --group 报销 > "$OUT/erp_invariants_expense.out" 2>&1

echo "--- collected ---"
ls -l "$OUT"
grep -E '^PASS' "$OUT/asserts.log" | wc -l | sed 's/^/asserts PASS=/'
grep -E '^FAIL' "$OUT/asserts.log" | wc -l | sed 's/^/asserts FAIL=/'
