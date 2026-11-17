#!/usr/bin/env bash
# 诊断力证明：用**新二进制**（含 nik-db-core 错误消息修复）+ **旧版 V0442**（硬护栏）
# 在 441 状态的库上启动 —— 期望日志里直接出现 V0442 的真实 RAISE 文案，而不是
# `query failed: : db error`。
set -uo pipefail
WS=/root/.hermes/kanban/boards/dev/workspaces/t_08e1816a
REPO=/opt/cloudpivot
DB=cloudpivot_test_v0442a
PORT=18243
MIG=$WS/mig_old442
LOG=$WS/logs/diag_after_kernel_fix.log
PROFILE=v0442tmp

rm -rf "$MIG"
mkdir -p "$MIG"
cp -a "$REPO/backend/app/migrations/." "$MIG/"
# 换回 HEAD 上的旧版（硬护栏）
git -C "$REPO" show HEAD:backend/app/migrations/business/V0442__perm_gate_orphans_and_n20.up.sql \
  > "$MIG/business/V0442__perm_gate_orphans_and_n20.up.sql"
echo "== 临时迁移目录里的 V0442 是否仍是旧版（应含硬 RAISE）"
grep -c "先应用系统菜单迁移';" "$MIG/business/V0442__perm_gate_orphans_and_n20.up.sql"

cat > "$REPO/backend/config/config-$PROFILE.toml" <<EOF
# 临时 profile（t_08e1816a 诊断力证明用，用完删除，不入库）
[database.migration]
path = "$MIG"
auto_migrate = true

[log]
level = "info"
EOF

export NIK_PROFILE=$PROFILE
export NIK_DATABASE_NAME="$DB"
export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_USER=cloudpivot
export NIK_DATABASE_PASSWORD=fbb1263691ff75c31ec99eaf258138b4
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32)
export CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32)
export NIK_ADMIN_PASSWORD='Admin@123456'

: > "$LOG"
cd "$REPO/backend" || exit 1
timeout 120 ./target/debug/cloudpivot > "$LOG" 2>&1
rc=$?
echo "== 退出码 $rc（未超时=120，说明启动自检失败并退出；124=超时被 kill）"
echo "== 日志里的迁移失败相关行"
grep -n '迁移失败\|FATAL\|V0442\|db error' "$LOG" | head -10

# 清理
rm -f "$REPO/backend/config/config-$PROFILE.toml"
