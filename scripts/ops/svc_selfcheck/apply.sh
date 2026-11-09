#!/bin/bash
# ============================================================================
# apply.sh —— 建/更新「最小权限自查账号」svc_selfcheck（HUMAN）+ 只读角色 OPS_SELFCHECK
#
# 卡：t_882d48c8 —— 替掉借用的真人账号 henry.liao（真人一改密，自查立刻 401；auth_logs 也分不清
# 机器与人的操作）。本脚本把「建账号 + 建只读角色 + 授权 + 改 env」做成一条可复跑的命令。
#
# 它做的事（顺序固定，任何一步失败就停，不留中间态）：
#   ① 解析库连接（只从 env 文件里取 NIK_DATABASE_*，不 source 整个文件）
#   ② 凭据：复用 /root/svc_selfcheck.cred；没有就生成（secrets.token_urlsafe(24) + argon2id PHC）
#   ③ 备份 sys_user/sys_role/sys_role_menu/sys_user_role（00_backup.sh）
#   ④ 跑 10_svc_selfcheck.up.sql（单事务 + 硬断言：只读码白名单、只能挂 1 个角色、三条必需码齐全）
#   ⑤ 打印四张表的 Δ 行数（复跑应为 0）
#   ⑥ 写 env：CLOUDPIVOT_OPS_USER=svc_selfcheck / CLOUDPIVOT_OPS_PASSWORD=<新口令>
#      （先备份、600、原子替换；口令不进日志、不进 argv）
#
# 用法：
#   bash apply.sh                                   # 生产：读 /etc/cloudpivot/cloudpivot.env
#   bash apply.sh --dry-run                          # 只体检，不改库/不改 env
#   bash apply.sh --env-file /tmp/x.env --no-env     # 隔离库演练：不碰 env
#   bash apply.sh --rotate                           # 改密（重新生成口令并写库+env）
#   bash apply.sh --down                             # 回滚（账号/角色软删，关系表硬删）
#   bash apply.sh --verify-url http://127.0.0.1:8023 # 落库+改 env 后立刻跑一遍验收
# 退出码：0 成功；1 失败；2 用法/环境问题；3 SQL 断言失败（整笔回滚）
# ============================================================================
set -uo pipefail

SELF_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ENV_FILE=/etc/cloudpivot/cloudpivot.env
CRED_FILE=/root/svc_selfcheck.cred
BACKUP_DIR=""
ROTATE=0
NO_ENV=0
DRY=0
DOWN=0
VERIFY_URL=""

say()  { printf '%s\n' "$*"; }
ok()   { printf '  ✅ %s\n' "$*"; }
bad()  { printf '  ✗ %s\n' "$*"; }
warn() { printf '  ⚠ %s\n' "$*"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --env-file)    ENV_FILE=$2; shift 2 ;;
    --cred-file)   CRED_FILE=$2; shift 2 ;;
    --backup-dir)  BACKUP_DIR=$2; shift 2 ;;
    --verify-url)  VERIFY_URL=$2; shift 2 ;;
    --rotate)      ROTATE=1; shift ;;
    --no-env)      NO_ENV=1; shift ;;
    --dry-run)     DRY=1; shift ;;
    --down)        DOWN=1; shift ;;
    -h|--help)     sed -n '2,30p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) say "✗ 未知参数：$1"; exit 2 ;;
  esac
done

OPS_USER=svc_selfcheck

# ---- ① 库连接（只取需要的键，不 source 整个 env） ---------------------------
_env_get() {
  local key=$1 line
  [ -r "$ENV_FILE" ] || return 0
  line=$(grep -m1 "^[[:space:]]*${key}=" "$ENV_FILE" 2>/dev/null) || return 0
  line=${line#*=}
  line=$(printf '%s' "$line" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's/^"//' -e 's/"$//' -e "s/^'//" -e "s/'$//")
  printf '%s' "$line"
}

DB_HOST=${NIK_DATABASE_HOST:-$(_env_get NIK_DATABASE_HOST)}
DB_PORT=${NIK_DATABASE_PORT:-$(_env_get NIK_DATABASE_PORT)}
DB_USER=${NIK_DATABASE_USER:-$(_env_get NIK_DATABASE_USER)}
DB_NAME=${NIK_DATABASE_NAME:-$(_env_get NIK_DATABASE_NAME)}
DB_PASS=${NIK_DATABASE_PASSWORD:-$(_env_get NIK_DATABASE_PASSWORD)}
DB_HOST=${DB_HOST:-127.0.0.1}; DB_PORT=${DB_PORT:-5432}; DB_NAME=${DB_NAME:-cloudpivot}

if [ -z "${DB_USER:-}" ] || [ -z "${DB_PASS:-}" ]; then
  bad "拿不到库连接：$ENV_FILE 里缺 NIK_DATABASE_USER / NIK_DATABASE_PASSWORD（或用 NIK_DATABASE_* 环境变量注入）"
  exit 2
fi
command -v psql >/dev/null 2>&1 || { bad "没有 psql"; exit 2; }

export PGPASSWORD="$DB_PASS"
PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -v ON_ERROR_STOP=1 -q)

say "════ svc_selfcheck $( [ "$DOWN" = 1 ] && echo 回滚 || echo 落地 ) $(date '+%F %T') ════"
say "  目标库: $DB_USER@$DB_HOST:$DB_PORT/$DB_NAME"
say "  env: $ENV_FILE$([ "$NO_ENV" = 1 ] && echo '（--no-env：不改 env）')"
say "  凭据文件: $CRED_FILE"
[ "$DRY" = 1 ] && say "  ** --dry-run：只体检，不改库/不改 env **"

_q() { "${PSQL[@]}" -At -F'|' -c "$1" 2>&1; }

# ---- ② 当前状态体检 ---------------------------------------------------------
say "== 1. 库里现状"
_q "SELECT 'user|'||username||'|type='||user_type||'|status='||status||'|must_change='||must_change_password||'|locked='||is_locked||'|fails='||login_fail_count||'|dept='||coalesce(dept_id::text,'-')
     FROM sys_user WHERE username='$OPS_USER' AND deleted_at IS NULL"
_q "SELECT 'role|'||role_code||'|scope='||data_scope||'|status='||status||'|menus='||(SELECT count(*) FROM sys_role_menu rm WHERE rm.role_id=r.id)
     FROM sys_role r WHERE role_code='OPS_SELFCHECK' AND deleted_at IS NULL"
_q "SELECT 'admin_dept='||coalesce(dept_id::text,'(null)') FROM sys_user WHERE username='admin' AND deleted_at IS NULL"

EXISTING_PWD=$(_q "SELECT password FROM sys_user WHERE username='$OPS_USER' AND deleted_at IS NULL" | head -1)
if [ -n "$EXISTING_PWD" ] && [ "$ROTATE" = 0 ] && [ "$DOWN" = 0 ]; then
  say "  · 账号已存在：本次**不改口令**（要改密加 --rotate；env 里的口令必须与库内一致）"
fi

if [ "$DOWN" = 1 ]; then
  if [ "$DRY" = 1 ]; then say "== dry-run：将跑 10_svc_selfcheck.down.sql（账号/角色软删）"; exit 0; fi
  "${PSQL[@]}" -f "$SELF_DIR/10_svc_selfcheck.down.sql"
  RC=$?
  if [ "$RC" -ne 0 ]; then bad "回滚失败 rc=$RC"; exit 3; fi
  ok "已回滚（sys_user/sys_role 软删，关系表硬删）"
  say "  ⚠ env 里的 CLOUDPIVOT_OPS_USER 未改：请手动指回原账号，否则自查会 401"
  exit 0
fi

if [ "$DRY" = 1 ]; then
  say "== dry-run：将做 备份 → up.sql（建账号+角色+授权）→ 写 env（CLOUDPIVOT_OPS_USER=$OPS_USER）"
  [ -n "$VERIFY_URL" ] && say "  然后验收 $VERIFY_URL"
  exit 0
fi

# ---- ③ 凭据 ---------------------------------------------------------------
say "== 2. 凭据"
pick_python() {
  local cand
  for cand in "${SVCC_PYTHON:-}" /root/.venvs/svcchk/bin/python python3; do
    [ -n "$cand" ] || continue
    [ -x "$cand" ] || command -v "$cand" >/dev/null 2>&1 || continue
    if "$cand" -E -c 'import argon2' >/dev/null 2>&1; then echo "$cand"; return 0; fi
  done
  return 1
}

if [ -f "$CRED_FILE" ] && [ "$ROTATE" = 0 ]; then
  PERM=$(stat -c %a "$CRED_FILE")
  if [ "$PERM" != "600" ]; then bad "凭据文件权限是 $PERM（要求 600）"; exit 2; fi
  # shellcheck disable=SC1090
  . "$CRED_FILE"
  ok "沿用已有凭据文件（账号 ${SVCC_USER:-$OPS_USER}，口令长度 ${#SVCC_PASSWORD}）"
else
  PY=$(pick_python) || {
    bad "找不到带 argon2-cffi 的 python：先跑 bash $SELF_DIR/setup_hash_env.sh（或在 145 上生成凭据文件再拷过来）"
    exit 2
  }
  say "  用 $PY 生成新凭据（--rotate=$ROTATE）"
  GEN_ARGS=(--out "$CRED_FILE" --user "$OPS_USER")
  [ "$ROTATE" = 1 ] && GEN_ARGS+=(--rotate)
  "$PY" "$SELF_DIR/gen_credential.py" "${GEN_ARGS[@]}" || { bad "生成凭据失败"; exit 2; }
  # shellcheck disable=SC1090
  . "$CRED_FILE"
fi
[ -n "${SVCC_PWHASH:-}" ] || { bad "凭据文件里没有 SVCC_PWHASH"; exit 2; }
[ "${SVCC_USER:-$OPS_USER}" = "$OPS_USER" ] || { bad "凭据文件里的账号是 ${SVCC_USER}，期望 $OPS_USER"; exit 2; }
ok "账号=$OPS_USER 口令长度=${#SVCC_PASSWORD} PHC=${SVCC_PWHASH%%\$v=19*}"

# ---- ④ 备份 ---------------------------------------------------------------
say "== 3. 备份四张鉴权表"
BK_ARGS=(--env-file "$ENV_FILE")
[ -n "$BACKUP_DIR" ] && BK_ARGS+=(--out "$BACKUP_DIR")
ENV_FILE="$ENV_FILE" bash "$SELF_DIR/00_backup.sh" "${BK_ARGS[@]}" || { bad "备份失败，停手（不写库）"; exit 1; }

# ---- ⑤ 落库 ---------------------------------------------------------------
say "== 4. 落库（单事务 + 硬断言）"
BEFORE=$(_q "SELECT (SELECT count(*) FROM sys_user)||'/'||(SELECT count(*) FROM sys_role)||'/'||(SELECT count(*) FROM sys_role_menu)||'/'||(SELECT count(*) FROM sys_user_role)")
say "  变更前 sys_user/sys_role/sys_role_menu/sys_user_role = $BEFORE"

ROT=0; [ "$ROTATE" = 1 ] && ROT=1
{
  printf "\\set svc_pwhash '%s'\n" "$SVCC_PWHASH"
  printf "\\set svc_rotate '%s'\n" "$ROT"
  cat "$SELF_DIR/10_svc_selfcheck.up.sql"
} | "${PSQL[@]}"
RC=$?
if [ "$RC" -ne 0 ]; then
  bad "SQL 断言失败（rc=$RC）：整笔已回滚，库内未留半套权限。看上面 RAISE EXCEPTION 的原因。"
  exit 3
fi
AFTER=$(_q "SELECT (SELECT count(*) FROM sys_user)||'/'||(SELECT count(*) FROM sys_role)||'/'||(SELECT count(*) FROM sys_role_menu)||'/'||(SELECT count(*) FROM sys_user_role)")
ok "变更后 = $AFTER（Δ = $BEFORE → $AFTER）"

# ---- ⑥ 写 env -------------------------------------------------------------
say "== 5. env"
if [ "$NO_ENV" = 1 ]; then
  warn "--no-env：跳过（隔离库演练）"
else
  [ -r "$ENV_FILE" ] || { bad "env 文件不可读：$ENV_FILE"; exit 2; }
  BK="$ENV_FILE.bak-$(date +%Y%m%d-%H%M%S)"
  install -m 600 "$ENV_FILE" "$BK" || { bad "env 备份失败"; exit 1; }
  ok "env 备份 $BK（600）"

  TMP=$(mktemp) || exit 1
  U="$OPS_USER" P="$SVCC_PASSWORD" awk '
    BEGIN { du = 0; dp = 0 }
    /^[[:space:]]*CLOUDPIVOT_OPS_USER=/     { print "CLOUDPIVOT_OPS_USER=" ENVIRON["U"]; du = 1; next }
    /^[[:space:]]*CLOUDPIVOT_OPS_PASSWORD=/ { print "CLOUDPIVOT_OPS_PASSWORD=" ENVIRON["P"]; dp = 1; next }
    { print }
    END {
      if (!du) print "CLOUDPIVOT_OPS_USER=" ENVIRON["U"]
      if (!dp) print "CLOUDPIVOT_OPS_PASSWORD=" ENVIRON["P"]
    }
  ' "$ENV_FILE" > "$TMP" || { rm -f "$TMP"; bad "改写 env 失败"; exit 1; }
  install -m 600 "$TMP" "$ENV_FILE" || { rm -f "$TMP"; bad "回写 env 失败"; exit 1; }
  rm -f "$TMP"
  chmod 600 "$ENV_FILE"

  # 自查：能读到、值对、权限对（不打印口令）
  VU=$(grep -m1 '^CLOUDPIVOT_OPS_USER=' "$ENV_FILE" | cut -d= -f2-)
  VP=$(grep -m1 '^CLOUDPIVOT_OPS_PASSWORD=' "$ENV_FILE" | cut -d= -f2-)
  if [ "$VU" = "$OPS_USER" ] && [ "$VP" = "$SVCC_PASSWORD" ]; then
    ok "CLOUDPIVOT_OPS_USER=$VU / CLOUDPIVOT_OPS_PASSWORD 长度=${#VP}（权限 $(stat -c %a "$ENV_FILE")）"
  else
    bad "env 回读不一致（user=$VU，口令长度 ${#VP}）—— 立刻用 $BK 还原"
    exit 1
  fi
fi

say "════ 完成 ════"
say "  下一步验收："
say "    bash $SELF_DIR/verify_svc_selfcheck.sh --cred-file $CRED_FILE --env-file $ENV_FILE --base-url ${VERIFY_URL:-http://127.0.0.1:8023}"
say "    bash /root/prod_selfcheck.sh"
say "  回滚：bash $SELF_DIR/apply.sh --env-file $ENV_FILE --down"

if [ -n "$VERIFY_URL" ]; then
  say "== 6. 立刻验收（$VERIFY_URL）"
  bash "$SELF_DIR/verify_svc_selfcheck.sh" --cred-file "$CRED_FILE" --env-file "$ENV_FILE" --base-url "$VERIFY_URL"
  exit $?
fi
exit 0
