#!/usr/bin/env bash
# P1-FIX-5C：起 / 停隔离实例（库 cloudpivot_lnk5b，端口 18132）。
#
#   BIN=/root/p1fix5c-target/debug/cloudpivot REPO=/opt/cloudpivot \
#     bash p1fix5c_instance.sh start | stop | status
#
# 与 5B 的脚本同一套口径（同一个隔离库，因为本卡**不新增迁移**、不改 schema：
# 库里的 3 条 5B 预置规则与库存数据直接复用）。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT=${REPO:-/opt/cloudpivot}
BIN=${BIN:-/root/p1fix5c-target/debug/cloudpivot}
DB=${LNK_DB:-cloudpivot_lnk5b}
PORT=${LNK_PORT:-18132}
LOG=${LNK_LOG:-/root/p1fix5c-server.log}
PIDFILE=${LNK_PIDFILE:-/root/p1fix5c-server.pid}

start() {
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    echo "已在运行 pid=$(cat "$PIDFILE")"
    return 0
  fi
  [ -x "$BIN" ] || { echo "二进制不存在：$BIN"; return 1; }
  export NIK_PROFILE=test
  export NIK_DATABASE_NAME="$DB"
  export NIK_DATABASE_HOST=127.0.0.1
  export NIK_DATABASE_PORT=5432
  export NIK_DATABASE_USER=alun
  export NIK_DATABASE_PASSWORD=alun
  export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
  export NIK_MIDDLEWARE_AUTH_ENABLED=true
  export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
  export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
  export NIK_ADMIN_PASSWORD='Admin@123456'
  rm -f "$LOG"
  cd "$ROOT/backend" || exit 1
  setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
  echo $! > "$PIDFILE"
  echo "启动 pid=$(cat "$PIDFILE") 库=$DB 端口=$PORT 日志=$LOG 树=$ROOT"
  for _ in $(seq 1 240); do
    code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" || true)
    if [ "$code" = "200" ]; then
      echo "健康检查通过（/health 200）"
      grep -m1 "数据库迁移完成" "$LOG" || true
      grant_linkage
      return 0
    fi
    kill -0 "$(cat "$PIDFILE")" 2>/dev/null || { echo "进程已退出，看 $LOG"; return 1; }
    sleep 1
  done
  echo "健康检查超时，看 $LOG"
  return 1
}

grant_linkage() {
  PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -At -v ON_ERROR_STOP=1 <<'SQL'
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, m.id
  FROM sys_role r
  CROSS JOIN (VALUES
        ('20000000-0000-0000-0000-000000001230'::uuid),
        ('20000000-0000-0000-0000-000000001231'::uuid),
        ('20000000-0000-0000-0000-000000001232'::uuid)) AS m(id)
 WHERE r.deleted_at IS NULL AND r.role_code = 'SUPER_ADMIN'
   AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id);
SQL
  local n
  n=$(PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc \
      "SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id
        JOIN sys_menu m ON m.id = rm.menu_id
       WHERE r.role_code = 'SUPER_ADMIN' AND m.auth_code LIKE 'business:flow:linkage:%'")
  echo "linkage 授权行（超管）= $n（应为 3）"
}

stop() {
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    kill "$(cat "$PIDFILE")"
    for _ in $(seq 1 30); do
      kill -0 "$(cat "$PIDFILE")" 2>/dev/null || break
      sleep 1
    done
    echo "已停 pid=$(cat "$PIDFILE")"
  else
    echo "没在运行"
  fi
  rm -f "$PIDFILE"
}

case "${1:-status}" in
  start) start ;;
  stop) stop ;;
  status)
    if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
      echo "running pid=$(cat "$PIDFILE")"
      curl -s -o /dev/null -w 'health=%{http_code}\n' "http://127.0.0.1:$PORT/health"
    else
      echo "stopped"
    fi
    ;;
  *) echo "用法：$0 {start|stop|status}" ;;
esac
