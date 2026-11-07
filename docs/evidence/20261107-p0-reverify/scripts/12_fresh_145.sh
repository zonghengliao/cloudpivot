#!/usr/bin/env bash
# REVERIFY 145：两个全新库 + 实例（8051 演示账号 / 8052 只有 admin）
set -euo pipefail
ENVF=/etc/cloudpivot/cloudpivot-dev.env
PW=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' "$ENVF")
export PGPASSWORD="$PW"

mkedb() { # $1=db
  psql -h 127.0.0.1 -U cloudpivot -d postgres -c "CREATE DATABASE $1 OWNER cloudpivot" 2>/dev/null \
    || su - postgres -c "psql -v ON_ERROR_STOP=1 -c \"CREATE DATABASE $1 OWNER cloudpivot\""
}
start() { # $1=db $2=port $3=seedflag $4=tag
  local db=$1 port=$2 seed=$3 tag=$4
  pkill -f "cloudpivot-rev-$tag" 2>/dev/null || true
  set -a; . "$ENVF"; set +a
  export NIK_DATABASE_NAME="$db" NIK_SERVER_LISTEN="127.0.0.1:$port" NIK_SEED_DEMO_USERS="$seed"
  cd /opt/cloudpivot/backend
  cp -f target/debug/cloudpivot-rev "target/debug/cloudpivot-rev-$tag"
  setsid nohup "./target/debug/cloudpivot-rev-$tag" > "/root/backup/rev_$tag.log" 2>&1 < /dev/null &
  echo $! > "/root/backup/.rev_$tag.pid"
  for i in $(seq 1 180); do
    code=$(curl -s -m 2 -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port/health" || true)
    if [ "$code" = "200" ]; then
      echo "$tag: health=200 after ${i}s (db=$db port=$port seed=$seed)"
      grep -E "数据库迁移完成" "/root/backup/rev_$tag.log" | tail -1
      return 0
    fi
    sleep 1
  done
  echo "$tag 未就绪"; tail -30 "/root/backup/rev_$tag.log"; return 1
}

mkedb cloudpivot_rev_demo
mkedb cloudpivot_rev_admin
start cloudpivot_rev_demo 8051 true demo
start cloudpivot_rev_admin 8052 false admin
export PGPASSWORD="$PW"
for db in cloudpivot_rev_demo cloudpivot_rev_admin; do
  echo "$db: migrations=$(psql -h 127.0.0.1 -U cloudpivot -d $db -tAc 'select count(*) from schema_migrations') users=$(psql -h 127.0.0.1 -U cloudpivot -d $db -tAc 'select count(*) from sys_user where deleted_at is null')"
done
psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_rev_demo -tAc "select username from sys_user where deleted_at is null order by 1" | tr '\n' ' '; echo
echo "demo: $(psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_rev_demo -tAc "select count(*) from biz_flow_def where status='PUBLISHED'") published flows"