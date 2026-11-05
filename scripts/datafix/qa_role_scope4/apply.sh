#!/usr/bin/env bash
# ============================================================================
# P0-FIX-3B · 应用 / 回滚 / 现状（V0249：QA 夹具角色 data_scope 1 → 4）
#
# 用法（在 144 上跑生产，在 99 上跑演练库）：
#   bash apply.sh --check                       # 只读：现状 + 30_verify（不写库）
#   QA4_ALLOW_PROD=1 QA4_REASON='…' bash apply.sh           # 备份闸 → up → 记账 → 验收
#   QA4_DB=cloudpivot_xxx bash apply.sh          # 非生产库（库名不是 cloudpivot 时不需 ALLOW_PROD）
#   QA4_DB=cloudpivot_xxx bash apply.sh --rollback          # down → 删 249 记账行 → 验收回到 1
#
# 三道闸（都不靠记忆）：
#   ① 生产写保护：目标库名为 cloudpivot（= 生产）时必须显式 QA4_ALLOW_PROD=1 + QA4_REASON，
#      并把理由落 /tmp/qa4-prod-write.log；
#   ② 备份闸：apply/rollback 前必须**当场**做一份本库 pg_dump（MANIFEST 里写库名/时间/sha256），
#      找不到本次备份就不写库；
#   ③ 判据闸：写库收尾跑 30_verify.sql（up 后 exp=4 / down 后 exp=1），出现任何 FAIL 即 exit 1。
#
# 环境变量：
#   QA4_DB / QA4_DB_USER / QA4_DB_PASSWORD / QA4_DB_HOST / QA4_DB_PORT
#   （默认取 NIK_DATABASE_*，即 source /etc/cloudpivot/cloudpivot.env 后的生产口径）
#   QA4_BACKUP_ROOT  备份根目录（默认 /root/datafix-backup-qa4）
# ============================================================================
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
UP="$REPO/backend/migrations/business/V0249__qa_fixture_role_scope_self_only.up.sql"
DOWN="$REPO/backend/migrations/business/V0249__qa_fixture_role_scope_self_only.down.sql"
LEDGER="$HERE/40_ledger_import.sql"
VERIFY="$HERE/30_verify.sql"

ENV_FILE="${QA4_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
if [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    set -a; . "$ENV_FILE"; set +a
fi

DB_HOST="${QA4_DB_HOST:-${NIK_DATABASE_HOST:-127.0.0.1}}"
DB_PORT="${QA4_DB_PORT:-${NIK_DATABASE_PORT:-5432}}"
DB_USER="${QA4_DB_USER:-${NIK_DATABASE_USER:-alun}}"
DB_NAME="${QA4_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"
DB_PASSWORD="${QA4_DB_PASSWORD:-${NIK_DATABASE_PASSWORD:-alun}}"
export PGPASSWORD="$DB_PASSWORD"

BACKUP_ROOT="${QA4_BACKUP_ROOT:-/root/datafix-backup-qa4}"
PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -X -v ON_ERROR_STOP=1)
MODE="apply"
while [ $# -gt 0 ]; do
    case "$1" in
        --check)    MODE="check" ;;
        --rollback) MODE="rollback" ;;
        *) echo "未知参数：$1（可用：--check / --rollback）" >&2; exit 2 ;;
    esac
    shift
done

say() { printf '%s\n' "$*"; }

say "=============================================================="
say " QA4  目标库：$DB_NAME @ $DB_HOST:$DB_PORT (user=$DB_USER)  模式：$MODE"
say " 时间：$(date '+%F %T %Z')"
say "=============================================================="

# ---- 现状 -------------------------------------------------------------------
current() {
    "${PSQL[@]}" -At -F'|' -c "
        select role_code, data_scope, status,
               (select count(*) from sys_user_role ur join sys_user u on u.id = ur.user_id
                 where ur.role_id = r.id and u.deleted_at is null and u.status = 1) as active_holders
          from sys_role r
         where r.deleted_at is null
           and r.role_code in ('QA_LIMITED','QA_VIEWER','MUTEX_A','MUTEX_B')
         order by role_code"
}

verify() {
    local exp=$1 tag=$2 rc=0
    "${PSQL[@]}" -v exp="$exp" -f "$VERIFY" > "/tmp/qa4_verify_${tag}.txt" 2>&1 || rc=$?
    if [ $rc -ne 0 ]; then say "❌ 30_verify 执行失败（$tag, rc=$rc）"; tail -20 "/tmp/qa4_verify_${tag}.txt"; return 1; fi
    if grep -q 'FAIL' "/tmp/qa4_verify_${tag}.txt"; then
        say "❌ 30_verify（exp=$exp, $tag）有 FAIL："
        grep 'FAIL' "/tmp/qa4_verify_${tag}.txt"
        return 1
    fi
    say "✅ 30_verify（exp=$exp, $tag）零 FAIL（明细 /tmp/qa4_verify_${tag}.txt）"
    return 0
}

if [ "$MODE" = "check" ]; then
    say ""
    say "---- 现状（4 个夹具角色）----"
    current | sed 's/^/    /'
    say ""
    say "---- 迁移记账 ----"
    "${PSQL[@]}" -At -F'|' -c "select version,name,applied_via from schema_migrations where version in ('246','247','248','249') order by version" | sed 's/^/    /'
    say ""
    verify 4 "check_exp4" || true
    exit 0
fi

# ---- 闸 ①：生产写保护 -------------------------------------------------------
if [ "$DB_NAME" = "cloudpivot" ] || [ "$DB_HOST" = "127.0.0.1" -a "$DB_NAME" = "${NIK_DATABASE_NAME:-}" -a -f /etc/cloudpivot/cloudpivot.env ]; then
    if [ "${QA4_ALLOW_PROD:-0}" != "1" ] || [ -z "${QA4_REASON:-}" ]; then
        say "❌ 目标是生产库 $DB_NAME：必须 QA4_ALLOW_PROD=1 且给 QA4_REASON='…'"
        exit 3
    fi
    say "⚠️  生产写放行：reason=$QA4_REASON" | tee -a /tmp/qa4-prod-write.log
    echo "$(date '+%F %T %Z') db=$DB_NAME mode=$MODE user=$(id -un) reason=$QA4_REASON" >> /tmp/qa4-prod-write.log
fi

# ---- 闸 ②：本次备份 ---------------------------------------------------------
STAMP="$(date '+%Y%m%d-%H%M%S')"
BK="$BACKUP_ROOT/$STAMP-$MODE"
mkdir -p "$BK"
say "→ 备份到 $BK"
pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" --no-owner --no-privileges \
        -t public.sys_role -t public.sys_user -t public.sys_user_role \
        -t public.schema_migrations -t public.sys_role_dept -t public.sys_role_menu \
        > "$BK/tables.sql" || { say "❌ pg_dump 失败，不写库"; exit 4; }
pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" --no-owner --no-privileges \
        > "$BK/full.sql" || { say "❌ 全库 pg_dump 失败，不写库"; exit 4; }
{
    echo "# QA4 备份 MANIFEST"
    echo "# 目标库  : $DB_NAME @ $DB_HOST:$DB_PORT"
    echo "# 用途    : P0-FIX-3B（V0249 QA 夹具角色 data_scope 1→4）$MODE 前快照"
    echo "# 时间    : $(date '+%F %T %Z')"
    echo "# 操作者  : $(id -un)@$(hostname)"
    echo "# 回滚    : psql -f $BK/tables.sql（仅覆盖 6 张权限相关表的行）"
    echo "# sha256  : tables.sql=$(sha256sum "$BK/tables.sql" | cut -d' ' -f1)"
    echo "# sha256  : full.sql=$(sha256sum "$BK/full.sql" | cut -d' ' -f1)"
} > "$BK/MANIFEST"
grep -q "^# 目标库  : $DB_NAME @" "$BK/MANIFEST" || { say "❌ MANIFEST 库名不符，不写库"; exit 4; }
say "✅ 备份闸：$BK（$(du -h "$BK/full.sql" | cut -f1)）"

# ---- 写库 -------------------------------------------------------------------
if [ "$MODE" = "apply" ]; then
    say ""
    say "---- 现状（应用前）----"
    current | sed 's/^/    /'
    say ""
    say "---- 执行 V0249 up ----"
    "${PSQL[@]}" -f "$UP" 2>&1 | grep -E 'NOTICE|WARNING|ERROR' | sed 's/^/    /'
    rc=${PIPESTATUS[0]}
    [ "$rc" -eq 0 ] || { say "❌ up 失败 rc=$rc"; exit 5; }
    say ""
    say "---- 记账（schema_migrations：V0247 / V0249，applied_via=imported）----"
    "${PSQL[@]}" -f "$LEDGER" 2>&1 | sed 's/^/    /'
    rc=${PIPESTATUS[0]}
    [ "$rc" -eq 0 ] || { say "❌ 记账失败 rc=$rc"; exit 6; }
    say ""
    verify 4 "after_apply" || exit 7
else
    say ""
    say "---- 执行 V0249 down ----"
    "${PSQL[@]}" -f "$DOWN" 2>&1 | grep -E 'NOTICE|WARNING|ERROR' | sed 's/^/    /'
    rc=${PIPESTATUS[0]}
    [ "$rc" -eq 0 ] || { say "❌ down 失败 rc=$rc"; exit 5; }
    say "-- 删除 V0249 的记账行（V0247 的记账保留：它确实被执行过）"
    "${PSQL[@]}" -c "DELETE FROM schema_migrations WHERE kind='versioned' AND name='qa_fixture_role_scope_self_only'" | sed 's/^/    /'
    say ""
    verify 1 "after_rollback" || exit 7
fi

say ""
say "---- 现状（写库后）----"
current | sed 's/^/    /'
say ""
say "---- 迁移记账（尾部）----"
"${PSQL[@]}" -At -F'|' -c "select version,name,applied_via from schema_migrations where version ~ '^[0-9]+$' and version::int >= 245 order by version::int" | sed 's/^/    /'
say ""
say "✅ 完成（$MODE）：备份在 $BK"
