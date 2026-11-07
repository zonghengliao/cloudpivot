#!/usr/bin/env bash
# t_be428a39 / SCOPE-PAGE：145 上「超管路径没被改坏」的取证
#
# 为什么要临时造一个超管账号：145 的 admin 口令已被改过（不是 NIK_ADMIN_PASSWORD），
# 不能猜也不能重置（会打断别人）；而 SUPER_ADMIN 会短路数据范围，正好用来验「不裁剪」。
# 做法：复制 emp_drill 的口令哈希建一个临时超管 → 探针 → 立刻硬删（不留残留）。
set -uo pipefail
export PATH=/usr/local/bin:/usr/bin:/bin:$PATH
set -a; . /etc/cloudpivot/cloudpivot-dev.env 2>/dev/null; set +a
export PGPASSWORD="${NIK_DATABASE_PASSWORD:-}"
P="psql -h 127.0.0.1 -U cloudpivot -d ${NIK_DATABASE_NAME:-cloudpivot_dev} -At"
B="http://127.0.0.1:${NIK_SERVER_LISTEN##*:}"
U=scope_probe_super

cleanup() {
  $P -c "delete from sys_user_role where user_id in (select id from sys_user where username='$U');
         delete from sys_user where username='$U';
         select 'residue=' || count(*) from sys_user where username='$U';" >/dev/null 2>&1
}
trap cleanup EXIT

echo "======== 超管探针（不裁剪才看得见两个部门的单）========"
$P -c "delete from sys_user_role where user_id in (select id from sys_user where username='$U');
       delete from sys_user where username='$U';" >/dev/null

$P -c "insert into sys_user (id, username, password, real_name, dept_id, status, user_type)
        select gen_random_uuid(), '$U', password, '范围探针超管', dept_id, 1, 'HUMAN'
          from sys_user where username='emp_drill'" >/dev/null
$P -c "insert into sys_user_role (user_id, role_id)
        select u.id, r.id from sys_user u, sys_role r
         where u.username='$U' and r.role_code='SUPER_ADMIN'" >/dev/null
echo "created: $($P -c "select u.username || ' roles=' || string_agg(r.role_code, ',') from sys_user u join sys_user_role ur on ur.user_id=u.id join sys_role r on r.id=ur.role_id where u.username='$U' group by 1")"

T=$(curl -sS -X POST $B/api/system/auth/login -H 'Content-Type: application/json' \
  -d "{\"username\":\"$U\",\"password\":\"Drill@123456\"}" | jq -r .data.access_token)
if [ -z "$T" ] || [ "$T" = "null" ]; then echo "登录失败，放弃探针"; exit 1; fi

probe() {
  local label="$1"; local qs="$2"
  local body code total n depts
  body=$(curl -sS -w '\n%{http_code}' -H "Authorization: Bearer $T" \
    "$B/api/business/erp/expense/page?page=1&page_size=100$qs")
  code=$(printf '%s' "$body" | tail -1)
  total=$(printf '%s' "$body" | sed '$d' | jq -r '.data.total // "ERR"' 2>/dev/null)
  n=$(printf '%s' "$body" | sed '$d' | jq -r '.data.list|length' 2>/dev/null)
  depts=$(printf '%s' "$body" | sed '$d' | jq -r '[.data.list[].dept_name]|unique|join("|")' 2>/dev/null)
  printf '%-34s http=%s total=%-4s rows=%-3s depts=%s\n' "$label" "$code" "$total" "$n" "$depts"
}
probe "无筛选"                  ""
probe "keyword=x"               "&keyword=x"
probe "status=DRAFT"            "&status=DRAFT"
probe "status=APPROVED"         "&status=APPROVED"
probe "pay_status=UNPAID"       "&pay_status=UNPAID"
probe "pay_status=PAID"         "&pay_status=PAID"
body=$(curl -sS -w '\n%{http_code}' -H "Authorization: Bearer $T" "$B/api/business/erp/expense/pay/page?page=1&page_size=100")
echo "待付款 pay/page               http=$(printf '%s' "$body" | tail -1) total=$(printf '%s' "$body" | sed '$d' | jq -r '.data.total // "ERR"')"

echo
echo "======== 预算分页（超管，同 URL 必须 200 且全量）========"
for qs in "" "&fiscal_year=2026" "&status=ACTIVE" "&keyword=%E5%B7%AE%E6%97%85"; do
  body=$(curl -sS -w '\n%{http_code}' -H "Authorization: Bearer $T" \
    "$B/api/business/erp/budget/page?page=1&page_size=100$qs")
  printf '%-24s http=%s total=%s\n' "${qs:-无筛选}" "$(printf '%s' "$body" | tail -1)" \
    "$(printf '%s' "$body" | sed '$d' | jq -r '.data.total // "ERR"')"
done
