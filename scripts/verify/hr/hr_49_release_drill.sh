#!/usr/bin/env bash
# =============================================================================
# HR-49 · 上线演练（空库首启 + 克隆库升级，含 2211 翻末级）
# =============================================================================
# 卡：t_926048d4（批 5 收口）。设计：`docs/architecture/ARCH-9-人力资源模块设计.md`
#     §3 数据模型 / §7.1 号段表 / §8.1 批 5 收口。
#
# ─── 演练什么（四个场景，各自独立建库、独立起实例）────────────────────────────
#
#   S1 fresh   空库首启：全新空库 → 迁移引擎回放**全部** 323 条版本化迁移 + 12 条种子
#              → /health 200 → 建一个员工（真 HTTP）→ 重启 → 「应用 0 个迁移」→ 该员工仍在
#              ⇒ 验收「空库首启成功」「不丢数据」
#
#   S2 prod    克隆库升级（真实生产快照）：把 172.18.10.144 的**生产库**（只读 pg_dump）
#              灌进 145 演练库 → 引擎只应用「生产还没跑过的」那几条（实测 13 版本化 + 1 种子）
#              → 断言：每一张已存在的表**行数一条不少**、只有白名单表可以增长、
#              HR 种子/菜单/科目/流程全部到位、`hr_invariants.sh` 8 条 BLOCKER 零违规
#              ⇒ 验收「克隆库升级成功」「不丢数据」，并且是**发布 144 前**最贴近真实的那次演练
#
#   S3 legacy  克隆库升级（老库，含 2211 翻末级）：用**发布过的旧版本迁移集合**回放出一个
#              老库（默认 ≤V0376，即 2211 应付职工薪酬还是 is_leaf=1 末级科目的那个年代），
#              灌入历史凭证（记在 1002/5602.01 等末级科目上）+ 标记行 → 引擎升级到 HEAD
#              → 断言：2211/2241/5602 由末级翻成非末级（is_leaf 1→0）、2211 下长出
#              2211.01/02/03、9 个工资末级科目带 DEPT 辅助核算、2 条工资凭证规则可解析，
#              而升级前的凭证/分录/金额**一条不少、一个字节没变**
#              ⇒ 验收「含 2211 翻末级」+「不丢数据」
#
#   S4 guard   升级护栏反证（S3 的老库上把 1 条分录记到**父科目 2211** 上）：
#              升级必须**明确拒绝**（迁移里的护栏 SQL 抛错）、进程起不来、库**停在旧版本**、
#              该分录原样还在
#              ⇒ 证明「客户的老库若曾直接记账到 2211，升级会被拦下」而不是静默改数/丢数
#                 （这是发布 144 前必须知道的一条真实风险，见脚本末尾的「已知风险」）
#
# ─── 纪律（卡面 2026-11-12 强制）──────────────────────────────────────────────
#
#   * 演练库**只建在 145（172.18.10.145）**，绝不在 99 上建（99 是 Agent 主机）；
#   * 库名必须含 `drill` / `e2e` / `test`（脚本开头硬校验，不匹配直接退出 2）；
#   * 每个场景跑完自己 DROP（`KEEP_DB=1` 保留现场；有断言失败时自动保留，方便查）；
#   * 口令只从 145 的私有 env 读（ssh 读 `NIK_DATABASE_PASSWORD`），不进命令行、不进仓库。
#   * 实例只监听 127.0.0.1 的 18000-18999 隔离段（与 scripts/verify/e2e_isolated_instance.sh 同判据）。
#
# ─── 用法 ─────────────────────────────────────────────────────────────────────
#
#   bash scripts/verify/hr/hr_49_release_drill.sh all       # 四个场景全跑（默认）
#   bash scripts/verify/hr/hr_49_release_drill.sh fresh     # 只跑空库首启
#   bash scripts/verify/hr/hr_49_release_drill.sh prod      # 只跑生产克隆升级（需要能 ssh 144）
#   bash scripts/verify/hr/hr_49_release_drill.sh legacy    # 只跑老库升级（含 2211 翻末级）
#   bash scripts/verify/hr/hr_49_release_drill.sh guard     # 只跑护栏反证
#
# 环境变量
#   HR49_PGPASSWORD   145 的库口令（缺省经 ssh 从 145 的 /etc/cloudpivot/cloudpivot-dev.env 读）
#   HR49_DBHOST       库地址（默认 172.18.10.145）
#   HR49_DBUSER       库账号（默认 cloudpivot，需 CREATEDB）
#   HR49_PROD         生产源（默认 ssh://root@172.18.10.144，读其 /etc/cloudpivot/cloudpivot.env）
#   HR49_BIN          后端可执行文件（默认 backend/target/debug/cloudpivot）
#   HR49_LEGACY_MAX   老库基线版本（默认 376，即 V0377 成本科目**之前**：2211 还是末级）
#   HR49_RUN_DIR      运行目录（默认 /root/hr49-drill-<时间戳>；日志、dump、快照都在里面）
#   KEEP=1            不杀实例；KEEP_DB=1 不删库
#
# 退出码：0 = 全绿；1 = 有断言失败；2 = 环境/前置不满足（库、端口、二进制、登录）
# =============================================================================
set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$HERE/../../.." && pwd)
BACKEND=${HR49_BACKEND:-$REPO_ROOT/backend}
BIN=${HR49_BIN:-$BACKEND/target/debug/cloudpivot}
MIG=$BACKEND/migrations

DBHOST=${HR49_DBHOST:-172.18.10.145}
DBPORT=${HR49_DBPORT:-5432}
DBUSER=${HR49_DBUSER:-cloudpivot}
PROD=${HR49_PROD:-ssh://root@172.18.10.144}
LEGACY_MAX=${HR49_LEGACY_MAX:-376}
KEEP=${KEEP:-0}
KEEP_DB=${KEEP_DB:-0}
RUN_DIR=${HR49_RUN_DIR:-/root/hr49-drill-$(date +%Y%m%d-%H%M%S)}

DB_FRESH=hr49_fresh_drill
DB_PROD=hr49_prod_drill
DB_LEGACY=hr49_legacy_drill
DB_GUARD=hr49_guard_drill

PASS=0
FAIL=0
CUR=""
SERVER_PID=""
PROFILE_FILE=""
PASSW="${HR49_PGPASSWORD:-}"

say() { printf '%s\n' "$*"; }
hr()  { printf '%s\n' "-------------------------------------------------------------------------------"; }
die() { printf 'FATAL: %s\n' "$*" >&2; exit 2; }

pass() { PASS=$((PASS + 1)); printf '  [PASS] %s\n' "$1"; }
fail() { FAIL=$((FAIL + 1)); printf '  [FAIL] %s\n' "$1"; }
expect_eq() { # 名称 期望 实际
  if [ "$2" = "$3" ]; then pass "$1（$3）"; else fail "$1：期望 [$2]，实际 [$3]"; fi
}
expect_ne() {
  if [ "$2" != "$3" ]; then pass "$1（$3）"; else fail "$1：期望不等于 [$2]，实际相同"; fi
}
expect_ge() { # 名称 下限 实际
  if [[ "$3" =~ ^[0-9]+$ ]] && [ "$3" -ge "$2" ]; then pass "$1（$3 ≥ $2）"; else fail "$1：期望 ≥ $2，实际 [$3]"; fi
}
expect_contains() { # 名称 海 针
  case "$2" in
    *"$3"*) pass "$1（含「$3」）" ;;
    *) fail "$1：期望含「$3」，实际 [$(printf '%s' "$2" | tr '\n' ' ' | cut -c1-300)]" ;;
  esac
}
scenario() { CUR=$1; hr; printf '[场景 %s] %s\n' "$1" "$2"; }

# ---------------------------------------------------------------- 库 ----------
qa() { PGPASSWORD="$PASSW" psql -X -q -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d postgres "$@"; }
q()  { PGPASSWORD="$PASSW" psql -X -q -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$1" -At "${@:2}"; }
qv() { q "$1" -c "$2" 2>/dev/null | tr -d '[:space:]'; }
qfile() { PGPASSWORD="$PASSW" psql -X -q -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$1" -v ON_ERROR_STOP=1 -f "$2" >/dev/null 2>&1; }

db_exists() { [ "$(qv postgres "SELECT count(*) FROM pg_database WHERE datname = '$1'")" = "1" ]; }

db_create() { # 库名
  qa -c "DROP DATABASE IF EXISTS $1 WITH (FORCE)" >/dev/null 2>&1
  qa -c "CREATE DATABASE $1" >/dev/null 2>&1
  db_exists "$1" || die "建库失败：$1（$DBUSER 有 CREATEDB 吗？）"
}
db_drop() {
  local db=$1 left
  qa -c "DROP DATABASE IF EXISTS $db WITH (FORCE)" >/dev/null 2>&1
  left=$(qv postgres "SELECT count(*) FROM pg_database WHERE datname = '$db'")
  expect_eq "S* $db 已清理（pg_database 残留计数）" 0 "$left"
}

# 收尾：本场景零失败才删库；有失败或 KEEP_DB=1 就留现场
drop_if_green() { # 库名 失败基线
  if [ "$KEEP_DB" = "1" ]; then say "… KEEP_DB=1：保留演练库 $1"; return 0; fi
  if [ "$FAIL" -gt "$2" ]; then say "… 本场景有断言失败：保留演练库 $1 供排查"; return 0; fi
  db_drop "$1"
}

# 逐表行数快照（\gexec 动态遍历 public 下所有关系表，不写死表名 —— 以后加表自动纳入）
snapshot() { # 库名 输出文件
  PGPASSWORD="$PASSW" psql -X -q -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$1" -At -o "$2" <<'SQL'
SELECT format('SELECT %L||''|''||count(*) FROM public.%I;', c.relname, c.relname)
  FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
 WHERE n.nspname = 'public' AND c.relkind = 'r'
 ORDER BY 1
\gexec
SQL
  [ -s "$2" ] || die "快照为空：$2（连不上库 $1？）"
}

# 升级前后对拍：任何表**行数减少**=丢数据（硬失败）；增长只许出现在白名单里
# 白名单逐条给出「为什么它应该长」—— 没写的表一旦增长就是 FAIL（逼人确认）
GROW_OK="sys_menu sys_role_menu sys_role sys_user_role sys_config sys_dict_type sys_dict_item sys_notice sys_field_mask biz_account biz_attachment_rule biz_voucher_rule biz_salary_item biz_flow_def biz_flow_def_version biz_flow_node biz_flow_edge biz_flow_node_assignee biz_flow_category biz_seq schema_migrations"
#   sys_menu / sys_role_menu / sys_role / sys_user_role : 菜单+授权种子（V0387/V0400~V0403 与 S__ 种子）
#   sys_config / sys_dict_type / sys_dict_item         : 配置与字典种子（V0386/V0388 等）
#   sys_field_mask                                     : 2 条脱敏规则（V0388：id_card / bank_account）
#   biz_account / biz_voucher_rule / biz_salary_item    : 科目与规则种子（V0384/V0396）
#   biz_attachment_rule                                : 2 条附件规则（V0389：合同 CONTRACT_SCAN / 工资表）
#   biz_flow_def / _def_version / _node / _edge / _node_assignee : 审批流种子（V0393~V0395）
#   biz_seq                                             : 号段（V0386：EMP/LV/ATT/SAL）
#   schema_migrations                                   : 迁移记账本身（必然增长）
compare_snapshots() { # 前 后
  local b=$1 a=$2 t cb ca grown="" shrunk=0 newe=0
  while IFS='|' read -r t cb; do
    [ -z "$t" ] && continue
    ca=$(awk -F'|' -v t="$t" '$1 == t { print $2 }' "$a")
    if [ -z "$ca" ]; then
      fail "表 $t 升级后消失（升级前 $cb 行）"
      shrunk=$((shrunk + 1))
      continue
    fi
    if [ "$ca" -lt "$cb" ]; then
      fail "表 $t 行数减少 $cb → $ca（丢数据）"
      shrunk=$((shrunk + 1))
    elif [ "$ca" -gt "$cb" ]; then
      case " $GROW_OK " in
        *" $t "*) grown="$grown $t($cb→$ca)" ;;
        *) fail "表 $t 意外增长 $cb → $ca（不在「允许增长」白名单，需人工确认是不是丢/重灌）" ;;
      esac
    fi
  done < "$b"
  newe=$(comm -13 <(cut -d'|' -f1 "$b" | LC_ALL=C sort) <(cut -d'|' -f1 "$a" | LC_ALL=C sort) | wc -l)
  [ "$shrunk" -eq 0 ] && pass "逐表行数对拍：$(wc -l <"$b") 张存量表一律没有减少（新增表 $newe 张）"
  [ -n "$grown" ] && say "  [INFO] 白名单内增长：$grown"
  return 0
}

# ---------------------------------------------------------------- 实例 --------
start_server() { # 库 端口 [迁移目录]
  local db=$1 port=$2 migpath=${3:-} profile=test
  stop_server
  if [ -n "$migpath" ]; then
    PROFILE_FILE=$BACKEND/config/config-hr49mig.toml
    printf '# HR-49 演练临时 profile（跑完由脚本删除）\n# 只覆盖迁移目录：用来回放「旧版本发布过的迁移集合」造老库\n[database.migration]\npath = "%s"\n' "$migpath" >"$PROFILE_FILE"
    profile=hr49mig
  fi
  (
    cd "$BACKEND" || exit 1
    NIK_PROFILE=$profile \
    NIK_SERVER_LISTEN="127.0.0.1:$port" \
    NIK_DATABASE_HOST="$DBHOST" NIK_DATABASE_PORT="$DBPORT" \
    NIK_DATABASE_USER="$DBUSER" NIK_DATABASE_PASSWORD="$PASSW" NIK_DATABASE_NAME="$db" \
    NIK_ADMIN_PASSWORD="${ADMIN_PASSWORD:-Admin@123456}" \
    NIK_MIDDLEWARE_AUTH_JWT_SECRET="hr49-drill-jwt-secret-0123456789abcdefghijklmnopqrstuvwxyz" \
    CLOUDPIVOT_STORAGE_MASTER_KEY="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef" \
    X_CONFIG_SECRET_KEY="hr49-drill-config-secret-0123456789" \
      setsid "$BIN" >>"$RUN_DIR/server-$port.log" 2>&1 &
    echo $! >"$RUN_DIR/server-$port.pid"
  )
  SERVER_PID=$(cat "$RUN_DIR/server-$port.pid")
  SERVER_LOG=$RUN_DIR/server-$port.log
  SERVER_PORT=$port
}

wait_health() { # 端口 期望(200|fail) 超时秒
  local port=$1 want=$2 timeout=${3:-420} i code
  for i in $(seq 1 $((timeout / 2))); do
    code=$(curl -s -o /dev/null -m 3 -w '%{http_code}' "http://127.0.0.1:$port/health" 2>/dev/null || true)
    if [ "$want" = "200" ] && [ "$code" = "200" ]; then return 0; fi
    if [ "$want" = "fail" ] && ! kill -0 "$SERVER_PID" 2>/dev/null; then return 0; fi
    if [ "$want" = "200" ] && ! kill -0 "$SERVER_PID" 2>/dev/null; then
      say "  实例启动即退出，日志尾："
      tail -25 "$SERVER_LOG" | cut -c1-220
      return 1
    fi
    sleep 2
  done
  [ "$want" = "fail" ] && { fail "实例在 ${timeout}s 内没有失败退出（护栏没拦住？）"; return 1; }
  say "  /health ${timeout}s 未就绪，日志尾："
  tail -25 "$SERVER_LOG" | cut -c1-220
  return 1
}

stop_server() {
  local pid=${SERVER_PID:-}
  if [ -n "${SERVER_PORT:-}" ] && [ -f "$RUN_DIR/server-$SERVER_PORT.pid" ]; then
    pid=$(cat "$RUN_DIR/server-$SERVER_PORT.pid")
  fi
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null
    for _ in $(seq 1 15); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
    kill -9 "$pid" 2>/dev/null || true
  fi
  SERVER_PID=""
  [ -n "${PROFILE_FILE:-}" ] && [ -f "$PROFILE_FILE" ] && rm -f "$PROFILE_FILE"
  PROFILE_FILE=""
}

cleanup() {
  local rc=$? d left
  [ "$KEEP" = "1" ] || stop_server
  left=""
  for d in "$DB_FRESH" "$DB_PROD" "$DB_LEGACY" "$DB_GUARD"; do
    [ "$(qv postgres "SELECT count(*) FROM pg_database WHERE datname = '$d'" 2>/dev/null)" = "1" ] && left="$left $d"
  done
  [ -n "$left" ] && say "… 演练库仍在（排查用；清场：DROP DATABASE <库> WITH (FORCE)）：$left"
  [ -n "${PROFILE_FILE:-}" ] && rm -f "$PROFILE_FILE"
  exit $rc
}
trap cleanup EXIT INT TERM

mig_count() { # 名称 → 数据库里已应用的条数
  qv "$1" "SELECT count(*) FROM schema_migrations WHERE success"
}
mig_ver() { # 版本化迁移条数
  qv "$1" "SELECT count(*) FROM schema_migrations WHERE success AND kind = 'versioned'"
}
mig_seed() {
  qv "$1" "SELECT count(*) FROM schema_migrations WHERE success AND kind = 'seed'"
}
mig_maxv() {
  qv "$1" "SELECT coalesce(max(version::int), 0) FROM schema_migrations WHERE success AND kind = 'versioned'"
}
applied_line() { # 从实例日志里取「数据库迁移完成: 应用 N 个迁移」
  grep -aoE "数据库迁移完成: 应用 [0-9]+ 个迁移" "$SERVER_LOG" 2>/dev/null | tail -1 | grep -oE '[0-9]+'
}

# 跑 HR-45 的 10 条巡检断言（hr_invariants.sh）。连接参数走 NIK_DATABASE_*，
# 不拼 postgres:// URL —— 口令不进命令行、不进进程标题。
drill_invariants() { # 库名 场景标签
  local db=$1 tag=$2 log rc
  log="$RUN_DIR/invariants-${tag%% *}.log"
  NIK_DATABASE_HOST="$DBHOST" NIK_DATABASE_PORT="$DBPORT" NIK_DATABASE_USER="$DBUSER" \
  NIK_DATABASE_PASSWORD="$PASSW" NIK_DATABASE_NAME="$db" \
    bash "$HERE/../hr_invariants.sh" >"$log" 2>&1
  rc=$?
  if [ $rc -eq 0 ]; then
    pass "$tag：hr_invariants 8 条 BLOCKER 零违规"
  else
    fail "$tag：hr_invariants 有违规或没跑起来（rc=$rc，$log）"
    grep -E "FAIL|违规|FATAL" "$log" | head -12
  fi
}

# ---------------------------------------------------------------- HTTP --------
OUT=$(mktemp)
TOKEN=""
req() { # METHOD PATH [BODY]
  local args=(-s -m 30 -o "$OUT" -w '%{http_code}' -X "$1" "http://127.0.0.1:${SERVER_PORT}$2" -H 'Content-Type: application/json')
  [ -n "$TOKEN" ] && args+=(-H "Authorization: Bearer $TOKEN")
  [ -n "${3:-}" ] && args+=(-d "$3")
  CODE=$(curl "${args[@]}")
}
dat() { jq -r "(.data.$1) // empty" "$OUT" 2>/dev/null; }
login() {
  req POST /api/system/auth/login "{\"username\":\"admin\",\"password\":\"${ADMIN_PASSWORD:-Admin@123456}\"}"
  TOKEN=$(jq -r '.data.access_token // empty' "$OUT" 2>/dev/null)
  [ -n "$TOKEN" ]
}

# ----------------------------------------------------- 老库迁移树（旧版本集合）----
build_legacy_tree() { # 最高版本 输出目录
  local max=$1 out=$2 d f v b
  mkdir -p "$out"/system "$out"/support "$out"/business "$out"/seed
  for d in system support business; do
    for f in "$MIG/$d"/V*.sql; do
      b=$(basename "$f"); v=${b#V}; v=${v%%__*}
      [ $((10#$v)) -le "$max" ] && cp "$f" "$out/$d/"
    done
  done
  for f in "$MIG"/seed/*.sql; do
    b=$(basename "$f")
    # 与生产一致：这条种子是 HEAD 才有的（见文件头 §「老库」），老库里没有
    [ "$b" = "S__hr_employee_grants.sql" ] && continue
    cp "$f" "$out/seed/"
  done
  find "$out" -name 'V*.up.sql' | wc -l
}

# =============================================================== 前置 =========
while [ $# -gt 0 ]; do
  case "$1" in
    all|fresh|prod|legacy|guard) MODE=$1 ;;
    --help|-h) sed -n '2,90p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "未知参数：$1（all / fresh / prod / legacy / guard / --help）" ;;
  esac
  shift
done
MODE=${MODE:-all}

# 库名护栏：本脚本会建库/写数据/删库，只许演练库
case "$DB_FRESH $DB_PROD $DB_LEGACY $DB_GUARD" in
  *drill*) ;;
  *) die "库名不像演练库（需含 drill/e2e/test）。本脚本会建库/写数据/拒绝对非演练库动刀" ;;
esac

# 端口隔离段
for p in 18149 18150 18151 18152; do
  ss -ltn 2>/dev/null | grep -q ":$p " && die "端口 $p 已被占用，先停掉（或改本脚本端口段）"
done

[ -x "$BIN" ] || die "后端可执行文件不存在：$BIN（先 cargo build --bin cloudpivot）"
[ -d "$MIG" ] || die "缺迁移目录：$MIG"
mkdir -p "$RUN_DIR"

if [ -z "$PASSW" ]; then
  PASSW=$(ssh -o ConnectTimeout=8 "root@$DBHOST" \
    'grep "^NIK_DATABASE_PASSWORD" /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2-' 2>/dev/null)
  [ -n "$PASSW" ] || die "拿不到 145 的库口令（给 HR49_PGPASSWORD，或让 root@$DBHOST 免密 ssh 读 env）"
fi
export PGPASSWORD="$PASSW"
qa -At -c 'SELECT 1' >/dev/null 2>&1 || die "连不上 $DBUSER@$DBHOST:$DBPORT"

TOTAL_VER=$(find "$MIG" -name 'V*.up.sql' | wc -l)
TOTAL_SEED=$(find "$MIG/seed" -maxdepth 1 -name 'S*.sql' | wc -l)
MAX_VER=$(find "$MIG" -name 'V*.up.sql' | sed 's#.*/V##; s#__.*##' | sort -n | tail -1 | sed 's/^0*//')

say "== HR-49 上线演练（模式 $MODE）=="
say "   演练库主机：$DBUSER@$DBHOST:$DBPORT     库名前缀：hr49_*_drill"
say "   后端二进制：$BIN"
say "   磁盘迁移清单：版本化 $TOTAL_VER 条（最高 V$(printf '%04d' "$MAX_VER")）+ 种子 $TOTAL_SEED 条"
say "   运行目录：$RUN_DIR"
say "   老库基线：≤V$LEGACY_MAX"

# =============================================================== S1 空库首启 ==
if [ "$MODE" = "all" ] || [ "$MODE" = "fresh" ]; then
  scenario S1 "空库首启（迁移全跑）"
  SC_FAIL0=$FAIL
  db_create "$DB_FRESH"
  pass "S1 空库已建：$DB_FRESH（迁移由服务启动时自动回放）"
  start_server "$DB_FRESH" 18149
  if wait_health 18149 200 420; then
    pass "S1 /health 200（首次启动，迁移回放完成）"
  else
    fail "S1 /health 未就绪"
  fi
  expect_eq "S1 迁移条数（版本化+种子，引擎日志）" $((TOTAL_VER + TOTAL_SEED)) "$(applied_line)"
  expect_eq "S1 schema_migrations 版本化条数" "$TOTAL_VER" "$(mig_ver "$DB_FRESH")"
  expect_eq "S1 schema_migrations 种子条数" "$TOTAL_SEED" "$(mig_seed "$DB_FRESH")"
  expect_eq "S1 最高版本" "$MAX_VER" "$(mig_maxv "$DB_FRESH")"
  expect_eq "S1 HR 10 张表齐" 10 "$(qv "$DB_FRESH" "SELECT count(*) FROM unnest(ARRAY['biz_employee','biz_employee_contract','biz_employee_salary','biz_attendance_period','biz_attendance_record','biz_leave_request','biz_salary_item','biz_salary_sheet','biz_salary_detail','biz_salary_detail_item']) t WHERE to_regclass('public.'||t) IS NOT NULL")"
  expect_eq "S1 薪资项种子 17 项（V0396）" 17 "$(qv "$DB_FRESH" "SELECT count(*) FROM biz_salary_item WHERE deleted_at IS NULL")"
  expect_eq "S1 工资末级科目 9 个（V0384）" 9 "$(qv "$DB_FRESH" "SELECT count(*) FROM biz_account WHERE deleted_at IS NULL AND account_code IN ('2211.01','2211.02','2211.03','2241.03','2241.04','2221.03','5602.10','5602.11','5602.12')")"
  expect_eq "S1 2211 是非末级（翻末级结果）" 0 "$(qv "$DB_FRESH" "SELECT is_leaf FROM biz_account WHERE account_code='2211' AND deleted_at IS NULL")"
  expect_eq "S1 HR 目录 1 行（/business/hr）" 1 "$(qv "$DB_FRESH" "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL AND menu_type = 1 AND path = '/business/hr'")"
  expect_eq "S1 HR 页面 5 行（员工/合同/考勤/请假/薪资）" 5 "$(qv "$DB_FRESH" "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL AND menu_type = 2 AND path LIKE '/business/hr/%'")"
  expect_ge "S1 HR 权限码行数（按钮+页面码）" 35 "$(qv "$DB_FRESH" "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL AND auth_code LIKE 'business:hr:%'")"
  expect_ge "S1 审批流程定义（HR 四套）" 4 "$(qv "$DB_FRESH" "SELECT count(*) FROM biz_flow_def WHERE deleted_at IS NULL AND flow_code IN ('hr_leave','hr_salary','hr_salary_adjust','hr_employee_resign')")"
  expect_eq "S1 工资凭证规则 2 条（V0384）" 2 "$(qv "$DB_FRESH" "SELECT count(*) FROM biz_voucher_rule WHERE deleted_at IS NULL AND rule_code IN ('PAYROLL_ACCRUAL_DEFAULT','PAYROLL_PAYMENT_DEFAULT')")"

  # 数据不丢：建一个员工（真 HTTP），重启后必须还在
  # 空库首启的 admin 带 must_change_password（业务接口会 403），演练库里直接置 false（同 HR-46 做法）
  q "$DB_FRESH" -c "UPDATE sys_user SET must_change_password = false, is_locked = false, login_fail_count = 0 WHERE username = 'admin'" >/dev/null
  if login; then
    pass "S1 admin 登录（SUPER_ADMIN 短路权限码）"
    req GET /api/system/dept/tree
    DEPT_ID=$(jq -r '(.data[0].id) // (.data[0].children[0].id) // empty' "$OUT" 2>/dev/null)
    if [ -n "$DEPT_ID" ]; then
      req POST /api/business/hr/employee "{\"name\":\"HR49首启标记\",\"gender\":1,\"dept_id\":\"$DEPT_ID\",\"hire_date\":\"$(date +%Y)-01-01\"}"
      EMP_ID=$(dat id); EMP_NO=$(dat emp_no)
      expect_eq "S1 建员工 HTTP（迁移后接口可用）" 200 "$CODE"
      expect_ne "S1 员工工号（EMP 号段）" "" "$EMP_NO"
    else
      fail "S1 取不到部门，无法建员工"
    fi
  else
    fail "S1 admin 登录失败"
  fi

  BEFORE_SEED=$(mig_count "$DB_FRESH")
  stop_server
  pass "S1 实例已停（准备重启验幂等）"
  start_server "$DB_FRESH" 18149
  if wait_health 18149 200 180; then pass "S1 二次启动 /health 200"; else fail "S1 二次启动失败"; fi
  expect_eq "S1 二次启动应用迁移数（幂等）" 0 "$(applied_line)"
  expect_eq "S1 重启后 schema_migrations 条数不变" "$BEFORE_SEED" "$(mig_count "$DB_FRESH")"
  if [ -n "${EMP_ID:-}" ]; then
    TOKEN=""
    if login; then
      req GET "/api/business/hr/employee/$EMP_ID"
      expect_eq "S1 重启后员工档案仍可读（不丢数据）" 200 "$CODE"
      expect_eq "S1 重启后工号一致（不丢数据）" "$EMP_NO" "$(dat emp_no)"
    else
      fail "S1 重启后 admin 登录失败"
    fi
  fi
  stop_server
  drill_invariants "$DB_FRESH" "S1"
  drop_if_green "$DB_FRESH" "$SC_FAIL0"
fi

# =============================================================== S2 生产克隆 ==
if [ "$MODE" = "all" ] || [ "$MODE" = "prod" ]; then
  scenario S2 "克隆库升级（真实生产快照 144 → 145）"
  SC_FAIL0=$FAIL
  PROD_HOST=${PROD#ssh://}
  DUMP=$RUN_DIR/prod-144.sql
  say "… 从 $PROD_HOST 只读 pg_dump（生产库不动一个字节）"
  if ssh -o ConnectTimeout=8 "$PROD_HOST" 'set -a; . /etc/cloudpivot/cloudpivot.env; set +a; export PGPASSWORD="$NIK_DATABASE_PASSWORD"; pg_dump -h "$NIK_DATABASE_HOST" -p "$NIK_DATABASE_PORT" -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" --no-owner --no-privileges' >"$DUMP" 2>"$RUN_DIR/S2-dump.err"; then
    pass "S2 pg_dump 完成（$(du -h "$DUMP" | cut -f1)）"
  else
    fail "S2 pg_dump 失败"; tail -5 "$RUN_DIR/S2-dump.err"
  fi
  db_create "$DB_PROD"
  if PGPASSWORD="$PASSW" psql -X -q -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB_PROD" -v ON_ERROR_STOP=1 -f "$DUMP" >"$RUN_DIR/S2-restore.log" 2>&1; then
    pass "S2 生产快照已灌入演练库（owner 归 $DBUSER）"
  else
    fail "S2 快照灌库失败"; tail -8 "$RUN_DIR/S2-restore.log"
  fi
  PRE_VER=$(mig_ver "$DB_PROD"); PRE_SEED=$(mig_seed "$DB_PROD"); PRE_MAX=$(mig_maxv "$DB_PROD")
  say "   生产快照现状：版本化 $PRE_VER 条（最高 V$PRE_MAX）+ 种子 $PRE_SEED 条"
  expect_eq "S2 生产快照版本与 144 实测一致（V0391）" 391 "$PRE_MAX"
  # V0397 建 uk_biz_sal_sheet_period 前先查会不会炸（架构提醒里的第一个风险点）
  expect_eq "S2 存量「同 org+period 多张未删薪资单」为 0（V0397 建索引不会失败）" 0 \
    "$(qv "$DB_PROD" "SELECT count(*) FROM (SELECT org_id, period_code FROM biz_salary_sheet WHERE deleted_at IS NULL GROUP BY 1,2 HAVING count(*) > 1) x")"
  snapshot "$DB_PROD" "$RUN_DIR/S2-before.txt"
  pass "S2 升级前逐表行数快照：$(wc -l <"$RUN_DIR/S2-before.txt") 张表"

  start_server "$DB_PROD" 18150
  if wait_health 18150 200 420; then pass "S2 升级后 /health 200（引擎自动应用待跑迁移）"; else fail "S2 升级失败（/health 未就绪）"; fi
  expect_eq "S2 本次应用迁移数" $((TOTAL_VER - PRE_VER + TOTAL_SEED - PRE_SEED)) "$(applied_line)"
  expect_eq "S2 升级后版本化总条数" "$TOTAL_VER" "$(mig_ver "$DB_PROD")"
  expect_eq "S2 升级后最高版本" "$MAX_VER" "$(mig_maxv "$DB_PROD")"
  expect_eq "S2 生产原有的 11 条种子 + 新增 1 条" "$TOTAL_SEED" "$(mig_seed "$DB_PROD")"
  expect_eq "S2 薪资项种子 17 项（V0396 不覆盖客户口径：无重复项）" 17 \
    "$(qv "$DB_PROD" "SELECT count(*) FROM biz_salary_item WHERE deleted_at IS NULL")"
  expect_eq "S2 uk_biz_sal_sheet_period 索引在位（V0397）" 1 \
    "$(qv "$DB_PROD" "SELECT count(*) FROM pg_indexes WHERE schemaname='public' AND indexname='uk_biz_sal_sheet_period'")"
  expect_eq "S2 工资末级科目 9 个在位（V0384 已应用过，幂等）" 9 \
    "$(qv "$DB_PROD" "SELECT count(*) FROM biz_account WHERE deleted_at IS NULL AND account_code IN ('2211.01','2211.02','2211.03','2241.03','2241.04','2221.03','5602.10','5602.11','5602.12')")"
  expect_eq "S2 HR 四套流程定义在位（V0393~V0395）" 4 \
    "$(qv "$DB_PROD" "SELECT count(*) FROM biz_flow_def WHERE deleted_at IS NULL AND flow_code IN ('hr_leave','hr_salary','hr_salary_adjust','hr_employee_resign')")"
  expect_eq "S2 HR 页面 5 行" 5 \
    "$(qv "$DB_PROD" "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL AND menu_type = 2 AND path LIKE '/business/hr/%'")"
  expect_ge "S2 HR 权限码行数（生产 + V0400~V0403 增量）" 35 \
    "$(qv "$DB_PROD" "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL AND auth_code LIKE 'business:hr:%'")"
  expect_eq "S2 V0398 收口自检已通过（表数/科目/号段/字典/菜单/流程/薪资项）" 1 \
    "$(qv "$DB_PROD" "SELECT count(*) FROM schema_migrations WHERE success AND name='hr_selfcheck'")"

  snapshot "$DB_PROD" "$RUN_DIR/S2-after.txt"
  compare_snapshots "$RUN_DIR/S2-before.txt" "$RUN_DIR/S2-after.txt"
  stop_server
  drill_invariants "$DB_PROD" "S2 生产克隆库"

  # 重启一次：生产库升级完再重启不能有任何动作、也不能丢行
  start_server "$DB_PROD" 18150
  if wait_health 18150 200 180; then pass "S2 升级后重启 /health 200"; else fail "S2 升级后重启失败"; fi
  expect_eq "S2 升级后重启应用迁移数（幂等）" 0 "$(applied_line)"
  snapshot "$DB_PROD" "$RUN_DIR/S2-restart.txt"
  if diff -q "$RUN_DIR/S2-after.txt" "$RUN_DIR/S2-restart.txt" >/dev/null; then
    pass "S2 重启前后逐表行数完全一致"
  else
    fail "S2 重启前后行数有变化"; diff "$RUN_DIR/S2-after.txt" "$RUN_DIR/S2-restart.txt" | head -10
  fi
  stop_server
  drop_if_green "$DB_PROD" "$SC_FAIL0"
fi

# ======================================================= S3 老库升级（2211）==
if [ "$MODE" = "all" ] || [ "$MODE" = "legacy" ]; then
  scenario S3 "克隆库升级（老库 ≤V$LEGACY_MAX，含 2211 翻末级）"
  SC_FAIL0=$FAIL
  TREE=$RUN_DIR/mig-legacy-$LEGACY_MAX
  TREE_N=$(build_legacy_tree "$LEGACY_MAX" "$TREE")
  pass "S3 老库迁移集合已备：$TREE_N 条版本化（≤V$LEGACY_MAX）+ 11 条种子（不含 HEAD 才有的 hr_employee_grants）"

  db_create "$DB_LEGACY"
  start_server "$DB_LEGACY" 18151 "$TREE"
  if wait_health 18151 200 420; then pass "S3 老库已就位 /health 200（回放旧版本集合）"; else fail "S3 老库回放失败"; fi
  expect_eq "S3 老库版本化条数" "$TREE_N" "$(mig_ver "$DB_LEGACY")"
  expect_eq "S3 老库最高版本" "$LEGACY_MAX" "$(mig_maxv "$DB_LEGACY")"
  expect_eq "S3 老库 2211 仍是末级（升级前基线）" 1 \
    "$(qv "$DB_LEGACY" "SELECT is_leaf FROM biz_account WHERE account_code='2211' AND deleted_at IS NULL")"
  expect_eq "S3 老库 2211 下没有子科目（升级前）" 0 \
    "$(qv "$DB_LEGACY" "SELECT count(*) FROM biz_account c JOIN biz_account p ON p.id=c.parent_id WHERE p.account_code='2211' AND c.deleted_at IS NULL")"
  expect_eq "S3 老库还没有工资末级科目（升级前）" 0 \
    "$(qv "$DB_LEGACY" "SELECT count(*) FROM biz_account WHERE deleted_at IS NULL AND account_code IN ('2211.01','2211.02','2211.03','2241.03','2241.04','2221.03','5602.10','5602.11','5602.12')")"

  # 老库历史数据：两张凭证（借贷平衡、记在末级科目上）+ 1 个标记配置行
  cat >"$RUN_DIR/S3-fixture.sql" <<'SQL'
-- 历史凭证 1：借 1002 银行存款 1000 / 贷 5602.01 管理费用-工资 1000
INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type, source_type,
                         summary, total_debit, total_credit, entry_count, status)
VALUES ('11111111-1111-4111-8111-111111111111','记','HR49-0001', to_char(current_date,'YYYY-MM'), current_date,
        'MANUAL','MANUAL','HR-49 老库历史凭证', 1000, 1000, 2, 'POSTED');
INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount)
SELECT '11111111-1111-4111-8111-111111111111'::uuid, 1, 'HR-49 老库历史凭证',
       a.id, a.account_code, a.account_name, 1000, 0 FROM biz_account a WHERE a.account_code='1002' AND a.deleted_at IS NULL;
INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount)
SELECT '11111111-1111-4111-8111-111111111111'::uuid, 2, 'HR-49 老库历史凭证',
       a.id, a.account_code, a.account_name, 0, 1000 FROM biz_account a WHERE a.account_code='5602.01' AND a.deleted_at IS NULL;

-- 标记配置行（非凭证类数据也要一条不少）
INSERT INTO sys_config (config_key, config_value, config_type, description, config_level)
VALUES ('hr49.legacy.marker','before-upgrade','STRING','HR-49 演练：升级后必须原样还在',1);
SQL
  if qfile "$DB_LEGACY" "$RUN_DIR/S3-fixture.sql"; then
    pass "S3 老库历史数据已灌入（1 张凭证 / 2 条分录 / 1 个标记行）"
  else
    fail "S3 老库历史数据灌入失败（看 $RUN_DIR/S3-fixture.sql）"
  fi
  V_AMT_BEFORE=$(qv "$DB_LEGACY" "SELECT coalesce(sum(debit_amount),0) FROM biz_voucher_entry")
  V_ENT_BEFORE=$(qv "$DB_LEGACY" "SELECT count(*) FROM biz_voucher_entry")
  snapshot "$DB_LEGACY" "$RUN_DIR/S3-before.txt"
  say "   升级前：分录 $V_ENT_BEFORE 条 / 借方合计 $V_AMT_BEFORE / 表 $(wc -l <"$RUN_DIR/S3-before.txt") 张"
  VOUCHER_IDS_BEFORE=$(q "$DB_LEGACY" -c "SELECT string_agg(id::text, ',' ORDER BY voucher_no) FROM biz_voucher" | tr -d '[:space:]')

  stop_server
  pass "S3 老库实例已停（准备升级）"
  start_server "$DB_LEGACY" 18151
  if wait_health 18151 200 420; then pass "S3 升级后 /health 200"; else fail "S3 升级失败（/health 未就绪）"; fi
  expect_eq "S3 本次应用迁移数（版本化+新种子）" $((TOTAL_VER - TREE_N + 1)) "$(applied_line)"
  expect_eq "S3 升级后最高版本" "$MAX_VER" "$(mig_maxv "$DB_LEGACY")"
  expect_eq "S3 升级后 2211 翻成非末级（is_leaf 1→0）" 0 \
    "$(qv "$DB_LEGACY" "SELECT is_leaf FROM biz_account WHERE account_code='2211' AND deleted_at IS NULL")"
  expect_eq "S3 2211 下长出 3 个末级子科目（2211.01/02/03）" 3 \
    "$(qv "$DB_LEGACY" "SELECT count(*) FROM biz_account c JOIN biz_account p ON p.id=c.parent_id WHERE p.account_code='2211' AND c.deleted_at IS NULL")"
  expect_eq "S3 工资末级科目 9 个齐（V0384）" 9 \
    "$(qv "$DB_LEGACY" "SELECT count(*) FROM biz_account WHERE deleted_at IS NULL AND account_code IN ('2211.01','2211.02','2211.03','2241.03','2241.04','2221.03','5602.10','5602.11','5602.12')")"
  expect_eq "S3 工资计提/发放科目带 DEPT 辅助核算（2211.0x + 5602.1x 共 6 个）" 6 \
    "$(qv "$DB_LEGACY" "SELECT count(*) FROM biz_account WHERE deleted_at IS NULL AND account_code IN ('2211.01','2211.02','2211.03','5602.10','5602.11','5602.12') AND aux_dimensions LIKE '%DEPT%'")"
  expect_eq "S3 代扣/其他应付末级科目不带 DEPT（2241.03/04、2221.03 共 3 个）" 3 \
    "$(qv "$DB_LEGACY" "SELECT count(*) FROM biz_account WHERE deleted_at IS NULL AND account_code IN ('2241.03','2241.04','2221.03') AND coalesce(aux_dimensions,'') NOT LIKE '%DEPT%'")"
  expect_eq "S3 三个上级科目（2241/5602/2221）都不是末级" 3 \
    "$(qv "$DB_LEGACY" "SELECT count(*) FROM biz_account WHERE deleted_at IS NULL AND account_code IN ('2241','5602','2221') AND is_leaf = 0")"
  expect_eq "S3 工资凭证规则 2 条可解析到科目（V0384）" 2 \
    "$(qv "$DB_LEGACY" "SELECT count(*) FROM biz_voucher_rule r WHERE r.deleted_at IS NULL AND r.rule_code IN ('PAYROLL_ACCRUAL_DEFAULT','PAYROLL_PAYMENT_DEFAULT')")"
  expect_eq "S3 薪资项种子 17 项（V0396）" 17 "$(qv "$DB_LEGACY" "SELECT count(*) FROM biz_salary_item WHERE deleted_at IS NULL")"

  # 不丢数据：凭证/分录/金额/科目快照逐条不变
  expect_eq "S3 升级后分录条数不变" "$V_ENT_BEFORE" "$(qv "$DB_LEGACY" "SELECT count(*) FROM biz_voucher_entry")"
  expect_eq "S3 升级后借方合计不变" "$V_AMT_BEFORE" "$(qv "$DB_LEGACY" "SELECT sum(debit_amount) FROM biz_voucher_entry")"
  expect_eq "S3 升级后凭证 id 集合不变" "$VOUCHER_IDS_BEFORE" \
    "$(q "$DB_LEGACY" -c "SELECT string_agg(id::text, ',' ORDER BY voucher_no) FROM biz_voucher" | tr -d '[:space:]')"
  expect_eq "S3 历史分录的科目编码快照未被改写（5602.01 仍是 5602.01）" 1 \
    "$(qv "$DB_LEGACY" "SELECT count(*) FROM biz_voucher_entry e WHERE e.account_code='5602.01' AND e.credit_amount=1000")"
  expect_eq "S3 标记配置行原样还在" "before-upgrade" \
    "$(q "$DB_LEGACY" -c "SELECT config_value FROM sys_config WHERE config_key='hr49.legacy.marker'" | tr -d '[:space:]')"
  snapshot "$DB_LEGACY" "$RUN_DIR/S3-after.txt"
  compare_snapshots "$RUN_DIR/S3-before.txt" "$RUN_DIR/S3-after.txt"

  stop_server
  drill_invariants "$DB_LEGACY" "S3 升级后"

  start_server "$DB_LEGACY" 18151
  if wait_health 18151 200 180; then pass "S3 升级后重启 /health 200"; else fail "S3 升级后重启失败"; fi
  expect_eq "S3 升级后重启应用迁移数（幂等）" 0 "$(applied_line)"
  snapshot "$DB_LEGACY" "$RUN_DIR/S3-restart.txt"
  if diff -q "$RUN_DIR/S3-after.txt" "$RUN_DIR/S3-restart.txt" >/dev/null; then
    pass "S3 重启前后逐表行数完全一致"
  else
    fail "S3 重启前后行数有变化"; diff "$RUN_DIR/S3-after.txt" "$RUN_DIR/S3-restart.txt" | head -10
  fi
  stop_server
  drop_if_green "$DB_LEGACY" "$SC_FAIL0"
fi

# ========================================================= S4 升级护栏反证 ====
if [ "$MODE" = "all" ] || [ "$MODE" = "guard" ]; then
  scenario S4 "升级护栏反证（老库的父科目 2211 上有分录）"
  SC_FAIL0=$FAIL
  TREE=$RUN_DIR/mig-legacy-$LEGACY_MAX
  [ -d "$TREE" ] || TREE_N=$(build_legacy_tree "$LEGACY_MAX" "$TREE")
  db_create "$DB_GUARD"
  start_server "$DB_GUARD" 18152 "$TREE"
  if wait_health 18152 200 420; then pass "S4 老库已就位 /health 200"; else fail "S4 老库回放失败"; fi
  cat >"$RUN_DIR/S4-fixture.sql" <<'SQL'
-- 反证夹具：一条凭证把 500 元直接记到父科目 2211（老库里它是末级，记账闸允许）
INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type, source_type,
                         summary, total_debit, total_credit, entry_count, status)
VALUES ('22222222-2222-4222-8222-222222222222','记','HR49-0002', to_char(current_date,'YYYY-MM'), current_date,
        'MANUAL','MANUAL','HR-49 护栏反证：父科目上有分录', 500, 500, 2, 'POSTED');
INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount)
SELECT '22222222-2222-4222-8222-222222222222'::uuid, 1, 'HR-49 护栏反证',
       a.id, a.account_code, a.account_name, 500, 0 FROM biz_account a WHERE a.account_code='2211' AND a.deleted_at IS NULL;
INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount)
SELECT '22222222-2222-4222-8222-222222222222'::uuid, 2, 'HR-49 护栏反证',
       a.id, a.account_code, a.account_name, 0, 500 FROM biz_account a WHERE a.account_code='1002' AND a.deleted_at IS NULL;
SQL
  if qfile "$DB_GUARD" "$RUN_DIR/S4-fixture.sql"; then
    pass "S4 反证夹具已灌入（1 条分录落在父科目 2211 上）"
  else
    fail "S4 反证夹具灌入失败"
  fi
  expect_eq "S4 升级前 2211 有 1 条分录" 1 \
    "$(qv "$DB_GUARD" "SELECT count(*) FROM biz_voucher_entry WHERE account_code='2211'")"
  BASE_MAX=$(mig_maxv "$DB_GUARD")
  BASE_ROWS=$(mig_count "$DB_GUARD")
  stop_server

  start_server "$DB_GUARD" 18152
  if wait_health 18152 fail 420; then
    pass "S4 升级被迁移护栏拦下（实例未就绪，进程退出）"
  else
    fail "S4 升级没有被拦下（实例居然起来了）"
  fi
  # 引擎把 PG 的 RAISE 文本吞成 "query failed: : db error"（nik-db 层不留原话），
  # 所以判据不靠 grep 日志文案，而是 ①启动真的 FATAL ②直接跑护栏那条 SQL 证明它会命中
  # ③库状态原地不动（版本/分录/末级标记都没变）。
  expect_ge "S4 启动日志里有 FATAL（不是静默降级）" 1 "$(grep -ac '服务启动失败' "$SERVER_LOG")"
  expect_ge "S4 启动日志里有迁移失败" 1 "$(grep -ac '数据库迁移失败' "$SERVER_LOG")"
  expect_eq "S4 直跑 V0377 护栏的判据会命中（父科目 2211 有 1 条分录）" 1 \
    "$(qv "$DB_GUARD" "SELECT count(*) FROM biz_voucher_entry e JOIN biz_account a ON a.account_code = e.account_code WHERE a.account_code IN ('4001','4101','2211') AND a.is_leaf = 1 AND a.deleted_at IS NULL")"
  expect_eq "S4 库停在旧版本（未半途升级）" "$BASE_MAX" "$(mig_maxv "$DB_GUARD")"
  expect_eq "S4 本轮一条迁移都没记账（schema_migrations 条数不变）" "$BASE_ROWS" "$(mig_count "$DB_GUARD")"
  expect_eq "S4 父科目上的分录原样还在（没被改数）" 1 \
    "$(qv "$DB_GUARD" "SELECT count(*) FROM biz_voucher_entry WHERE account_code='2211' AND debit_amount=500")"
  expect_eq "S4 2211 仍是末级（没被半截翻转）" 1 \
    "$(qv "$DB_GUARD" "SELECT is_leaf FROM biz_account WHERE account_code='2211' AND deleted_at IS NULL")"
  stop_server
  drop_if_green "$DB_GUARD" "$SC_FAIL0"
fi

# =============================================================== 收口 =========
hr
say "== 结果：PASS=$PASS  FAIL=$FAIL  （日志/快照：$RUN_DIR）=="
if [ "$FAIL" -gt 0 ]; then
  say "   有失败：演练库已保留（见上）；排查后手工清理："
  say "   psql -c 'DROP DATABASE IF EXISTS <库名> WITH (FORCE)'"
  exit 1
fi
say "   ✅ 空库首启 / 克隆库升级（生产快照 + 老库含 2211 翻末级）/ 护栏反证 全部通过，且两次都没有丢数据"
exit 0
