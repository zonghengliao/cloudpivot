#!/usr/bin/env bash
# ============================================================================
# E2ERES2 · 应用 / 回滚 / 现状（每个 SQL 脚本一个事务；幂等；可逆）
# ----------------------------------------------------------------------------
# 用法：
#   bash scripts/datafix/e2e_residue2/apply.sh            # 备份闸 → 10 前置校验 → 20 删除 → 30 自检
#   bash scripts/datafix/e2e_residue2/apply.sh --check    # 只看现状（跑 30 自检 + 计数，不写库）
#   bash scripts/datafix/e2e_residue2/apply.sh --rollback # 20 的 down（按快照整行写回）
#
# 环境：
#   E2ERES2_DB=<db>             目标库（默认 NIK_DATABASE_NAME，生产中 = cloudpivot）
#   E2ERES2_BACKUP_DIR=<dir>    显式指定备份目录（默认按 MANIFEST 自动找本库最新一份）
#   E2ERES2_ENV_FILE            默认 /etc/cloudpivot/cloudpivot.env
#   FORCE=1                     跳过「必须存在本库备份」的保护（**演练库**也要先跑 00_backup.sh，
#                               真生产一律不要用这个开关）
#
# 安全：
#   * 应用前必须能在 /root/datafix-backup-e2eres2-* 里找到**本库**的备份
#     （校验 MANIFEST 里的 `目标库  : <db> @`），避免拿演练库的备份给生产当挡箭牌。
#   * 全部前置校验写在 SQL 里（数量不符 / 在飞审批 / 单据引用 / 串行引用 → RAISE EXCEPTION 整脚本回滚）
#   * 30_verify.sql 出现任何 FAIL → 退出码 1
# ============================================================================
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

ENV_FILE="${E2ERES2_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
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
DB_NAME="${E2ERES2_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"

PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -X -v ON_ERROR_STOP=1)

MODE="apply"
case "${1:-}" in
    --check)    MODE="check" ;;
    --rollback) MODE="rollback" ;;
    "")         MODE="apply" ;;
    *) echo "未知参数：$1（可用：--check / --rollback）" >&2; exit 2 ;;
esac

echo "=============================================================="
echo " E2ERES2  目标库：$DB_NAME @ $DB_HOST:$DB_PORT  模式：$MODE"
echo " 时间：$(date '+%F %T %Z')"
echo "=============================================================="

snapshot() {
    echo "---- 现状快照 ----"
    "${PSQL[@]}" -At -F'|' <<'SQL'
select 'alive_flow_def            = ' || count(*) from biz_flow_def where deleted_at is null;
select 'soft_deleted_flow_def      = ' || count(*) from biz_flow_def where deleted_at is not null;
select 'SET_A1_def(前缀判据)       = ' || count(*) from datafix_e2eres2_v_target_def;
select 'SET_A2_inst(孤儿实例)      = ' || count(*) from datafix_e2eres2_v_target_inst;
select 'soft_deleted_laoliu_test   = ' || count(*) from biz_flow_def
 where deleted_at is not null and create_by = (select id from sys_user where username='laoliu_test');
select 'instance PENDING(全库)     = ' || count(*) from biz_flow_instance where deleted_at is null and status='PENDING';
select 'flow_task PENDING          = ' || count(*) from biz_flow_task where status='PENDING';
select 'biz_status_log             = ' || count(*) from biz_status_log;
select '各表总量 def/ver/node/edge/asg/inst/inode/task/cc = '
    || (select count(*) from biz_flow_def) || '/' || (select count(*) from biz_flow_def_version)
    || '/' || (select count(*) from biz_flow_node) || '/' || (select count(*) from biz_flow_edge)
    || '/' || (select count(*) from biz_flow_node_assignee) || '/' || (select count(*) from biz_flow_instance)
    || '/' || (select count(*) from biz_flow_instance_node) || '/' || (select count(*) from biz_flow_task)
    || '/' || (select count(*) from biz_flow_cc);
SQL
}

run_verify() {
    echo "---- 30 自检 ----"
    local out
    out="$("${PSQL[@]}" -q -f "$HERE/30_verify.sql")"
    echo "$out"
    if grep -qE '^FAIL\|' <<<"$out"; then
        echo "!! 自检有 FAIL，退出码 1" >&2
        return 1
    fi
    echo "---- 自检：无 FAIL（WARN 见上）----"
}

find_backup() {
    local dir
    for dir in $(ls -1dt /root/datafix-backup-e2eres2-* 2>/dev/null); do
        if [ -f "$dir/MANIFEST" ] && grep -q "^# 目标库  : ${DB_NAME} @" "$dir/MANIFEST"; then
            echo "$dir"; return 0
        fi
    done
    return 1
}

# --check 只用「判据视图」，即使 10 从没跑过也能看现状。
# 视图是 create or replace（幂等 DDL，不动任何数据行），四种模式都先刷一遍。
"${PSQL[@]}" -q -f "$HERE/05_views.sql"

snapshot

case "$MODE" in
check)
    if [ "$("${PSQL[@]}" -At -c "select to_regclass('public.datafix_e2eres2_guard') is not null")" = "t" ]; then
        run_verify || exit 1
    else
        echo "（库里还没有 datafix_e2eres2_guard：说明 10 没跑过，--check 只给上面的现状计数）"
    fi
    ;;

apply)
    if [ "${FORCE:-0}" != "1" ]; then
        if [ -n "${E2ERES2_BACKUP_DIR:-}" ]; then
            BK="$E2ERES2_BACKUP_DIR"
            grep -q "^# 目标库  : ${DB_NAME} @" "$BK/MANIFEST" 2>/dev/null || {
                echo "!! 指定备份 $BK 不是本库（$DB_NAME）的备份，拒绝执行" >&2; exit 1; }
        else
            BK="$(find_backup)" || {
                echo "!! 找不到 $DB_NAME 的备份（/root/datafix-backup-e2eres2-*/MANIFEST）。" >&2
                echo "   先跑：bash $HERE/00_backup.sh   （演练库也要跑；确要跳过用 FORCE=1）" >&2
                exit 1; }
        fi
        echo "---- 备份闸通过：$BK ----"
        for f in flow_tables.dump biz_status_log.dump biz_task.dump target/classification.csv; do
            [ -s "$BK/$f" ] || { echo "!! 备份不完整：缺 $f，拒绝执行" >&2; exit 1; }
        done
        grep -E '^# (目标库|口径|git HEAD)' "$BK/MANIFEST" | sed 's/^/   /'
    else
        echo "---- FORCE=1：跳过备份闸（仅限演练/演练库）----"
    fi

    "${PSQL[@]}" --single-transaction -f "$HERE/10_precheck.sql"
    "${PSQL[@]}" --single-transaction -f "$HERE/20_purge_residue2.up.sql"
    "${PSQL[@]}" --single-transaction -f "$HERE/25_ledger.sql"
    run_verify || exit 1
    snapshot
    ;;

rollback)
    "${PSQL[@]}" --single-transaction -f "$HERE/20_purge_residue2.down.sql"
    snapshot
    ;;
esac

echo "==> E2ERES2 $MODE 结束（$(date '+%F %T')）"
