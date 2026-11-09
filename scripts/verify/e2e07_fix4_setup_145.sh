#!/usr/bin/env bash
# 145 上起「FIX-4 探针实例」：拉最新 main → 构建 debug → 起隔离实例（8034 / cp_fix4_e2e）
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
cd /opt/cloudpivot || exit 1

echo "=== 1) git 同步 ==="
git status --porcelain | head -5
git pull --ff-only origin main || { echo "PULL_FAILED"; exit 1; }
git log --oneline -1

echo "=== 2) 构建 debug 二进制 ==="
cd backend && cargo build --bin cloudpivot 2>&1 | tail -5
ls -l target/debug/cloudpivot

echo "=== 3) 探针环境文件（隔离库 + 8034） ==="
sed -e 's/^NIK_DATABASE_NAME=.*/NIK_DATABASE_NAME=cp_fix4_e2e/' \
    -e 's/^NIK_SERVER_LISTEN=.*/NIK_SERVER_LISTEN=127.0.0.1:8034/' \
    /etc/cloudpivot/cloudpivot-dev.env > /tmp/fix4_probe.env
grep -E '^NIK_(DATABASE_NAME|SERVER_LISTEN)=' /tmp/fix4_probe.env

echo "=== 4) 起实例（迁移在启动时自动跑） ==="
pkill -f 'fix4_probe.env' 2>/dev/null
for p in $(ss -lntp 2>/dev/null | grep ':8034 ' | grep -o 'pid=[0-9]*' | cut -d= -f2); do kill "$p" 2>/dev/null; done
sleep 1
set -a; . /tmp/fix4_probe.env; set +a
mkdir -p /tmp/fix4_probe_log
setsid nohup /opt/cloudpivot/backend/target/debug/cloudpivot > /tmp/fix4_probe_log/server.log 2>&1 &
echo "started pid=$!"
for i in $(seq 1 90); do
  code=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8034/health || true)
  [ "$code" = 200 ] && { echo "health=200（第 ${i}s）"; break; }
  sleep 1
done
curl -s -o /dev/null -w 'health_final=%{http_code}\n' http://127.0.0.1:8034/health
echo "=== 5) 迁移落地情况 ==="
PGPASSWORD="$NIK_DATABASE_PASSWORD" psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d cp_fix4_e2e -X -q -tA \
  -c "select count(*) || ' 条迁移，最大 V' || max(version::int) from schema_migrations where kind='versioned'"
tail -5 /tmp/fix4_probe_log/server.log
