#!/usr/bin/env bash
# PDF-05：145 隔离实例（库 cloudpivot_pdf05 / 端口 8079 / redis db10 / 代码 2f17908）
# 只动 /root/pdf04-app 与独立库端口，不碰 8033 的 cloudpivot-test，不碰 144
set -u

PID=$(ss -ltnp 2>/dev/null | grep ':8079' | sed -E 's/.*pid=([0-9]+).*/\1/' | head -1)
if [ -n "${PID:-}" ]; then
    kill "$PID" 2>/dev/null
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        ss -ltn 2>/dev/null | grep -q ':8079' || break
        sleep 1
    done
fi

cd /root/pdf04-app/backend || exit 1
set -a
# shellcheck disable=SC1091
. /root/pdf05.env
set +a
setsid ./target/debug/cloudpivot >>/root/pdf05_app_145.log 2>&1 </dev/null &
echo "started pid=$!"
