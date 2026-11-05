#!/usr/bin/env bash
# ============================================================================
# ORGLANDING（P0-FIX-2B-A）· 应用 / 回滚 / 现状
# ----------------------------------------------------------------------------
# 用法：
#   bash scripts/datafix/org_landing/apply.sh              # 应用 10（组织落位；幂等）
#   bash scripts/datafix/org_landing/apply.sh --rollback   # 按快照还原（回到落位前）
#   bash scripts/datafix/org_landing/apply.sh --check      # 只看现状（不写库）
#
# 环境：
#   ORGLANDING_DB=<db>        目标库（默认取 NIK_DATABASE_NAME，生产中 = cloudpivot）
#   FORCE=1                   跳过「必须存在本库备份」的保护（演练库里也建议先跑 00_backup.sh）
#   CLOUDPIVOT_ENV_FILE       默认 /etc/cloudpivot/cloudpivot.env
#
# 安全：
#   * 应用前必须能在 /root/datafix-backup-orglanding-* 里找到**本库**的 full.dump
#     （校验 MANIFEST.txt 里的 `database  : <db> on `，避免拿别的库的备份当挡箭牌）
#   * 前置校验全写在 SQL 里（部门树缺失 / 账号缺失 / 快照行数不符 → RAISE EXCEPTION 整脚本回滚）
#   * 组织树缺失时脚本以非 0 退出，调用方（发版脚本 / 人）必须看见
#
# 为什么是 datafix 脚本而不是版本迁移（V0248+）：
#   生产要能在**不回滚代码**的前提下单独跑 / 单独回滚；写成迁移就得连带发一版二进制，
#   而组织落位是数据决定（dept_code → uuid 各环境不同），与代码版本无关。
# ============================================================================
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

ENV_FILE="${CLOUDPIVOT_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
if [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    set -a; . "$ENV_FILE"; set +a
fi
DB_PASSWORD="${NIK_DATABASE_PASSWORD:-${ALUN_DATABASE_PASSWORD:-}}"
: "${DB_PASSWORD:?需要 NIK_DATABASE_PASSWORD（先 . /etc/cloudpivot/cloudpivot.env）}"
export PGPASSWORD="$DB_PASSWORD"

DB_HOST="${NIK_DATABASE_HOST:-${ALUN_DATABASE_HOST:-127.0.0.1}}"
DB_PORT="${NIK_DATABASE_PORT:-${ALUN_DATABASE_PORT:-5432}}"
DB_USER="${NIK_DATABASE_USER:-${ALUN_DATABASE_USER:-cloudpivot}}"
DB_NAME="${ORGLANDING_DB:-${NIK_DATABASE_NAME:-${ALUN_DATABASE_NAME:-cloudpivot}}}"

PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -v ON_ERROR_STOP=1)

snapshot() {
    echo "---- 现状快照（$(date '+%H:%M:%S')）----"
    "${PSQL[@]}" -X -q -At -F'|' <<'SQL'
SELECT '  ' || rpad(u.username, 14) || ' → ' || coalesce(d.dept_code, '(无部门)')
  FROM sys_user u LEFT JOIN sys_dept d ON d.id = u.dept_id
 WHERE u.username IN ('admin','laoliu','laoliu_test','cl_43870','henry.liao','verify-ai','qa.viewer')
 ORDER BY u.username;
SELECT '  sys_user_role 行数 = ' || count(*) FROM sys_user_role;
SELECT '  biz_task.dept_id 非空 = ' || count(*) FROM biz_task WHERE dept_id IS NOT NULL;
SELECT '  biz_project.dept_id 非空 = ' || count(*) FROM biz_project WHERE dept_id IS NOT NULL;
SELECT '  sys_dept.leader_id 非空 = ' || count(*) FROM sys_dept WHERE leader_id IS NOT NULL;
SQL
    local has_snap
    has_snap=$("${PSQL[@]}" -X -q -At -c "SELECT coalesce(to_regclass('public.datafix_orglanding_snapshot')::text,'')")
    if [ -n "$has_snap" ]; then
        echo "  datafix_orglanding_snapshot 行数 = $("${PSQL[@]}" -X -q -At -c \
            "SELECT count(*) FROM datafix_orglanding_snapshot WHERE batch='ORGLANDING-20261105'")"
    else
        echo "  datafix_orglanding_snapshot 行数 = (表还没建；本库还没跑过 apply)"
    fi
}

mode="apply"
case "${1:-}" in
    --rollback) mode="rollback" ;;
    --check)    mode="check" ;;
    "")        ;;
    *) echo "未知参数：$1（可用 --rollback / --check）" >&2; exit 2 ;;
esac

echo "== 目标库：$DB_NAME @ $DB_HOST:$DB_PORT（$mode）=="
snapshot

if [ "$mode" = "check" ]; then
    exit 0
fi

if [ "$mode" = "apply" ]; then
    if [ "${FORCE:-0}" != "1" ]; then
        latest=""
        while IFS= read -r dir; do
            [ -s "$dir/full.dump" ] || continue
            if grep -q "database  : $DB_NAME on " "$dir/MANIFEST.txt" 2>/dev/null; then
                latest="$dir"
                break
            fi
        done < <(ls -1dt /root/datafix-backup-orglanding-* 2>/dev/null || true)
        if [ -z "$latest" ]; then
            echo "✗ 找不到本库（$DB_NAME）的备份 full.dump：先跑 00_backup.sh（或用 FORCE=1 跳过）" >&2
            exit 1
        fi
        echo "==> 使用备份：$latest"
    fi
    echo "==> 应用 10_org_landing"
    "${PSQL[@]}" -q -1 -f "$HERE/10_org_landing.up.sql"
else
    echo "==> 回滚 10_org_landing"
    "${PSQL[@]}" -q -1 -f "$HERE/10_org_landing.down.sql"
fi

snapshot
echo "== 完成（$mode）=="
