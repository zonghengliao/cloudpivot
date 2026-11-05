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
# 扫目录口径（**必须与引擎一致**）：
#   `config.toml` 的 `[database.migration] path = "migrations"` 是**递归**装载，
#   `backend/migrations/{business,system,support}/` 全在引擎扫描范围内。
#   本工具早期版本写死 `backend/migrations/business`、source 拼死
#   `migrations/business/…`，于是 system/support 的迁移会「缺文件 / 记账 source 错」
#   —— 2026-11-05 发版漏掉 `system/V0235__create_sys_invariant_result` 就是这个原因。
#   现在递归扫全部子目录，`source` 按文件**真实路径**记账。
#
# 记账口径（照 vendor/nik-db/src/migrate/{exec,history}.rs）：
#   kind         = 'versioned'
#   name         = 文件名里 `__` 之后、去掉 `.up.sql`
#   version      = 文件名里的数字原样（**不补零**，库里存的是 '195'）
#   checksum     = sha256(文件内容，行尾统一 \n 后)
#                  ⇒ 对 LF + 单个结尾换行的文件，等价于 `sha256sum`
#   applied_via  = 'imported'（内容确被执行过，但不是引擎自己跑的）
#   applied_seq  = max(applied_seq)+1
#   source       = 文件相对 backend/ 的真实路径，**保留子目录**：
#                  'migrations/business/V0195__xxx.up.sql'
#                  'migrations/system/V0235__create_sys_invariant_result.up.sql'
#
# 用法：
#   set -a; . /etc/cloudpivot/cloudpivot.env; set +a
#   bash scripts/verify/deploya_migrate.sh <db> 195:erp_contract 196:...
#   bash scripts/verify/deploya_migrate.sh <db> --from 195      # 自动取仓内 ≥195 的 up 脚本
#   DEPLOY_ENV_FILE=/path/to/env bash scripts/verify/deploya_migrate.sh …   # 演练/复现时换环境变量文件
# =============================================================================
set -uo pipefail
ENV_FILE="${DEPLOY_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
[ -f "$ENV_FILE" ] || { echo "FAIL: 找不到环境变量文件 $ENV_FILE（可用 DEPLOY_ENV_FILE 指定）" >&2; exit 2; }
set -a; . "$ENV_FILE"; set +a
export PGPASSWORD="${NIK_DATABASE_PASSWORD:?}"
DBUSER="${NIK_DATABASE_USER:-cloudpivot}"
DBHOST="${NIK_DATABASE_HOST:-127.0.0.1}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIGROOT="$ROOT/backend/migrations"   # 引擎口径：递归全部子目录（business/system/support/…）
DB="${1:?用法: deploya_migrate.sh <db> [<version>:<name> ... | --from <n>]}"
shift

Q() { psql -h "$DBHOST" -U "$DBUSER" -d "$DB" -Atq -c "$1"; }

# -----------------------------------------------------------------------------
# 迁移清单：<version>\t<name>\t<绝对路径>，跨子目录一起按版本升序。
# 认不出的文件名 / 重名一律**显式失败**，绝不静默跳过——静默跳过正是本次缺陷的成因。
MIG_ROWS=()
declare -A MIG_PATH=()
declare -A MIG_NAMES_SEEN=()
while IFS=$'\t' read -r _ver _name _path; do
  if [ "$_ver" = "FAIL" ]; then
    echo "FAIL: 无法解析的版本化迁移文件名：$_name" >&2
    echo "      本工具只认 V<数字>__<字母数字下划线>.up.sql；引擎若认别的写法，请先统一命名" >&2
    exit 2
  fi
  _key="$_ver:$_name"
  if [ -n "${MIG_NAMES_SEEN[$_name]:-}" ]; then
    echo "FAIL: 迁移名 $_name 重复（${MIG_NAMES_SEEN[$_name]} 与 $_path）" >&2
    echo "      引擎按 (kind,name) 记账，同名不同目录会互相覆盖，必须先改名" >&2
    exit 2
  fi
  MIG_NAMES_SEEN["$_name"]="$_path"
  MIG_ROWS+=("$_key")
  MIG_PATH["$_key"]="$_path"
done < <(
  for f in "$MIGROOT"/*/V*.up.sql; do
    [ -f "$f" ] || continue
    b="${f##*/}"
    if [[ "$b" =~ ^V([0-9]+)__([A-Za-z0-9_]+)\.up\.sql$ ]]; then
      printf '%d\t%s\t%s\n' "$((10#${BASH_REMATCH[1]}))" "${BASH_REMATCH[2]}" "$f"
    else
      printf 'FAIL\t%s\n' "$f"
    fi
  done | sort -t$'\t' -k1,1n -k3,3
)
[ "${#MIG_ROWS[@]}" -gt 0 ] || { echo "FAIL: $MIGROOT 下没找到任何版本化迁移"; exit 2; }

if [ "${1:-}" = "--from" ]; then
  START="${2:?--from 需要版本号}"
  [[ "$START" =~ ^[0-9]+$ ]] || { echo "FAIL: --from 需要数字版本号（收到 $START）"; exit 2; }
  PAIRS=()
  for k in "${MIG_ROWS[@]}"; do
    [ "${k%%:*}" -ge "$((10#$START))" ] && PAIRS+=("$k")
  done
else
  PAIRS=( "$@" )
fi
[ "${#PAIRS[@]}" -gt 0 ] || { echo "没有待应用的迁移"; exit 0; }

apply_one() { # <version>:<name>
  local key="$1" ver name file rel log t0 t1 ms rc cksum seq after
  ver="${key%%:*}"; name="${key#*:}"
  file="${MIG_PATH[$key]:-}"
  [ -n "$file" ] || {
    echo "FAIL: V$ver:$name 不在迁移清单里（已递归扫 $MIGROOT 下全部子目录）"
    return 1
  }
  [ -f "$file" ] || { echo "FAIL: 缺文件 $file"; return 1; }
  rel="${file#"$ROOT"/backend/}"     # migrations/<子目录>/V....up.sql
  log="/tmp/deploya-apply-V${ver}.log"
  echo "-----------------------------------------------------------------"
  echo "==> [V$(printf '%04d' "$ver")] $rel"
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
    VALUES ('versioned','$name','$ver','$cksum','imported', $(date +%s), $seq, $ms, true, '$rel')
    ON CONFLICT (kind, name) DO UPDATE SET
      version = excluded.version, checksum = excluded.checksum, applied_via = excluded.applied_via,
      applied_at = excluded.applied_at, applied_seq = excluded.applied_seq,
      execution_ms = excluded.execution_ms, success = excluded.success, source = excluded.source;" \
    || { echo "FAIL: 记账失败 V$ver"; return 1; }
  after=$(Q "select count(*) from schema_migrations where kind='versioned' and version='$ver' and success")
  echo "APPLY_OK V$ver checksum=${cksum:0:16}… seq=$seq ms=${ms}ms 记录数=$after source=$rel"
  [ "$after" = "1" ]
}

for key in "${PAIRS[@]}"; do
  apply_one "$key" || { echo "!!! 停止：V${key%%:*} 失败"; exit 1; }
done
echo "===== 本次应用 ${#PAIRS[@]} 条：$(Q "select string_agg(version, ',' order by version::int) from schema_migrations where kind='versioned' and version ~ '^[0-9]+\$' and version::int >= ${PAIRS[0]%%:*}") ====="
