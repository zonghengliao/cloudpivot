#!/usr/bin/env bash
# D5 生产部署（144）：拉代码 → 编 release → 编前端 dist → 重启 → 核对
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
TS=$(date +%Y%m%d%H%M%S)
cd /opt/cloudpivot

echo "== 1) 拉代码（main 跟踪 github，必须 merge origin/main）=="
git fetch origin
git merge --ff-only origin/main || { echo "MERGE_FAILED"; exit 1; }
git log --oneline -1

echo "== 2) 备份旧二进制 =="
cp -a backend/target/release/cloudpivot /root/cloudpivot.bak.$TS && echo "backup=/root/cloudpivot.bak.$TS"

echo "== 3) 编 release（只编增量）=="
cd backend
cargo build --release 2>&1 | tail -3
BIN_MD5=$(md5sum target/release/cloudpivot | awk '{print $1}')
ls -l target/release/cloudpivot
echo "binary_md5=$BIN_MD5"

echo "== 4) 编前端 dist =="
cd ../frontend
OLD_ENTRY=$(grep -oh "BasicLayout-[A-Za-z0-9_-]*\.js" dist/index.html dist/assets/index-*.js 2>/dev/null | head -1)
pnpm build 2>&1 | tail -6
NEW_ENTRY=$(grep -oh "BasicLayout-[A-Za-z0-9_-]*\.js" dist/index.html dist/assets/index-*.js 2>/dev/null | head -1)
echo "entry_chunk: ${OLD_ENTRY:-<none>} → ${NEW_ENTRY:-<none>}"
echo "-- 新包特征（每个应命中 ≥1 个文件）--"
for token in btn-payable-drawer-pay payable-pay-blocked payable-overpay-risk payable-payment-voucher- payable-payment-payable- payable-drawer-actions; do
  n=$(grep -rl "$token" dist/assets/*.js 2>/dev/null | wc -l)
  echo "  $token → $n"
done

echo "== 5) 重启服务 =="
systemctl restart cloudpivot
for i in $(seq 1 40); do
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

echo "== 7) 迁移与启动公告 =="
journalctl -u cloudpivot --since "-3 min" --no-pager | grep -E "数据库迁移完成|自检通过|NOTICE" | tail -6
echo "== 8) 静态首页 =="
curl -s -o /dev/null -w 'index=%{http_code}\n' http://127.0.0.1/
echo "DONE"
