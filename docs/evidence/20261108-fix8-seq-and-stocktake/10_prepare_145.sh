#!/usr/bin/env bash
# ============================================================================
# FIX-8（t_6ddb0036）145 取证 · 步骤 1：拉代码 / 建二进制副本 / 建**拷贝库**
#
# 为什么用 cloudpivot_dev 的拷贝：
#   · 本卡要证明「跳号是测试推进、不是全局单行」—— 需要库里**真实存在的号段水位**
#     （TK current_no=98 而库内只有 2 张 TK 单，这个对照只有在存量数据上才成立）；
#   · 同时证明「存量库升级」路径（启动时把本卡之前的迁移应用到老库上）。
#   8033 服役实例不动（它连的仍是 cloudpivot_dev，本卡一个字节都不改它）。
#
# 跑法：ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix8-seq-and-stocktake/10_prepare_145.sh'
# ============================================================================
set -euo pipefail
export PATH=/root/.cargo/bin:$PATH

cd /opt/cloudpivot
git fetch -q origin main
git pull --ff-only origin main
echo "HEAD=$(git rev-parse --short HEAD) $(git log -1 --format=%s)"

cd /opt/cloudpivot/backend
cargo build --bin cloudpivot
install -m 0755 target/debug/cloudpivot target/debug/cloudpivot-fix8
echo "binary_ok=$(stat -c %s target/debug/cloudpivot-fix8) bytes"
cd /opt/cloudpivot

export PGPASSWORD=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' /etc/cloudpivot/cloudpivot-dev.env)
mkdir -p /root/backup
TS=$(date +%Y%m%d-%H%M%S)

DUMP=/root/backup/cloudpivot_dev_FIX8_${TS}.dump
pg_dump -h 127.0.0.1 -U cloudpivot -Fc -f "$DUMP" cloudpivot_dev
echo "backup=$DUMP ($(du -h "$DUMP" | cut -f1))"

DB="cloudpivot_fix8_${TS//-/}"
psql -h 127.0.0.1 -U cloudpivot -d postgres -c "CREATE DATABASE $DB OWNER cloudpivot" 2>/dev/null \
  || su - postgres -c "psql -v ON_ERROR_STOP=1 -c \"CREATE DATABASE $DB OWNER cloudpivot\""
pg_restore -h 127.0.0.1 -U cloudpivot -d "$DB" --no-owner --no-privileges "$DUMP"

echo "$DB" > /root/backup/.fix8_db
echo "db=$DB"
echo "迁移前 schema 版本：$(psql -h 127.0.0.1 -U cloudpivot -d "$DB" -tAc "select max(version::int) from schema_migrations")"

EV="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
mkdir -p "$EV/raw"
# 号段水位快照：本卡「不是全局单行 + 跳号来自测试」的原始证据
psql -h 127.0.0.1 -U cloudpivot -d "$DB" -X -tAF'|' -c "
  select 'seq_rows', count(*)::text from biz_seq where deleted_at is null
  union all
  select 'seq_' || seq_type, current_no::text from biz_seq where deleted_at is null
  order by 1" > "$EV/raw/pre_seq_state.txt"
psql -h 127.0.0.1 -U cloudpivot -d "$DB" -X -tAF'|' -c "
  select 'TK_docs_in_db', count(*)::text, min(transfer_no), max(transfer_no)
    from biz_stock_transfer where transfer_no like 'TK-%'
  union all
  select 'TK_watermark', current_no::text, period_key, ''
    from biz_seq where seq_type = 'TK' and deleted_at is null" > "$EV/raw/tk_jump_evidence.txt"
echo "号段快照："
cat "$EV/raw/tk_jump_evidence.txt"
