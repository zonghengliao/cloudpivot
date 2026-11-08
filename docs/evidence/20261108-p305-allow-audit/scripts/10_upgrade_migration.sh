#!/usr/bin/env bash
# ============================================================================
# P3-05 验收 ①：迁移 V0338（自审金额上限参数 + 自审监控菜单）
#
#   ① 空库首启：全部迁移一次装起（含 V0338）→ 参数行 + 菜单行 + 授权行都在、兄弟 sort 不撞；
#   ② 重启：「应用 0 个迁移」（幂等 + checksum 通过）；
#   ③ down 可回滚（参数 / 菜单 / 授权全没了）→ 删登记再启动 = 「应用 1 个」→ 一键复原。
#
# 环境文件 `/root/p305/migrate.env`（600，含三把密钥 + admin 口令，**不落仓库**）。
# 端口 18306 / 库 cloudpivot_p305_mig（与 e2e 的 18305 / cloudpivot_p305_ut 分开）。
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

DB=${DB:-cloudpivot_p305_mig}
PORT=${PORT:-18306}
ENVF=${ENVF:-/root/p305/migrate.env}
BIN=${BIN:-/root/p305/bin/cloudpivot}
RUN_DIR=/opt/cloudpivot/backend
LOG=/root/p305/migrate_instance.log
PIDF=/root/p305/migrate.pid
OUT="$RAW/10_upgrade.txt"
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1

say "================ P3-05 ① 迁移 V0338（$DB / :$PORT）================"

stop_instance() {
  local pid
  pid=$(cat "$PIDF" 2>/dev/null) || return 0
  kill "$pid" 2>/dev/null || return 0
  for _ in $(seq 1 30); do
    kill -0 "$pid" 2>/dev/null || return 0
    sleep 1
  done
  kill -9 "$pid" 2>/dev/null || true
}

start_instance() {
  local i pid
  ( cd "$RUN_DIR" && setsid env $(grep -v '^#' "$ENVF" | grep -v '^$' | tr '\n' ' ') \
      NIK_DATABASE_NAME="$DB" NIK_SERVER_LISTEN="127.0.0.1:$PORT" \
      "$BIN" >"$LOG" 2>&1 </dev/null & )
  for i in $(seq 1 120); do
    pid=$(ss -ltnp 2>/dev/null | sed -n "s/.*127\.0\.0\.1:$PORT .*pid=\([0-9]*\).*/\1/p" | head -1)
    if [ -n "$pid" ]; then echo "$pid" >"$PIDF"; return 0; fi
    sleep 1
  done
  return 1
}

wait_http() {
  local i=0
  while [ "$i" -lt 180 ]; do
    if curl -s -o /dev/null "http://127.0.0.1:$PORT/health"; then return 0; fi
    sleep 1
    i=$((i + 1))
  done
  return 1
}

stop_instance
su postgres -c "dropdb --if-exists --force $DB" >/dev/null 2>&1
su postgres -c "createdb -O alun $DB" || { bad "建空库失败"; summary; }

say ""
say "--- 1) 空库首启：全部迁移一次装起 ---"
start_instance || { bad "实例没起来"; tail -20 "$LOG"; summary; }
wait_http || { bad "实例 180s 内没起来"; tail -40 "$LOG"; summary; }
APP=$(grep -oE "应用 [0-9]+ 个迁移" "$LOG" | tail -1)
say "首启迁移日志：${APP:-<未找到>}"
[ -n "$APP" ] && ok "首启确实跑了迁移（$APP）" || bad "首启没打印迁移行数"

eq "V0338 已登记" \
  "$(q "$DB" "select count(*) from schema_migrations where version='338'")" "1"
eq "迁移最大号 = 338" "$(q "$DB" "select max(version::int) from schema_migrations")" "338"

say ""
say "  --- 参数行（sys_config）---"
q "$DB" "select config_key || ' | ' || config_value || ' | ' || config_type || ' | ' || category from sys_config where config_key='erp.separation_of_duties.self_approval_max_amount'"
eq "参数存在且默认值 = 0（不限额）" \
  "$(q "$DB" "select config_value from sys_config where config_key='erp.separation_of_duties.self_approval_max_amount' and deleted_at is null")" "0"
eq "参数类型 = NUMBER（配置页渲染数字输入框）" \
  "$(q "$DB" "select config_type from sys_config where config_key='erp.separation_of_duties.self_approval_max_amount'")" "NUMBER"
eq "参数可修改 + 启用" \
  "$(q "$DB" "select is_modifiable::text || '/' || status::text from sys_config where config_key='erp.separation_of_duties.self_approval_max_amount'")" "true/1"

say ""
say "  --- 菜单行（sys_menu）---"
q "$DB" "select id || ' | ' || name || ' | ' || path || ' | ' || component || ' | sort=' || sort_order from sys_menu where path='/flow/stats'"
eq "页面菜单 path / component / 父目录 / 权限码与前端路由一致" \
  "$(q "$DB" "select path || '|' || component || '|' || parent_id::text || '|' || auth_code from sys_menu where path='/flow/stats'")" \
  "/flow/stats|business/flow/stats/index|20000000-0000-0000-0000-000000000860|business:flow:instance:read"
eq "审批流目录下 sort_order 无重复（MENU-FIX 不变量）" \
  "$(q "$DB" "select count(*) from (select sort_order from sys_menu where parent_id='20000000-0000-0000-0000-000000000860'::uuid and deleted_at is null and menu_type=2 group by sort_order having count(*)>1) d")" "0"
GRANTS=$(q "$DB" "select count(*) from sys_role_menu rm join sys_menu m on m.id=rm.menu_id where m.path='/flow/stats'")
say "菜单已授权给 $GRANTS 个角色（种子里的超管角色）"
[ "$GRANTS" -ge 1 ] && ok "页面菜单已授权（超管可见）" || bad "页面菜单未授权给任何角色"

say ""
say "--- 2) 重启：应用 0 个迁移（幂等）---"
stop_instance
: >"$LOG"
start_instance || { bad "重启失败"; tail -20 "$LOG"; summary; }
wait_http || { bad "重启后没起来"; tail -30 "$LOG"; summary; }
APP2=$(grep -oE "应用 [0-9]+ 个迁移" "$LOG" | tail -1)
say "重启迁移日志：${APP2:-<未找到>}"
eq "重启应用 0 个迁移" "${APP2:-<未找到>}" "应用 0 个迁移"
eq "重启后参数仍在" \
  "$(q "$DB" "select count(*) from sys_config where config_key='erp.separation_of_duties.self_approval_max_amount' and deleted_at is null")" "1"

say ""
say "--- 3) down 回滚 → 再 up 重放 ---"
stop_instance
psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -v ON_ERROR_STOP=1 \
  -f /opt/cloudpivot/backend/migrations/business/V0338__flow_self_approval_monitor.down.sql >/dev/null 2>&1 \
  && ok "down 脚本执行成功" || bad "down 脚本执行失败"
eq "down 后参数没了" \
  "$(q "$DB" "select count(*) from sys_config where config_key='erp.separation_of_duties.self_approval_max_amount'")" "0"
eq "down 后菜单没了" \
  "$(q "$DB" "select count(*) from sys_menu where path='/flow/stats'")" "0"
eq "down 后授权行没了" \
  "$(q "$DB" "select count(*) from sys_role_menu rm join sys_menu m on m.id=rm.menu_id where m.path='/flow/stats'")" "0"

# 删登记 → 重启会重新应用这一条（与「从 V0337 升级到 V0338」在迁移器眼里是同一条路径）
q "$DB" "delete from schema_migrations where version='338'" >/dev/null
: >"$LOG"
start_instance || { bad "重放启动失败"; tail -20 "$LOG"; summary; }
wait_http || { bad "重放后没起来"; tail -30 "$LOG"; summary; }
APP3=$(grep -oE "应用 [0-9]+ 个迁移" "$LOG" | tail -1)
say "重放迁移日志：${APP3:-<未找到>}"
eq "重放 = 应用 1 个迁移" "${APP3:-<未找到>}" "应用 1 个迁移"
eq "重放后参数回来了（默认 0）" \
  "$(q "$DB" "select config_value from sys_config where config_key='erp.separation_of_duties.self_approval_max_amount'")" "0"
eq "重放后菜单回来了" \
  "$(q "$DB" "select count(*) from sys_menu where path='/flow/stats'")" "1"

say ""
say "--- 4) 收尾：停实例（库与日志留档备查）---"
stop_instance && ok "实例已停"
summary
