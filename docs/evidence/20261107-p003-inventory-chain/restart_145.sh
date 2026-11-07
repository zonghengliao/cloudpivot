#!/usr/bin/env bash
# 145 上重启测试环境后端并等 /health（无人值守时 systemctl 必须走脚本，不能内联）
set -u
systemctl restart cloudpivot-test
for i in $(seq 1 15); do
  sleep 2
  code=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8033/health)
  if [ "$code" = "200" ]; then
    echo "health=200 after $((i*2))s"
    break
  fi
done
echo "service=$(systemctl is-active cloudpivot-test)"
ps -eo pid,lstart,cmd | grep 'target/debug/cloudpivot$' | grep -v grep | head -2
