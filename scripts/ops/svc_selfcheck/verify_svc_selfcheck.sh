#!/bin/bash
# ============================================================================
# verify_svc_selfcheck.sh —— 验收 svc_selfcheck（HTTP + 库侧双口径）
#
# 卡 t_882d48c8 的验收项逐条落到这里：
#   ① 能登录（单次尝试，失败即停，绝不重试 —— 5 次锁 30 分钟）
#   ② 三个自查列表接口 200（/business/task/page、/business/erp/sales-order/page、
#      /business/erp/voucher/page）
#   ③ /api/system/auth/menus 200 且带出只读页面节点
#   ④ 越权探测**必须 403**：建销售订单、建用户（空 body：有权限会变 400/422，
#      没权限是 403，所以「403」既证明最小权限、又不会真写库）
#   ⑤ 连续 N 次登录全 200（默认 6）
#   ⑥ 库侧：svc_selfcheck / admin 均 is_locked=false、login_fail_count=0；
#      账号只挂 1 个角色；角色里没有任何写/导出/审批码
#
# 用法：
#   bash verify_svc_selfcheck.sh [--base-url URL] [--cred-file PATH] [--env-file PATH]
#                                [--sessions 6] [--no-db] [--allow-locked]
# 退出码：0 = 全过；1 = 有硬失败；2 = 缺凭据/环境
# ============================================================================
set -uo pipefail

SELF_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BASE=http://127.0.0.1:8023
CRED_FILE=/root/svc_selfcheck.cred
ENV_FILE=/etc/cloudpivot/cloudpivot.env
SESSIONS=6
CHECK_DB=1

while [ $# -gt 0 ]; do
  case "$1" in
    --base-url)  BASE=$2; shift 2 ;;
    --cred-file) CRED_FILE=$2; shift 2 ;;
    --env-file)  ENV_FILE=$2; shift 2 ;;
    --sessions)  SESSIONS=$2; shift 2 ;;
    --no-db)     CHECK_DB=0; shift ;;
    -h|--help)   sed -n '2,24p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "✗ 未知参数：$1" >&2; exit 2 ;;
  esac
done

FAIL=0; WARN=0
ok()   { echo "  ✅ $*"; }
bad()  { echo "  ✗ $*"; FAIL=$((FAIL + 1)); }
warn() { echo "  ⚠ $*"; WARN=$((WARN + 1)); }

[ -r "$CRED_FILE" ] || { echo "✗ 读不到凭据文件：$CRED_FILE" >&2; exit 2; }
PERM=$(stat -c %a "$CRED_FILE")
[ "$PERM" = "600" ] || { echo "✗ 凭据文件权限 $PERM（要求 600）" >&2; exit 2; }
# shellcheck disable=SC1090
. "$CRED_FILE"
U=${SVCC_USER:?凭据文件里没有 SVCC_USER}
P=${SVCC_PASSWORD:?凭据文件里没有 SVCC_PASSWORD}

echo "════ svc_selfcheck 验收 $(date '+%F %T') ════"
echo "  BASE=$BASE  账号=$U  口令长度=${#P}"

# 单次登录；返回 body 到 $1，http code 到 stdout
_login() {
  local out=$1
  curl -s -o "$out" -w '%{http_code}' -m 15 -X POST "$BASE/api/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "$(python3 - "$U" "$P" <<'PY'
import json, sys
print(json.dumps({"username": sys.argv[1], "password": sys.argv[2]}))
PY
)"
}

_get() { curl -s -o /tmp/svcc_body.json -w '%{http_code}' -m 15 -H "Authorization: Bearer $1" "$BASE$2"; }
_send() { curl -s -o /tmp/svcc_body.json -w '%{http_code}' -m 15 -X "$2" -H "Authorization: Bearer $1" -H 'Content-Type: application/json' -d "${4:-{}}" "$BASE$3"; }

echo "== 1. 登录（单次尝试，失败不重试）"
CODE=$(_login /tmp/svcc_login.json)
TOKEN=""
if [ "$CODE" = "200" ]; then
  TOKEN=$(python3 - <<'PY'
import json
try:
    d = json.load(open('/tmp/svcc_login.json'))
    print((d.get('data') or {}).get('access_token') or '')
except Exception:
    print('')
PY
)
  if [ -n "$TOKEN" ]; then ok "登录 200（token ${#TOKEN} 字符）"; else bad "登录 200 但无 access_token"; fi
else
  bad "登录 http=$CODE：$(head -c 200 /tmp/svcc_login.json)。
     401 = env/凭据文件里的口令不是该账号现用口令（改对再跑，**别重试**）；403 = 锁定/禁用；429 = 限流等 60 秒。"
fi

echo "== 2. 自查必需的三个列表接口（必须 200）"
if [ -n "$TOKEN" ]; then
  for EP in "/api/business/task/page?page=1&page_size=2" \
            "/api/business/erp/sales-order/page?page=1&page_size=2" \
            "/api/business/erp/voucher/page?page=1&page_size=2"; do
    CC=$(_get "$TOKEN" "$EP")
    if [ "$CC" = "200" ]; then ok "200  $EP"; else bad "http=$CC  $EP（缺只读权限码？看库侧第 6 节）"; fi
  done

  echo "== 3. 菜单接口（/api/system/auth/menus）"
  CC=$(_get "$TOKEN" "/api/system/auth/menus")
  if [ "$CC" = "200" ]; then
    python3 - <<'PY'
import json, sys
d = json.load(open('/tmp/svcc_body.json'))
paths = set()
def walk(ns):
    for n in ns or []:
        paths.add(n.get('path') or '')
        walk(n.get('children'))
walk(d.get('data') or [])
print(f"  ✅ menus 200，节点 {len(paths)} 个")
need = ['/task/list', '/business/erp/sales-order', '/business/erp/voucher',
        '/business/erp/seq', '/business/erp/inventory-location']
missing = [p for p in need if p not in paths]
if missing:
    print("  ✗ 缺菜单节点：" + ", ".join(missing))
    sys.exit(1)
print("  ✅ 只读页面节点齐全：" + ", ".join(need))
PY
    [ $? -eq 0 ] || bad "菜单节点缺失（见上）"
  else
    bad "menus http=$CC"
  fi

  echo "== 4. 越权探测（空 body：有权限→400，无权限→403；这里必须是 403）"
  CC=$(_send "$TOKEN" POST "/api/business/erp/sales-order")
  [ "$CC" = "403" ] && ok "建销售订单 403（无 business:erp:sale-order:create）" || bad "建销售订单 http=$CC（要求 403 —— 权限给多了或路由变了）"
  CC=$(_send "$TOKEN" POST "/api/system/user")
  [ "$CC" = "403" ] && ok "建用户 403（无 system:user:create）" || bad "建用户 http=$CC（要求 403）"
  CC=$(_get "$TOKEN" "/api/system/user/page?page=1&page_size=1")
  [ "$CC" = "403" ] && ok "查用户列表 403（无 system:user:list）" || bad "查用户列表 http=$CC（要求 403）"
else
  warn "无 token，跳过接口检查"
fi

echo "== 5. 连续 $SESSIONS 次登录"
# 限流口径：同一「用户名 + IP」60 秒内第 11 次起 429（LOGIN_RATE_MAX_ATTEMPTS=10）。
# 本脚本自己就要 1+$SESSIONS 次登录，所以：紧跟在别的自查后面跑可能正好撞上限 ——
# 429 一律记「告警」并在提示里给「等 60 秒再跑」，绝不当成口令错误或账号锁定。
SUCC=0; FAILN=0; HIT429=0
for i in $(seq 1 "$SESSIONS"); do
  C=$(_login /tmp/svcc_login_$i.json)
  if [ "$C" = "200" ]; then SUCC=$((SUCC + 1)); printf '    第 %d 次: http=200\n' "$i"
  elif [ "$C" = "429" ]; then HIT429=1; printf '    第 %d 次: http=429 ← 命中登录限流（10 次/60 秒），立刻停\n' "$i"; break
  else FAILN=$((FAILN + 1)); printf '    第 %d 次: http=%s  ← 立刻停\n' "$i" "$C"; break; fi
done
if [ "$FAILN" -eq 0 ] && [ "$SUCC" -eq "$SESSIONS" ]; then ok "连续登录 $SUCC/$SESSIONS 全部 200"
elif [ "$HIT429" = 1 ]; then warn "连续登录只跑成 $SUCC/$SESSIONS（撞限流 429）：60 秒后单独复跑本脚本拿干净证据（不是口令问题，账号没锁）"
else bad "连续登录 $SUCC/$SESSIONS（失败 $FAILN 次）"; fi

echo "== 6. 库侧（角色/权限/锁定）"
if [ "$CHECK_DB" = 1 ]; then
  _env_get() {
    local key=$1 line
    [ -r "$ENV_FILE" ] || return 0
    line=$(grep -m1 "^[[:space:]]*${key}=" "$ENV_FILE" 2>/dev/null) || return 0
    line=${line#*=}
    line=$(printf '%s' "$line" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's/^"//' -e 's/"$//' -e "s/^'//" -e "s/'$//")
    printf '%s' "$line"
  }
  DB_HOST=${NIK_DATABASE_HOST:-$(_env_get NIK_DATABASE_HOST)}; DB_HOST=${DB_HOST:-127.0.0.1}
  DB_PORT=${NIK_DATABASE_PORT:-$(_env_get NIK_DATABASE_PORT)}; DB_PORT=${DB_PORT:-5432}
  DB_USER=${NIK_DATABASE_USER:-$(_env_get NIK_DATABASE_USER)}
  DB_NAME=${NIK_DATABASE_NAME:-$(_env_get NIK_DATABASE_NAME)}; DB_NAME=${DB_NAME:-cloudpivot}
  DB_PASS=${NIK_DATABASE_PASSWORD:-$(_env_get NIK_DATABASE_PASSWORD)}
  if [ -n "${DB_USER:-}" ] && [ -n "${DB_PASS:-}" ] && command -v psql >/dev/null 2>&1; then
    Q() { PGPASSWORD="$DB_PASS" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -At -F'|' -c "$1" 2>/dev/null; }
    while IFS='|' read -r u locked fails; do
      [ -z "${u:-}" ] && continue
      if [ "$locked" = "t" ]; then bad "$u 已锁定（login_fail_count=$fails）"; else ok "$u is_locked=false / login_fail_count=$fails"; fi
    done <<< "$(Q "select username,is_locked,login_fail_count from sys_user where deleted_at is null and username in ('admin','$U') order by username")"

    ROLES=$(Q "select coalesce(string_agg(r.role_code, ','), '(none)') from sys_user_role ur join sys_role r on r.id=ur.role_id where ur.user_id=(select id from sys_user where username='$U' and deleted_at is null) and r.deleted_at is null")
    if [ "$ROLES" = "OPS_SELFCHECK" ]; then ok "账号只挂 1 个角色：$ROLES"; else bad "账号角色 = ${ROLES:-（空）}（要求恰好 OPS_SELFCHECK）"; fi

    BAD=$(Q "select coalesce(string_agg(m.auth_code, ','), '') from sys_role_menu rm join sys_role r on r.id=rm.role_id join sys_menu m on m.id=rm.menu_id where r.role_code='OPS_SELFCHECK' and r.deleted_at is null and m.deleted_at is null and m.auth_code<>'' and m.auth_code !~ ':(list|view|read|page|detail)\$'")
    if [ -z "$BAD" ]; then ok "角色里没有写/导出/审批码"; else bad "角色里有非只读码：$BAD"; fi

    N=$(Q "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id where r.role_code='OPS_SELFCHECK' and r.deleted_at is null")
    [ "${N:-0}" -gt 0 ] && ok "授权节点 $N 个" || bad "授权节点 0 个"
  else
    warn "无库连接信息，跳过库侧检查"
  fi
else
  warn "--no-db：跳过库侧"
fi

echo "== 汇总"
if [ "$FAIL" -eq 0 ]; then echo "  ✅ 验收通过（硬失败 0 / 告警 $WARN）"; exit 0; fi
echo "  ✗ 验收失败：硬失败 $FAIL / 告警 $WARN"; exit 1
