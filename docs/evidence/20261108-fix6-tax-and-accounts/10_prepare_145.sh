#!/usr/bin/env bash
# ============================================================================
# FIX-6（t_728f716e）145 取证 · 步骤 1：拉代码 / 建二进制副本 / 建**拷贝库**
#
# 跑法：ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix6-tax-and-accounts/10_prepare_145.sh'
#
# 为什么用 cloudpivot_dev 的拷贝而不是空库：
#   · 拷贝库有真实的科目表 / 客户 / 物料 / 历史单据，V0327 的「存量 0.13 → 13」才有活数据可迁；
#   · 同时天然证明**存量库升级**路径（启动时把 V0327/28/29 应用到有数据的老库上）。
# 8033 的服役实例不动（它连的还是 cloudpivot_dev，本卡一个字节都不改它）。
# ============================================================================
set -euo pipefail
export PATH=/root/.cargo/bin:$PATH

cd /opt/cloudpivot
git fetch -q origin main
git pull --ff-only origin main
echo "HEAD=$(git rev-parse --short HEAD) $(git log -1 --format=%s)"

cd /opt/cloudpivot/backend
cargo build --bin cloudpivot
install -m 0755 target/debug/cloudpivot target/debug/cloudpivot-fix6
echo "binary_ok=$(stat -c %s target/debug/cloudpivot-fix6) bytes"
cd /opt/cloudpivot

export PGPASSWORD=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' /etc/cloudpivot/cloudpivot-dev.env)
mkdir -p /root/backup
TS=$(date +%Y%m%d-%H%M%S)

# 迁移前备份（卡面要求：迁移前备份 145 测试库）
DUMP=/root/backup/cloudpivot_dev_FIX6_${TS}.dump
pg_dump -h 127.0.0.1 -U cloudpivot -Fc -f "$DUMP" cloudpivot_dev
echo "backup=$DUMP ($(du -h "$DUMP" | cut -f1))"

DB="cloudpivot_fix6_${TS//-/}"
psql -h 127.0.0.1 -U cloudpivot -d postgres -v ON_ERROR_STOP=0 -c "CREATE DATABASE $DB OWNER cloudpivot" 2>/dev/null \
  || su - postgres -c "psql -v ON_ERROR_STOP=1 -c \"CREATE DATABASE $DB OWNER cloudpivot\""
pg_restore -h 127.0.0.1 -U cloudpivot -d "$DB" --no-owner --no-privileges "$DUMP"

echo "$DB" > /root/backup/.fix6_db
echo "db=$DB"
echo "迁移前 schema 版本：$(psql -h 127.0.0.1 -U cloudpivot -d "$DB" -tAc "select max(version::int) from schema_migrations")"

EV="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
mkdir -p "$EV/raw"
# 迁移前金额快照：E2E 用它证明「存量单据金额一个字没动」
psql -h 127.0.0.1 -U cloudpivot -d "$DB" -X -tA -F'|' -c \
  "select 'invoice', count(*), coalesce(sum(amount),0), coalesce(sum(tax_amount),0), coalesce(sum(total_with_tax),0) from biz_invoice" \
  | sed 's/^invoice|/invoice|/' > "$EV/raw/pre_migration_amounts.txt"
psql -h 127.0.0.1 -U cloudpivot -d "$DB" -X -tAc \
  "select 'material_rows|' || count(*) from biz_material" >> "$EV/raw/pre_migration_amounts.txt"
echo "迁移前快照：" && cat "$EV/raw/pre_migration_amounts.txt"
echo "迁移前存量税率（应能看到小数形态）："
psql -h 127.0.0.1 -U cloudpivot -d "$DB" -X -c \
  "select 'invoice' as t, count(*) filter (where tax_rate > 0 and tax_rate <= 1) as decimal_rows, count(*) as total from biz_invoice
   union all
   select 'material', count(*) filter (where tax_rate > 0 and tax_rate <= 1), count(*) from biz_material"
