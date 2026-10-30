#!/bin/bash
# W1a · V0137 验收 ①②：空库从 0 跑到 V0137（走真实迁移器）+ 幂等重跑「应用 0 个迁移」
#
# 前置：本机 PG 18；/etc/cloudpivot/cloudpivot-dev.env 可 source；release 二进制已构建。
# 用法：REPO=/opt/cloudpivot bash scripts/verify/w1a_v0137_accept_1_2.sh
set -uo pipefail
REPO=${REPO:-/opt/cloudpivot}
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a

DB=${W1A_DB:-cloudpivot_w1a}
PORT=${W1A_PORT:-18097}
BIN=${W1A_BIN:-$REPO/backend/target/release/cloudpivot}
APP=$REPO/backend
LOG1=/tmp/w1a_run1.log
LOG2=/tmp/w1a_run2.log
export NIK_DATABASE_NAME=$DB
export NIK_SERVER_LISTEN=127.0.0.1:$PORT
export NIK_REDIS_URL=redis://127.0.0.1:6379/9
export RUST_LOG=info
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PSQL="psql -h 127.0.0.1 -p 5432 -U $NIK_DATABASE_USER -d $DB -At"

wait_ready() {  # $1=log
  for _ in $(seq 1 120); do
    if grep -qE "数据库迁移完成|服务启动失败|FATAL" "$1" 2>/dev/null; then sleep 2; return 0; fi
    sleep 1
  done
  return 1
}

echo "################ 准备：重建空库 $DB ################"
su - postgres -c "psql -Atc \"select 1 from pg_database where datname='$DB'\"" | grep -q 1 && \
  su - postgres -c "psql -Atc \"drop database $DB\"" >/dev/null
su - postgres -c "createdb -O $NIK_DATABASE_USER $DB"
echo "库已重建：$($PSQL -c "select current_database()")  业务表数=$($PSQL -c "select count(*) from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname='public' and c.relkind='r'")"
md5sum "$BIN"

echo
echo "################ ① 第一次启动（0 → V0137）################"
cd "$APP" || exit 1
setsid nohup "$BIN" > "$LOG1" 2>&1 < /dev/null &
echo $! > /tmp/w1a.pid
wait_ready "$LOG1" || { echo "!! 启动超时"; tail -30 "$LOG1"; exit 1; }
echo "--- 迁移日志 ---"
grep -nE "迁移" "$LOG1" | head -20
echo "--- 迁移后 schema_migrations 记录数 ---"
$PSQL -c "select count(*) from schema_migrations"
echo "--- 13 张表 + 种子 ---"
$PSQL -c "select count(*) from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname='public' and c.relkind='r' and relname in ('biz_unit','biz_account','biz_supplier','biz_accounting_period','biz_purchase_order','biz_purchase_order_item','biz_seq','biz_warehouse','biz_material','biz_inventory','biz_inventory_txn','biz_location','biz_inventory_location')"
$PSQL -c "select 'unit='||(select count(*) from biz_unit)||' wh='||(select count(*) from biz_warehouse)||' loc='||(select count(*) from biz_location)||' mat='||(select count(*) from biz_material)||' acct='||(select count(*) from biz_account)||' sup='||(select count(*) from biz_supplier)||' period='||(select count(*) from biz_accounting_period)||' seq='||(select count(*) from biz_seq)"
echo "--- 自检 NOTICE ---"
grep -n "V0137 自检通过\|V0137 种子核对\|V0137 种子：" "$LOG1"
echo "--- 服务健康 ---"
curl -s -o /dev/null -w "health=%{http_code}\n" http://127.0.0.1:$PORT/health || true
kill "$(cat /tmp/w1a.pid)" 2>/dev/null
sleep 3

echo
echo "################ ② 第二次启动（应「应用 0 个迁移」）################"
setsid nohup "$BIN" > "$LOG2" 2>&1 < /dev/null &
echo $! > /tmp/w1a.pid
wait_ready "$LOG2" || { echo "!! 启动超时"; tail -30 "$LOG2"; exit 1; }
grep -nE "数据库迁移完成" "$LOG2" | head -5
echo "--- 第二次启动是否有 V0137 动作（应为空）---"
grep -n "V0137" "$LOG2" || echo "(无 V0137 输出，符合预期)"
kill "$(cat /tmp/w1a.pid)" 2>/dev/null
sleep 2
echo "DONE"
