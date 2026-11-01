#!/usr/bin/env bash
# R5 生产部署（144）：拉代码 → 编 release → 编前端 dist → 重启 → 核对
# 用法（在 144 上）：bash scripts/verify/r5_deploy_144.sh
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
TS=$(date +%Y%m%d%H%M%S)
cd /opt/cloudpivot

echo "== 1) 拉代码（main 跟踪 bare 远端，必须 ff-only）=="
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
for token in btn-opening-refresh opening-period-select opening-state-tag opening-tabs btn-opening-save btn-opening-save-inventory opening-balance-state opening-voucher-info opening-readonly-alert btn-opening-add-row; do
  n=$(grep -rl "$token" dist/assets/*.js 2>/dev/null | wc -l)
  echo "  $token → $n"
done

echo "== 5) 重启服务 =="
systemctl restart cloudpivot
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

echo "== 7) 迁移与启动公告 =="
journalctl -u cloudpivot --since "-5 min" --no-pager | grep -E "数据库迁移完成|V0189|自检通过" | tail -8
echo "== 8) 期初节点 / 约束 / 索引 =="
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -tAc \
  "select max(version::int) from schema_migrations"
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -tAc \
  "select count(*) from sys_menu where auth_code like 'business:erp:opening:%' and deleted_at is null"
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -tAc \
  "select pg_get_constraintdef(oid) from pg_constraint where conname='ck_biz_voucher_type'"
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -tAc \
  "select indexname from pg_indexes where indexname='uk_biz_voucher_opening_period'"

echo "== 9) 静态首页 =="
curl -s -o /dev/null -w 'index=%{http_code}\n' http://127.0.0.1/
echo "DONE"
