#!/usr/bin/env bash
# P1-FIX-5B：起 / 停隔离实例（库 cloudpivot_lnk5b，端口 18130）。
#
#   REPO=/root/lnk5b-wt BIN=/root/p1fix4-target/debug/cloudpivot bash p1fix5b_instance.sh start
#   bash p1fix5b_instance.sh stop | status
#
# REPO 指向**独立的 git worktree**（detached 在 P1-FIX-5A 的提交 + 本卡改动）：
# 共享工作区 /opt/cloudpivot 同时有其它卡在跑（transfer_handler 拆分中，树会短暂不可编译），
# 验收必须钉在一棵「只有本卡改动」的树上，否则跑出来的二进制是别人的半成品。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT=${REPO:-/root/lnk5b-wt}
BIN=${BIN:-/root/p1fix4-target/debug/cloudpivot}
DB=${LNK_DB:-cloudpivot_lnk5b}
PORT=${LNK_PORT:-18130}
LOG=${LNK_LOG:-/root/p1fix5b-server.log}
PIDFILE=${LNK_PIDFILE:-/root/p1fix5b-server.pid}

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
  # `config.toml` 里 [middleware.auth] enabled = false（默认关）；但权限校验层因为
  # `#[permission]` 宏注解会被**强制挂载**，auth 不跑 ⇒ 每个受保护接口都 403
  # （日志：`jwt_auth=false` + 「AuthClaims 未注入」）。真机验收必须把 auth 打开。
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
      # 空库首启的授权窗口：V0253 的 sys_role_menu 授权段在 sys_role 建出来之前跑（空操作），
      # 这里在 bootstrap 之后、**任何登录之前**幂等重放一次 —— 这样主体权限缓存里不会留旧值。
      grant_linkage
      return 0
    fi
    kill -0 "$(cat "$PIDFILE")" 2>/dev/null || { echo "进程已退出，看 $LOG"; return 1; }
    sleep 1
  done
  echo "健康检查超时，看 $LOG"
  return 1
}

# 幂等补齐 linkage 的 2 个权限码给 SUPER_ADMIN（3 个 menu 节点）
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
