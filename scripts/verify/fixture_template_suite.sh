#!/usr/bin/env bash
# ============================================================================
# fixture_template_suite.sh —— 夹具复用：**建一次库，多套测试共用**（P2-60）
#
# 为什么要它：
#   过去每跑一段 lib 用例都要重新「建库 → 跑 336 个迁移 → 种演示组织」（≈20 s 起，
#   还要手抄 4 条 DB_URL），或者反过来所有人挤在同一个 `cloudpivot_test` 上互相踩夹具。
#   本脚本把它固化成两步：
#     ① **建一次库**：从已经「迁移 + 种子」完备的模板库克隆（`CREATE DATABASE … TEMPLATE`，
#        181 表秒级到位，不重跑迁移、不重种数据）；
#     ② **多套共用**：同一份克隆上依次跑 N 套 lib 用例（默认 erp::voucher / hr / system），
#        每套都串行（`RUST_TEST_THREADS=1`）+ 各自打点，最后汇总。
#   库状态由各套自己的夹具负责回到空库口径（`erp::fixture_hygiene` /
#   `hr::fixture_hygiene`），因此「共用一份库」不等于「互相踩」。
#
# 模板库从哪来（只在模板过期/不存在时做一次，约 20 s）：
#     PGHOST=… DB_USER=… DB_PASS=… bash scripts/verify/make_full_suite_db.sh
#   它建出的库就是本脚本默认的 `TPL=cloudpivot_test_full`。
#
# 用法：
#   bash scripts/verify/fixture_template_suite.sh                      # 默认三套
#   bash scripts/verify/fixture_template_suite.sh business::erp::cost   # 只跑一套
#   bash scripts/verify/fixture_template_suite.sh business::erp::cost business::hr system::
#   KEEP=1 bash scripts/verify/fixture_template_suite.sh   # 保留克隆库（排障）
#
# 环境变量：
#   TPL      模板库名（默认 cloudpivot_test_full）
#   CLONE    本次克隆的库名（默认 cloudpivot_test_p260_shared）
#   PGHOST / PGPORT / PGUSER / PGPASS   连库参数（99 上默认 145；145 上默认 loopback）
#   REPO     仓根（默认按脚本位置推导）
#   JOBS     编译并行度（默认 2，避免和其它 worker 抢 CPU）
#
# 退出码：0 = 每套都绿；1 = 有套红了（打印红套的名字与首条失败）；2 = 前置缺失
# ============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=${REPO:-$(git -C "$SCRIPT_DIR/../.." rev-parse --show-toplevel 2>/dev/null)}
REPO=${REPO:-$(cd "$SCRIPT_DIR/../.." && pwd)}

TPL=${TPL:-cloudpivot_test_full}
CLONE=${CLONE:-cloudpivot_test_p260_shared}
PGPORT=${PGPORT:-5432}
PGUSER=${PGUSER:-cloudpivot}
PGPASS=${PGPASS:-fbb1263691ff75c31ec99eaf258138b4}
export PGPASSWORD="$PGPASS"
JOBS=${JOBS:-2}

# 99 本机 PG 已 purge（且 99 上的 127.0.0.1:5432 是到 145 的转发，卡面**禁止**用它）；
# 145 上必须走 loopback。判据用「本机有没有 172.18.10.145 这个地址」，不看连通性。
if [ -z "${PGHOST:-}" ]; then
  if hostname -I 2>/dev/null | tr ' ' '\n' | grep -qx '172.18.10.145'; then
    PGHOST=127.0.0.1
  else
    PGHOST=172.18.10.145
  fi
fi

SUITES=("$@")
if [ ${#SUITES[@]} -eq 0 ]; then
  SUITES=(business::erp::voucher business::hr system::)
fi

say()  { printf '%s\n' "$*"; }
die()  { printf '!! %s\n' "$*" >&2; exit 2; }
psql_q() { psql -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$1" -Atc "$2" 2>/dev/null; }

say "==> 仓根 $REPO"
say "==> PG   $PGHOST:$PGPORT（user=$PGUSER）"
say "==> 模板库 $TPL → 本次克隆 $CLONE"

# ---------- ① 建一次库：克隆模板（秒级） ----------
psql_q postgres "select 1" >/dev/null || die "连不上 PG（$PGHOST:$PGPORT）"
[ "$(psql_q postgres "select 1 from pg_database where datname='$TPL'")" = "1" ] \
  || die "模板库 $TPL 不存在：先跑一次 PGHOST=$PGHOST DB_USER=$PGUSER DB_PASS=<口令> bash scripts/verify/make_full_suite_db.sh"

t_clone0=$(date +%s%3N)
dropdb -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" --if-exists "$CLONE" || die "删旧克隆库失败"
createdb -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -T "$TPL" "$CLONE" \
  || die "克隆模板库失败（模板库有活动连接？）"
t_clone1=$(date +%s%3N)
tables=$(psql_q "$CLONE" "select count(*) from information_schema.tables where table_schema='public'")
say "==> 克隆完成：${tables} 表，耗时 $((t_clone1 - t_clone0)) ms（迁移 0 次，种子 0 次）"
say ""

URL="postgres://${PGUSER}:${PGPASS}@${PGHOST}:${PGPORT}/${CLONE}"
export ERP_DB_URL="$URL" SYS_DB_URL="$URL" TASK_DB_URL="$URL" P0FIX_DB_URL="$URL"
export NIK_PROFILE=test CARGO_BUILD_JOBS="$JOBS"

cd "$REPO/backend" || die "进不了 $REPO/backend"

# ---------- ② 多套测试共用同一份克隆 ----------
fail=0
failed=()
printf '%-34s %8s %10s %10s  %s\n' "SUITE" "TESTS" "WALL(ms)" "RESULT" "库"
for suite in "${SUITES[@]}"; do
  t0=$(date +%s%3N)
  RUST_TEST_THREADS=1 cargo test --lib "$suite" > "/tmp/fixture_suite_$$.log" 2>&1
  rc=$?
  t1=$(date +%s%3N)
  # `cargo test --lib` 在 workspace 里会对**每个** lib target（app / kernel / doc）各打一行
  # `test result:`，所以条数要跨行求和，不能取最后一行（kernel 那行恒为 0 passed，会显示成 0 条）。
  n=$(grep -a '^test result:' "/tmp/fixture_suite_$$.log" \
      | sed -n 's/.* \([0-9][0-9]*\) passed;.*/\1/p' \
      | awk '{s += $1} END {printf "%d", s}')
  printf '%-34s %8s %10s %10s  %s\n' "$suite" "${n:-?}" "$((t1 - t0))" "$([ $rc = 0 ] && echo OK || echo FAIL)" "$CLONE"
  if [ "$rc" != 0 ]; then
    fail=1
    failed+=("$suite")
    printf '!! %s 红了，首条失败：\n' "$suite" >&2
    grep -a -m1 -A3 '^---- .*stdout ----' "/tmp/fixture_suite_$$.log" | head -6 | sed 's/^/    /' >&2 || \
      tail -20 "/tmp/fixture_suite_$$.log" | sed 's/^/    /' >&2
  fi
done
rm -f "/tmp/fixture_suite_$$.log"

say ""
if [ "$fail" = 0 ]; then
  say "==> 全部绿：一次建库（$((t_clone1 - t_clone0)) ms）→ ${#SUITES[@]} 套共用同一份 $CLONE"
else
  say "==> 有红套：${failed[*]}（克隆库 $CLONE 已保留现场就不删）" >&2
fi

if [ "${KEEP:-0}" = "1" ] || [ "$fail" != 0 ]; then
  say "==> 保留克隆库 $CLONE（KEEP=1 或 有红套）"
else
  dropdb -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" --if-exists "$CLONE"
  say "==> 已删除克隆库 $CLONE"
fi

exit "$fail"
