#!/usr/bin/env bash
# t_be428a39 / SCOPE-PAGE：预算分页（budget_page）在「有数据权限裁剪的 principal」下的取证
#
# 145 上只有 demo_lead / lead_drill 同时具备 DEPT_LEADER(3=DEPT_AND_CHILD) 与
# business:erp:budget:list，但口令未知，所以照 super_probe.sh 的做法复制 emp_drill 的口令哈希
# 建临时账号 → 探针 → 立刻硬删。
set -uo pipefail
export PATH=/usr/local/bin:/usr/bin:/bin:$PATH
set -a; . /etc/cloudpivot/cloudpivot-dev.env 2>/dev/null; set +a
export PGPASSWORD="${NIK_DATABASE_PASSWORD:-}"
P="psql -h 127.0.0.1 -U cloudpivot -d ${NIK_DATABASE_NAME:-cloudpivot_dev} -At"
B="http://127.0.0.1:${NIK_SERVER_LISTEN##*:}"
U=scope_probe_lead

cleanup() {
  $P -c "delete from sys_user_role where user_id in (select id from sys_user where username='$U');
         delete from sys_user where username='$U';" >/dev/null 2>&1
  echo "清理后残留行数：$($P -c "select count(*) from sys_user where username='$U'")"
}
trap cleanup EXIT

echo "======== 预算数据真值（全库 / 按部门）========"
$P -c "select 'all=' || count(*) from biz_budget where deleted_at is null"
$P -c "select dept_name || '=' || count(*) from biz_budget where deleted_at is null group by 1 order by 1"

$P -c "delete from sys_user_role where user_id in (select id from sys_user where username='$U');
       delete from sys_user where username='$U';" >/dev/null
$P -c "insert into sys_user (id, username, password, real_name, dept_id, status, user_type)
        select gen_random_uuid(), '$U', password, '预算范围探针', dept_id, 1, 'HUMAN'
          from sys_user where username='emp_drill'" >/dev/null
$P -c "insert into sys_user_role (user_id, role_id)
        select u.id, r.id from sys_user u, sys_role r
         where u.username='$U' and r.role_code='DEPT_LEADER'" >/dev/null
echo "探针账号角色：$($P -c "select string_agg(r.role_code || '(scope=' || r.data_scope || ')', ',') from sys_user u join sys_user_role ur on ur.user_id=u.id join sys_role r on r.id=ur.role_id where u.username='$U'")"
echo "所在部门：$($P -c "select d.dept_name from sys_user u join sys_dept d on d.id=u.dept_id where u.username='$U'")"

T=$(curl -sS -X POST $B/api/system/auth/login -H 'Content-Type: application/json' \
  -d "{\"username\":\"$U\",\"password\":\"Drill@123456\"}" | jq -r .data.access_token)
if [ -z "$T" ] || [ "$T" = "null" ]; then echo "登录失败，放弃探针"; exit 1; fi

echo
echo "======== 预算分页 /business/erp/budget/page（带筛选，期望 200 且按部门收敛）========"
probe() {
  local label="$1"; local qs="$2"
  local body code total n depts
  body=$(curl -sS -w '\n%{http_code}' -H "Authorization: Bearer $T" \
    "$B/api/business/erp/budget/page?page=1&page_size=100$qs")
  code=$(printf '%s' "$body" | tail -1)
  total=$(printf '%s' "$body" | sed '$d' | jq -r '.data.total // "ERR"' 2>/dev/null)
  n=$(printf '%s' "$body" | sed '$d' | jq -r '.data.list|length' 2>/dev/null)
  depts=$(printf '%s' "$body" | sed '$d' | jq -r '[.data.list[].dept_name]|unique|join("|")' 2>/dev/null)
  printf '%-30s http=%s total=%-4s rows=%-3s depts=%s\n' "$label" "$code" "$total" "$n" "$depts"
}
probe "无筛选"                 ""
probe "fiscal_year=2026"       "&fiscal_year=2026"
probe "status=ACTIVE"          "&status=ACTIVE"
probe "keyword=差旅"           "&keyword=%E5%B7%AE%E6%97%85"
probe "fiscal_year+status"     "&fiscal_year=2026&status=ACTIVE"
