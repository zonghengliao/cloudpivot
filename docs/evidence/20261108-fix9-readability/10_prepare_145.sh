#!/usr/bin/env bash
# ============================================================================
# FIX-9 145 取证 · 步骤 1：拉代码 / 建二进制副本 / 建**拷贝库**
#
# 为什么用 cloudpivot_dev 的拷贝：
#   · ② 要证明「物料没配存货科目时摘要仍写物料编码」—— 需要**演示物料 DEMO-P101**
#     （`inventory_account_id IS NULL`）这种存量形状；
#   · ③ 要证明「姓名快照」—— 需要真实 `sys_user.real_name`（超级管理员 等）；
#   · ① 要证明「审批意见回写」—— 需要已发布的 `stock_take_post` 流程。
#   8033 服役实例不动（它连的仍是 cloudpivot_dev，本卡一个字节都不改它）。
#
# 跑法：ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix9-readability/10_prepare_145.sh'
# ============================================================================
set -euo pipefail
export PATH=/root/.cargo/bin:$PATH

cd /opt/cloudpivot
git fetch -q origin main
git pull --ff-only origin main
echo "HEAD=$(git rev-parse --short HEAD) $(git log -1 --format=%s)"

cd /opt/cloudpivot/backend
cargo build --bin cloudpivot
install -m 0755 target/debug/cloudpivot target/debug/cloudpivot-fix9
echo "binary_ok=$(stat -c %s target/debug/cloudpivot-fix9) bytes"
cd /opt/cloudpivot

export PGPASSWORD=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' /etc/cloudpivot/cloudpivot-dev.env)
mkdir -p /root/backup
TS=$(date +%Y%m%d-%H%M%S)

DUMP=/root/backup/cloudpivot_dev_FIX9_${TS}.dump
pg_dump -h 127.0.0.1 -U cloudpivot -Fc -f "$DUMP" cloudpivot_dev
echo "backup=$DUMP ($(du -h "$DUMP" | cut -f1))"

DB="cloudpivot_fix9_${TS//-/}"
psql -h 127.0.0.1 -U cloudpivot -d postgres -c "CREATE DATABASE $DB OWNER cloudpivot" 2>/dev/null \
  || su - postgres -c "psql -v ON_ERROR_STOP=1 -c \"CREATE DATABASE $DB OWNER cloudpivot\""
pg_restore -h 127.0.0.1 -U cloudpivot -d "$DB" --no-owner --no-privileges "$DUMP"

echo "$DB" > /root/backup/.fix9_db
echo "db=$DB"
echo "迁移前 schema 版本：$(psql -h 127.0.0.1 -U cloudpivot -d "$DB" -tAc "select max(version::bigint) from schema_migrations where version ~ '^[0-9]+\$'")"

EV="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
mkdir -p "$EV/raw"
# 前置形状快照：演示物料（无存货科目）/ 审批人 / 已发布流程 —— 后面每条断言都靠它们
psql -h 127.0.0.1 -U cloudpivot -d "$DB" -X -tAF'|' -c "
  select 'material', material_code, coalesce(inventory_account_id::text,'NULL')
    from biz_material where material_code in ('DEMO-P101','DEMO-P102') and deleted_at is null
  union all
  select 'user', username, coalesce(real_name,'NULL')
    from sys_user where username in ('admin','demo_lead') and deleted_at is null
  union all
  select 'flow', flow_code, status
    from biz_flow_def where flow_code in ('stock_take_post','payable_pay')
  order by 1,2" > "$EV/raw/00_fixture_shape.txt"
cat "$EV/raw/00_fixture_shape.txt"
