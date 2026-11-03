#!/usr/bin/env bash
# ============================================================================
# run_lib_tests_by_module.sh —— 把真库 lib 单测**按模块分库、分进程**跑并汇总
#
# 为什么必须分库分进程（实测根因，证据见 docs/evidence/20261104-test-hygiene/）：
#
#   ① 同库数据残留：`business::flow::` 段会往库里写流程定义 / 实例 / 待办 / 用户
#      （实测：共享库跑完 flow 段后 biz_flow_def 20 行、biz_flow_instance 15 行、
#        biz_flow_task 22 行、sys_user 39 行；干净库只有 11 行种子定义 + 1 个 admin）。
#      erp 段里有「全库口径」的断言，于是被别的模块的行判死：
#        · purchase_handler::tests::dc01_submit_without_published_flow_is_409
#          —— 它只把 `po_approve` 置 DISABLED，flow 段留下的 7 条 PUBLISHED
#             `p0fix_*` 定义（biz_type 同样是 PO_APPROVE）仍能命中，于是不再 409；
#        · purchase_handler::tests::zz_teardown_purges_po_approval_residue
#          —— 断言「全库不许有 PENDING 的 PO_APPROVE 实例」，flow 段正好留下 3 条；
#        · 更下游的 a4 / cancel 系列：污染数据让更早的用例走进与干净库不同的分支，
#          连进程内的连接状态一起带偏（a4 子集在同一个污染库上**单跑仍 13/13 全绿**，
#          说明它们不是被残留数据直接判死）。
#
#   ② 进程级 `set_db` 先到先得：`set_db()` 是 `OnceLock`（vendor/nik-web/src/resources.rs），
#      一个进程只能装一个全局连接。`business::flow::internal_control` 的 `ensure_db()` 想连
#      `P0FIX_DB_URL`，但它先 `if try_db().is_some() { return true }`——只要同进程里更早的
#      用例已经用 `ERP_DB_URL` 装好了全局连接，它就直接拿别人的库开跑，`P0FIX_DB_URL` 形同虚设。
#      实测：整段 `business::flow::` 跑完，7 条 `p0fix_*` 定义落进了 ERP 库，而 P0FIX 库一行没有；
#      把 `P0FIX_DB_URL` 指到一个**根本不存在的库**，flow 段照样 345/0 全绿——这就是证据。
#
# 结论：真库用例的隔离单位是「模块 × 进程 × 库」，不是「一次 cargo test 跑全部」。
#       本脚本把这条跑法固化下来，并在跑之前/之后做洁净性与泄漏校验。
#
# 用法：
#   bash scripts/verify/run_lib_tests_by_module.sh                # changelog/flow_p0fix/flow/erp，各跑 1 遍
#   RUNS=2 bash scripts/verify/run_lib_tests_by_module.sh         # 幂等性守门：同一库连跑两遍都必须全绿
#   MODULES="erp" RUNS=2 bash scripts/verify/run_lib_tests_by_module.sh
#   MODULES="changelog flow" bash scripts/verify/run_lib_tests_by_module.sh
#   MODULES="my= business::erp::contract_" bash …                # 自定义「名字=cargo test 过滤器」
#   KEEP_DB=1 bash …                                              # 复用已存在的库（默认每次先删后建）
#   TEMPLATE_DB=my_migrated_db bash …                             # 用已有「已迁移到最新版本」的库当模板
#
# 环境变量（都有默认值）：
#   MODULES          默认 "changelog flow_p0fix flow erp"
#   RUNS             默认 1；发版 / 交卡前必须 RUNS=2（同一库连跑两遍）
#   TEMPLATE_DB      默认 cloudpivot_libtest_template（不存在时用后端二进制跑一次启动迁移建出来）
#   DB_PREFIX        默认 cp_libtest，模块库名 = ${DB_PREFIX}_<模块>，另有 <库名>_p0fix
#   REBUILD_TEMPLATE 1 = 丢弃并重建模板库
#   LIBTEST_APP_BIN  后端二进制路径，默认 backend/target/debug/cloudpivot
#   LOG_DIR          每轮原始日志落点，默认 ${TMPDIR:-/tmp}/cloudpivot-libtest-logs
#   CARGO_BIN        默认 cargo
#   DB_HOST/DB_PORT/DB_USER/DB_PASS  默认 127.0.0.1/5432/alun/alun（与 scripts/verify 其它脚本同口径）
#
# 退出码：0 = 所有模块所有轮次全绿；1 = 有失败（含「没有 test result 行」「轮次间计数不一致」）
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BACKEND="$ROOT/backend"

DB_HOST="${DB_HOST:-127.0.0.1}"
DB_PORT="${DB_PORT:-5432}"
DB_USER="${DB_USER:-alun}"
DB_PASS="${DB_PASS:-alun}"

TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_libtest_template}"
DB_PREFIX="${DB_PREFIX:-cp_libtest}"
MODULES="${MODULES:-changelog flow_p0fix flow erp}"
RUNS="${RUNS:-1}"
KEEP_DB="${KEEP_DB:-0}"
REBUILD_TEMPLATE="${REBUILD_TEMPLATE:-0}"

APP_BIN="${LIBTEST_APP_BIN:-${CARGO_TARGET_DIR:-$BACKEND/target}/debug/cloudpivot}"
CARGO_BIN="${CARGO_BIN:-cargo}"
LOG_DIR="${LOG_DIR:-${TMPDIR:-/tmp}/cloudpivot-libtest-logs}"

if [ -t 1 ]; then RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; BOLD=$'\033[1m'; OFF=$'\033[0m'
else RED=""; GREEN=""; YELLOW=""; BOLD=""; OFF=""; fi

say()  { printf '%s\n' "$*"; }
ok()   { printf '%s\n' "${GREEN}$*${OFF}"; }
warn() { printf '%s\n' "${YELLOW}$*${OFF}"; }
bad()  { printf '%s\n' "${RED}$*${OFF}"; }

FAILED_MODULES=()
SUMMARY=()
SEED_USERS=1

# ── psql 小工具 ─────────────────────────────────────────────────────────────
psql_at() { # <db> <sql>
  PGPASSWORD="$DB_PASS" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$1" -Atc "$2"
}

CAN_CREATE=0
dropdb_at() {
  local db="$1"
  if [ "$CAN_CREATE" = 1 ]; then
    PGPASSWORD="$DB_PASS" dropdb -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" --if-exists "$db" >/dev/null 2>&1
  else
    su - postgres -c "dropdb --force --if-exists '$db'" >/dev/null 2>&1
  fi
}
createdb_at() { # <db> [template]
  local db="$1" tmpl="${2:-}"
  if [ "$CAN_CREATE" = 1 ]; then
    if [ -n "$tmpl" ]; then
      PGPASSWORD="$DB_PASS" createdb -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -T "$tmpl" "$db"
    else
      PGPASSWORD="$DB_PASS" createdb -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" "$db"
    fi
  else
    if [ -n "$tmpl" ]; then
      su - postgres -c "createdb -O '$DB_USER' -T '$tmpl' '$db'"
    else
      su - postgres -c "createdb -O '$DB_USER' '$db'"
    fi
  fi
}
db_exists() { [ "$(psql_at postgres "select count(*) from pg_database where datname='$1'")" = 1 ]; }

probe_create_right() {
  local probe="${DB_PREFIX}_probe_$$"
  if PGPASSWORD="$DB_PASS" createdb -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" "$probe" >/dev/null 2>&1; then
    CAN_CREATE=1
    dropdb_at "$probe"
  fi
}

# ── 模板库：一个「已迁移到最新版本 + 种子就绪」的空库 ──────────────────────
ensure_template() {
  if db_exists "$TEMPLATE_DB" && [ "$REBUILD_TEMPLATE" != 1 ]; then
    say "模板库：复用 $TEMPLATE_DB"
    return 0
  fi
  say "模板库：创建 $TEMPLATE_DB（用后端二进制跑一次启动迁移 + 种子）"
  if [ ! -x "$APP_BIN" ]; then
    say "  二进制不存在：$APP_BIN —— 先 cargo build --bin cloudpivot"
    ( cd "$BACKEND" && "$CARGO_BIN" build --bin cloudpivot ) || return 1
  fi
  dropdb_at "$TEMPLATE_DB"
  createdb_at "$TEMPLATE_DB" || return 1
  local port=$(( 18000 + (RANDOM % 900) ))
  local log="${LOG_DIR}/migrate-$TEMPLATE_DB.log"
  local pidfile="${LOG_DIR}/migrate-$TEMPLATE_DB.pid"
  : > "$log"
  (
    cd "$BACKEND" || exit 1   # 二进制要按相对路径找 config/config.toml 与 migrations/
    # 注意：`config/config-test.toml` 是仓库 .gitignore 的本地文件，干净克隆里没有；
    # 所以这里**不强制** NIK_PROFILE（默认走 config.toml 的 [database] enabled = true），
    # 需要特定 profile 时用 LIBTEST_NIK_PROFILE=xxx 显式指定。
    if [ -n "${LIBTEST_NIK_PROFILE:-}" ]; then export NIK_PROFILE="$LIBTEST_NIK_PROFILE"; fi
    export NIK_DATABASE_HOST="$DB_HOST" NIK_DATABASE_PORT="$DB_PORT"
    export NIK_DATABASE_USER="$DB_USER" NIK_DATABASE_PASSWORD="$DB_PASS"
    export NIK_DATABASE_NAME="$TEMPLATE_DB"
    export NIK_SERVER_LISTEN="127.0.0.1:$port"
    export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
    export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
    export NIK_ADMIN_PASSWORD="${NIK_ADMIN_PASSWORD:-Admin@123456}"
    "$APP_BIN" > "$log" 2>&1 &
    echo $! > "$pidfile"
    for _ in $(seq 1 300); do
      grep -qE "已写入管理员账号种子数据|管理员账号已存在" "$log" && exit 0
      kill -0 "$(cat "$pidfile")" 2>/dev/null || exit 1
      sleep 1
    done
    exit 1
  ) >/dev/null 2>&1
  local seeded=$?
  # 收尾：服务进程是子 shell 的后台作业，子 shell 退出后仍活着 —— 按 pidfile 杀掉
  if [ -f "$pidfile" ]; then
    sleep 2
    kill "$(cat "$pidfile")" 2>/dev/null
    sleep 1
    kill -9 "$(cat "$pidfile")" 2>/dev/null
    rm -f "$pidfile"
  fi
  if [ "$seeded" != 0 ]; then
    bad "模板库迁移失败（没有看到种子就绪标记），日志：$log"
    tail -20 "$log" | sed 's/^/    /'
    return 1
  fi
  local tables
  tables=$(psql_at "$TEMPLATE_DB" "select count(*) from information_schema.tables where table_schema='public'")
  say "  模板库就绪：public 表 ${tables:-0} 张，schema_migrations 最大版本 $(psql_at "$TEMPLATE_DB" "select max(version::int) from schema_migrations")"
  [ "${tables:-0}" -gt 50 ] || { bad "模板库表数异常：$tables"; return 1; }
}

# ── 模块 → cargo test 过滤器 ────────────────────────────────────────────────
filter_of() {
  local module="$1"
  case "$module" in
    *=*)  printf '%s' "${module#*=}"; return 0 ;;
  esac
  case "$module" in
    changelog)   printf '%s' 'business::changelog' ;;
    flow)        printf '%s' 'business::flow::' ;;
    flow_p0fix)  printf '%s' 'business::flow::internal_control::db_tests' ;;
    erp)         printf '%s' 'business::erp::' ;;
    *)           printf '%s' '' ;;
  esac
}
module_db_name() { printf '%s' "${1%%=*}"; }

# ── 洁净性前置断言：模块库必须是「刚建出来的空库」 ─────────────────────────
preflight() { # <db> <label>
  local db="$1" label="$2" row
  row=$(psql_at "$db" "select 'instances='||(select count(*) from biz_flow_instance)
                             ||' tasks='||(select count(*) from biz_flow_task)
                             ||' po='||(select count(*) from biz_purchase_order)
                             ||' p0fix_defs='||(select count(*) from biz_flow_def where flow_code like 'p0fix%')
                             ||' users='||(select count(*) from sys_user)") || {
    bad "[$label] 连不上库 $db"; return 1; }
  say "  preflight[$label] $db :: $row"
  local instances tasks po p0fix users dirty=""
  instances=$(sed 's/.*instances=\([0-9]*\).*/\1/' <<<"$row")
  tasks=$(sed 's/.*tasks=\([0-9]*\).*/\1/' <<<"$row")
  po=$(sed 's/.*po=\([0-9]*\).*/\1/' <<<"$row")
  p0fix=$(sed 's/.*p0fix_defs=\([0-9]*\).*/\1/' <<<"$row")
  users=$(sed 's/.*users=\([0-9]*\).*/\1/' <<<"$row")
  [ "$instances" = 0 ] || dirty="$dirty 流程实例=$instances"
  [ "$tasks" = 0 ] || dirty="$dirty 流程待办=$tasks"
  [ "$po" = 0 ] || dirty="$dirty 采购单=$po"
  [ "$p0fix" = 0 ] || dirty="$dirty p0fix夹具定义=$p0fix"
  [ "$users" -le "$SEED_USERS" ] || dirty="$dirty 用户=$users"
  if [ -n "$dirty" ]; then
    bad "[$label] 库不干净（$db）：$dirty"
    bad "        → 这是「复用了一个跑过别的模块的库」，正是本脚本要避免的那件事；"
    bad "          去掉 KEEP_DB=1 重跑（默认每次先删后建）。"
    return 1
  fi
  return 0
}

# ── 单模块单轮 ─────────────────────────────────────────────────────────────
run_once() { # <module> <run#> <db> <p0fix_db> <run_log> <module_log>
  local module="$1" run="$2" db="$3" p0fix="$4" run_log="$5" module_log="$6"
  local filter; filter=$(filter_of "$module")
  {
    say ""
    say "==================================================================="
    say "[$module] 第 $run 轮  filter='$filter'"
    say "  库：$db    P0FIX：$p0fix"
    say "  时间：$(date '+%F %T %Z')  HEAD：$(cd "$ROOT" && git rev-parse --short HEAD 2>/dev/null || echo n/a)"
    say "  命令：ERP_DB_URL=…/$db TASK_DB_URL=…/$db P0FIX_DB_URL=…/$p0fix \\"
    say "        $CARGO_BIN test --lib '$filter' -- --test-threads=1"
    say "==================================================================="
  } | tee -a "$module_log" > "$run_log"

  ( cd "$BACKEND" && \
    NIK_PROFILE=test \
    NIK_DATABASE_HOST="$DB_HOST" NIK_DATABASE_PORT="$DB_PORT" \
    NIK_DATABASE_USER="$DB_USER" NIK_DATABASE_PASSWORD="$DB_PASS" \
    NIK_DATABASE_NAME="$db" \
    ERP_DB_URL="postgres://$DB_USER:$DB_PASS@$DB_HOST:$DB_PORT/$db" \
    TASK_DB_URL="postgres://$DB_USER:$DB_PASS@$DB_HOST:$DB_PORT/$db" \
    P0FIX_DB_URL="postgres://$DB_USER:$DB_PASS@$DB_HOST:$DB_PORT/$p0fix" \
    "$CARGO_BIN" test --lib "$filter" -- --test-threads=1 2>&1 ) \
    | tee -a "$run_log" \
    | grep -aE '^test result:|^failures:$|^error(\[|:)|^running [0-9]+ tests' || true
  local rc=${PIPESTATUS[0]}
  sed -n '/^test result:/,$p' "$run_log" >> "$module_log"

  local result; result=$(grep -a '^test result:' "$run_log" | tail -1)
  if [ -z "$result" ]; then
    bad "[$module] 第 $run 轮：没有 test result 行（编译失败 / 进程崩溃）→ $run_log"
    return 1
  fi
  local passed failed ignored
  passed=$(sed 's/.*ok\. \([0-9]*\) passed.*/\1/; s/.*FAILED\. \([0-9]*\) passed.*/\1/' <<<"$result")
  failed=$(sed 's/.*; \([0-9]*\) failed.*/\1/' <<<"$result")
  ignored=$(sed 's/.*; \([0-9]*\) ignored.*/\1/' <<<"$result")
  say "  → $result  (cargo rc=$rc)"
  # 防假绿：过滤器打错 / 模块改名后会「0 个用例 + 全绿」，那不是通过
  if [ "${passed:-0}" = 0 ]; then
    bad "[$module] 第 $run 轮：过滤器 '$filter' 没命中任何用例（passed=0）→ 判 FAIL（假绿）"
    SUMMARY+=("$module|$run|$db|$passed|$failed|$ignored|99")
    return 1
  fi
  SUMMARY+=("$module|$run|$db|$passed|$failed|$ignored|$rc")

  if [ "$failed" != 0 ] || [ "$rc" != 0 ]; then
    bad "[$module] 第 $run 轮 FAILED：failed=$failed rc=$rc → 失败清单见 $run_log"
    grep -a '^    business::' "$run_log" | sed 's/^/      /' | head -20
    return 1
  fi
  return 0
}

# ── main ───────────────────────────────────────────────────────────────────
mkdir -p "$LOG_DIR"
say "${BOLD}按模块分库跑 lib 真库用例${OFF}  模块：$MODULES  轮次：$RUNS"
say "工作树：$ROOT"
say "库：$DB_USER@$DB_HOST:$DB_PORT  模板：$TEMPLATE_DB  前缀：$DB_PREFIX"
if [ "$RUNS" -lt 2 ]; then
  warn "提示：RUNS=$RUNS（只跑一遍）。幂等性守门要求 RUNS=2 —— 同一个库连跑两遍都必须 0 failed；"
  warn "      只跑一遍时，「上一轮残留把这一轮判死」这类问题看不出来。"
fi

probe_create_right
if [ "$CAN_CREATE" != 1 ]; then
  say "应用账号无 CREATEDB 权限 → 建/删库走 postgres 超级用户（需要 root）"
fi

ensure_template || { bad "模板库不可用，中止"; exit 1; }
SEED_USERS=$(psql_at "$TEMPLATE_DB" "select count(*) from sys_user")
say "模板库种子用户数：$SEED_USERS（preflight 上限）"

MODULE_LOG="$LOG_DIR/run_lib_tests_by_module.log"
: > "$MODULE_LOG"

for module in $MODULES; do
  filter=$(filter_of "$module")
  mname=$(module_db_name "$module")
  if [ -z "$filter" ]; then
    bad "未知模块：$module（可用：changelog flow_p0fix flow erp，或写成 名字=过滤器）"
    FAILED_MODULES+=("$mname")
    continue
  fi
  db="${DB_PREFIX}_${mname}"
  p0fix_db="${db}_p0fix"

  say ""
  say "─── 模块 $mname   filter='$filter' ───"
  if db_exists "$db" && [ "$KEEP_DB" = 1 ]; then
    say "  复用库：$db（KEEP_DB=1）"
  else
    dropdb_at "$db"; dropdb_at "$p0fix_db"
    createdb_at "$db" "$TEMPLATE_DB" || { bad "建库失败：$db"; FAILED_MODULES+=("$mname"); continue; }
    createdb_at "$p0fix_db" "$TEMPLATE_DB" || { bad "建库失败：$p0fix_db"; FAILED_MODULES+=("$mname"); continue; }
    say "  新建：$db 与 $p0fix_db（模板 $TEMPLATE_DB）"
  fi

  pf_out=$(preflight "$db" "$mname" 2>&1); pf_rc=$?
  printf '%s\n' "$pf_out"
  printf '%s\n' "$pf_out" >> "$MODULE_LOG"
  [ "$pf_rc" = 0 ] || { FAILED_MODULES+=("$mname"); continue; }

  module_ok=1
  base_passed=""
  for run in $(seq 1 "$RUNS"); do
    run_log="$LOG_DIR/${mname}_run${run}.txt"
    if run_once "$module" "$run" "$db" "$p0fix_db" "$run_log" "$MODULE_LOG"; then :; else module_ok=0; fi
    cur_passed=$(printf '%s' "${SUMMARY[-1]}" | cut -d'|' -f4)
    if [ -z "$base_passed" ]; then
      base_passed="$cur_passed"
    elif [ "$cur_passed" != "$base_passed" ]; then
      bad "[$mname] 第 $run 轮用例数从 $base_passed 变成 $cur_passed —— 两次跑的不是同一批用例（有 SKIP 漂移）"
      module_ok=0
    fi
  done

  # 泄漏校验：flow_p0fix 必须真连 P0FIX 库（守「set_db 先到先得」这个坑）
  if [ "$mname" = flow_p0fix ]; then
    got_p0fix=$(psql_at "$p0fix_db" "select count(*) from biz_flow_def where flow_code like 'p0fix%'")
    got_main=$(psql_at "$db" "select count(*) from biz_flow_def where flow_code like 'p0fix%'")
    say "  泄漏校验：P0FIX 库 p0fix 夹具=$got_p0fix，模块库=$got_main"
    if [ "${got_p0fix:-0}" -lt 1 ] || [ "${got_main:-0}" -ne 0 ]; then
      bad "  P0FIX_DB_URL 没生效：内控夹具写进了模块库（=$got_main），P0FIX 库只有 ${got_p0fix:-0} 行。"
      bad "  成因：同进程里更早的用例已经 set_db()，internal_control::ensure_db 直接复用了那个连接。"
      module_ok=0
    fi
  fi

  if [ "$module_ok" = 1 ]; then ok "  ✔ $mname 全绿"; else bad "  ✘ $mname 有失败"; FAILED_MODULES+=("$mname"); fi
done

say ""
say "${BOLD}================ 汇总 ================${OFF}"
printf '%-14s %-4s %-34s %8s %8s %8s %6s\n' 模块 轮 库 passed failed ignored 结果
for row in "${SUMMARY[@]}"; do
  IFS='|' read -r m r mdb passed failed ignored rc <<<"$row"
  status="${GREEN}OK${OFF}"
  { [ "$failed" = 0 ] && [ "$rc" = 0 ]; } || status="${RED}FAIL${OFF}"
  printf '%-14s %-4s %-34s %8s %8s %8s %b\n' "$m" "$r" "$mdb" "$passed" "$failed" "$ignored" "$status"
done
say "日志目录：$LOG_DIR"

if [ "${#FAILED_MODULES[@]}" -gt 0 ]; then
  bad "FAILED 模块：${FAILED_MODULES[*]}"
  exit 1
fi
ok "全部模块 × ${RUNS} 轮全绿（分库 + 分进程跑法）"
exit 0
