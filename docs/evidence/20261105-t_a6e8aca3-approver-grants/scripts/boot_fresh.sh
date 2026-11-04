#!/bin/bash
# 在空库上跑一遍全部迁移（含新 V0239），证明「干净库装得起来」。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
S=/root/.hermes/profiles/backend-developer/cache/scratch
DB=cloudpivot_pg239_fresh
PORT=18161
PIDF=$S/fresh.pid

# 1) 空库
su postgres -c "dropdb --if-exists $DB" 2>/dev/null
su postgres -c "createdb -O alun $DB" || { echo "createdb FAILED"; exit 1; }
export PGPASSWORD="${ALUN_DATABASE_PASSWORD:-alun}"   # 本机 dev 库口令，允许用环境变量覆盖
echo "fresh db rows in sys_role: $(psql -h 127.0.0.1 -U alun -d $DB -Atc "select count(*) from pg_tables where schemaname='public'")  (tables, expect 0)"

# 2) 从 manual2 实例抄一份 env（含 JWT / 主密钥 / admin 口令），只覆盖库与端口
tr '\0' '\n' < /proc/3234164/environ | grep -E '^(NIK_|CLOUDPIVOT_)' > $S/fresh.env
cat >> $S/fresh.env <<EOF
NIK_DATABASE_NAME=$DB
NIK_SERVER_LISTEN=127.0.0.1:$PORT
NIK_REDIS_URL=redis://127.0.0.1:6379/13
EOF
echo "env keys: $(grep -c = $S/fresh.env)"

cd /opt/cloudpivot/backend
setsid env $(grep -v '^#' $S/fresh.env | tr '\n' ' ') ./target/debug/cloudpivot > $S/fresh.log 2>&1 &
echo $! > $PIDF
echo "launched pid $(cat $PIDF)"
