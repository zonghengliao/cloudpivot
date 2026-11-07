#!/usr/bin/env bash
# t_be428a39 / SCOPE-PAGE：145 真机取证（只读；不建卡不迁移）
# 用法：bash /root/scope_page_probe.sh  → 输出到 stdout，可 tee 成 raw 证据
set -uo pipefail
export PATH=/usr/local/bin:/usr/bin:/bin:$PATH
set -a; . /etc/cloudpivot/cloudpivot-dev.env 2>/dev/null; set +a
export PGPASSWORD="${NIK_DATABASE_PASSWORD:-}"
P="psql -h 127.0.0.1 -U cloudpivot -d ${NIK_DATABASE_NAME:-cloudpivot_dev} -At"
B="http://127.0.0.1:${NIK_SERVER_LISTEN##*:}"

echo "======== 0) 环境 ========"
echo "HEAD=$(cd /opt/cloudpivot && git rev-parse --short HEAD) service=$(systemctl is-active cloudpivot-test) health=$(curl -s -o /dev/null -w '%{http_code}' -m 8 $B/health)"
$P -c "select current_database(), (select count(*) from biz_expense_report where deleted_at is null) as reports_all"

echo
echo "======== 1) emp_drill 的身份与数据范围 ========"
$P -c "select u.username, u.dept_id, d.dept_name from sys_user u left join sys_dept d on d.id=u.dept_id where u.username='emp_drill'"
echo "-- 角色 / data_scope（1=ALL 2=DEPT 3=DEPT_AND_CHILD 4=SELF 5=CUSTOM）"
$P -c "select r.role_code, r.data_scope from sys_role r join sys_user_role ur on ur.role_id=r.id join sys_user u on u.id=ur.user_id where u.username='emp_drill' and r.deleted_at is null"

echo
echo "======== 2) 数据真值（全库 vs emp_drill 可见部门集合）========"
$P -c "select 'reports_all' k, count(*) v from biz_expense_report where deleted_at is null
        union all select 'dept_of_emp_drill', count(*) from biz_expense_report e join sys_user u on u.id=e.reporter_id
          where e.deleted_at is null and u.username='emp_drill'
        union all select 'per_dept', 0"
echo "-- 每部门单量（前 8）"
$P -c "select e.dept_name, count(*) from biz_expense_report e where e.deleted_at is null group by 1 order by 2 desc limit 8"

echo
echo "======== 3) 接口：emp_drill（非超管，带裁剪）========"
T_EMP=$(curl -sS -X POST $B/api/system/auth/login -H 'Content-Type: application/json' \
  -d '{"username":"emp_drill","password":"Drill@123456"}' | jq -r .data.access_token)
probe() {
  local label="$1"; local qs="$2"; local tok="$3"
  local code body
  body=$(curl -sS -w '\n%{http_code}' -H "Authorization: Bearer $tok" \
    "$B/api/business/erp/expense/page?page=1&page_size=100$qs")
  code=$(printf '%s' "$body" | tail -1)
  local json; json=$(printf '%s' "$body" | sed '$d')
  local total depts n
  total=$(printf '%s' "$json" | jq -r '.data.total // "ERR"' 2>/dev/null)
  n=$(printf '%s' "$json" | jq -r '.data.list|length' 2>/dev/null)
  depts=$(printf '%s' "$json" | jq -r '[.data.list[].dept_name]|unique|join("|")' 2>/dev/null)
  printf '%-46s http=%s total=%-4s rows=%-3s depts=%s\n' "$label" "$code" "$total" "$n" "$depts"
}
echo "-- emp_drill（期望：全部 200，且 total 按范围收敛）"
probe "无筛选"                        ""                                      "$T_EMP"
probe "keyword=x（命中所有 EXP 单号）" "&keyword=x"                            "$T_EMP"
probe "status=DRAFT"                  "&status=DRAFT"                         "$T_EMP"
probe "status=APPROVED"               "&status=APPROVED"                      "$T_EMP"
probe "pay_status=UNPAID"             "&pay_status=UNPAID"                    "$T_EMP"
probe "pay_status=PAID"               "&pay_status=PAID"                      "$T_EMP"
probe "keyword+status+pay_status"     "&keyword=%E6%8A%A5%E9%94%80&status=DRAFT&pay_status=UNPAID" "$T_EMP"

echo
echo "======== 4) 接口：admin（超管，不做裁剪；必须仍是全量）========"
T_ADM=$(curl -sS -X POST $B/api/system/auth/login -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}' | jq -r .data.access_token)
probe "无筛选"                        ""                                      "$T_ADM"
probe "keyword=x"                     "&keyword=x"                            "$T_ADM"
probe "status=DRAFT"                  "&status=DRAFT"                         "$T_ADM"
probe "status=APPROVED"               "&status=APPROVED"                      "$T_ADM"
probe "pay_status=UNPAID"             "&pay_status=UNPAID"                    "$T_ADM"
probe "pay_status=PAID"               "&pay_status=PAID"                      "$T_ADM"

echo
echo "======== 5) 数据真值对照（同筛选、全库口径）========"
$P -c "select 'ALL' k, count(*) from biz_expense_report where deleted_at is null
        union all select 'DRAFT', count(*) from biz_expense_report where deleted_at is null and status='DRAFT'
        union all select 'APPROVED', count(*) from biz_expense_report where deleted_at is null and status='APPROVED'
        union all select 'UNPAID', count(*) from biz_expense_report where deleted_at is null and pay_status='UNPAID'
        union all select 'PAID', count(*) from biz_expense_report where deleted_at is null and pay_status='PAID'"

echo
echo "======== 6) 同款隐患：预算分页（budget_page，非超管带筛选）========"
bprobe() {
  local label="$1"; local qs="$2"; local tok="$3"
  local body code total
  body=$(curl -sS -w '\n%{http_code}' -H "Authorization: Bearer $tok" \
    "$B/api/business/erp/budget/page?page=1&page_size=100$qs")
  code=$(printf '%s' "$body" | tail -1)
  total=$(printf '%s' "$body" | sed '$d' | jq -r '.data.total // "ERR"' 2>/dev/null)
  printf '%-40s http=%s total=%s\n' "$label" "$code" "$total"
}
bprobe "emp_drill 无筛选"        ""                      "$T_EMP"
bprobe "emp_drill fiscal_year"   "&fiscal_year=2026"     "$T_EMP"
bprobe "emp_drill status=ACTIVE" "&status=ACTIVE"        "$T_EMP"
bprobe "emp_drill keyword"       "&keyword=%E5%B7%AE%E6%97%85" "$T_EMP"
bprobe "admin    fiscal_year"    "&fiscal_year=2026"     "$T_ADM"

echo
echo "======== 7) 待付款列表（report_payable_page，同样走 scope::paginate）========"
for who in EMP ADM; do
  tok=$([ $who = EMP ] && echo "$T_EMP" || echo "$T_ADM")
  body=$(curl -sS -w '\n%{http_code}' -H "Authorization: Bearer $tok" "$B/api/business/erp/expense/payable?page=1&page_size=100")
  printf '%-4s http=%s total=%s\n' "$who" "$(printf '%s' "$body" | tail -1)" \
    "$(printf '%s' "$body" | sed '$d' | jq -r '.data.total // "ERR"' 2>/dev/null)"
done
