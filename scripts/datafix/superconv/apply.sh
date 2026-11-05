#!/usr/bin/env bash
# ============================================================================
# SUPERCONV（P0-FIX-2B-C）· 应用 / 回退 / 现状
# ----------------------------------------------------------------------------
# 授权：卡面评论 `laoliu` 2026-11-05 16:36「确认，同意」+ 16:45「henry.liao 是我建的
#       自己的测试号。可以调整」。
# 批准的执行方案（取代卡面「OPS_ADMIN 单角色」版）：
#   laoliu      : 摘 SUPER_ADMIN + 补 5 个业务角色
#   laoliu_test : 摘 SUPER_ADMIN + GENERAL_MANAGER（保留 3 个 3 档角色）
#   henry.liao  : 落位 HQ + 摘 SUPER_ADMIN（保留 4 个业务角色）
#   admin       : 不动（唯一真超管）
#
# 阶段（顺序不可颠倒；摘权永远在最后）：
#   05_land_henry_hq      henry.liao 的 dept_id → HQ（N2 巡检归零的前提）
#   15_grant_menu_dirs    5 个业务角色补祖先目录（否则摘权后侧边栏是空的）
#   10_grant_laoliu_roles laoliu 绑 5 个业务角色（此时他还是超管，没有「两边都空」窗口）
#   20_revoke_super       摘 4 行超管绑定 + 推进 3 个账号 token_version
#
# 用法：
#   bash apply.sh --check              # 只看现状，不写库
#   bash apply.sh --grant              # 05 → 15 → 10（不摘权）
#   bash apply.sh --revoke             # 20（摘权）
#   bash apply.sh --rollback-revoke    # 回退 20（4 行绑定加回去 + 还原 token_version）
#   bash apply.sh --rollback           # 20.down → 10.down → 15.down → 05.down
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
       || '（' || count(*) || ' 个）'
  FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
  JOIN sys_role r ON r.id = ur.role_id WHERE r.role_code = 'SUPER_ADMIN';
SELECT '  ' || rpad(u.username, 12) || ' → dept=' || rpad(coalesce(d.dept_name, 'NULL'), 8)
       || ' status=' || u.status || ' roles=' ||
       coalesce(string_agg(r.role_code, ',' ORDER BY r.role_code), '(无角色)')
  FROM sys_user u
  LEFT JOIN sys_dept d ON d.id = u.dept_id
  LEFT JOIN sys_user_role ur ON ur.user_id = u.id
  LEFT JOIN sys_role r ON r.id = ur.role_id
 WHERE u.username IN ('admin','laoliu','laoliu_test','henry.liao')
 GROUP BY u.username, d.dept_name, u.status ORDER BY u.username;
SELECT '  sys_user_role 行数 = ' || count(*) FROM sys_user_role;
SELECT '  业务角色缺祖先目录（处） = ' || count(*) FROM (
  WITH RECURSIVE tr AS (
    SELECT id FROM sys_role
     WHERE role_code IN ('DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER')
       AND deleted_at IS NULL
  ), seed AS (
    SELECT rm.role_id, m.id AS menu_id, m.parent_id
      FROM sys_role_menu rm JOIN sys_menu m ON m.id = rm.menu_id
     WHERE rm.role_id IN (SELECT id FROM tr) AND m.deleted_at IS NULL AND m.menu_type = 2
  ), up AS (
    SELECT s.role_id, s.parent_id AS node_id FROM seed s WHERE s.parent_id IS NOT NULL
    UNION
    SELECT u.role_id, m.parent_id FROM up u JOIN sys_menu m ON m.id = u.node_id
     WHERE m.parent_id IS NOT NULL AND m.deleted_at IS NULL
  )
  SELECT u.role_id, u.node_id FROM up u
    JOIN sys_menu d ON d.id = u.node_id AND d.deleted_at IS NULL AND d.status = 1
   WHERE d.menu_type = 1
     AND NOT EXISTS (SELECT 1 FROM sys_role_menu x WHERE x.role_id = u.role_id AND x.menu_id = u.node_id)
) o;
SELECT '  biz_flow_task PENDING = ' || count(*) FROM biz_flow_task WHERE status = 'PENDING';
SQL
    local has_snap
    has_snap=$("${PSQL[@]}" -X -q -At -c "SELECT coalesce(to_regclass('public.datafix_superconv_snapshot')::text,'')")
    if [ -n "$has_snap" ]; then
        "${PSQL[@]}" -X -q -At -c "SELECT '  快照行数：' || string_agg(batch || '=' || n, ', ' ORDER BY batch) FROM (
            SELECT batch, count(*) AS n FROM datafix_superconv_snapshot GROUP BY batch) t"
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
    --check|--grant|--revoke|--probe|--rollback-revoke|--rollback) ;;
    "") echo "用法：apply.sh --check|--grant|--revoke|--probe|--rollback-revoke|--rollback" >&2; exit 2 ;;
    *) echo "未知参数：$mode" >&2; exit 2 ;;
esac

echo "== 目标库：$DB_NAME @ $DB_HOST:$DB_PORT（$mode）=="
snapshot

case "$mode" in
    --check)
        exit 0 ;;
    --probe)
        echo "==> HTTP 验收探针（需要 SUPERCONV_BASE / 目标账号 / JWT 密钥，见 probe_conv.py --help）"
        shift || true
        exec python3 "$HERE/probe_conv.py" "$@" ;;
    --grant)
        require_backup
        run 05_land_henry_hq.up.sql
        run 15_grant_menu_dirs.up.sql
        run 10_grant_laoliu_roles.up.sql ;;
    --revoke)
        require_backup
        run 20_revoke_super.up.sql ;;
    --rollback-revoke)
        run 20_revoke_super.down.sql ;;
    --rollback)
        # 顺序：先把超管加回（20.down），再摘掉业务角色（10.down），再拆目录（15.down），最后撤落位（05.down）
        run 20_revoke_super.down.sql
        run 10_grant_laoliu_roles.down.sql
        run 15_grant_menu_dirs.down.sql
        run 05_land_henry_hq.down.sql ;;
esac

snapshot
echo "== 完成（$mode）==（别忘了：INCR perm:user_scope_epoch 与 DEL perm:user_perms:* —— 见 README §缓存失效）"
