#!/usr/bin/env bash
# ============================================================================
# BIZPERM（P0-FIX-2B-C 跟进）· 应用 / 现状 / 回退
# ----------------------------------------------------------------------------
# 干什么：把「业务角色缺的 43/59 个业务页权限码」按职能归位补上
#   （任务/项目/采购/供应商/合同/物料/仓储/库存/凭证/总账/报表 …）
# 计划：plan.sql（唯一的授予矩阵）；展开：expand.sql；施工：20_grant.up.sql
# 决策：docs/architecture/P0-FIX-2B-C-业务角色权限覆盖-决策.md
#
# 用法：
#   bash apply.sh --check       # 只读现状（不写库）
#   bash apply.sh --snapshot    # 05_snapshot.up.sql（必须先于 --grant）
#   bash apply.sh --grant       # 20_grant.up.sql（幂等，可重放）
#   bash apply.sh --rollback    # 20_grant.down.sql + 05_snapshot.down.sql（可逆）
#
# 环境：
#   BIZPERM_DB=<db>            目标库（默认 NIK_DATABASE_NAME）
#   FORCE=1                    跳过「必须存在本库备份」保护（演练库用）
#   CLOUDPIVOT_ENV_FILE        默认 /etc/cloudpivot/cloudpivot.env
#
# 安全：
#   * --grant 要求存在 /root/datafix-backup-bizperm-*/MANIFEST.txt 且写的是本库
#   * 每个 .sql 单独一个事务（psql -1），失败整事务回退
#   * 断言全在 SQL 的 DO 块里（不满足 → RAISE → 回滚）
#   * 改完授权**必须**跑 cache_invalidate.sh（推进 token_version 已在 SQL 内做，
#     redis 侧的 epoch/缓存由 shell 做）
# ============================================================================
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$HERE"

ENV_FILE="${CLOUDPIVOT_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
if [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    set -a; . "$ENV_FILE"; set +a
fi
DB_PASSWORD="${NIK_DATABASE_PASSWORD:-${ALUN_DATABASE_PASSWORD:-}}"
: "${DB_PASSWORD:?需要 NIK_DATABASE_PASSWORD（先 . /etc/cloudpivot/cloudpivot.env）}"
export PGPASSWORD="$DB_PASSWORD"

DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"
DB_USER="${NIK_DATABASE_USER:-cloudpivot}"
DB_NAME="${BIZPERM_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"

PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -v ON_ERROR_STOP=1)

snapshot() {
    echo "---- 现状（$(date '+%H:%M:%S')，库 $DB_NAME）----"
    "${PSQL[@]}" -X -q -P pager=off <<'SQL'
\pset border 1
\echo 'SUPER_ADMIN 成员（目标 = 1，只有 admin）'
SELECT u.username FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
  JOIN sys_role r ON r.id = ur.role_id WHERE r.role_code = 'SUPER_ADMIN' ORDER BY 1;
\echo '5 个业务角色：data_scope / 授权行数 / 可见业务页'
SELECT r.role_code, r.data_scope,
       count(*) FILTER (WHERE m.menu_type = 1) AS dirs,
       count(*) FILTER (WHERE m.menu_type = 2) AS pages,
       count(*) FILTER (WHERE m.menu_type = 3) AS buttons
  FROM sys_role r LEFT JOIN sys_role_menu rm ON rm.role_id = r.id
  LEFT JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
 WHERE r.role_code IN ('DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER')
 GROUP BY 1,2 ORDER BY 1;
\echo '账号 → 角色 / 部门'
SELECT u.username, d.dept_code, u.status, u.token_version,
       string_agg(r.role_code, ',' ORDER BY r.role_code) AS roles
  FROM sys_user u LEFT JOIN sys_dept d ON d.id = u.dept_id
  LEFT JOIN sys_user_role ur ON ur.user_id = u.id
  LEFT JOIN sys_role r ON r.id = ur.role_id
 WHERE u.username IN ('admin','laoliu','henry.liao','laoliu_test')
 GROUP BY 1,2,3,4 ORDER BY 1;
\echo '快照表（有则在）'
SELECT coalesce(to_regclass('public.datafix_bizperm_snapshot')::text, '(表未创建)') AS snapshot_table;
SQL
}

require_backup() {
    [ "${FORCE:-0}" = "1" ] && { echo "==> FORCE=1：跳过备份检查"; return 0; }
    local latest=""
    while IFS= read -r dir; do
        [ -s "$dir/MANIFEST.txt" ] || continue
        if grep -q "database  : $DB_NAME on " "$dir/MANIFEST.txt" 2>/dev/null; then latest="$dir"; break; fi
    done < <(ls -1dt /root/datafix-backup-bizperm-* 2>/dev/null || true)
    if [ -z "$latest" ]; then
        echo "✗ 找不到本库（$DB_NAME）的备份：先跑 00_backup.sh（或 FORCE=1 跳过）" >&2
        exit 1
    fi
    echo "==> 使用备份：$latest"
}

run() { echo "==> $1"; "${PSQL[@]}" -q -1 -f "$HERE/$1"; }

mode="${1:-}"
case "$mode" in
    --check|--snapshot|--grant|--rollback) ;;
    *) echo "用法：apply.sh --check|--snapshot|--grant|--rollback" >&2; exit 2 ;;
esac

echo "== 目标库：$DB_NAME @ $DB_HOST:$DB_PORT（$mode）=="
snapshot

case "$mode" in
    --check) exit 0 ;;
    --snapshot) run 05_snapshot.up.sql ;;
    --grant)    require_backup; run 20_grant.up.sql ;;
    --rollback) run 20_grant.down.sql; run 05_snapshot.down.sql ;;
esac

snapshot
echo "== 完成（$mode）；改过授权请接着跑 cache_invalidate.sh =="
