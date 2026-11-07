#!/usr/bin/env bash
# ⑦ 预算执行分析：卡面写的 /budget/analysis 在实现里叫 /budget/report（先证伪再证真）
set -uo pipefail
cd /opt/cloudpivot/docs/evidence/20261107-p006-budget-chain
source ./00_env.sh

ADMIN=$(cat "$(tokf admin)")
DEPT=$(cat "$EV/raw/dept.id")

echo "===== ⑦-0 卡面路径 /business/erp/budget/analysis?year=2026 ====="
api GET "/business/erp/budget/analysis?year=2026" "$ADMIN" > "$EV/raw/a0_analysis_404.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/a0_analysis_404.json" 2>/dev/null || head -c 200 "$EV/raw/a0_analysis_404.json")"

echo "===== ⑦-1 真实路径 /business/erp/budget/report?fiscal_year=2026 ====="
api GET "/business/erp/budget/report?fiscal_year=2026" "$ADMIN" > "$EV/raw/a1_report_2026.json"
echo "http=$HTTP 预算行数=$(jq '.data|length' "$EV/raw/a1_report_2026.json")"
echo "--- 本次演练部门 DRILL_BDG 的执行行 ---"
jq -c '.data[] | select(.dept_id=="'"$DEPT"'") | {budget_id,dept_name,account_code,total_amount,occupied,consumed,used,available,exec_ratio,over,month_11:[.months[]|select(.period_code=="2026-11")|{amount,used,available}]}' "$EV/raw/a1_report_2026.json"
echo "--- 按部门过滤（带 dept_id 参数再验一次）---"
api GET "/business/erp/budget/report?fiscal_year=2026&dept_id=$DEPT" "$ADMIN" > "$EV/raw/a2_report_dept.json"
echo "http=$HTTP 行数=$(jq '.data|length' "$EV/raw/a2_report_dept.json") $(jq -c '[.data[]|{account_code,total_amount,used,available,exec_ratio,over}]' "$EV/raw/a2_report_dept.json")"

echo "===== ⑦-2 与 DB 台账逐项勾稽 ====="
cat > /tmp/p006_report_check.sql <<SQL
\pset pager off
\echo --- DB：ACTIVE 预算 × 槽内占用（同口径：OCCUPIED + CONSUMED）---
select b.budget_no, b.dept_name, b.account_code, b.total_amount,
       coalesce(round(sum(u.amount) filter (where u.status = 'OCCUPIED'), 4), 0)   as occupied,
       coalesce(round(sum(u.amount) filter (where u.status = 'CONSUMED'), 4), 0)   as consumed,
       coalesce(round(sum(u.amount) filter (where u.status in ('OCCUPIED','CONSUMED')), 4), 0) as used,
       b.total_amount - coalesce(round(sum(u.amount) filter (where u.status in ('OCCUPIED','CONSUMED')), 4), 0) as available,
       case when b.total_amount > 0 then round(coalesce(sum(u.amount) filter (where u.status in ('OCCUPIED','CONSUMED')),0) / b.total_amount * 100, 2) else 0 end as exec_ratio
  from biz_budget b
  left join biz_budget_usage u
    on u.dept_id = b.dept_id and u.account_id = b.account_id and u.fiscal_year = b.fiscal_year
   and u.deleted_at is null
 where b.status = 'ACTIVE' and b.deleted_at is null and b.fiscal_year = 2026
 group by b.budget_no, b.dept_name, b.account_code, b.total_amount
 order by b.dept_name, b.account_code;
\echo --- DB：11 月分解 vs 11 月已用 ---
select b.budget_no, p.period_code, p.amount as limit_amount,
       coalesce((select sum(u.amount) from biz_budget_usage u
                  where u.dept_id = b.dept_id and u.account_id = b.account_id
                    and u.period_code = p.period_code and u.status in ('OCCUPIED','CONSUMED')
                    and u.deleted_at is null), 0) as used,
       p.amount - coalesce((select sum(u.amount) from biz_budget_usage u
                  where u.dept_id = b.dept_id and u.account_id = b.account_id
                    and u.period_code = p.period_code and u.status in ('OCCUPIED','CONSUMED')
                    and u.deleted_at is null), 0) as available
  from biz_budget b join biz_budget_period p on p.budget_id = b.id
 where b.status = 'ACTIVE' and b.fiscal_year = 2026 and p.period_code = '2026-11'
 order by b.budget_no;
SQL
psqlf /tmp/p006_report_check.sql

echo "===== ⑦-3 勾稽断言（报表 vs DB，逐行比对）====="
python3 - "$EV/raw/a2_report_dept.json" <<'PY'
import json,sys
rows=json.load(open(sys.argv[1]))["data"]
bad=0
for r in rows:
    used=float(r["occupied"])+float(r["consumed"])
    avail=float(r["total_amount"])-used
    ratio=round(used/float(r["total_amount"])*100,2) if float(r["total_amount"])>0 else 0.0
    ok = (abs(float(r["used"])-used)<1e-6 and abs(float(r["available"])-avail)<1e-6
          and abs(float(r["exec_ratio"])-ratio)<0.01 and (r["over"] == (avail<0))
          and len(r["months"])==12)
    m11=[m for m in r["months"] if m["period_code"]=="2026-11"]
    m11ok = bool(m11) and abs(float(m11[0]["available"])-(float(m11[0]["amount"])-float(m11[0]["used"])))<1e-6
    if not (ok and m11ok): bad+=1
    print(f'  {r["account_code"]}: total={r["total_amount"]} used={r["used"]} avail={r["available"]} ratio={r["exec_ratio"]}% over={r["over"]} months={len(r["months"])} 11月={m11[0] if m11 else None} -> {"OK" if ok and m11ok else "MISMATCH"}')
print("MISMATCH_ROWS=",bad)
PY
