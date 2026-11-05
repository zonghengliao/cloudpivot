#!/usr/bin/env bash
# ============================================================================
# E2ERES3 · 应用 / 回滚 / 现状（每个 SQL 一个事务；幂等；可逆）
# ----------------------------------------------------------------------------
# 用法：
#   bash scripts/datafix/e2e_residue3/apply.sh             # 备份闸 → 10 → 20 up → 30 → 标记交叉断言
#   bash scripts/datafix/e2e_residue3/apply.sh --check     # 只看现状（跑 e2e_marker.py，不写库）
#   bash scripts/datafix/e2e_residue3/apply.sh --rollback  # 20 down（按快照整行写回）
#
# 环境：
#   E2ERES3_DB=<db>            目标库（默认 NIK_DATABASE_NAME，生产中 = cloudpivot）
#   E2ERES3_BACKUP_DIR=<dir>   显式指定备份目录（默认按 MANIFEST 自动找**本库**最新一份）
#   E2ERES3_ENV_FILE           默认 /etc/cloudpivot/cloudpivot.env
#   FORCE=1                    跳过「必须存在本库备份」的保护（**演练库**也要先跑 00_backup.sh；
#                              真生产一律不要用这个开关）
#
# 两道闸（都不靠记忆）：
#   ① 备份闸：MANIFEST 里必须是**本库**（`目标库 : <db> @`），拿别库的备份糊弄不过去；
#   ② 判据闸：清完必须让 `scripts/verify/e2e_marker.py --assert-clean` 也认干净 ——
#      本包自带的 SQL 判据与 e2e_marker 是两份实现，**只按宽的那份绿不算绿**。
# ============================================================================
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERIFY="$HERE/../../verify"

ENV_FILE="${E2ERES3_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
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
DB_NAME="${E2ERES3_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"

# ⚠️ 必须导出：apply.sh 会调 python 侧的判据（e2e_marker.py），它只认 NIK_DATABASE_*。
# 2026-11-05 演练踩过：这里只写 `DB=$DB_NAME python3 …` 时，python 读到的是
# source 进来的**生产库名** ⇒ 演练库清干净了、判据却在查生产，红绿全靠巧合。
# 导出让「杀谁」与「查谁」永远是同一个库。
export NIK_DATABASE_NAME="$DB_NAME"
export NIK_DATABASE_HOST="$DB_HOST"
export NIK_DATABASE_PORT="$DB_PORT"
export NIK_DATABASE_USER="$DB_USER"

PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -X -v ON_ERROR_STOP=1)

MODE="apply"
case "${1:-}" in
    --check)    MODE="check" ;;
    --rollback) MODE="rollback" ;;
    "")         MODE="apply" ;;
    *) echo "未知参数：$1（可用：--check / --rollback）" >&2; exit 2 ;;
esac

echo "=============================================================="
echo " E2ERES3  目标库：$DB_NAME @ $DB_HOST:$DB_PORT  模式：$MODE"
echo " 时间：$(date '+%F %T %Z')"
echo "=============================================================="

# ---------------------------------------------------------------- 闸 ①：备份
find_backup() {
    if [ -n "${E2ERES3_BACKUP_DIR:-}" ]; then
        echo "$E2ERES3_BACKUP_DIR"; return
    fi
    local d
    for d in $(ls -dt /root/datafix-backup-e2eres3-* 2>/dev/null); do
        if grep -q "^# 目标库  : $DB_NAME @" "$d/MANIFEST" 2>/dev/null; then
            echo "$d"; return
        fi
    done
}

if [ "$MODE" = "apply" ]; then
    BK="$(find_backup)"
    if [ -z "$BK" ] || [ ! -f "$BK/MANIFEST" ]; then
        if [ "${FORCE:-0}" = "1" ]; then
            echo "!! FORCE=1：跳过备份闸（只允许演练库这么用）" >&2
        else
            cat >&2 <<EOF
!! 拒绝执行：找不到「本库 $DB_NAME」的备份。
!! 先跑：bash $HERE/00_backup.sh
!! （生产上不允许无备份就改数据 —— 这是本包唯一的硬保护）
EOF
            exit 4
        fi
    else
        echo "闸① 备份：$BK（MANIFEST 里目标库 = $DB_NAME）"
        sed -n '1,6p' "$BK/MANIFEST" | sed 's/^/    /'
    fi
fi

run_sql() { # run_sql <文件> [说明]
    echo "---- $2（$(basename "$1")）"
    "${PSQL[@]}" -q -f "$1" || { echo "!! 失败：$1" >&2; exit 1; }
}

# ---------------------------------------------------------------- 闸 ③：生产写保护
# 与 `scripts/verify/db_guard.py` 同一套规矩（**不是**新发明一份判据）：
#   目标库 == 生产库 → 必须 `E2E_ALLOW_PROD=1` + `E2E_PROD_REASON='<谁批准/为什么，≥8 字>'`，
#   放行后 db_guard 会打横幅并 append 到 /tmp/e2e-prod-target.log 留痕。
# 演练库（cloudpivot_e2eres3_drill）库名不是生产库，自动放行 —— 演练不用编理由。
assert_db_writable() {
    if python3 -c "import sys; sys.path.insert(0, '$VERIFY'); import db_guard; \
db_guard.require_non_prod('$DB_NAME', '$DB_HOST', '$DB_PORT', \
script='scripts/datafix/e2e_residue3/apply.sh')"; then
        return 0
    fi
    cat >&2 <<EOF
!! 拒绝执行：目标库 $DB_NAME 是生产库，且没有给放行口。
!! 确实要改生产数据（本卡 t_44766ab3 的场景）：
!!   E2E_ALLOW_PROD=1 E2E_PROD_REASON='<谁批准/为什么，≥8 字>' bash $0
EOF
    exit 4
}

case "$MODE" in
# ---------------------------------------------------------------- 现状
check)
    echo "---- 现状（标记判据 e2e_marker.py，只读）"
    python3 "$VERIFY/e2e_marker.py"
    rc=$?
    echo
    echo "---- 账本 / 快照"
    "${PSQL[@]}" -At -F'|' -c "
        select 'guard  '||item||' = '||val from datafix_e2eres3_guard
         where batch='E2ERES3-20261105' order by item;" 2>/dev/null || echo "（本库还没跑过本包）"
    "${PSQL[@]}" -At -F'|' -c "
        select 'snapshot '||tbl||' = '||count(*) from datafix_e2eres3_snapshot
         where batch='E2ERES3-20261105' group by tbl order by tbl;" 2>/dev/null || true
    exit $rc
    ;;
# ---------------------------------------------------------------- 回滚
rollback)
    assert_db_writable
    run_sql "$HERE/20_softdelete_e2e_residue.down.sql" "回滚（按快照写回）"
    echo "---- 回滚后现状"
    python3 "$VERIFY/e2e_marker.py" || true
    exit 0
    ;;
# ---------------------------------------------------------------- 应用
apply)
    assert_db_writable
    run_sql "$HERE/10_precheck.sql" "前置校验 + 判据 DDL"
    run_sql "$HERE/20_softdelete_e2e_residue.up.sql" "软删 + 禁用（单事务）"
    run_sql "$HERE/30_verify.sql" "自检（残留=0 / 账号已禁用 / 账务行数不变）"

    echo "---- 闸② 判据交叉断言（e2e_marker.py --assert-clean；判据库 = $NIK_DATABASE_NAME）"
    if python3 "$VERIFY/e2e_marker.py" --assert-clean; then
        echo "     ✅ e2e_marker 也认干净（两份独立实现同时通过）"
    else
        echo "     ✗ e2e_marker 仍认为有残留 —— 本包判据比它宽，收紧判据后重跑" >&2
        exit 1
    fi

    echo
    echo "=============================================================="
    echo " E2ERES3 完成。复跑同一条命令是幂等的（第二遍命中 0 行）。"
    echo " 账务不变量：python3 $VERIFY/erp_invariants.py"
    echo "=============================================================="
    ;;
esac
