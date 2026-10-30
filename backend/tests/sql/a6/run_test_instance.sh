#!/usr/bin/env bash
# A6 验收用临时实例：profile=test / 库 cloudpivot_test / 端口 8026（不碰 prod 8023、dev 8024/8025）
#
# 用法（在 172.18.10.144 上，仓库工作副本里执行）：
#   bash backend/tests/sql/a6/run_test_instance.sh        # 起实例
#   python3 backend/tests/sql/a6/acceptance.py            # 跑三条硬指标
#
# 说明：二进制取 `cargo build --bin cloudpivot` 的 debug 产物（本卡用独立
# CARGO_TARGET_DIR=/var/tmp/a6-target，避免与生产 release 产物 / 其它卡互相覆盖）。
set -euo pipefail
RUN=/var/tmp/a6-run
REPO=${REPO:-/var/tmp/a6}
BIN=${BIN:-/var/tmp/a6-target/debug/cloudpivot}

mkdir -p "$RUN/logs" "$RUN/uploads" "$RUN/downloads"
mkdir -p "$RUN/config"
cp -a "$REPO/backend/config/." "$RUN/config/"
ln -sfn "$REPO/backend/migrations" "$RUN/migrations"

# 只杀本卡起的实例：按**可执行文件路径**匹配。
# 不能用 `pkill -f a6-run`：进程 cmdline 里没有工作目录（A6 实测踩过——旧进程没被杀，
# 新进程绑不上端口，health 检查打到旧进程上，误以为重启成功，白排查一轮）。
pkill -f "$BIN" 2>/dev/null || true
for _ in $(seq 1 20); do
    ss -ltn 2>/dev/null | grep -q ":8026 " || break
    sleep 0.5
done
sleep 1

cd "$RUN"
set -a
# shellcheck disable=SC1091
. /etc/cloudpivot/cloudpivot-test.env
set +a
export NIK_SERVER_LISTEN=127.0.0.1:8026
setsid nohup "$BIN" profile=test > "$RUN/server.log" 2>&1 &
for i in $(seq 1 40); do
    if curl -s -m 2 http://127.0.0.1:8026/health > /tmp/a6_health.json 2>/dev/null; then
        echo "健康检查通过（第 ${i} 次尝试）"
        cat /tmp/a6_health.json
        echo
        exit 0
    fi
    sleep 1
done
echo "启动失败，日志尾部："
tail -40 "$RUN/server.log"
exit 1
