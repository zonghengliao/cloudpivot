#!/usr/bin/env bash
# ============================================================================
# FIX-6 145 取证 · 收尾：只杀本卡的隔离实例（8047/8049 的 cloudpivot-fix6），
# 并复核 8033 服役实例仍然健康（cloudpivot_dev 一个字节都没动）
# ============================================================================
set -uo pipefail

echo "--- kill 本卡隔离实例（按二进制名，不按端口乱杀）"
if [ -f /root/backup/.fix6_pid ]; then
  kill "$(cat /root/backup/.fix6_pid)" 2>/dev/null || true
fi
pkill -f 'target/debug/cloudpivot-fix6' 2>/dev/null || true
sleep 2
LEFT=$(pgrep -fc 'target/debug/cloudpivot-fix6' || true)
echo "残留隔离进程数=${LEFT:-0}"

echo "--- 8033 服役实例健康复核（cloudpivot-test.service）"
systemctl is-active cloudpivot-test.service || true
curl -s -m 3 -o /dev/null -w 'health=%{http_code}\n' http://127.0.0.1:8033/health || true

echo "--- cloudpivot_dev 的税率形态（应仍是迁移前的样子：本卡从未连过它）"
export PGPASSWORD=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' /etc/cloudpivot/cloudpivot-dev.env)
psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_dev -X -tAc \
  "select 'invoice_mode_col=' || count(*) from information_schema.columns where table_name='biz_invoice' and column_name='tax_rate_mode'"
