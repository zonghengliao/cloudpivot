#!/usr/bin/env bash
# T1 集成测试：建验证库 → 跑迁移 → cargo test（真库）
set -uo pipefail

DB=cloudpivot_t1v
REPO=/opt/cloudpivot

echo "== 1) 重建验证库 $DB =="
su postgres -c "psql -q -c 'DROP DATABASE IF EXISTS $DB'" >/dev/null 2>&1
su postgres -c "psql -q -c 'CREATE DATABASE $DB OWNER cloudpivot'"

echo "== 2) 跑迁移（起一次服务，自动迁移后退出） =="
cd "$REPO/backend" || exit 1
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PATH=$HOME/.cargo/bin:$PATH
export ALUN_DATABASE_NAME=$DB
export ALUN_SERVER_LISTEN=127.0.0.1:8099
timeout 45 ./target/debug/cloudpivot >/tmp/t1v_up.log 2>&1
grep -E "数据库迁移完成" /tmp/t1v_up.log | tail -2
echo "表数 = $(su postgres -c "psql -d $DB -tAc 'SELECT count(*) FROM information_schema.tables WHERE table_schema=\$\$public\$\$'")"

echo "== 3) cargo test（真库集成测试，串行） =="
export TASK_DB_URL="postgres://cloudpivot:${ALUN_DATABASE_PASSWORD}@127.0.0.1:5432/$DB"
echo "TASK_DB_URL = postgres://cloudpivot:***@127.0.0.1:5432/$DB"
cargo test --test batchT_t1_task -- --nocapture --test-threads=1 2>&1 | tail -80
