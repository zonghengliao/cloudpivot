#!/bin/bash
# ============================================================================
# prod_selfcheck.sh — 生产自查（部署后 ⑦ 步与日常巡检共用这一个）
#
# 口径（卡 t_d4e92b99 收口）：
#   · 登录用 **运维账号** `CLOUDPIVOT_OPS_USER` / `CLOUDPIVOT_OPS_PASSWORD`，
#     不再拿 `NIK_ADMIN_PASSWORD` 猜 admin 口令（那是「空库首启种子」语义，必然 401）；
#   · 登录**只尝试一次**：失败立刻停并给出处置，绝不重试 —— 5 次失败会把账号锁 30 分钟
#     （backend/src/shared/constants.rs MAX_LOGIN_FAIL_COUNT=5 / LOCK_MINUTES=30），
#     且 60 秒内超过 10 次会命中登录限流（LOGIN_RATE_MAX_ATTEMPTS=10 / WINDOW=60s）。
#
# 用法：
#   bash /root/prod_selfcheck.sh                # 144 本机，读 /etc/cloudpivot/cloudpivot.env
#   OPS_ENV_FILE=... OPS_BASE_URL=... bash ...  # 换环境/端口
# 退出码：0 = 登录+health 全过（业务接口告警不致命）；1 = 有硬失败；2 = 缺凭据/环境
# ============================================================================
set -uo pipefail

SELF_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ENV_FILE=${OPS_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}
BASE=${OPS_BASE_URL:-http://127.0.0.1:8023}

if [ -f "$SELF_DIR/ops_cred.sh" ]; then
  # shellcheck disable=SC1091
  . "$SELF_DIR/ops_cred.sh"
elif [ -f /root/ops_cred.sh ]; then
  # shellcheck disable=SC1091
  . /root/ops_cred.sh
else
  echo "✗ 找不到 ops_cred.sh（应与本脚本同目录，或 /root/ops_cred.sh）" >&2
  exit 2
fi

# env 文件里的凭据/库连接都读进来（**进程环境优先**：先暂存外部传入的凭据，
# 读完文件再盖回去 —— 否则 env 文件里的值会把命令行/调用方的覆盖吞掉）
_ENV_OPS_USER=${CLOUDPIVOT_OPS_USER:-}
_ENV_OPS_PASS=${CLOUDPIVOT_OPS_PASSWORD:-}
if [ -r "$ENV_FILE" ]; then
  set -a
  # shellcheck disable=SC1090
  . "$ENV_FILE"
  set +a
fi
[ -n "$_ENV_OPS_USER" ] && CLOUDPIVOT_OPS_USER=$_ENV_OPS_USER
[ -n "$_ENV_OPS_PASS" ] && CLOUDPIVOT_OPS_PASSWORD=$_ENV_OPS_PASS

ops_cred_load "$ENV_FILE" || exit 2

FAIL=0; WARN=0
ok()   { echo "  ✅ $*"; }
bad()  { echo "  ✗ $*"; FAIL=$((FAIL + 1)); }
warn() { echo "  ⚠ $*"; WARN=$((WARN + 1)); }

echo "════ 生产自查 $(date '+%F %T') ════"
echo "== 0. 口径"
echo "  BASE=$BASE"
echo "  凭据: $(ops_cred_describe)   env=$ENV_FILE"
echo "  登录策略: 失败 5 次锁 30 分钟；限流 10 次 / 60 秒（本脚本单次尝试，失败即停）"

echo "== 1. health"
HCODE=$(curl -s -o /dev/null -w '%{http_code}' -m 10 "$BASE/health" 2>/dev/null)
[ "$HCODE" = "200" ] && ok "health 200" || bad "health http=$HCODE"

echo "== 2. 登录（单次尝试，失败不重试）"
RESP=$(curl -s -w '\n%{http_code}' -m 15 -X POST "$BASE/api/system/auth/login" \
  -H 'Content-Type: application/json' \
  -d "$(jq -nc --arg u "$OPS_CRED_USER" --arg p "$OPS_CRED_PASS" '{username:$u,password:$p}')" 2>/dev/null)
CODE=${RESP##*$'\n'}; BODY=${RESP%$'\n'*}
TOKEN=$(printf '%s' "$BODY" | jq -r '.data.access_token // empty' 2>/dev/null)
case "$CODE" in
  200)
    if [ -n "$TOKEN" ]; then
      ok "登录 200（账号 $OPS_CRED_USER，token ${#TOKEN} 字符）"
    else
      bad "登录 http=200 但没有 access_token：$(printf '%s' "$BODY" | head -c 200)"
    fi ;;
  401)
    bad "登录 401「用户名或密码错误」→ env 里的 CLOUDPIVOT_OPS_PASSWORD 不是「$OPS_CRED_USER」现用口令。
       处置：核对 /etc/cloudpivot/cloudpivot.env 的 CLOUDPIVOT_OPS_*（改对再跑）；
       **不要重试**——再错 4 次就把该账号锁 30 分钟。规程见 /etc/cloudpivot/README。" ;;
  403)
    bad "登录 403：$(printf '%s' "$BODY" | jq -r '.msg // empty' 2>/dev/null)（账号被锁定/禁用，等 30 分钟或人工解锁）" ;;
  429)
    bad "登录 429：命中登录限流（10 次 / 60 秒），等 60 秒再跑，别连环重跑" ;;
  *)
    bad "登录异常 http=$CODE body=$(printf '%s' "$BODY" | head -c 200)" ;;
esac

echo "== 3. 业务接口（以登录账号的角色可见性为准）"
if [ -n "$TOKEN" ]; then
  for EP in "/api/business/task/page?page=1&page_size=2" \
            "/api/business/erp/sales-order/page?page=1&page_size=2" \
            "/api/business/erp/voucher/page?page=1&page_size=2"; do
    CC=$(curl -s -o /dev/null -w '%{http_code}' -m 10 -H "Authorization: Bearer $TOKEN" "$BASE$EP" 2>/dev/null)
    if [ "$CC" = "200" ]; then ok "prod $CC  $EP"; else warn "prod $CC  $EP"; fi
  done

  echo "== 4. 菜单可见性（按 $OPS_CRED_USER 的角色过滤，仅供参考）"
  curl -s -m 10 -H "Authorization: Bearer $TOKEN" "$BASE/api/system/auth/menus" > /tmp/selfcheck-menus.json 2>/dev/null
  python3 - <<'PY' 2>/dev/null || warn "菜单解析失败"
import json
try:
    d = json.load(open('/tmp/selfcheck-menus.json'))
except Exception as ex:
    print('  ⚠ 菜单 JSON 解析失败:', ex); raise SystemExit(0)
paths = {}
def walk(ns, parent=None):
    for n in ns or []:
        paths[n.get('path') or ''] = parent
        walk(n.get('children'), n.get('name') or n.get('title'))
walk((d.get('data') or []))
print(f"  菜单节点: {len(paths)}")
for k, exp in [('/business/erp/seq', '基础数据'), ('/business/erp/inventory-location', '仓库管理'),
               ('/system/data-scope', 'system')]:
    got = paths.get(k, '(不在)')
    print(('  ✅ ' if got == exp else '  · ') + k + ' 父=' + str(got))
PY
else
  warn "无 token，跳过业务接口与菜单检查"
fi

echo "== 5. 库侧锁定状态（admin / 运维账号）"
if [ -n "${NIK_DATABASE_USER:-}" ] && [ -n "${NIK_DATABASE_PASSWORD:-}" ] && command -v psql >/dev/null 2>&1; then
  LOCKROWS=$(PGPASSWORD="$NIK_DATABASE_PASSWORD" psql -h "${NIK_DATABASE_HOST:-127.0.0.1}" \
    -p "${NIK_DATABASE_PORT:-5432}" -U "$NIK_DATABASE_USER" -d "${NIK_DATABASE_NAME:-cloudpivot}" \
    -At -F'|' -c "select username,is_locked,login_fail_count from sys_user where deleted_at is null and username in ('admin','$OPS_CRED_USER') order by username" 2>/dev/null)
  if [ -z "$LOCKROWS" ]; then
    warn "读不到 sys_user 锁定状态（psql 失败或无权限）"
  else
    while IFS='|' read -r u locked fails; do
      [ -z "$u" ] && continue
      if [ "$locked" = "t" ]; then
        bad "$u 已锁定（is_locked=t, login_fail_count=$fails）—— 等 30 分钟或人工解锁"
      else
        ok "$u is_locked=false / login_fail_count=$fails"
      fi
    done <<< "$LOCKROWS"
  fi
else
  warn "无 NIK_DATABASE_* 或 psql，跳过库侧锁定状态"
fi

echo "== 汇总"
if [ "$FAIL" -eq 0 ]; then
  echo "  自查通过（硬失败 0 / 告警 $WARN）"
  exit 0
fi
echo "  自查失败：硬失败 $FAIL / 告警 $WARN"
exit 1
