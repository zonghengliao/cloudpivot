#!/bin/bash
# L1 修复 E2E：起隔离实例（独立库 cloudpivot_l1fix / 端口 18206 / redis 5）
set -u
export PATH=/root/.cargo/bin:$PATH
set -a
. /etc/cloudpivot/cloudpivot-test.env
set +a

DB=cloudpivot_l1fix
PORT=18206

echo "=== 1. 数据库 ==="
export PGPASSWORD="$ALUN_DATABASE_PASSWORD"
psql -h 127.0.0.1 -U "$ALUN_DATABASE_USER" -d postgres -Atc "SELECT 1 FROM pg_database WHERE datname='$DB'" | grep -q 1 \
  && echo "库已存在：$DB（保留，实例启动会自动迁移）" \
  || { su - postgres -c "psql -c \"CREATE DATABASE $DB OWNER $ALUN_DATABASE_USER\"" && echo "已建库：$DB（owner=$ALUN_DATABASE_USER）"; }

echo "=== 2. 构建 debug 二进制 ==="
cd /opt/cloudpivot/backend || exit 1
cargo build -j 2 2>&1 | tail -3

echo "=== 3. 停旧实例（只认自己的 pid 文件） ==="
if [ -f /tmp/l1fix/pid ]; then
  kill "$(cat /tmp/l1fix/pid)" 2>/dev/null && echo "已停旧实例 $(cat /tmp/l1fix/pid)"
  sleep 2
fi
mkdir -p /tmp/l1fix

echo "=== 4. 启动实例 ==="
export ALUN_PROFILE=test
export ALUN_SERVER_LISTEN=127.0.0.1:$PORT
export ALUN_DATABASE_NAME=$DB
export ALUN_REDIS_URL=redis://127.0.0.1:6379/5
export CLOUDPIVOT_AIAGENT_HERMES_BIN=/root/.hermes/hermes-agent/hermes
export CLOUDPIVOT_AIAGENT_CLONE_FROM=agent-template
export RUST_LOG=info
setsid nohup /opt/cloudpivot/backend/target/debug/cloudpivot > /tmp/l1fix/server.log 2>&1 < /dev/null &
echo $! > /tmp/l1fix/pid
echo "pid=$(cat /tmp/l1fix/pid)"

echo "=== 5. 等就绪 ==="
for i in $(seq 1 40); do
  sleep 1
  if curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" 2>/dev/null | grep -q 200; then
    echo "就绪（${i}s）"; break
  fi
  if ! kill -0 "$(cat /tmp/l1fix/pid)" 2>/dev/null; then echo "进程已退出！"; tail -30 /tmp/l1fix/server.log; exit 1; fi
done
echo "=== 6. 启动日志关键行 ==="
grep -E "迁移完成|AI 用户不变式|listen|ERROR|FATAL" /tmp/l1fix/server.log | tail -10
