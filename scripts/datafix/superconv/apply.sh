#!/usr/bin/env bash
# ============================================================================
# SUPERCONV（P0-FIX-2B-C）· 应用 / 回退 / 现状
# ----------------------------------------------------------------------------
# 用法（四个阶段分开跑，摘权永远在最后）：
#   bash apply.sh --check              # 只看现状，不写库
#   bash apply.sh --grant              # 阶段 1：建 OPS_ADMIN + 授 laoliu（不摘权）
#   bash apply.sh --probe              # 阶段 3 的一半：对外 HTTP 探针（见 probe_ops.py）
#   bash apply.sh --revoke             # 阶段 2：摘 4 行超管绑定 + 推 token_version
#   bash apply.sh --rollback-revoke    # 回退阶段 2（4 行绑定加回去）
#   bash apply.sh --rollback-grant     # 回退阶段 1（删 OPS_ADMIN）
#   bash apply.sh --rollback           # 两段一起退（先退阶段 2，再退阶段 1）
#
# 环境：
#   SUPERCONV_DB=<db>        目标库（默认 NIK_DATABASE_NAME）
#   FORCE=1                  跳过「必须存在本库备份」保护（演练库也建议先跑 00_backup.sh）
#   CLOUDPIVOT_ENV_FILE      默认 /etc/cloudpivot/cloudpivot.env
#
# 安全：
#   * --grant/--revoke 要求 /root/datafix-backup-superconv-*/full.dump 里有**本库**的备份
#     （校验 MANIFEST 里的 `database  : <db> on `，防止拿别的库的备份当挡箭牌）
#   * 所有前置/形状/行数断言都写在 SQL 的 DO 块里（不满足 → RAISE EXCEPTION → 整事务回滚）
#   * psql -1：每个脚本一个事务，失败即全回退
# ============================================================================
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

ENV_FILE="${CLOUDPIVOT_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
if [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    set -a; . "$ENV_FILE"; set +a
fi
DB_PASSWORD="${NIK_DATABASE_PASSWORD:-${ALUN_DATABASE_PASSWORD:-}}"
: "${DB_PASSWORD:?需要 NIK_DATABASE_PASSWORD（先 . /etc/cloudpivot/cloudpivot.env 或用 CLOUDPIVOT_ENV_FILE 指定）}"
export PGPASSWORD="$DB_PASSWORD"

DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"
DB_USER="${NIK_DATABASE_USER:-cloudpivot}"
DB_NAME="${SUPERCONV_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"

PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -v ON_ERROR_STOP=1)

snapshot() {
    echo "---- 现状（$(date '+%H:%M:%S')）----"
    "${PSQL[@]}" -X -q -At -F'|' <<'SQL'
SELECT '  SUPER_ADMIN 成员 = ' || coalesce(string_agg(u.username, ',' ORDER BY u.username), '(无)')
  FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
  JOIN sys_role r ON r.id = ur.role_id WHERE r.role_code = 'SUPER_ADMIN';
SELECT '  ' || rpad(u.username, 12) || ' → ' ||
       coalesce(string_agg(r.role_code, ',' ORDER BY r.role_code), '(无角色)')
  FROM sys_user u LEFT JOIN sys_user_role ur ON ur.user_id = u.id
  LEFT JOIN sys_role r ON r.id = ur.role_id
 WHERE u.username IN ('admin','laoliu','laoliu_test','henry.liao')
 GROUP BY u.username ORDER BY u.username;
SELECT '  sys_user_role 行数 = ' || count(*) FROM sys_user_role;
SELECT '  OPS_ADMIN 存在 = ' || coalesce((SELECT 'yes / 码=' || count(DISTINCT m.auth_code)
         FROM sys_role r LEFT JOIN sys_role_menu rm ON rm.role_id = r.id
         LEFT JOIN sys_menu m ON m.id = rm.menu_id AND m.auth_code <> ''
        WHERE r.role_code = 'OPS_ADMIN' AND r.deleted_at IS NULL), 'no');
SELECT '  biz_flow_task PENDING = ' || count(*) FROM biz_flow_task WHERE status = 'PENDING';
SQL
    local has_snap
    has_snap=$("${PSQL[@]}" -X -q -At -c "SELECT coalesce(to_regclass('public.datafix_superconv_snapshot')::text,'')")
    if [ -n "$has_snap" ]; then
        echo "  datafix_superconv_snapshot 行数 = $("${PSQL[@]}" -X -q -At -c \
            "SELECT count(*) FROM datafix_superconv_snapshot WHERE batch='SUPERCONV-20261105'")"
    else
        echo "  datafix_superconv_snapshot = (表还没建；本库还没跑过 apply)"
    fi
}

require_backup() {
    [ "${FORCE:-0}" = "1" ] && { echo "==> FORCE=1：跳过备份检查"; return 0; }
    local latest=""
    while IFS= read -r dir; do
        [ -s "$dir/full.dump" ] || continue
        if grep -q "database  : $DB_NAME on " "$dir/MANIFEST.txt" 2>/dev/null; then
            latest="$dir"; break
        fi
    done < <(ls -1dt /root/datafix-backup-superconv-* 2>/dev/null || true)
    if [ -z "$latest" ]; then
        echo "✗ 找不到本库（$DB_NAME）的备份 full.dump：先跑 00_backup.sh（或 FORCE=1 跳过）" >&2
        exit 1
    fi
    echo "==> 使用备份：$latest"
}

run() { echo "==> $1"; "${PSQL[@]}" -q -1 -f "$HERE/$1"; }

mode="${1:-}"
case "$mode" in
    --check|--grant|--probe|--revoke|--rollback-revoke|--rollback-grant|--rollback) ;;
    "") echo "用法：apply.sh --check|--grant|--probe|--revoke|--rollback-revoke|--rollback-grant|--rollback" >&2; exit 2 ;;
    *) echo "未知参数：$mode" >&2; exit 2 ;;
esac

echo "== 目标库：$DB_NAME @ $DB_HOST:$DB_PORT（$mode）=="
snapshot

case "$mode" in
    --check)
        exit 0 ;;
    --probe)
        echo "==> HTTP 探针（需要 SUPERCONV_BASE + 登录凭据或令牌，见 probe_ops.py --help）"
        exec python3 "$HERE/probe_ops.py" ;;
    --grant)
        require_backup
        run 10_grant_ops_admin.up.sql ;;
    --revoke)
        require_backup
        run 20_revoke_super.up.sql ;;
    --rollback-revoke)
        run 20_revoke_super.down.sql ;;
    --rollback-grant)
        run 10_grant_ops_admin.down.sql ;;
    --rollback)
        run 20_revoke_super.down.sql
        run 10_grant_ops_admin.down.sql ;;
esac

snapshot
echo "== 完成（$mode）==（别忘了：INCR perm:user_scope_epoch 与清 perm:user_perms:* 见 README §缓存失效）"
