#!/usr/bin/env bash
# CTR-02 隔离实例环境（99 本机）：空库 cloudpivot_ctr02 + 127.0.0.1:18228
#   up   ：建库（若不存在）+ 起实例（首启自动跑迁移），日志 /tmp/ctr02_app.log
#   down ：kill 实例（不动库，便于复跑 drill）
#   reset：kill 实例 + 重建空库
#
# ⚠️ 只用于本机隔离验证：不碰 :8023 服役实例，不碰 145 / 144。
set -uo pipefail
cd /opt/cloudpivot/backend

DB=cloudpivot_ctr02
PORT=18228

export PATH=/root/.cargo/bin:$PATH
export NIK_PROFILE=test
export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_NAME=$DB
export NIK_DATABASE_USER=alun
export NIK_DATABASE_PASSWORD=alun
export NIK_SERVER_LISTEN=127.0.0.1:$PORT
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export X_CONFIG_SECRET_KEY="$(openssl rand -hex 16)"
# 只给隔离库用；首登仍被 must_change_password 强制改密
export NIK_ADMIN_PASSWORD=Admin@123456

case "${1:-up}" in
  up)
    su - postgres -c "createdb -O alun $DB" 2>/dev/null || true
    nohup ./target/debug/cloudpivot >/tmp/ctr02_app.log 2>&1 &
    echo "实例已起（pid=$!），日志 /tmp/ctr02_app.log；健康检查：curl -s 127.0.0.1:$PORT/health"
    ;;
  down)
    pkill -f "target/debug/cloudpivot" && echo "实例已停" || echo "没有在跑的实例"
    ;;
  reset)
    pkill -f "target/debug/cloudpivot" 2>/dev/null || true
    su - postgres -c "psql -tAc \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$DB'\"" >/dev/null
    su - postgres -c "dropdb --if-exists $DB" && su - postgres -c "createdb -O alun $DB"
    echo "库已重建：$DB"
    ;;
  *)
    echo "用法：$0 {up|down|reset}"; exit 1;;
esac
