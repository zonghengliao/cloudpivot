#!/usr/bin/env bash
# ============================================================================
# FLOWCLEAN · 应用 / 回滚 / 现状（每个 SQL 脚本一个事务；幂等；可逆）
# ----------------------------------------------------------------------------
# 用法：
#   bash scripts/datafix/flowclean/apply.sh              # 备份闸 → 10 前置校验 → 20 删除 → 30 自检
#   bash scripts/datafix/flowclean/apply.sh --rollback   # 20 的 down（按快照原样恢复）→ 基线对账
#   bash scripts/datafix/flowclean/apply.sh --check      # 只看现状，不写库
#
# 环境：
#   FLOWCLEAN_DB=<db>            目标库（默认 NIK_DATABASE_NAME，生产中 = cloudpivot）
#   FLOWCLEAN_BACKUP_DIR=<dir>   显式指定备份目录（默认按 MANIFEST 自动找本库最新一份）
#   FORCE=1                      跳过「必须存在本库备份」的保护（演练库也建议先跑 00_backup.sh）
#   CLOUDPIVOT_ENV_FILE          默认 /etc/cloudpivot/cloudpivot.env
#
# 安全：
#   * 应用前必须能在 /root/datafix-backup-flowclean-* 里找到**本库**的备份（校验 MANIFEST 里的
#     `database : <db> on `），避免拿别的库（例如演练库）的备份当挡箭牌 —— P10 踩过这个坑。
#   * 所有前置校验写在 SQL 里（数量不符 / 保留流程缺失 → RAISE EXCEPTION，整脚本回滚）
#   * 30_verify.sql 出现任何 FAIL → 退出码 1
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
DB_NAME="${FLOWCLEAN_DB:-${NIK_DATABASE_NAME:-${ALUN_DATABASE_NAME:-cloudpivot}}}"

PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -v ON_ERROR_STOP=1)

snapshot() {
    echo "---- 现状快照（$(date '+%H:%M:%S')）----"
    "${PSQL[@]}" -At -F'|' <<'SQL'
select '存活流程定义            = ' || count(*) from biz_flow_def where deleted_at is null;
select '待清理 6 个（存活）     = ' || count(*) from biz_flow_def
 where deleted_at is null and flow_code in ('acc_tasknode_141800','acc_tn_141831','d5_281866',
                                            'd5_282996','e2e_cs_084913','p12_canvas_demo');
select '保留 6 个（PUBLISHED）  = ' || count(*) from biz_flow_def
 where deleted_at is null and status='PUBLISHED' and flow_code in ('task_accept','stock_transfer',
       'stock_in_cancel','payable_pay','po_approve','stock_in_post');
select '实例总数 / 孤儿实例     = ' || (select count(*) from biz_flow_instance)
     || ' / ' || (select count(*) from biz_flow_instance i
                   where i.deleted_at is null
                     and not exists (select 1 from biz_flow_def d where d.flow_code=i.flow_code and d.deleted_at is null));
select '待办总数 / 孤儿待办     = ' || (select count(*) from biz_flow_task)
     || ' / ' || (select count(*) from biz_flow_task ft
                   where ft.status='PENDING'
                     and not exists (select 1 from biz_flow_instance i
                                       join biz_flow_def d on d.flow_code=i.flow_code and d.deleted_at is null
                                      where i.id=ft.instance_id));
select 'flow 家族行数 def/ver/node/edge/asg/inst/inode/task = '
     || (select count(*) from biz_flow_def) || '/' || (select count(*) from biz_flow_def_version)
     || '/' || (select count(*) from biz_flow_node) || '/' || (select count(*) from biz_flow_edge)
     || '/' || (select count(*) from biz_flow_node_assignee) || '/' || (select count(*) from biz_flow_instance)
     || '/' || (select count(*) from biz_flow_instance_node) || '/' || (select count(*) from biz_flow_task);
SQL
}

verify() {
    local out rc
    out="$(mktemp)"
    echo "---- 30_verify.sql 自检 ----"
    set +e
    "${PSQL[@]}" -At -F'|' -f "$HERE/30_verify.sql" | tee "$out"
    rc=${PIPESTATUS[0]}
    set -e
    if [ "$rc" -ne 0 ]; then
        echo "✗ 自检脚本执行失败（psql 退出码 $rc）" >&2
        rm -f "$out"
        return 1
    fi
    if grep -q '^FAIL' "$out"; then
        echo "✗ 自检有 FAIL —— 不许当成功交付" >&2
        rm -f "$out"
        return 1
    fi
    rm -f "$out"
    echo "✓ 自检全绿（FAIL=0）"
    return 0
}

require_backup() {
    if [ "${FORCE:-0}" = "1" ]; then
        echo "⚠ FORCE=1 —— 跳过备份存在性检查"
        return 0
    fi
    if [ -n "${FLOWCLEAN_BACKUP_DIR:-}" ]; then
        if [ ! -s "$FLOWCLEAN_BACKUP_DIR/flow_tables.dump" ]; then
            echo "✗ FLOWCLEAN_BACKUP_DIR=$FLOWCLEAN_BACKUP_DIR 里没有 flow_tables.dump" >&2
            exit 1
        fi
        if ! grep -qE "database +: $DB_NAME on " "$FLOWCLEAN_BACKUP_DIR/MANIFEST.txt"; then
            echo "✗ 备份目录 $FLOWCLEAN_BACKUP_DIR 的 MANIFEST 不是库 $DB_NAME 的 —— 拒绝应用" >&2
            exit 1
        fi
        echo "==> 使用指定备份：$FLOWCLEAN_BACKUP_DIR"
        return 0
    fi
    local latest=""
    while IFS= read -r dir; do
        [ -s "$dir/flow_tables.dump" ] || continue
        if grep -qE "database +: $DB_NAME on " "$dir/MANIFEST.txt" 2>/dev/null; then
            latest="$dir"; break
        fi
    done < <(ls -1dt /root/datafix-backup-flowclean-* 2>/dev/null || true)
    if [ -z "$latest" ]; then
        echo "✗ 找不到本库（$DB_NAME）的备份 MANIFEST：先跑 00_backup.sh（或 FORCE=1 跳过）" >&2
        exit 1
    fi
    echo "==> 使用备份：$latest"
    FLOWCLEAN_BACKUP_DIR="$latest"
}

mode="apply"
case "${1:-}" in
    --rollback) mode="rollback" ;;
    --check)    mode="check" ;;
    "") ;;
    *) echo "未知参数：$1（可用 --rollback / --check）" >&2; exit 2 ;;
esac

echo "== FLOWCLEAN 目标库：$DB_NAME @ $DB_HOST:$DB_PORT（$mode）=="
snapshot

if [ "$mode" = "check" ]; then
    verify || true
    exit 0
fi

if [ "$mode" = "apply" ]; then
    require_backup
    echo "==> 10_precheck.sql（硬闸：6 待删 / 6 保留 / 无计划外流程）"
    "${PSQL[@]}" -q -1 -f "$HERE/10_precheck.sql"
    echo "==> 20_delete_test_flows.up.sql（快照 + 删除；幂等）"
    "${PSQL[@]}" -q -1 -f "$HERE/20_delete_test_flows.up.sql"
else
    echo "==> 20_delete_test_flows.down.sql（按快照原样恢复；幂等）"
    "${PSQL[@]}" -q -1 -f "$HERE/20_delete_test_flows.down.sql"
fi

snapshot
if [ "$mode" = "apply" ]; then
    verify
else
    echo "== 回滚完成：6 个流程已复原（阶段一/二之前的状态）=="
fi
echo "== 完成（$mode）=="
