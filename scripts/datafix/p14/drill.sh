#!/usr/bin/env bash
# ============================================================================
# P14 · 演练（在 prod 的 pg_dump 副本库上把 up / 幂等 / down 全跑一遍）
# ----------------------------------------------------------------------------
# 为什么必须演练：prod 是活库，脚本里的前置校验/幂等/回滚只有在**同一份真实数据**上
#   跑过才算证据；自造夹具演不出「恰好 10 行」「qty=10/amount=0」这些真实形态。
#
# 流程：
#   ① pg_dump 源库 → createdb 演练库 → pg_restore → 记基线
#   ② 00_backup.sh（P14_DB=演练库，标签 drill）
#   ③ apply.sh 第 1 遍：10 → 20 → 30 自检（全绿）
#   ④ apply.sh 第 2 遍：幂等（NOTICE「幂等跳过」，两次基线逐项相同）
#   ⑤ apply.sh --rollback：20 down → 10 down，回滚后必须与基线逐项相等
#   ⑥ 回滚后重新 apply（证明可逆且可重放）
#   ⑦ drop 演练库
#
# 用法：
#   bash scripts/datafix/p14/drill.sh
#   P14_SRC_DB=cloudpivot P14_DRILL_DB=cloudpivot_p14drill bash .../drill.sh
# ============================================================================
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

ENV_FILE="${CLOUDPIVOT_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
if [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    set -a; . "$ENV_FILE"; set +a
fi
DB_PASSWORD="${NIK_DATABASE_PASSWORD:-${ALUN_DATABASE_PASSWORD:-}}"
: "${DB_PASSWORD:?需要 NIK_DATABASE_PASSWORD}"
export PGPASSWORD="$DB_PASSWORD"

DB_HOST="${NIK_DATABASE_HOST:-${ALUN_DATABASE_HOST:-127.0.0.1}}"
DB_PORT="${NIK_DATABASE_PORT:-${ALUN_DATABASE_PORT:-5432}}"
DB_USER="${NIK_DATABASE_USER:-${ALUN_DATABASE_USER:-cloudpivot}}"
SRC_DB="${P14_SRC_DB:-${NIK_DATABASE_NAME:-${ALUN_DATABASE_NAME:-cloudpivot}}}"
DRILL_DB="${P14_DRILL_DB:-cloudpivot_p14drill}"
TS="$(date +%Y%m%d-%H%M%S)"
LOG="${P14_DRILL_LOG:-/root/datafix-p14-drill-$TS.log}"
DUMP="/tmp/p14-$TS-prod.dump"

PSQL_SRC=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$SRC_DB" -At -v ON_ERROR_STOP=1)
PSQL_DR=(psql  -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DRILL_DB" -At -v ON_ERROR_STOP=1)

FAIL=0
ok()  { echo "  [PASS] $*"; }
bad() { echo "  [FAIL] $*"; FAIL=$((FAIL+1)); }
say() { echo; echo "### $*"; }
val() { echo "$1" | grep "^$2|" | cut -d'|' -f2; }

# 业务基线：清理前应有的样子（回滚后必须逐项回到这里）
baseline_of() {
    "$@" -F'|' <<'SQL'
select 'serials_alive', count(*)::text
  from biz_material_serial s
 where s.deleted_at is null and s.source_type='PRODUCE' and s.source_id is null
   and s.create_by=(select id from sys_user where username='henry.liao')
   and s.created_at >= timestamptz '2026-10-31 16:17:00+08'
   and s.created_at <  timestamptz '2026-10-31 16:18:00+08'
union all
select 'inv_qty', coalesce(max(i.qty),0)::numeric(18,4)::text
  from biz_inventory i join biz_material m on m.id=i.material_id and m.material_code='M-W1A-004'
union all
select 'inv_amount', coalesce(max(i.amount),0)::numeric(18,4)::text
  from biz_inventory i join biz_material m on m.id=i.material_id and m.material_code='M-W1A-004'
union all
select 'mat_txn_rows', count(*)::text
  from biz_inventory_txn t join biz_material m on m.id=t.material_id and m.material_code='M-W1A-004'
union all
select 'p14_txns', count(*)::text from biz_inventory_txn where doc_no='DATAFIX-20261101-P14'
union all
select 'db_inv_qty', coalesce(sum(qty),0)::numeric(18,4)::text from biz_inventory
union all
select 'db_inv_amount', coalesce(sum(amount),0)::numeric(18,4)::text from biz_inventory
union all
select 'db_loc_qty', coalesce(sum(qty),0)::numeric(18,4)::text from biz_inventory_location
union all
select 'db_txn_rows', count(*)::text from biz_inventory_txn
union all
select 'db_serial_alive', count(*)::text from biz_material_serial where deleted_at is null
union all
select 'db_vouchers', count(*)::text from biz_voucher where deleted_at is null
SQL
}

run_drill() {
    say "P14 演练开始 $(date '+%Y-%m-%d %H:%M:%S %Z')  host=$(hostname)"
    echo "源库=$SRC_DB  演练库=$DRILL_DB  @ $DB_HOST:$DB_PORT"

    # ---------- ① 建演练库 ----------
    say "① 建演练库（prod 的 pg_dump 副本）"
    "${PSQL_SRC[@]}" -c "select 'src migration_max = ' || coalesce(max(version::int),0) from schema_migrations"
    if [ "$("${PSQL_SRC[@]}" -c "select count(*) from pg_database where datname='$DRILL_DB'")" != "0" ]; then
        echo "  演练库已存在 → 先 drop"
        dropdb -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" --force "$DRILL_DB" || return 1
    fi
    if createdb -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -O "$DB_USER" "$DRILL_DB" 2>/dev/null; then
        echo "  createdb 用应用账号成功"
    else
        echo "  应用账号无 CREATEDB → 走 su - postgres"
        su - postgres -c "createdb -O '$DB_USER' '$DRILL_DB'" || return 1
    fi
    pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$SRC_DB" -Fc -f "$DUMP" || return 1
    pg_restore -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DRILL_DB" --no-owner --no-privileges "$DUMP" || return 1
    echo "  副本恢复完成（dump $(du -h "$DUMP" | cut -f1)）"

    BASE="$(baseline_of "${PSQL_DR[@]}")"
    say "基线（演练库刚建好 = prod 当前状态）"
    echo "$BASE" | sed 's/^/  /'

    # ---------- ② 备份 ----------
    say "② 00_backup.sh（P14_DB=$DRILL_DB，标签 drill）"
    P14_DB="$DRILL_DB" bash "$HERE/00_backup.sh" drill 2>&1 | tail -22
    if [ -s "/root/datafix-backup-p14-drill/target_tables.dump" ]; then
        ok "演练库备份已生成（/root/datafix-backup-p14-drill）"
    else
        bad "演练库备份缺失"
    fi

    # ---------- ③ 第 1 遍 ----------
    say "③ apply.sh 第 1 遍（软删 10 / 冲销 10 / 自检全绿）"
    P14_DB="$DRILL_DB" bash "$HERE/apply.sh" > /tmp/p14-drill-apply1.log 2>&1
    local rc=$?
    grep -E 'NOTICE|PASS|FAIL|幂等' /tmp/p14-drill-apply1.log | sed 's/^/  /'
    [ $rc -eq 0 ] && ok "第 1 遍 exit=0" || bad "第 1 遍 exit=$rc"
    grep -q '^FAIL' /tmp/p14-drill-apply1.log && bad "第 1 遍自检出现 FAIL" || ok "第 1 遍自检无 FAIL"

    local after1
    after1="$(baseline_of "${PSQL_DR[@]}")"
    say "第 1 遍之后"
    echo "$after1" | sed 's/^/  /'
    [ "$(val "$after1" serials_alive)" = "0" ]      && ok "序列号存活 0（10 个全软删）" || bad "序列号存活不为 0"
    [ "$(val "$after1" inv_qty)" = "0.0000" ]       && ok "成本账 qty=0.0000"          || bad "成本账 qty 不为 0"
    [ "$(val "$after1" inv_amount)" = "0.0000" ]    && ok "成本账 amount=0.0000"       || bad "成本账 amount 不为 0"
    [ "$(val "$after1" p14_txns)" = "1" ]           && ok "冲销流水 1 条"               || bad "冲销流水不是 1 条"
    [ "$(val "$after1" db_loc_qty)" = "207.0000" ]  && ok "全库库位账仍 207.0000（未误伤）" || bad "全库库位账变了"
    [ "$(val "$after1" db_vouchers)" = "9" ]        && ok "全库凭证数仍 9（未新增凭证）"     || bad "凭证数变了"

    # ---------- ④ 第 2 遍（幂等） ----------
    say "④ apply.sh 第 2 遍（幂等：exit=0 且 0 行受影响）"
    P14_DB="$DRILL_DB" bash "$HERE/apply.sh" > /tmp/p14-drill-apply2.log 2>&1
    rc=$?
    grep -E 'NOTICE|PASS|FAIL|幂等' /tmp/p14-drill-apply2.log | sed 's/^/  /'
    [ $rc -eq 0 ] && ok "第 2 遍 exit=0" || bad "第 2 遍 exit=$rc"
    [ "$(grep -c '幂等跳过' /tmp/p14-drill-apply2.log)" -ge 2 ] \
        && ok "第 2 遍两步都命中幂等分支（NOTICE 幂等跳过 ×2）" || bad "第 2 遍未命中幂等分支"
    local after2
    after2="$(baseline_of "${PSQL_DR[@]}")"
    [ "$after1" = "$after2" ] && ok "两遍结果逐项相同（第二遍 0 行受影响）" || { bad "两遍结果不一致"; diff <(echo "$after1") <(echo "$after2") | sed 's/^/  /'; }

    # ---------- ⑤ 回滚 ----------
    say "⑤ apply.sh --rollback（20 down → 10 down）"
    P14_DB="$DRILL_DB" bash "$HERE/apply.sh" --rollback > /tmp/p14-drill-rollback.log 2>&1
    rc=$?
    grep -E 'NOTICE|PASS|FAIL' /tmp/p14-drill-rollback.log | sed 's/^/  /'
    [ $rc -eq 0 ] && ok "回滚 exit=0" || bad "回滚 exit=$rc"

    local rolled
    rolled="$(baseline_of "${PSQL_DR[@]}")"
    say "回滚之后 vs 基线"
    echo "$rolled" | sed 's/^/  /'
    if [ "$rolled" = "$BASE" ]; then
        ok "回滚后与基线逐项完全相同（序列号/成本账/流水/全库合计/凭证数）"
    else
        bad "回滚后与基线不一致"
        diff <(echo "$BASE") <(echo "$rolled") | sed 's/^/  /'
    fi

    # ---------- ⑥ 回滚后重放 ----------
    say "⑥ 回滚后重新 apply（证明脚本可逆且可重放）"
    P14_DB="$DRILL_DB" bash "$HERE/apply.sh" > /tmp/p14-drill-reapply.log 2>&1
    rc=$?
    [ $rc -eq 0 ] && ok "重放 exit=0 且自检全绿" || bad "重放 exit=$rc"
    [ "$(baseline_of "${PSQL_DR[@]}")" = "$after1" ] && ok "重放后状态与第 1 遍一致" || bad "重放后状态与第 1 遍不一致"

    # ---------- ⑦ 收尾 ----------
    say "⑦ 收尾：drop 演练库"
    dropdb -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" --force "$DRILL_DB" && ok "演练库已删除" || bad "演练库删除失败"
    rm -f "$DUMP"

    say "汇总：FAIL=$FAIL"
    [ $FAIL -eq 0 ]
}

run_drill 2>&1 | tee "$LOG"
rc=${PIPESTATUS[0]}
echo "演练日志：$LOG"
exit $rc
