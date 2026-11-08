#!/usr/bin/env bash
# CTR-02-F1 隔离实例环境（99 本机）：空库 cloudpivot_ctr02 + 127.0.0.1:18228
#   up    ：重建空库 + 起实例（首启自动跑迁移）
#   down  ：停实例（不动库）
#   reset ：停实例 + 重建空库
#
# 二进制来自隔离 worktree /root/ctr02f1-wt（HEAD=356c57d + 本卡 3 个文件），
# 独立 target /root/ctr02f1-target —— 不碰共享工作区的 target，也不碰 :8023 / 145 / 144。
set -uo pipefail
DB=cloudpivot_ctr02
PORT=18228
BIN=/root/ctr02f1-target/debug/cloudpivot
LOG=/tmp/ctr02f1_app.log
PIDF=/tmp/ctr02f1_app.pid

export PATH=/root/.cargo/bin:$PATH
export PGPASSWORD=alun
PSQL_Q() { psql -h 127.0.0.1 -U alun -At -d "$DB" -c "$1"; }

stop() {
  if [ -f "$PIDF" ]; then
    kill "$(cat "$PIDF")" 2>/dev/null || true
    sleep 2
    kill -9 "$(cat "$PIDF")" 2>/dev/null || true
    rm -f "$PIDF"
  fi
  fuser -k "${PORT}/tcp" 2>/dev/null || true
  sleep 1
}

recreate() {
  su - postgres -c "psql -At -c \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$DB'\"" >/dev/null 2>&1 || true
  su - postgres -c "dropdb --if-exists $DB" >/dev/null 2>&1 || echo "WARN: dropdb 失败（可能不存在）"
  su - postgres -c "createdb -O alun $DB" || { echo "FATAL: createdb 失败"; exit 1; }
  echo "✅ 空库 $DB 已重建（owner alun），迁移由后端首启自跑"
}

case "${1:-up}" in
  down) stop; echo "实例已停"; exit 0;;
  reset) stop; recreate; exit 0;;
  up) : ;;
  *) echo "用法：$0 {up|down|reset}"; exit 1;;
esac

stop
# 只有 reset 才需要重建库；up 保留已有库（便于复跑 drill）
[ "${2:-}" = "--fresh" ] && recreate
psql -h 127.0.0.1 -U alun -lqt 2>/dev/null | cut -d'|' -f1 | tr -d ' ' | grep -qx "$DB" || recreate

cd /root/ctr02f1-wt/backend || exit 1
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
export NIK_ADMIN_PASSWORD=Admin@123456

setsid nohup "$BIN" >"$LOG" 2>&1 </dev/null &
echo $! >"$PIDF"

code=""
for _ in $(seq 1 150); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$PORT/health" || true)
  [ "$code" = "200" ] && break
  if ! kill -0 "$(cat "$PIDF")" 2>/dev/null; then echo "FATAL: 实例提前退出"; tail -40 "$LOG"; exit 1; fi
  sleep 2
done
[ "$code" = "200" ] || { echo "FATAL: health=$code"; tail -40 "$LOG"; exit 1; }
grep -q '安全守卫拒绝启动' "$LOG" && { echo "FATAL: 安全守卫"; tail -20 "$LOG"; exit 1; }

echo "✅ 实例健康 health=$code port=$PORT pid=$(cat "$PIDF") log=$LOG"
echo "--- 环境指纹"
echo "bin_sha256=$(sha256sum "$BIN" | cut -d' ' -f1)"
echo "git_head=$(git -C /root/ctr02f1-wt rev-parse HEAD)"
echo "worktree_dirty=$(git -C /root/ctr02f1-wt status --porcelain | tr '\n' ';')"
echo "migrations=$(PSQL_Q 'select count(*) from schema_migrations where success')"
echo "max_migration=$(PSQL_Q 'select max(version) from schema_migrations where success')"
echo "seed=$(PSQL_Q "select 'users='||(select count(*) from sys_user)||' sup='||(select count(*) from biz_supplier)||' cust='||(select count(*) from biz_customer)||' contract='||(select count(*) from biz_contract)||' job='||(select count(*) from sys_job)")"
echo "health_body=$(curl -s -m 5 http://127.0.0.1:$PORT/health)"
