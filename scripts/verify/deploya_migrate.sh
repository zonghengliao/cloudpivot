#!/usr/bin/env bash
# =============================================================================
# DEPLOY-A 生产迁移落地工具（逐条应用 + 按迁移引擎口径记账）
#
# 为什么不是「跑一次服务让它自己迁移」：
#   卡面要求「6 个迁移一个一个跑，每跑一个立即验证」，而应用的迁移入口只有
#   「启动时一次性应用所有待应用迁移」这一条路（本仓库没有 migrate CLI）。
#   所以这里用 psql 逐条执行 up 脚本（`--single-transaction` = 引擎的
#   execute_script + 独立事务），成功后按 `nik-db` 引擎**完全相同的口径**补
#   `schema_migrations` 记录；随后重启服务时日志应出现「应用 0 个迁移」——
#   那一行就是「记账与引擎一致」的证明（引擎会顺便校验 checksum）。
#
# 记账口径（照 vendor/nik-db/src/migrate/{exec,history}.rs）：
#   kind         = 'versioned'
#   name         = 文件名里 `__` 之后、去掉 `.up.sql`
#   version      = 文件名里的数字原样（**不补零**，库里存的是 '195'）
#   checksum     = sha256(文件内容，行尾统一 \n 后)
#                  ⇒ 对 LF + 单个结尾换行的文件，等价于 `sha256sum`
#   applied_via  = 'imported'（内容确被执行过，但不是引擎自己跑的）
#   applied_seq  = max(applied_seq)+1
#   source       = 'migrations/business/V0195__xxx.up.sql'
#
# 用法：
#   set -a; . /etc/cloudpivot/cloudpivot.env; set +a
#   bash scripts/verify/deploya_migrate.sh <db> 195:erp_contract 196:...
#   bash scripts/verify/deploya_migrate.sh <db> --from 195      # 自动取仓内 ≥195 的 up 脚本
# =============================================================================
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="${NIK_DATABASE_PASSWORD:?}"
DBUSER="${NIK_DATABASE_USER:-cloudpivot}"
DBHOST="${NIK_DATABASE_HOST:-127.0.0.1}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIGDIR="$ROOT/backend/migrations/business"
DB="${1:?用法: deploya_migrate.sh <db> [<version>:<name> ... | --from <n>]}"
shift

Q() { psql -h "$DBHOST" -U "$DBUSER" -d "$DB" -Atq -c "$1"; }

if [ "${1:-}" = "--from" ]; then
  START="${2:?--from 需要版本号}"
  # shellcheck disable=SC2207
  PAIRS=( $(ls "$MIGDIR"/V*.up.sql \
            | sed -E 's#.*/V0*([0-9]+)__([a-z0-9_]+)\.up\.sql#\1:\2#' \
            | awk -F: -v s="$START" '$1>=s' | sort -t: -k1,1n) )
else
  PAIRS=( "$@" )
fi
[ "${#PAIRS[@]}" -gt 0 ] || { echo "没有待应用的迁移"; exit 0; }

apply_one() { # <version> <name>
  local ver="$1" name="$2"
  local file="$MIGDIR/V$(printf '%04d' "$ver")__${name}.up.sql"
  local log="/tmp/deploya-apply-V${ver}.log" t0 t1 ms rc cksum seq after
  [ -f "$file" ] || { echo "FAIL: 缺文件 $file"; return 1; }
  echo "-----------------------------------------------------------------"
  echo "==> [V$(printf '%04d' "$ver")] $(basename "$file")"
  t0=$(date +%s%N)
  psql -h "$DBHOST" -U "$DBUSER" -d "$DB" -v ON_ERROR_STOP=1 --single-transaction -f "$file" > "$log" 2>&1
  rc=$?
  t1=$(date +%s%N); ms=$(( (t1 - t0) / 1000000 ))
  grep -v -E '^$' "$log" | sed 's/^/    /'
  [ "$rc" -eq 0 ] || { echo "APPLY_FAILED V$ver rc=$rc（完整输出 $log）"; return 1; }

  cksum=$(sha256sum "$file" | cut -d' ' -f1)
  seq=$(Q "select coalesce(max(applied_seq),0)+1 from schema_migrations")
  psql -h "$DBHOST" -U "$DBUSER" -d "$DB" -q -v ON_ERROR_STOP=1 -c "
    INSERT INTO schema_migrations
      (kind, name, version, checksum, applied_via, applied_at, applied_seq, execution_ms, success, source)
    VALUES ('versioned','$name','$ver','$cksum','imported', $(date +%s), $seq, $ms, true,
            'migrations/business/V$(printf '%04d' "$ver")__${name}.up.sql')
    ON CONFLICT (kind, name) DO UPDATE SET
      version = excluded.version, checksum = excluded.checksum, applied_via = excluded.applied_via,
      applied_at = excluded.applied_at, applied_seq = excluded.applied_seq,
      execution_ms = excluded.execution_ms, success = excluded.success, source = excluded.source;" \
    || { echo "FAIL: 记账失败 V$ver"; return 1; }
  after=$(Q "select count(*) from schema_migrations where kind='versioned' and version='$ver' and success")
  echo "APPLY_OK V$ver checksum=${cksum:0:16}… seq=$seq ms=${ms}ms 记录数=$after"
  [ "$after" = "1" ]
}

for pair in "${PAIRS[@]}"; do
  apply_one "${pair%%:*}" "${pair#*:}" || { echo "!!! 停止：V${pair%%:*} 失败"; exit 1; }
done
echo "===== 本次应用 ${#PAIRS[@]} 条：$(Q "select string_agg(version, ',' order by version::int) from schema_migrations where kind='versioned' and version::int >= ${PAIRS[0]%%:*}") ====="
