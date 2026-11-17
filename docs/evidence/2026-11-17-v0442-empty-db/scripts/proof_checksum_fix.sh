#!/usr/bin/env bash
# 指纹修复证据（不碰 cloudpivot_dev 的活动会话）：
#   ① 口径校准（旧/新指纹实算 vs cloudpivot_dev 记录）
#   ② 反证：把 442 的 checksum 改回旧值（= 「应用过旧版 V0442 的库」）→ 新二进制启动被挡住
#   ③ 正证：同一库跑 apply.sh --apply → 再启动 → 迁移与 bootstrap 正常
#   ④ 幂等：再跑一次 apply.sh = no-op
set -uo pipefail
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
WS=/root/.hermes/kanban/boards/dev/workspaces/t_08e1816a
REPO=/opt/cloudpivot
H=172.18.10.145
U=cloudpivot
OLD=2e6fbee6a98c6ca081ada172f91623b922bad2ea459f431db50cce98b11cdafe
BASE=cloudpivot_test_v0442b      # 已修好的新库（442 记录 = 新指纹，443 已应用）
DB=cloudpivot_test_v0442e        # 用来模拟「应用过旧版 442」的库
PORT=18245
LOG=$WS/logs/checksum_probe.log
OLD_FILE=$WS/old_v0442_up.sql
git -C "$REPO" show HEAD:backend/app/migrations/business/V0442__perm_gate_orphans_and_n20.up.sql > "$OLD_FILE"

q() { psql -h "$H" -U $U -d "$1" -Atc "$2"; }

echo "===== ① 指纹口径校准"
echo -n "旧版 V0442 实算 : "; python3 "$REPO/scripts/datafix/v0442-checksum/recompute_checksum.py" "$OLD_FILE"
echo -n "dev 记录        : "; q cloudpivot_dev "select checksum from schema_migrations where kind='versioned' and version='442'"
echo -n "新版 V0442 实算 : "; python3 "$REPO/scripts/datafix/v0442-checksum/recompute_checksum.py" "$REPO/backend/app/migrations/business/V0442__perm_gate_orphans_and_n20.up.sql"
echo -n "新库 442 记录   : "; q $BASE "select checksum from schema_migrations where kind='versioned' and version='442'"

echo
echo "===== 造「应用过旧版 442」的库 $DB"
dropdb -h "$H" -U $U --if-exists --force $DB >/dev/null 2>&1
createdb -h "$H" -U $U -O $U -T $BASE $DB && echo "    createdb -T $BASE OK"
psql -h "$H" -U $U -d $DB -Atc "update schema_migrations set checksum='$OLD' where kind='versioned' and version='442'"
echo -n "    现值 = "; q $DB "select checksum from schema_migrations where kind='versioned' and version='442'"

start_and_report() { # <标签>
  export NIK_PROFILE=test NIK_DATABASE_NAME=$DB NIK_DATABASE_HOST=$H NIK_DATABASE_PORT=5432
  export NIK_DATABASE_USER=$U NIK_DATABASE_PASSWORD=$PGPASSWORD
  export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
  export NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32)
  export CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32)
  export NIK_ADMIN_PASSWORD='Admin@123456'
  : > "$LOG"
  ( cd "$REPO/backend" && timeout 90 ./target/debug/cloudpivot >> "$LOG" 2>&1 )
  echo "    [$1] 退出码 $?（1=启动失败退出；124=跑满 90s 被我停掉=起服务成功）"
  grep -a 'migration 442\|MigrationChecksum\|数据库迁移失败\|FATAL\|数据库迁移完成' "$LOG" | head -4 | sed 's/^/      /'
}

echo
echo "===== ② 反证：不修指纹就启动"
start_and_report "修前"

echo
echo "===== ③ 修指纹（apply.sh --apply）"
ERP_DB_URL="postgres://$U:$PGPASSWORD@$H:5432/$DB" \
  bash "$REPO/scripts/datafix/v0442-checksum/apply.sh" --apply 2>&1 | sed 's/^/    /'
start_and_report "修后"

echo
echo "===== ④ 幂等：再跑一次 apply.sh"
ERP_DB_URL="postgres://$U:$PGPASSWORD@$H:5432/$DB" \
  bash "$REPO/scripts/datafix/v0442-checksum/apply.sh" 2>&1 | tail -2 | sed 's/^/    /'
