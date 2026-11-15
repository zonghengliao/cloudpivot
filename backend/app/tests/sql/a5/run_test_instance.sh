#!/usr/bin/env bash
# 用**独立 target 目录**编译（共享 /opt/cloudpivot/backend/target 会被并行卡覆盖成别人的二进制），
# 然后把本卡的二进制复制到运行目录启动，绝不碰 prod:8023 / dev:8024。
set -euo pipefail
export PATH=/root/.cargo/bin:$PATH
RUN=/var/tmp/a5-run
TARGET=/var/tmp/a5-target
BIN=$RUN/cloudpivot-a5

# 1) 停掉监听 8025 的旧实例（只按端口定位，避免误杀 8023/8024）
OLD_PID=$(ss -ltnp 2>/dev/null | awk '/:8025 /{print $NF}' | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2 || true)
if [ -n "${OLD_PID:-}" ]; then
    echo "== 停止旧实例 pid=$OLD_PID"
    kill "$OLD_PID" 2>/dev/null || true
    sleep 2
fi

# 2) 独立 target 目录编译
cd /var/tmp/a5/backend
CARGO_TARGET_DIR=$TARGET cargo build 2>&1 | tail -5
cp -a "$TARGET/debug/cloudpivot" "$BIN"
echo "== 二进制指纹"
ls -l "$BIN"
sha256sum "$BIN"

# 3) 启动
cd "$RUN"
set -a
. /etc/cloudpivot/cloudpivot-test.env
set +a
setsid nohup "$BIN" profile=test > "$RUN/server.log" 2>&1 &
for i in $(seq 1 60); do
    if curl -s -m 2 http://127.0.0.1:8025/health > /tmp/a5_health.json 2>/dev/null; then
        echo "== 健康检查通过（第 ${i} 次尝试）：$(cat /tmp/a5_health.json)"
        break
    fi
    sleep 1
done

echo "== 通知路由探测（未带 token，期望 401 而不是 404）"
curl -s -o /tmp/a5_route_probe.json -w "HTTP %{http_code}\n" http://127.0.0.1:8025/api/business/notice/unread-count
head -c 300 /tmp/a5_route_probe.json; echo
