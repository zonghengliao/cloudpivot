#!/usr/bin/env bash
# PDF-04：145 隔离实例（库 cloudpivot_pdf04 / 端口 8078 / redis db9 / 代码 b7dbf40）
# 只动 /root/pdf04-app 与独立库端口，不碰 8033 的 cloudpivot-test，不碰 144
set -u

# 只杀监听 8078 的那一个（不能按进程名 pkill：8033 的 cloudpivot-test 同名同二进制名）
PID=$(ss -ltnp 2>/dev/null | grep ':8078' | sed -E 's/.*pid=([0-9]+).*/\1/' | head -1)
if [ -n "${PID:-}" ]; then
    kill "$PID" 2>/dev/null
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        ss -ltn 2>/dev/null | grep -q ':8078' || break
        sleep 1
    done
fi

cd /root/pdf04-app/backend || exit 1
set -a
# shellcheck disable=SC1091
. /root/pdf04.env
set +a
setsid ./target/debug/cloudpivot >>/root/pdf04_app_145.log 2>&1 </dev/null &
echo "started pid=$!"
