#!/bin/bash
# CODE-01 在 145（测试环境）上的发布：pull → 编 debug 二进制 → 重启 → 等 health
set -u
export PATH=/root/.cargo/bin:$PATH
LOG=/tmp/code01-deploy.log
exec >"$LOG" 2>&1
echo "==== $(date -Is) CODE-01 deploy start"

cd /opt/cloudpivot || exit 1
echo "-- 当前 HEAD: $(git rev-parse --short HEAD)"
git pull --ff-only origin main || { echo "PULL FAILED"; exit 1; }
echo "-- pull 后 HEAD: $(git rev-parse --short HEAD)"
git log --oneline -1

echo "-- 确认 V0337 文件在盘上"
ls -l backend/migrations/business/V0337__master_data_seq.up.sql

cd backend || exit 1
echo "-- cargo build（debug，增量）"
cargo build --bin cloudpivot || { echo "BUILD FAILED"; exit 1; }
echo "-- 二进制: $(ls -l target/debug/cloudpivot | awk '{print $6,$7,$8,$5}')"

echo "-- 二进制里是否含 seq 模块符号（新代码真的进去了）"
if grep -aq 'master_data_seq' target/debug/cloudpivot; then
  echo "OK: 二进制含 master_data_seq 文案"
else
  echo "WARN: 二进制未见 master_data_seq（可能被优化掉，继续）"
fi

echo "-- systemctl restart cloudpivot-test"
systemctl restart cloudpivot-test
for i in $(seq 1 60); do
  code=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8023/health 2>/dev/null)
  if [ "$code" = "200" ]; then echo "-- health 200（第 ${i} 次探活）"; break; fi
  sleep 2
done
echo "-- systemd 状态: $(systemctl is-active cloudpivot-test)"
echo "-- 迁移日志"
journalctl -u cloudpivot-test --since '-3 min' --no-pager | grep -E '迁移|V0337|master_data' | tail -10
echo "==== $(date -Is) CODE-01 deploy done"
