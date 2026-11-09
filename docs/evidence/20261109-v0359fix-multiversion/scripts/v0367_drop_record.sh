#!/bin/bash
# 反证：把 V0359 的手工跳过记录删掉 → 迁移器会重跑 V0359 → 再次撞 uk_biz_flow_def_version
# （所以 144 上线**不能**先删那条记录；V0367 靠「保留记录 + 动态扫描」把问题真正修好）
set -uo pipefail
DB=cloudpivot_preview3
STAGE=/tmp/v0367stage2
BIN=/opt/cloudpivot/backend/target/debug/cloudpivot
LOG=/tmp/drop_record_rerun_v0359.log

echo "--- 删掉 manual-skip 记录（模拟卡面 ⑦「先删记录再部署」）---"
su - postgres -c "psql -d $DB -c \"delete from schema_migrations where version='359' and source='manual-skip-deploy-incident'\""

set -a
# shellcheck disable=SC1091
. /etc/cloudpivot/cloudpivot-dev.env
set +a
export NIK_DATABASE_NAME=$DB
export NIK_SERVER_LISTEN=127.0.0.1:18096
export NIK_REDIS_URL="$(printf '%s' "$NIK_REDIS_URL" | sed -E 's#/[0-9]+$##')/6"
export RUST_LOG=info
cd "$STAGE" || exit 1

timeout 120 "$BIN" > "$LOG" 2>&1
echo "service_rc=$?  （非 0 = 启动失败）"
echo "--- 迁移日志 ---"
grep -E "数据库迁移|duplicate key|uk_biz_flow_def_version|FATAL|ERROR|NOTICE:  V0359" "$LOG" | sed 's/^.*Z //' | head -20
