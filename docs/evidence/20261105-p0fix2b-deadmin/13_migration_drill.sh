set -e
export PGPASSWORD=e2eiso_2026
PSQL="psql -h 127.0.0.1 -U e2erole -X -q -v ON_ERROR_STOP=1"
SU="su - postgres -c"

for db in cloudpivot_deadmin_a cloudpivot_deadmin_b; do
  exists=$($PSQL -d postgres -tAc "select 1 from pg_database where datname='$db'")
  if [ "$exists" = "1" ]; then
    $SU "psql -q -c 'drop database $db'" || true
  fi
  $SU "createdb -O e2erole $db"
  echo "… dump prod → $db"
  ssh root@172.18.10.144 'set -a; . /etc/cloudpivot/cloudpivot.env; set +a; PGPASSWORD="$NIK_DATABASE_PASSWORD" pg_dump -h "$NIK_DATABASE_HOST" -p "$NIK_DATABASE_PORT" -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" --no-owner --no-privileges' \
    | $PSQL -d $db >/dev/null
  echo "✅ $db ready: $($PSQL -d $db -tAc 'select count(*) from sys_user') users"
done

UP=/opt/cloudpivot/backend/migrations/business/V0251__flow_deadmin_assignee_roles.up.sql
V248=/opt/cloudpivot/backend/migrations/business/V0248__stock_in_cancel_reviewer_role.up.sql

echo "=== A: 按发版顺序 V0248 → V0251（新部署路径）==="
$PSQL -d cloudpivot_deadmin_a -f $V248 2>&1 | tail -3
$PSQL -d cloudpivot_deadmin_a -f $UP 2>&1 | tail -6
echo "=== A: 幂等重跑 V0251 ==="
$PSQL -d cloudpivot_deadmin_a -f $UP 2>&1 | tail -6

echo "=== B: 只跑 V0251（生产手工 apply 路径，v1 仍是 ADMIN）==="
$PSQL -d cloudpivot_deadmin_b -f $UP 2>&1 | tail -6
echo "=== B: 幂等重跑 V0251 ==="
$PSQL -d cloudpivot_deadmin_b -f $UP 2>&1 | tail -6
