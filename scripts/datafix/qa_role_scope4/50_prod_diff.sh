#!/usr/bin/env bash
# 生产「应用前后」差异核算：只允许 4 个夹具角色的 data_scope 变化，其它角色/账号/绑定一行都不许变。
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BK=${1:-/root/datafix-backup-qa4/20261105-135700-apply/tables.sql}
PSQL="psql -h $NIK_DATABASE_HOST -p $NIK_DATABASE_PORT -U $NIK_DATABASE_USER -d $NIK_DATABASE_NAME -X -At -F|"

echo "=== 备份文件：$BK"
echo
echo "=== ① sys_role：role_code | data_scope | status | 是否软删（备份 = 应用前）==="
awk '/^COPY public.sys_role \(/,/^\\\.$/' "$BK" | awk -F'\t' 'NF>10 {print $2"|"$NF"|"$5"|"($12=="\\N"?"alive":"deleted")}' | sort > /tmp/qa4_role_before.txt
$PSQL -c "select role_code||'|'||data_scope||'|'||status||'|'||case when deleted_at is null then 'alive' else 'deleted' end
            from sys_role order by role_code" | sort > /tmp/qa4_role_after.txt
diff -u /tmp/qa4_role_before.txt /tmp/qa4_role_after.txt | sed -n '1,40p' || true
echo
echo "=== ② 行数对比（应用前 → 应用后，应完全相同）==="
before_cnt() { awk -v t="$1" '$0 ~ "^COPY public\\."t" \\(" {f=1; next} f && /^\\\.$/ {f=0} f && NF>0 {c++} END {print c+0}' "$BK"; }
for t in sys_role sys_user sys_user_role sys_role_menu sys_role_dept; do
    b=$(before_cnt "$t")
    a=$($PSQL -c "select count(*) from $t")
    flag=$([ "$b" = "$a" ] && echo "✅" || echo "❌")
    echo "  $flag $t: $b → $a"
done
echo
echo "=== ③ 每个角色的绑定数（应用前后；备份列序 id,user_id,role_id → 取 \$3）==="
awk '/^COPY public\.sys_user_role \(/,/^\\\.$/' "$BK" | awk -F'\t' 'NF>2 {print $3}' | sort | uniq -c | awk '{print $2" "$1}' | sort > /tmp/qa4_ur_before.txt
$PSQL -c "select role_id from sys_user_role" | sort | uniq -c | awk '{print $2" "$1}' | sort > /tmp/qa4_ur_after.txt
diff /tmp/qa4_ur_before.txt /tmp/qa4_ur_after.txt > /tmp/qa4_ur_diff.txt && echo "  ✅ sys_user_role 绑定关系逐 role_id 完全一致（0 处差异）" || { echo "  ❌ 绑定关系有差异："; cat /tmp/qa4_ur_diff.txt; }
echo
echo "=== ④ 在用账号（status=1 且未软删）==="
$PSQL -c "select username||'|'||status from sys_user where deleted_at is null and status=1 order by username"
