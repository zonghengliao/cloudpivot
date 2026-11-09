#!/usr/bin/env bash
# 云枢数据库恢复（从 pg_dump -Fc 备份恢复到指定库，恢复后自动核对基线）
#
# 用法：
#   bash scripts/ops/restore_db.sh <dump-file> --target-db <dbname> --yes
#                                   [--env-file PATH] [--force-prod]
#                                   [--confirm-db-name NAME] [--pre-backup-dir PATH]
#
# 参数：
#   <dump-file>               backup_db.sh 产出的 <db>-<YYYYmmdd-HHMMSS>.dump
#   --target-db NAME          恢复目标库（不存在会自动创建，owner 为应用账号）
#   --yes                     跳过「确认执行」交互（非交互环境必须给）
#   --env-file PATH           显式指定连接用环境变量文件（缺省自动探测 /etc/cloudpivot/*.env）
#   --force-prod              允许恢复到「受保护库」（生产库/当前 env 库/template*）
#   --confirm-db-name NAME    非交互地确认库名（配合 --force-prod；必须与 --target-db 完全一致）
#   --pre-backup-dir PATH     恢复前自动备份当前目标的目录（默认 /root/db-backup/pre-restore）
#
# 行为：
#   ① pg_restore --list 校验 dump ② 拒绝恢复到生产库（除非 --force-prod + 库名二次确认）
#   ③ 打印「即将把 X 恢复到 Y，现有数据会被清除」④ 需 --yes ⑤ 恢复前自动备份当前库
#   ⑥ pg_restore --clean --if-exists ⑦ 核对表数 / schema_migrations / 关键表行数
#   ⑧ 与 dump 同名 .meta 基线逐项比对，任一不一致即报警（退出码 2）
#
# 退出码：0 成功 / 1 参数或执行失败 / 2 恢复成功但基线不一致（需要人工判断）
#
# 受保护库（默认拒绝恢复）：
#   · --env-file 当前 env 的 NIK_DATABASE_NAME
#   · /etc/cloudpivot/cloudpivot.env（生产 env）里的 NIK_DATABASE_NAME
#   · 字面量 cloudpivot（生产库标准名）
#   · template0 / template1 / postgres
#   · 环境变量 PROTECTED_DBS="db1 db2" 里追加的库名
set -euo pipefail

PROG="$(basename "$0")"
DUMP=""
TARGET_DB=""
ENV_FILE_EXPLICIT=""
ASSUME_YES=0
FORCE_PROD=0
CONFIRM_DB_NAME=""
PRE_BACKUP_DIR="/root/db-backup/pre-restore"
RESTORE_LOG_DIR="/root/db-backup/logs"
PRE_BACKUP_KEEP=5

log()  { printf '[%s] INFO  %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%:z')" "$*"; }
warn() { printf '[%s] WARN  %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%:z')" "$*"; }
err()  { printf '[%s] ERROR %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%:z')" "$*" >&2; }
die()  { err "$*"; exit 1; }

usage() { sed -n '2,38p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit "${1:-0}"; }

[ $# -ge 1 ] || usage 1
case "$1" in -h|--help) usage 0 ;; -*) die "第一个参数必须是 dump 文件路径：$1" ;; *) DUMP="$1"; shift ;; esac

while [ $# -gt 0 ]; do
  case "$1" in
    --target-db)        TARGET_DB="${2:?--target-db 需要参数}"; shift 2 ;;
    --target-db=*)      TARGET_DB="${1#*=}"; shift ;;
    --yes|-y)           ASSUME_YES=1; shift ;;
    --env-file)         ENV_FILE_EXPLICIT="${2:?--env-file 需要参数}"; shift 2 ;;
    --env-file=*)       ENV_FILE_EXPLICIT="${1#*=}"; shift ;;
    --force-prod)       FORCE_PROD=1; shift ;;
    --confirm-db-name)  CONFIRM_DB_NAME="${2:?--confirm-db-name 需要参数}"; shift 2 ;;
    --confirm-db-name=*) CONFIRM_DB_NAME="${1#*=}"; shift ;;
    --pre-backup-dir)   PRE_BACKUP_DIR="${2:?--pre-backup-dir 需要参数}"; shift 2 ;;
    --pre-backup-dir=*) PRE_BACKUP_DIR="${1#*=}"; shift ;;
    -h|--help)          usage 0 ;;
    *)                  die "未知参数：$1（--help 看用法）" ;;
  esac
done

[ -n "$DUMP" ] || die "缺少 dump 文件参数"
[ -n "$TARGET_DB" ] || die "缺少 --target-db <dbname>"
[ -f "$DUMP" ] || die "dump 文件不存在：$DUMP"
[ -r "$DUMP" ] || die "dump 文件不可读：$DUMP"
case "$TARGET_DB" in *[!a-zA-Z0-9_]*) die "--target-db 只接受字母数字下划线：$TARGET_DB" ;; esac
[ -n "$TARGET_DB" ] || die "--target-db 不能为空"

command -v pg_restore >/dev/null || die "找不到 pg_restore"
command -v psql       >/dev/null || die "找不到 psql"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BACKUP_SH="$SCRIPT_DIR/backup_db.sh"
[ -x "$BACKUP_SH" ] || [ -f "$BACKUP_SH" ] || die "找不到同目录的 backup_db.sh：$BACKUP_SH"

# ---------- 连接参数 ----------
if [ -n "$ENV_FILE_EXPLICIT" ]; then
  [ -r "$ENV_FILE_EXPLICIT" ] || die "env 文件不存在或不可读：$ENV_FILE_EXPLICIT"
  # shellcheck disable=SC1090
  set -a; . "$ENV_FILE_EXPLICIT"; set +a
  log "连接参数来源：$ENV_FILE_EXPLICIT"
elif [ -z "${NIK_DATABASE_NAME:-}" ]; then
  for cand in /etc/cloudpivot/cloudpivot.env \
              /etc/cloudpivot/cloudpivot-test.env \
              /etc/cloudpivot/cloudpivot-dev.env; do
    if [ -r "$cand" ]; then
      # shellcheck disable=SC1090
      set -a; . "$cand"; set +a
      log "连接参数来源：$cand"
      break
    fi
  done
else
  log "连接参数来源：当前 shell"
fi

DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"
DB_USER="${NIK_DATABASE_USER:-postgres}"
CURRENT_DB="${NIK_DATABASE_NAME:-}"

export PGPASSWORD="${NIK_DATABASE_PASSWORD:-}"
export PGCONNECT_TIMEOUT="${PGCONNECT_TIMEOUT:-10}"
PSQL=(psql -X -q -A -t -v ON_ERROR_STOP=1 -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER")
psql_db() { "${PSQL[@]}" -d "$1" -c "$2"; }

# ---------- 受保护库清单 ----------
prod_env_db=""
if [ -r /etc/cloudpivot/cloudpivot.env ]; then
  prod_env_db="$( ( set -a; . /etc/cloudpivot/cloudpivot.env >/dev/null 2>&1; printf '%s' "${NIK_DATABASE_NAME:-}" ) 2>/dev/null || true )"
fi

is_protected() {
  local db="$1" w
  case "$db" in
    template0|template1|postgres) return 0 ;;
    cloudpivot) return 0 ;;
  esac
  [ -n "$CURRENT_DB" ] && [ "$db" = "$CURRENT_DB" ] && return 0
  [ -n "$prod_env_db" ] && [ "$db" = "$prod_env_db" ] && return 0
  for w in ${PROTECTED_DBS:-}; do
    [ "$db" = "$w" ] && return 0
  done
  return 1
}

# ---------- ① 校验 dump ----------
log "校验 dump：$(basename "$DUMP")（$(stat -c %s "$DUMP") 字节）"
toc_tmp="$(mktemp "${TMPDIR:-/tmp}/restore-toc.XXXXXX")"
trap 'rm -f -- "$toc_tmp"' EXIT
if ! pg_restore --list "$DUMP" > "$toc_tmp" 2>"$toc_tmp.err"; then
  err "pg_restore --list 失败：$(tr '\n' ' ' < "$toc_tmp.err")"
  rm -f -- "$toc_tmp.err"
  die "dump 文件已损坏或不是 custom format（-Fc），拒绝恢复"
fi
rm -f -- "$toc_tmp.err"
toc_entries="$(grep -c '^[0-9]\+; ' "$toc_tmp" || true)"
data_entries="$(grep -c 'TABLE DATA' "$toc_tmp" || true)"
dump_db_from_toc="$(sed -n 's/^;[[:space:]]*dbname:[[:space:]]*//p' "$toc_tmp" | head -1)"
toc_server="$(sed -n 's/^;[[:space:]]*Dumped from database version:[[:space:]]*//p' "$toc_tmp" | head -1)"
[ "$toc_entries" -gt 0 ] || die "TOC 为空，dump 不可用：$DUMP"
log "dump 校验通过：TOC ${toc_entries} 条（TABLE DATA ${data_entries} 条），源库 ${dump_db_from_toc:-未知}，源服务端 ${toc_server:-未知}"

# ---------- .meta 基线（同目录同名，backup_db.sh 生成） ----------
META="${DUMP%.dump}.meta"
[ -f "$META" ] || META="$DUMP.meta"
HAS_META=0
META_DB="" META_AT="" META_MIG_COUNT="" META_MIG_MAX="" META_TABLES=""
if [ -f "$META" ]; then
  META_DB="$(sed -n 's/^[[:space:]]*"db": "\(.*\)",$/\1/p' "$META" | head -1)"
  META_AT="$(sed -n 's/^[[:space:]]*"dumped_at": "\(.*\)",$/\1/p' "$META" | head -1)"
  META_MIG_COUNT="$(sed -n 's/^[[:space:]]*"migrations": {"count": \([0-9]*\),.*/\1/p' "$META" | head -1)"
  META_MIG_MAX="$(sed -n 's/^[[:space:]]*"migrations": {"count": [0-9]*, "max_version": \([0-9]*\)}.*/\1/p' "$META" | head -1)"
  META_TABLES="$(sed -n 's/^[[:space:]]*"tables": {"count": \([0-9]*\)}.*/\1/p' "$META" | head -1)"
  if [ -n "$META_MIG_COUNT" ] && [ -n "$META_TABLES" ]; then
    HAS_META=1
    log "读到备份基线：$META（备份于 ${META_AT:-未知}，迁移 ${META_MIG_COUNT} / 版本 ${META_MIG_MAX} / 表 ${META_TABLES}）"
  else
    warn "基线文件格式无法解析，本次只打印实际值、不比对：$META"
  fi
else
  warn "未找到基线文件 $META，恢复后只能打印实际值、无法与 dump 时基线比对"
fi

key_table_names=()
key_table_expect=()
if [ "$HAS_META" -eq 1 ]; then
  while IFS=: read -r k v; do
    [ -n "${k// /}" ] || continue
    v="$(printf '%s' "$v" | tr -d ' ')"
    key_table_names+=("$k")
    key_table_expect+=("$v")
  done < <(awk '/^[[:space:]]*"key_tables": \{/{f=1;next} f&&/^[[:space:]]*\},?[[:space:]]*$/{f=0} f{gsub(/[",]/,"");print}' "$META" | sed 's/^[[:space:]]*//')
fi

# ---------- ② 生产库防护 ----------
target_protected=0
is_protected "$TARGET_DB" && target_protected=1

log "源 dump  : $(basename "$DUMP")（库 ${dump_db_from_toc:-?}，TOC ${toc_entries} 条）"
log "目标库   : $TARGET_DB @ $DB_HOST:$DB_PORT（当前 env 库：${CURRENT_DB:-未设置}，生产 env 库：${prod_env_db:-未找到}）"
log "即将把 $(basename "$DUMP") 恢复到 $TARGET_DB，目标库现有数据会被清除"

if [ "$target_protected" -eq 1 ]; then
  warn "目标库 $TARGET_DB 属于「受保护库」（生产库/当前 env 库/template*）"
  [ "$FORCE_PROD" -eq 1 ] || die "拒绝恢复到受保护库 $TARGET_DB。确属演练/回滚请显式加 --force-prod 并完成库名二次确认"
  confirmed=0
  if [ -n "$CONFIRM_DB_NAME" ]; then
    [ "$CONFIRM_DB_NAME" = "$TARGET_DB" ] || die "--confirm-db-name 与 --target-db 不一致：'$CONFIRM_DB_NAME' != '$TARGET_DB'"
    confirmed=1
    log "库名二次确认通过（--confirm-db-name $CONFIRM_DB_NAME）"
  elif [ -t 0 ]; then
    printf '危险操作：请重新输入目标库名 "%s" 以确认（其他任何输入都会中止）：' "$TARGET_DB" > /dev/tty
    read -r typed < /dev/tty || typed=""
    [ "$typed" = "$TARGET_DB" ] || die "库名确认不匹配（输入 '$typed'），已中止，未做任何改动"
    confirmed=1
    log "库名二次确认通过"
  fi
  [ "$confirmed" -eq 1 ] || die "非交互环境下恢复到受保护库必须同时给 --confirm-db-name $TARGET_DB"
fi

# ---------- ④ 执行确认 ----------
if [ "$ASSUME_YES" -eq 1 ]; then
  log "已给 --yes，跳过交互确认"
else
  if [ -t 0 ]; then
    printf '确认执行恢复？输入 yes 继续，其他任意输入中止：' > /dev/tty
    read -r ans < /dev/tty || ans=""
    [ "$ans" = "yes" ] || die "用户未确认，已中止（未做任何改动）"
  else
    die "非交互环境必须显式加 --yes 才会执行恢复"
  fi
fi

# ---------- 目标库是否存在 ----------
target_exists=0
if psql_db postgres "select 1 from pg_database where datname = '$TARGET_DB'" | grep -q 1; then
  target_exists=1
fi

# ---------- ⑤ 恢复前自动备份当前库 ----------
if [ "$target_exists" -eq 1 ]; then
  log "目标库已存在 → 恢复前自动备份到 $PRE_BACKUP_DIR（防止恢复错了没退路）"
  mkdir -p "$PRE_BACKUP_DIR"
  if ! bash "$BACKUP_SH" --db "$TARGET_DB" --dir "$PRE_BACKUP_DIR" --keep "$PRE_BACKUP_KEEP"; then
    die "恢复前备份失败，中止恢复（不冒没有退路的险）"
  fi
  log "恢复前备份完成"
else
  log "目标库不存在 → 将新建（无需恢复前备份）"
fi

# ---------- 建库（不存在时） ----------
if [ "$target_exists" -eq 0 ]; then
  log "创建目标库 $TARGET_DB（owner=$DB_USER）"
  if ! psql_db postgres "create database \"$TARGET_DB\" owner \"$DB_USER\"" >/dev/null 2>&1; then
    warn "应用账号建库失败（可能无 CREATEDB 权限），回退到系统账号 postgres"
    if command -v runuser >/dev/null; then
      runuser -u postgres -- createdb -O "$DB_USER" "$TARGET_DB" || die "回退建库也失败：$TARGET_DB"
    else
      su - postgres -c "createdb -O '$DB_USER' '$TARGET_DB'" || die "回退建库也失败：$TARGET_DB"
    fi
  fi
  target_exists=1
fi

# ---------- ⑥ 执行恢复 ----------
mkdir -p "$RESTORE_LOG_DIR"
restore_log="$RESTORE_LOG_DIR/restore-$TARGET_DB-$(date '+%Y%m%d-%H%M%S').log"
log "开始恢复：pg_restore --clean --if-exists --no-owner --no-privileges -d $TARGET_DB（日志 $restore_log）"
start_epoch="$(date +%s)"
set +e
pg_restore --clean --if-exists --no-owner --no-privileges \
  -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$TARGET_DB" "$DUMP" > "$restore_log" 2>&1
restore_rc=$?
set -e
restore_elapsed=$(( $(date +%s) - start_epoch ))
error_lines="$(grep -c -E '^pg_restore: error:|^.*ERROR:' "$restore_log" 2>/dev/null || true)"

# known-benign：恢复到「已存在库」时 --clean 阶段对 INHERITS 子表的继承约束做
# DROP CONSTRAINT IF EXISTS，PG 会报 "cannot drop inherited constraint"（父表约束无法
# 单独删除）。数据本身由 TABLE DATA 段整表重建，不影响恢复结果 → 计入 tolerated。
# 判定基准是恢复后的基线比对（表数/迁移/关键表行数），而不是 pg_restore 的退出码。
benign_lines=0
nonbenign_lines=0
if [ "$error_lines" -gt 0 ]; then
  while IFS= read -r l; do
    case "$l" in
      *"cannot drop inherited constraint"*|*"does not exist"*) benign_lines=$((benign_lines + 1)) ;;
      *) nonbenign_lines=$((nonbenign_lines + 1)); err "  $l" ;;
    esac
  done < <(grep -E '^pg_restore: error:|^.*ERROR:' "$restore_log" 2>/dev/null || true)
fi

if [ "$restore_rc" -ne 0 ] && { [ "$nonbenign_lines" -gt 0 ] || [ "$HAS_META" -eq 0 ]; }; then
  err "pg_restore 退出码 $restore_rc：真实错误 ${nonbenign_lines} 行 / 良性 ${benign_lines} 行（日志 $restore_log）"
  tail -20 "$restore_log" | while read -r l; do err "  $l"; done
  [ "$HAS_META" -eq 0 ] && err "且无基线文件可比对，无法证明恢复完整 → 判失败"
  die "恢复失败"
fi
if [ "$restore_rc" -ne 0 ]; then
  warn "pg_restore 退出码 $restore_rc，但错误全部为已知良性（${benign_lines} 行：--clean 无法删除 INHERITS 子表的继承约束）；以基线比对为准"
elif [ "$error_lines" -gt 0 ]; then
  warn "pg_restore 返回 0，日志里 ${error_lines} 行 ERROR 中良性 ${benign_lines} 行"
else
  log "pg_restore 完成，耗时 ${restore_elapsed}s，日志无 ERROR"
fi

# ---------- ⑦⑧ 恢复后核对 + 基线比对 ----------
log "恢复后核对（目标库 $TARGET_DB）…"
actual_tables="$(psql_db "$TARGET_DB" "select count(*) from information_schema.tables where table_schema = 'public' and table_type = 'BASE TABLE'")"
mig_line="$(psql_db "$TARGET_DB" "
  select case when to_regclass('public.schema_migrations') is null then '0|0'
         else (select count(*)::text || '|' ||
                      coalesce(max(case when version ~ '^[0-9]+$' then version::int end), 0)::text
                 from schema_migrations where success)
         end")"
actual_mig_count="${mig_line%%|*}"
actual_mig_max="${mig_line##*|}"

printf '%-34s %-14s %-14s %s\n' "指标" "基线(dump时)" "恢复后" "结果"
mismatches=0
cmp_row() { # $1 指标 $2 期望 $3 实际
  local verdict="OK"
  if [ "$2" != "$3" ]; then verdict="MISMATCH"; mismatches=$((mismatches + 1)); fi
  printf '%-34s %-14s %-14s %s\n' "$1" "$2" "$3" "$verdict"
  if [ "$verdict" = "MISMATCH" ]; then err "不一致：$1 期望 $2，实际 $3"; fi
}

if [ "$HAS_META" -eq 1 ]; then
  cmp_row "tables.count" "$META_TABLES" "$actual_tables"
  cmp_row "migrations.count" "$META_MIG_COUNT" "$actual_mig_count"
  cmp_row "migrations.max_version" "$META_MIG_MAX" "$actual_mig_max"
  i=0
  while [ "$i" -lt "${#key_table_names[@]}" ]; do
    t="${key_table_names[$i]}"
    exp="${key_table_expect[$i]}"
    act="$(psql_db "$TARGET_DB" "select case when to_regclass('public.$t') is null then 'null' else (select count(*)::text from public.$t) end")"
    cmp_row "key_tables.$t" "$exp" "$act"
    i=$((i + 1))
  done
else
  printf '%-34s %-14s %-14s %s\n' "tables.count" "-" "$actual_tables" "INFO"
  printf '%-34s %-14s %-14s %s\n' "migrations.count" "-" "$actual_mig_count" "INFO"
  printf '%-34s %-14s %-14s %s\n' "migrations.max_version" "-" "$actual_mig_max" "INFO"
  warn "无基线可比对：以上为恢复后实际值，请人工确认为预期库"
fi

log "===== 恢复结果 ====="
log "dump      : $DUMP"
log "目标库    : $TARGET_DB"
log "耗时      : ${restore_elapsed}s（pg_restore --clean --if-exists）"
log "日志      : $restore_log"
if [ "$mismatches" -gt 0 ]; then
  log "基线比对  : FAIL（$mismatches 项不一致）—— 请勿切换业务流量，先排查"
  log "===== 结束（退出码 2）====="
  exit 2
fi
log "基线比对  : PASS（表数 / 迁移 / 关键表行数全部一致）"
log "===== 完成 ====="
exit 0
