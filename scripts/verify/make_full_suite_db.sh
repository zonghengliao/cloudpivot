#!/usr/bin/env bash
# ============================================================================
# make_full_suite_db.sh —— 造一份「全量 lib 套件」能跑绿的种子库（可复现、可重跑）
#
# 为什么需要它（卡 t_13ddcb84）：
#   同一个提交，`cargo test --lib -- --test-threads=1` 在两种「看起来都干净」的库上
#   红的是两批完全不同的用例：
#     · 只跑 migrations 的空库  → 缺「演示组织 / 部门 / 角色成员」
#       （`asset_linkage_tests` 的 `库里没有部门`、`expense_linkage_tests` /
#        `sales_approval_bridge` / `receivable_aging` 的「发起审批 65018 没有有效审批人」）
#     · 生产/测试环境的快照库 → 有历史卡 e2e 留下的不平数据
#       （`income_statement::r2_real_*`「基线必须平」、`cash_flow::r3_real_*`「恰好 1 条未分类」）
#   ⇒ 「干净库」这个口径过去没人写清。本脚本把它钉死成**一个可复现的种子集**：
#
#     ① 建空库（owner = 表属主，和迁移同口径）；
#     ② 用**被测树自己编译的二进制**起一次实例：应用 migrations/ 全部迁移 + bootstrap
#        （admin / 角色 / 菜单）；
#     ③ 打开 `NIK_SEED_DEMO_USERS=true` 让 bootstrap 种下**演示组织**：
#        `sys_dept(DEPT_DEMO/演示事业部)` + 9 个演示账号 + 关键角色绑定
#        （SALES_SUPERVISOR / PURCHASE_MANAGER / FINANCE_MANAGER / DEPT_LEADER /
#          WAREHOUSE_MANAGER / GENERAL_MANAGER / SALES_CLERK）；
#        这条种子就在产品代码 `src/system/setup/model.rs::seed_demo_users` 里，
#        **不手写 SQL**、不复制别人的库、跑 1 次和跑 100 次行数一样（按 username 判重）；
#     ④ 自检：关键表非空 + 6 个关键角色都有在职成员 + 种子审批流是 PUBLISHED；
#     ⑤ 打印库名与四条连接串（ERP/SYS/TASK/P0FIX 必须**同一个库**，见下）。
#
# 为什么四条 URL 都指同一个库：
#   · `set_db()` 是进程级 OnceLock（先到先得）。四条 URL 指同一个库时，任何模块先连都
#     连到同一个库 —— `master_data` / `support::job::db_tests` 的
#     「`SELECT current_database()` 必须等于我这条 URL 的库名」断言才成立（不指同库会 SKIP 假绿）。
#   · `flow::internal_control::db_tests`（会签 / 内控那 7 条）要求 `P0FIX_DB_URL` 的库名
#     含 `p0fix` 或 `cloudpivot_test`；本脚本的库名以 `cloudpivot_test` 开头，天然满足，
#     于是**不需要** `--skip business::flow::internal_control::db_tests`。
#   · `tests/testd` 的 `ensure_db` 也硬校验连接串含 `/cloudpivot_test` —— 同一个库名对它同样成立。
#
# 用法：
#   bash scripts/verify/make_full_suite_db.sh                  # 建 cloudpivot_test_full（先删后建）
#   ERP_DB=cloudpivot_test_<卡号> bash scripts/verify/make_full_suite_db.sh
#   REPO=/root/<tree> BIN=/root/<tree>/backend/target/debug/cloudpivot \
#     bash scripts/verify/make_full_suite_db.sh               # 用隔离树自己的二进制
#   KEEP_DB=1 bash scripts/verify/make_full_suite_db.sh       # 不删库，只补种子（幂等）
#   NO_DEMO_SEED=1 bash scripts/verify/make_full_suite_db.sh  # 反例：只跑迁移（对比用）
#
# 跑完它会打印：
#   export ERP_DB_URL=… SYS_DB_URL=… TASK_DB_URL=… P0FIX_DB_URL=…
#   cd backend && cargo test --lib -- --test-threads=1
#
# 退出码：0 = 库已就绪且自检全过；1 = 建库/迁移/自检失败
# ============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=${REPO:-$(git -C "$SCRIPT_DIR/../.." rev-parse --show-toplevel 2>/dev/null)}
# git archive 出来的隔离树没有 .git → 退回脚本自己的物理仓根（别默默指到 /opt/cloudpivot）
REPO=${REPO:-$(cd "$SCRIPT_DIR/../.." && pwd)}
DB=${ERP_DB:-cloudpivot_test_full}
PORT=${PORT:-18242}
PGHOST=${PGHOST:-127.0.0.1}
PGPORT=${PGPORT:-5432}
DB_USER=${DB_USER:-alun}
DB_PASS=${DB_PASS:-alun}
OWNER=${OWNER:-}
BIN=${BIN:-$REPO/backend/target/debug/cloudpivot}
LOG=${LOG:-/root/make_full_suite_db.log}
PIDFILE=${PIDFILE:-/root/make_full_suite_db.pid}
DEMO_SEED=${NO_DEMO_SEED:+0}
DEMO_SEED=${DEMO_SEED:-1}

say()  { printf '%s\n' "$*"; }
die()  { printf '!! %s\n' "$*" >&2; exit 1; }
psql_q() { PGPASSWORD="$DB_PASS" psql -h "$PGHOST" -p "$PGPORT" -U "$DB_USER" -d "$1" -Atc "$2"; }

[ -n "$REPO" ] || die "推导不出仓根（REPO=… 显式指定）"
[ -d "$REPO/backend/migrations" ] || die "迁移目录不存在：$REPO/backend/migrations"
[ -x "$BIN" ] || die "二进制不存在：$BIN（先 cargo build --bin cloudpivot，或用 BIN=… 指定）"

# ---------- 1) owner（role "cloudpivot" 在 99 上不存在，实际 owner 是 alun） ----------
if [ -z "$OWNER" ]; then
  for probe in cloudpivot_test cloudpivot_dev cloudpivot; do
    OWNER=$(psql_q "$probe" "select tableowner from pg_tables where tablename='sys_menu'" 2>/dev/null | head -1)
    [ -n "$OWNER" ] && break
  done
fi
[ -n "$OWNER" ] || die "推不出表 owner，请显式传 OWNER=<库属主>"
say "==> 仓根 $REPO"
say "==> 二进制 $BIN（migrations 编译期内嵌，必须是被测树自己编的）"
say "==> 库 $DB 表属主 $OWNER"

# ---------- 2) 建库（默认先删后建 ⇒ 重跑结果完全一致） ----------
exists=$(psql_q postgres "select 1 from pg_database where datname='$DB'" || true)
if [ "${KEEP_DB:-0}" = "1" ] && [ "$exists" = "1" ]; then
  say "==> KEEP_DB=1：复用已有库 $DB（种子按 username/dept_code 判重，幂等）"
else
  if [ "$exists" = "1" ]; then
    su - postgres -c "dropdb --force $DB" || die "dropdb 失败：$DB"
    say "==> 已删除旧库 $DB"
  fi
  su - postgres -c "createdb -O $OWNER $DB" || die "createdb 失败：$DB"
  say "==> 已建空库 $DB（owner=$OWNER）"
fi

# ---------- 3) 起一次实例：迁移 + bootstrap + 演示组织种子 ----------
export NIK_PROFILE=test
export NIK_DATABASE_NAME="$DB"
export NIK_DATABASE_HOST="$PGHOST"
export NIK_DATABASE_PORT="$PGPORT"
export NIK_DATABASE_USER="$DB_USER"
export NIK_DATABASE_PASSWORD="$DB_PASS"
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export NIK_ADMIN_PASSWORD="${NIK_ADMIN_PASSWORD:-Admin@123456}"
if [ "$DEMO_SEED" = "1" ]; then
  export NIK_SEED_DEMO_USERS=true
  export NIK_DEMO_PASSWORD="${NIK_DEMO_PASSWORD:-ChangeMe@2026}"
else
  export NIK_SEED_DEMO_USERS=false
fi

: > "$LOG"
cd "$REPO/backend" || die "进不了 $REPO/backend（config/config.toml 是相对 CWD 读的）"
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
echo $! > "$PIDFILE"
say "==> 启动 pid=$(cat "$PIDFILE") 端口=$PORT 日志=$LOG（migrations + bootstrap + 演示组织）"

# 轮询：等超管就绪（**不要只等「数据库迁移完成」**：那时 bootstrap 还没跑完）
admin_ready=0
for _ in $(seq 1 300); do
  n=$(psql_q "$DB" "select count(*) from sys_user u join sys_user_role ur on ur.user_id=u.id \
                      join sys_role r on r.id=ur.role_id \
                     where u.username='admin' and r.role_code='SUPER_ADMIN'" 2>/dev/null || echo 0)
  if [ "${n:-0}" = "1" ]; then admin_ready=1; break; fi
  kill -0 "$(cat "$PIDFILE")" 2>/dev/null || break
  sleep 1
done
# 演示账号是 bootstrap 里紧跟着跑的种子；等最后一个（demo_gm）落库再收工
if [ "$DEMO_SEED" = "1" ]; then
  for _ in $(seq 1 120); do
    n=$(psql_q "$DB" "select count(*) from sys_user where username='demo_gm'" 2>/dev/null || echo 0)
    [ "${n:-0}" = "1" ] && break
    sleep 1
  done
fi
sleep 2
kill "$(cat "$PIDFILE")" 2>/dev/null
sleep 2
kill -9 "$(cat "$PIDFILE")" 2>/dev/null
rm -f "$PIDFILE"
say "==> 实例已停（日志尾部）"
tail -3 "$LOG" | sed 's/^/    /'
[ "$admin_ready" = "1" ] || { tail -40 "$LOG" | sed 's/^/    /'; die "bootstrap 没跑完（库里没有 admin+SUPER_ADMIN），见 $LOG"; }

# ---------- 4) 自检（关键表非空 + 关键角色有在职成员 + 种子流程 PUBLISHED） ----------
say "==> 自检"
fail=0
check() { # <说明> <sql> <期望：非空/gt0/eq:值>
  local label="$1" sql="$2" want="${3:-gt0}" got
  got=$(psql_q "$DB" "$sql" 2>/dev/null | head -1)
  local ok=0
  case "$want" in
    nonempty) [ -n "$got" ] && ok=1 ;;
    gt0)      [ "${got:-0}" -gt 0 ] 2>/dev/null && ok=1 ;;
    eq:*)     [ "$got" = "${want#eq:}" ] && ok=1 ;;
  esac
  printf '    %-46s %-12s %s\n' "$label" "${got:-<空>}" "$([ $ok = 1 ] && echo ✓ || echo ✗)"
  [ $ok = 1 ] || fail=1
}
check "schema_migrations(条数)"        "select count(*) from schema_migrations"                                gt0
check "迁移最新版本"                    "select max(version::int) from schema_migrations"                        gt0
check "public 表数"                     "select count(*) from information_schema.tables where table_schema='public'" gt0
check "sys_user(含 admin)"              "select count(*) from sys_user where deleted_at is null"                 gt0
check "sys_role"                        "select count(*) from sys_role where deleted_at is null"                 gt0
check "sys_menu(种子菜单)"              "select count(*) from sys_menu where deleted_at is null"                 gt0
check "biz_account(科目表)"             "select count(*) from biz_account where deleted_at is null"              gt0
check "biz_flow_def(已发布审批流)"      "select count(*) from biz_flow_def where status='PUBLISHED' and deleted_at is null" gt0
check "biz_report_item(报表项)"         "select count(*) from biz_report_item"                                   gt0
if [ "$DEMO_SEED" = "1" ]; then
  check "sys_dept(演示组织/部门)"        "select count(*) from sys_dept where deleted_at is null"                 gt0
  check "演示部门 DEPT_DEMO"             "select dept_code from sys_dept where dept_code='DEPT_DEMO' and deleted_at is null" nonempty
  for role in SALES_SUPERVISOR PURCHASE_MANAGER FINANCE_MANAGER DEPT_LEADER WAREHOUSE_MANAGER GENERAL_MANAGER; do
    check "角色 $role 在职成员数" \
      "select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id and u.deleted_at is null \
         join sys_role r on r.id=ur.role_id where r.role_code='$role' and r.deleted_at is null" gt0
  done
fi

# 不干净的前置：全量套件要求「没有任何历史单据」（否则 cash_flow/income_statement 的
# 「基线必须平」「恰好 1 条未分类」这类断言会被别人的历史数据打红）
check "biz_voucher(应为 0：无历史凭证)"  "select count(*) from biz_voucher"                                     eq:0
check "biz_sales_order(应为 0)"          "select count(*) from biz_sales_order"                                 eq:0
check "biz_purchase_order(应为 0)"       "select count(*) from biz_purchase_order"                              eq:0
check "biz_flow_instance(应为 0)"        "select count(*) from biz_flow_instance"                               eq:0

if [ "$fail" != 0 ]; then
  die "自检未过（上面标 ✗ 的项），库 $DB 不可用于全量套件"
fi

URL="postgres://$DB_USER:$DB_PASS@$PGHOST:$PGPORT/$DB"
cat <<EOF

==> 就绪：$DB

四条连接串（**必须同一个库**，原因见脚本头注释）：
  export ERP_DB_URL='$URL'
  export SYS_DB_URL="\$ERP_DB_URL"
  export TASK_DB_URL="\$ERP_DB_URL"
  export P0FIX_DB_URL="\$ERP_DB_URL"

全量套件（串行；不许 --skip / --ignored）：
  cd $REPO/backend
  cargo test --lib -- --test-threads=1

口径文档：docs/测试库口径-全量套件.md
EOF

# RUN=1：造完库顺手把全量套件跑一遍（约 30 分钟；建议 setsid nohup 甩后台，别用前台）
if [ "${RUN:-0}" = "1" ]; then
  say ""
  say "==> RUN=1：开始跑全量 lib 套件（日志 $LOG.suite）"
  ( cd "$REPO/backend" || exit 1
    ERP_DB_URL="$URL" SYS_DB_URL="$URL" TASK_DB_URL="$URL" P0FIX_DB_URL="$URL" \
    NIK_PROFILE=test NIK_DATABASE_HOST="$PGHOST" NIK_DATABASE_PORT="$PGPORT" \
    NIK_DATABASE_USER="$DB_USER" NIK_DATABASE_PASSWORD="$DB_PASS" NIK_DATABASE_NAME="$DB" \
    cargo test --lib -- --test-threads=1 2>&1 | tee "$LOG.suite" | tail -40 )
  rc=${PIPESTATUS[0]}
  grep -a '^test result:' "$LOG.suite" | tail -1
  [ "$rc" = 0 ] || die "全量套件没跑绿（rc=$rc，日志 $LOG.suite）"
  say "==> 全量套件 0 failed"
fi

