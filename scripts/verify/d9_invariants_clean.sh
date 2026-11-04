#!/usr/bin/env bash
# D9 不变量验收（两条口径，技能 §6.48）：
#   ① 干净库（新建 + 迁移完、零夹具）必须全绿；
#   ② 跑完**本卡用例**的库再巡检一次，与本卡相关的检查必须仍绿（用例自己清干净夹具）。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
DB_INV=cloudpivot_d9inv
LOG=/tmp/d9_inv_migrate.log

cd /opt/cloudpivot/backend

echo "== ① 新建干净库 $DB_INV 并跑迁移 =="
runuser -u postgres -- psql -qc "DROP DATABASE IF EXISTS $DB_INV"
runuser -u postgres -- psql -qc "CREATE DATABASE $DB_INV OWNER alun"
rm -f /tmp/d9_inv.pid
NIK_PROFILE=test \
NIK_DATABASE_NAME=$DB_INV \
NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun \
NIK_SERVER_LISTEN=127.0.0.1:18098 \
NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
setsid nohup ./target/debug/cloudpivot > $LOG 2>&1 < /dev/null &
echo $! > /tmp/d9_inv.pid
for i in $(seq 1 120); do
  if grep -q "数据库迁移完成" $LOG 2>/dev/null; then break; fi
  if ! kill -0 "$(cat /tmp/d9_inv.pid)" 2>/dev/null; then echo "实例提前退出"; tail -20 $LOG; exit 1; fi
  sleep 2
done
for i in $(seq 1 60); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 http://127.0.0.1:18098/health || true)
  [ "$code" = "200" ] && break
  sleep 2
done
echo "health=$code"
grep -E "数据库迁移完成" $LOG | tail -1
kill "$(cat /tmp/d9_inv.pid)" 2>/dev/null || true
sleep 3
kill -9 "$(cat /tmp/d9_inv.pid)" 2>/dev/null || true

echo "== ① 干净库不变量（期望全绿、BLOCKER 0、WARN 0）=="
bash /opt/cloudpivot/scripts/verify/d9_invariants.sh "$DB_INV" | tail -8

echo "== ② 只跑本卡用例（cloudpivot_d9）后再巡检 =="
export ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/cloudpivot_d9"
export TASK_DB_URL="$ERP_DB_URL"
export NIK_PROFILE=test
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
cargo test --lib d9_ -- --test-threads=1 2>&1 | grep -E "^test result:"
cargo test --lib business::task::handler::tests::copy_title -- --test-threads=1 2>&1 | grep -E "^test result:"
bash /opt/cloudpivot/scripts/verify/d9_invariants.sh cloudpivot_d9 | tail -8
