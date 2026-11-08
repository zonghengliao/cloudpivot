#!/usr/bin/env bash
# ============================================================================
# ATT-03（t_761295b2）145 取证 · 准备：拉代码 / 编二进制 / 拷贝库 / 起隔离实例
#
# 跑法：scp 到 145 后 `bash /root/att03/10_prepare.sh`
#
# 隔离原则：
#   · 8033（cloudpivot-test，服役实例）与 /etc 里的 cloudpivot-dev.env 一律不改；
#   · 本卡用 18229 + cloudpivot_dev 的**拷贝库** cloudpivot_att03（上传 / 改脏数据都落在拷贝库）；
#   · 令牌落 /root/att03/tokens，不进仓库。
# ============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
ROOT=/root/att03
mkdir -p "$ROOT"
exec > >(tee -a "$ROOT/prepare.log") 2>&1

echo "=== ATT-03 prepare $(date '+%F %T') ==="

# ---------- 1. 拉代码 ----------
cd /opt/cloudpivot || exit 1
git fetch -q origin
git pull --ff-only origin main || { echo "FATAL: git pull 失败"; exit 1; }
echo "HEAD=$(git rev-parse --short HEAD) $(git log -1 --format=%s)"
grep -q "uploader_name" backend/src/support/file/dto.rs || { echo "FATAL: 代码里没有 uploader_name"; exit 1; }

# ---------- 2. 编二进制 ----------
cd /opt/cloudpivot/backend || exit 1
cargo build 2>&1 | tail -5
BIN=/opt/cloudpivot/backend/target/debug/cloudpivot
[ -x "$BIN" ] || { echo "FATAL: 二进制不存在"; exit 1; }
grep -ac "uploader_name" "$BIN" | sed 's/^/二进制含 uploader_name 字面量次数: /'

# ---------- 3. 拷贝库 ----------
set -a
# shellcheck disable=SC1091
. /etc/cloudpivot/cloudpivot-dev.env
set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB=cloudpivot_att03
# 停旧实例（按 pid 文件，别 pkill）
if [ -f "$ROOT/instance.pid" ]; then
  kill "$(cat "$ROOT/instance.pid")" 2>/dev/null && sleep 2
  rm -f "$ROOT/instance.pid"
fi
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d postgres -X -q -c "drop database if exists $DB" 2>/dev/null ||
  su - postgres -c "psql -X -q -c \"drop database if exists $DB\"" 2>/dev/null
if ! psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d postgres -X -q -c "create database $DB" 2>/dev/null; then
  su - postgres -c "psql -X -q -c \"create database $DB owner $NIK_DATABASE_USER\"" || { echo "FATAL: 建库失败"; exit 1; }
fi
# 结构 + 数据都拷（沿用既有卡的拷贝库打法）
pg_dump -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d cloudpivot_dev --no-owner --no-acl |
  psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$DB" -X -q > /dev/null
echo "拷贝库 $DB 行数抽样: sys_file=$(psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$DB" -tAc 'select count(*) from sys_file')"

# ---------- 4. 起隔离实例（18229） ----------
mkdir -p "$ROOT/upload"
export NIK_DATABASE_NAME="$DB"
export NIK_SERVER_LISTEN=127.0.0.1:18229
export NIK_REDIS_URL=redis://127.0.0.1:6379/9
cd /opt/cloudpivot/backend || exit 1
setsid nohup ./target/debug/cloudpivot > "$ROOT/server.log" 2>&1 < /dev/null &
echo $! > "$ROOT/instance.pid"
for i in $(seq 1 60); do
  code=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:18229/health || true)
  [ "$code" = "200" ] && { echo "health 200 after ${i}s (pid $(cat "$ROOT/instance.pid"))"; break; }
  sleep 1
done
[ "$code" = "200" ] || { echo "FATAL: /health 非 200（见 $ROOT/server.log）"; tail -20 "$ROOT/server.log"; exit 1; }
grep -E "数据库迁移完成|安全守卫" "$ROOT/server.log" | tail -3
echo "=== prepare done ==="
