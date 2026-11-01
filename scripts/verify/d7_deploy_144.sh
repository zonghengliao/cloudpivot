#!/usr/bin/env bash
# D7 生产部署（144）：拉代码 → 编 release → 编前端 dist → 重启 → 结构/只读冒烟
# 用法（在 144 上）：bash /tmp/d7_deploy_144.sh
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
TS=$(date +%Y%m%d%H%M%S)
cd /opt/cloudpivot || exit 1

echo "== 1) 拉代码（main 跟踪 bare 远端，必须 ff-only）=="
git fetch origin
git merge --ff-only origin/main || { echo "MERGE_FAILED"; exit 1; }
git log --oneline -1

echo "== 2) 备份旧二进制 =="
cp -a backend/target/release/cloudpivot /root/cloudpivot.bak.$TS && echo "backup=/root/cloudpivot.bak.$TS"

echo "== 3) 编 release（增量）=="
cd backend
cargo build --release 2>&1 | tail -3
BIN_MD5=$(md5sum target/release/cloudpivot | awk '{print $1}')
ls -l target/release/cloudpivot
echo "binary_md5=$BIN_MD5"

echo "== 4) 编前端 dist =="
cd ../frontend
OLD_ENTRY=$(grep -oh "BasicLayout-[A-Za-z0-9_-]*\.js" dist/index.html dist/assets/index-*.js 2>/dev/null | head -1)
pnpm build 2>&1 | tail -4
NEW_ENTRY=$(grep -oh "BasicLayout-[A-Za-z0-9_-]*\.js" dist/index.html dist/assets/index-*.js 2>/dev/null | head -1)
echo "entry_chunk: ${OLD_ENTRY:-<none>} → ${NEW_ENTRY:-<none>}"
echo "-- 8 个页面的批量 testid（每个应命中 ≥1 个 js）--"
for token in stockin-batch purchase-batch transfer-batch payable-batch voucher-batch \
             serial-batch flow-batch task-batch; do
  n=$(grep -rl "$token" dist/assets/*.js 2>/dev/null | wc -l)
  echo "  $token → $n"
done

echo "== 5) 重启服务 =="
systemctl restart cloudpivot
code=""
for i in $(seq 1 60); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 http://127.0.0.1:8023/health || true)
  [ "$code" = "200" ] && break
  sleep 2
done
echo "health=$code"

echo "== 6) 进程 == 磁盘 核对 =="
MAINPID=$(systemctl show -p MainPID --value cloudpivot)
PROC_MD5=$(md5sum /proc/$MAINPID/exe | awk '{print $1}')
echo "main_pid=$MAINPID proc_md5=$PROC_MD5 disk_md5=$BIN_MD5"
[ "$PROC_MD5" = "$BIN_MD5" ] && echo "PROC_EQ_DISK=OK" || echo "PROC_EQ_DISK=FAIL"

echo "== 7) 启动日志（迁移 / 自检 / 报错）=="
journalctl -u cloudpivot --since "-6 min" --no-pager | grep -E "数据库迁移完成|自检通过|ERROR|FATAL" | tail -6

echo "== 8) 只读冒烟：11 条批量路由全部已注册（未登录应 401，不是 404）=="
for path in \
  /api/business/erp/stock-in/batch-status \
  /api/business/erp/stock-in/batch-delete \
  /api/business/erp/stock-in/batch-export \
  /api/business/erp/purchase-order/batch-status \
  /api/business/erp/transfer/batch-status \
  /api/business/erp/serial/batch-status \
  /api/business/erp/payable/batch-export \
  /api/business/erp/voucher/batch-export \
  /api/business/flow/task/batch-approve \
  /api/business/task/batch-status \
  /api/business/task/batch-assign ; do
  out=$(curl -s -o /dev/null -w '%{http_code}' -m 5 -X POST -H 'Content-Type: application/json' \
        -d '{"ids":[]}' "http://127.0.0.1:8023$path")
  echo "  $out  $path"
done

echo "== 9) 生产不变量（只读）=="
cd /opt/cloudpivot
python3 scripts/verify/erp_invariants.py 2>&1 | tail -4
