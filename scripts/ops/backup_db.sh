#!/usr/bin/env bash
# 云枢数据库备份（pg_dump -Fc + 完整性校验 + 基线 .meta + 保留策略）
#
# 用法：
#   bash scripts/ops/backup_db.sh [--keep N] [--dir PATH] [--db NAME]
#                                 [--env-file PATH] [--min-free-bytes N] [--dry-run]
#
# 参数：
#   --keep N           保留最近 N 份（默认 14，最小 1），超出的删最旧
#   --dir PATH         备份目录（默认 /root/db-backup，权限 700）
#   --db NAME          覆盖 $NIK_DATABASE_NAME（用于备份非当前 env 的库，如恢复前的临时库）
#   --env-file PATH    显式指定环境变量文件；缺省按 /etc/cloudpivot/cloudpivot.env →
#                      cloudpivot-test.env → cloudpivot-dev.env 顺序取第一个存在的
#   --min-free-bytes N 覆盖磁盘余量下限（默认 = 2 × 预估 dump 大小）；仅用于受限环境/演练
#   --dry-run          只做连通性 + 容量预检，不落盘
#
# 行为：
#   ① 读 NIK_DATABASE_*（env 或 env-file）→ ② pg_dump -Fc → ③ 输出
#   <dir>/<db>-<YYYYmmdd-HHMMSS>.dump → ④ pg_restore --list 校验归档可读
#   → ⑤ 写同名 .meta（迁移数/最大版本/表数/关键表行数/大小，供恢复后比对）
#   → ⑥ 按 --keep 清理旧备份 → ⑦ 打印文件名/大小/耗时/校验结果/保留份数
#
# 安全：
#   · 口令只走 PGPASSWORD 环境变量，不出现在命令行（ps 不可见）与日志
#   · 落盘前检查磁盘余量（默认要求 ≥ 2 × 预估 dump 大小）
#   · 用 flock 串行化，避免 cron 与手工并发互相覆盖
#   · 只删除本脚本命名规则的 <db>-<ts>.dump(+.meta)，绝不动目录里别的手工备份
#   · dump 先写 .tmp 再 rename，半截文件不会被当成有效备份
#
# 退出码：0 成功 / 1 失败（供 cron 判断）
set -euo pipefail

PROG="$(basename "$0")"
KEEP=14
BACKUP_DIR="/root/db-backup"
DB_OVERRIDE=""
ENV_FILE_EXPLICIT=""
MIN_FREE_BYTES=""
DRY_RUN=0

# 关键表：恢复后逐表比对行数（只需在 .meta 里存在，库里缺表即判不一致）
KEY_TABLES=(sys_user sys_role sys_menu biz_purchase_order biz_voucher biz_inventory)

log()  { printf '[%s] INFO  %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%:z')" "$*"; }
warn() { printf '[%s] WARN  %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%:z')" "$*"; }
err()  { printf '[%s] ERROR %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%:z')" "$*" >&2; }

usage() {
  sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

die() { err "$*"; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --keep)           KEEP="${2:?--keep 需要参数}"; shift 2 ;;
    --keep=*)         KEEP="${1#*=}"; shift ;;
    --dir)            BACKUP_DIR="${2:?--dir 需要参数}"; shift 2 ;;
    --dir=*)          BACKUP_DIR="${1#*=}"; shift ;;
    --db)             DB_OVERRIDE="${2:?--db 需要参数}"; shift 2 ;;
    --db=*)           DB_OVERRIDE="${1#*=}"; shift ;;
    --env-file)       ENV_FILE_EXPLICIT="${2:?--env-file 需要参数}"; shift 2 ;;
    --env-file=*)     ENV_FILE_EXPLICIT="${1#*=}"; shift ;;
    --min-free-bytes) MIN_FREE_BYTES="${2:?--min-free-bytes 需要参数}"; shift 2 ;;
    --min-free-bytes=*) MIN_FREE_BYTES="${1#*=}"; shift ;;
    --dry-run)        DRY_RUN=1; shift ;;
    -h|--help)        usage 0 ;;
    *)                die "未知参数：$1（--help 看用法）" ;;
  esac
done

case "$KEEP" in
  ''|*[!0-9]*) die "--keep 必须是正整数，收到：$KEEP" ;;
esac
[ "$KEEP" -ge 1 ] || die "--keep 至少为 1（收到：$KEEP）"

# ---------- ① 解析数据库连接 ----------
if [ -n "$ENV_FILE_EXPLICIT" ]; then
  [ -r "$ENV_FILE_EXPLICIT" ] || die "env 文件不存在或不可读：$ENV_FILE_EXPLICIT"
  # shellcheck disable=SC1090
  set -a; . "$ENV_FILE_EXPLICIT"; set +a
  log "环境变量来源：$ENV_FILE_EXPLICIT"
elif [ -z "${NIK_DATABASE_NAME:-}" ]; then
  for cand in /etc/cloudpivot/cloudpivot.env \
              /etc/cloudpivot/cloudpivot-test.env \
              /etc/cloudpivot/cloudpivot-dev.env; do
    if [ -r "$cand" ]; then
      # shellcheck disable=SC1090
      set -a; . "$cand"; set +a
      log "环境变量来源：$cand"
      break
    fi
  done
else
  log "环境变量来源：当前 shell（NIK_DATABASE_NAME=$NIK_DATABASE_NAME）"
fi

DB_NAME="${DB_OVERRIDE:-${NIK_DATABASE_NAME:-}}"
DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"
DB_USER="${NIK_DATABASE_USER:-postgres}"

[ -n "$DB_NAME" ] || die "缺少数据库名：设置 NIK_DATABASE_NAME 或传 --db"
[ -n "${NIK_DATABASE_PASSWORD:-}" ] || warn "NIK_DATABASE_PASSWORD 为空，将依赖 .pgpass / trust 认证"

# 口令只经环境变量传给客户端，不落命令行、不进日志
export PGPASSWORD="${NIK_DATABASE_PASSWORD:-}"
export PGCONNECT_TIMEOUT="${PGCONNECT_TIMEOUT:-10}"

PSQL=(psql -X -q -A -t -v ON_ERROR_STOP=1 -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER")

psql_db() { "${PSQL[@]}" -d "$1" -c "$2"; }

command -v pg_dump    >/dev/null || die "找不到 pg_dump"
command -v pg_restore >/dev/null || die "找不到 pg_restore"
command -v psql       >/dev/null || die "找不到 psql"
command -v flock      >/dev/null || die "找不到 flock（util-linux），无法串行化备份"

# ---------- 预检：连通性 / 版本 / 库大小 ----------
[ "$DRY_RUN" -eq 1 ] || mkdir -p "$BACKUP_DIR"
[ "$DRY_RUN" -eq 1 ] || chmod 700 "$BACKUP_DIR" 2>/dev/null || true

server_version_num="$(psql_db "$DB_NAME" 'show server_version_num' 2>/dev/null || true)"
if [ -z "$server_version_num" ]; then
  die "无法连接数据库 $DB_USER@$DB_HOST:$DB_PORT/$DB_NAME（检查 env / 口令 / pg_hba）"
fi
server_major=$((server_version_num / 10000))
dump_major="$(pg_dump --version | sed -n 's/.* \([0-9]\+\)\(\.[0-9]\+\)*.*/\1/p')"
[ -n "$dump_major" ] || die "无法解析 pg_dump 版本"
if [ "$dump_major" -lt "$server_major" ]; then
  die "pg_dump 主版本 $dump_major < 服务端 $server_major，拒绝备份（会导出不完整）"
fi

db_size_bytes="$(psql_db "$DB_NAME" 'select pg_database_size(current_database())')"
table_count="$(psql_db "$DB_NAME" "select count(*) from information_schema.tables where table_schema = 'public' and table_type = 'BASE TABLE'")"
log "目标库 $DB_NAME@$DB_HOST:$DB_PORT（服务端 $server_version_num / pg_dump $dump_major，${db_size_bytes} 字节，${table_count} 张表）"

# ---------- 磁盘余量检查（至少 2 × 预估 dump 大小） ----------
expected_bytes=0
last_dump="$(ls -1t "$BACKUP_DIR/$DB_NAME"-*.dump 2>/dev/null | head -1 || true)"
if [ -n "$last_dump" ] && [ -f "$last_dump" ]; then
  expected_bytes="$(stat -c %s "$last_dump" 2>/dev/null || echo 0)"
  [ "$expected_bytes" -gt 0 ] || expected_bytes=$((db_size_bytes / 5))
else
  # 无历史备份时按库大小的 20% 保守估算（本库实测 custom format 约 7%~10%）
  expected_bytes=$((db_size_bytes / 5))
fi
[ "$expected_bytes" -gt 0 ] || expected_bytes=1048576
need_bytes=$((expected_bytes * 2))
if [ -n "$MIN_FREE_BYTES" ]; then
  case "$MIN_FREE_BYTES" in ''|*[!0-9]*) die "--min-free-bytes 必须是整数" ;; esac
  warn "--min-free-bytes 覆盖生效：磁盘余量下限 ${MIN_FREE_BYTES} 字节（默认 ${need_bytes}）"
  need_bytes="$MIN_FREE_BYTES"
fi
if [ "$DRY_RUN" -eq 1 ]; then
  probe_dir="$BACKUP_DIR"
  [ -d "$probe_dir" ] || probe_dir="$(dirname "$BACKUP_DIR")"
else
  probe_dir="$BACKUP_DIR"
fi
avail_bytes="$(df -B1 --output=avail "$probe_dir" | tail -1 | tr -d ' ')"
log "磁盘余量检查：$probe_dir 可用 ${avail_bytes} 字节 / 需要 ${need_bytes} 字节（2 × 预估 dump ${expected_bytes}）"
if [ "$avail_bytes" -lt "$need_bytes" ]; then
  die "磁盘余量不足：可用 ${avail_bytes} < 需要 ${need_bytes} 字节（预估 dump ${expected_bytes} 的 2 倍）。请清理 $BACKUP_DIR 或换 --dir"
fi
log "磁盘余量检查：通过"

if [ "$DRY_RUN" -eq 1 ]; then
  log "--dry-run 结束：连通性与容量均通过，未生成备份"
  exit 0
fi

# ---------- 串行化 ----------
exec 9>"$BACKUP_DIR/.backup-$DB_NAME.lock"
if ! flock -w 600 9; then
  die "另一个备份进程持有 $BACKUP_DIR/.backup-$DB_NAME.lock 超过 600s，放弃本次备份"
fi

ts="$(date '+%Y%m%d-%H%M%S')"
dump_name="$DB_NAME-$ts.dump"
dump_path="$BACKUP_DIR/$dump_name"
meta_path="${dump_path%.dump}.meta"
tmp_path="$dump_path.tmp"

dump_err="$BACKUP_DIR/.dump-$DB_NAME.err"
cleanup_tmp() { rm -f -- "$tmp_path" "$dump_err" 2>/dev/null || true; }
trap cleanup_tmp EXIT

log "开始备份：pg_dump -Fc -d $DB_NAME → $dump_path"
start_epoch="$(date +%s)"
if ! pg_dump -Fc -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -f "$tmp_path" 2>"$dump_err"; then
  while IFS= read -r l; do err "pg_dump: $l"; done < "$dump_err"
  if grep -qi 'permission denied' "$dump_err"; then
    warn "权限不足：库 $DB_NAME 里有不属于 $DB_USER 的表（历史 datafix 备份表/人工建表）。"
    warn "修复（二选一，需 DBA）：ALTER TABLE <表> OWNER TO $DB_USER; 或 GRANT SELECT ON <表> TO $DB_USER;"
    warn "自查：select relname, pg_get_userbyid(relowner) from pg_class where relnamespace='public'::regnamespace and relkind='r' and pg_get_userbyid(relowner) <> '$DB_USER';"
  fi
  die "pg_dump 失败（库 $DB_NAME），未生成备份"
fi
elapsed=$(( $(date +%s) - start_epoch ))

[ -s "$tmp_path" ] || die "pg_dump 输出为空：$tmp_path"
mv -f -- "$tmp_path" "$dump_path"
dump_bytes="$(stat -c %s "$dump_path")"
log "dump 生成：$dump_name（${dump_bytes} 字节，耗时 ${elapsed}s）"

# ---------- ④ 完整性校验 ----------
toc_path="$dump_path.toc"
if ! pg_restore --list "$dump_path" > "$toc_path" 2>"$dump_path.toc.err"; then
  err "pg_restore --list 失败：$(tr '\n' ' ' < "$dump_path.toc.err" 2>/dev/null || true)"
  rm -f -- "$dump_path" "$dump_path.toc.err"
  die "备份完整性校验失败，已删除损坏的 dump：$dump_name"
fi
toc_entries="$(grep -c '^[0-9]\+; ' "$toc_path" || true)"
data_entries="$(grep -c 'TABLE DATA' "$toc_path" || true)"
rm -f -- "$toc_path" "$dump_path.toc.err"
log "完整性校验：pg_restore --list 可读，TOC 条目 ${toc_entries} 个（其中 TABLE DATA ${data_entries} 个）"
if [ "$toc_entries" -le 0 ]; then
  rm -f -- "$dump_path"
  die "TOC 为空，dump 不可用，已删除：$dump_name"
fi
if [ "$table_count" -gt 0 ] && [ "$data_entries" -le 0 ]; then
  rm -f -- "$dump_path"
  die "库里有 ${table_count} 张表但 dump 无任何 TABLE DATA 段，拒绝留用：$dump_name"
fi

# ---------- ⑤ 基线 .meta（供恢复后比对） ----------
migrations_line="$(
  psql_db "$DB_NAME" "
    select case when to_regclass('public.schema_migrations') is null then '0|0'
           else (select count(*)::text || '|' ||
                        coalesce(max(case when version ~ '^[0-9]+$' then version::int end), 0)::text
                   from schema_migrations where success)
           end"
)"
mig_count="${migrations_line%%|*}"
mig_max="${migrations_line##*|}"

key_lines=""
first_key=1
for t in "${KEY_TABLES[@]}"; do
  cnt="$(psql_db "$DB_NAME" "select case when to_regclass('public.$t') is null then 'null' else (select count(*)::text from public.$t) end")"
  [ -n "$cnt" ] || cnt=null
  if [ "$first_key" -eq 1 ]; then key_lines="    \"$t\": $cnt"; first_key=0
  else key_lines="$key_lines,
    \"$t\": $cnt"; fi
done

dumped_at="$(date '+%Y-%m-%dT%H:%M:%S%:z')"
cat > "$meta_path" <<META
{
  "db": "$DB_NAME",
  "dumped_at": "$dumped_at",
  "dump_file": "$dump_name",
  "host": "$DB_HOST",
  "port": $DB_PORT,
  "server_version_num": $server_version_num,
  "migrations": {"count": $mig_count, "max_version": $mig_max},
  "tables": {"count": $table_count},
  "key_tables": {
$key_lines
  },
  "size_bytes": $dump_bytes,
  "db_size_bytes": $db_size_bytes,
  "verify": {"toc_entries": $toc_entries, "table_data_entries": $data_entries},
  "elapsed_seconds": $elapsed
}
META
chmod 600 "$meta_path"
log "基线 .meta 已写入：$(basename "$meta_path")（迁移 ${mig_count} 条 / 最大版本 ${mig_max} / 表 ${table_count} 张）"

# ---------- ⑥ 保留策略：只清本命名规则的文件，删最旧 ----------
keep_count=0
removed=0
# shellcheck disable=SC2012
mapfile -t all_dumps < <(ls -1t "$BACKUP_DIR/$DB_NAME"-*.dump 2>/dev/null || true)
for f in "${all_dumps[@]}"; do
  [ -n "$f" ] || continue
  keep_count=$((keep_count + 1))
  if [ "$keep_count" -gt "$KEEP" ]; then
    rm -f -- "$f" "${f%.dump}.meta"
    removed=$((removed + 1))
    log "保留策略：删除最旧备份 $(basename "$f")（--keep $KEEP）"
  fi
done
keep_count=$((keep_count - removed))

total_bytes=0
for f in "${all_dumps[@]:0:$keep_count}"; do
  [ -f "$f" ] && total_bytes=$((total_bytes + $(stat -c %s "$f")))
done

# ---------- ⑦ 汇总 ----------
log "===== 备份结果 ====="
log "文件      : $dump_path"
log "大小      : ${dump_bytes} 字节"
log "耗时      : ${elapsed}s"
log "校验      : PASS（pg_restore --list 可读，TOC ${toc_entries} 条）"
log "保留份数  : ${keep_count} / ${KEEP}（目录占用 ${total_bytes} 字节）"
log "本次清理  : ${removed} 份最旧备份"
log "===== 完成 ====="
exit 0
