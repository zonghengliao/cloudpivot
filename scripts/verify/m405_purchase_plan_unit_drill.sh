#!/usr/bin/env bash
# ============================================================
# M4-05 采购计划「单测」演练（纯逻辑 + 本模块真库用例）
#
# 卡：t_2bfb7729（M4-05【M4·采购】采购计划：单测 / 含执行率）
#
# 跑什么（每一步都对应卡面 / 新用例里的一句原话）：
#   ① 纯逻辑（不连库）：本卡新增的 state 套件全绿，且**失败数 0**
#   ② 假绿对照：同一批用例**不设** ERP_DB_URL 时，真库用例被跳过（打印的 SKIP），
#      纯逻辑用例仍全绿 —— 证明「跳过」与「通过」在输出里分得开
#   ③ 空库首启：起隔离实例跑完全部迁移（含 V0446~V0449），两个执行率视图落位
#   ④ 真库：ERP_DB_URL 指向同一演练库，整组 purchase_plan 用例 **SKIP=0**（真跑），
#      通过数 ≥ 纯逻辑通过数 + 本模块真库用例数（46）—— 少一个就说明有用例静默跳过
#   ⑤ 两条门禁：`cargo fmt --check` 与 `cargo clippy --all-targets -D warnings`
#
# 用法（99 开发机）：
#   bash scripts/verify/m405_purchase_plan_unit_drill.sh
#   DBHOST=127.0.0.1 bash …        # 145 测试机上跑（走 loopback）
#   环境变量：DBUSER/DBPASS/DBOWNER/DBPORT/PORT/KEEP_DB 可覆盖
#
# 退出码：0 = 全过；1 = 有断言失败；2 = 环境问题（建库 / 编译 / 起不来）
# ============================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DBHOST="${DBHOST:-172.18.10.145}"
DBPORT="${DBPORT:-5432}"
DBUSER="${DBUSER:-cloudpivot}"
DBPASS="${DBPASS:-fbb1263691ff75c31ec99eaf258138b4}"
DBOWNER="${DBOWNER:-cloudpivot}"
DB="${DB:-cp_m405_ut_drill}"
PORT="${PORT:-18105}"
KEEP_DB="${KEEP_DB:-0}"
LOG=/root/m405-drill-server.log
PIDFILE=/root/m405-drill-server.pid
OUTDIR="${OUTDIR:-$ROOT/docs/evidence/2026-11-18-m4-05-purchase-plan-unit}"

# 本卡新套件的用例数（24 条）与本模块四份真库文件的用例数（11+14+11+10 = 46）。
PURE_SUITE_TESTS="${PURE_SUITE_TESTS:-24}"
MODULE_DB_TESTS="${MODULE_DB_TESTS:-46}"

PASS=0
FAIL=0

ok() { PASS=$((PASS + 1)); printf '[PASS] %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf '[FAIL] %s\n' "$1"; }
assert_eq() {
    if [ "$1" = "$2" ]; then ok "$3（$2）"; else bad "$3：期望 [$1] 实际 [$2]"; fi
}
assert_ge() {
    if [ "${1:-0}" -ge "${2:-0}" ] 2>/dev/null; then ok "$3（$1 ≥ $2）"; else bad "$3：期望 ≥ [$2] 实际 [$1]"; fi
}

psql_q() { PGPASSWORD="$DBPASS" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -Atc "$1" 2>&1 | head -1; }

stop_server() {
    local pids=""
    pids=$(ss -ltnp 2>/dev/null | grep ":$PORT " | grep -oE 'pid=[0-9]+' | cut -d= -f2 | tr '\n' ' ')
    if [ -f "$PIDFILE" ]; then pids="$pids $(cat "$PIDFILE")"; fi
    for p in $pids; do
        [ -n "$p" ] && kill "$p" 2>/dev/null
    done
    for _ in $(seq 1 30); do
        ss -ltn 2>/dev/null | grep -q ":$PORT " || break
        sleep 1
    done
    for p in $pids; do
        [ -n "$p" ] && kill -9 "$p" 2>/dev/null
    done
    psql_q "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE datname = '$DB'" >/dev/null 2>&1
    rm -f "$PIDFILE"
}

cleanup_db() {
    stop_server
    if [ "$KEEP_DB" = "1" ]; then
        printf '    演练库保留：%s\n' "$DB"
        return
    fi
    PGPASSWORD="$DBPASS" dropdb -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" --if-exists "$DB" >/dev/null 2>&1
}
trap 'cleanup_db' EXIT

# 「test result: ok. 24 passed; 0 failed; 0 ignored」→ 三个数
result_field() { # $1=输出文件 $2=passed|failed|ignored（多测试目标时取最大值）
    grep -oE "test result: (ok|FAILED)\. [0-9]+ passed; [0-9]+ failed; [0-9]+ ignored" "$1" \
        | awk -v k="$2" '{for (i = 1; i <= NF; i++) if ($(i+1) == k";") {print $i; break}}' \
        | sort -n | tail -1
}

mkdir -p "$OUTDIR"
echo "== 证据目录：$OUTDIR"

# ---------- ① 纯逻辑（不连库） ----------
echo '==> 1. 纯逻辑：本卡 state 套件（不连库）'
unset ERP_DB_URL
(cd "$ROOT/backend" && cargo test --lib business::erp::purchase::purchase_plan_handler::purchase_plan_state_tests -- --test-threads=1) \
    >"$OUTDIR/00_pure_suite.out" 2>&1
RC=$?
tail -5 "$OUTDIR/00_pure_suite.out"
assert_eq "0" "$RC" '纯逻辑套件返回 0'
PURE_PASSED=$(result_field "$OUTDIR/00_pure_suite.out" passed)
PURE_FAILED=$(result_field "$OUTDIR/00_pure_suite.out" failed)
assert_eq "$PURE_FAILED" "0" '纯逻辑套件 0 failed'
assert_ge "$PURE_PASSED" "$PURE_SUITE_TESTS" '纯逻辑套件用例数（新增 24 条都在跑）'

# ---------- ② 假绿对照：不设 ERP_DB_URL 时真库用例被跳过 ----------
echo '==> 2. 假绿对照：整组 purchase_plan 用例**不**连库（真库用例应打印 SKIP）'
(cd "$ROOT/backend" && cargo test --lib business::erp::purchase::purchase_plan -- --test-threads=1 --nocapture) \
    >"$OUTDIR/01_module_without_db.out" 2>&1
RC=$?
NO_DB_PASSED=$(result_field "$OUTDIR/01_module_without_db.out" passed)
NO_DB_FAILED=$(result_field "$OUTDIR/01_module_without_db.out" failed)
NO_DB_SKIPS=$(grep -c 'SKIP: ' "$OUTDIR/01_module_without_db.out" || true)
assert_eq "0" "$RC" '不连库时整组返回 0（跳过的用例不算失败）'
assert_eq "0" "$NO_DB_FAILED" '不连库时 0 failed'
assert_ge "$NO_DB_SKIPS" "1" '不连库时至少打出 1 条 SKIP（探库守卫确实在，不是静默假绿）'
# 没库的时候这 46 条只是「提前 return」，所以 passed 数几乎不变 —— 真正区分真假绿的是
# 第 4 步：连库后这 46 条必须逐条 `... ok`（SKIP=0）。这里先证明它们**被收集到了**。
DB_TEST_LINES=$(grep -cE '^test business::erp::purchase::purchase_plan_(db_tests|handler_db_tests|convert_db_tests|execution_db_tests)::' "$OUTDIR/01_module_without_db.out")
assert_eq "$MODULE_DB_TESTS" "$DB_TEST_LINES" '四份真库用例共 46 条都在被收集（没被过滤器漏掉）'

# ---------- ③ 空库首启（迁移跑到 V0449） ----------
echo '==> 3. 空库首启：建**空**库 → 起隔离实例跑完全部迁移'
export PGPASSWORD="$DBPASS"
stop_server
dropdb -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" --if-exists "$DB" >/dev/null 2>&1
createdb -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -O "$DBOWNER" "$DB" || {
    echo 'FAIL: 建库失败（检查 DBHOST/DBPASS）' >&2; exit 2; }

(cd "$ROOT/backend" && cargo build -q) || { echo 'FAIL: 后端没编译出来' >&2; exit 2; }
export NIK_PROFILE=test
export NIK_DATABASE_NAME="$DB"
export NIK_DATABASE_HOST="$DBHOST"
export NIK_DATABASE_PORT="$DBPORT"
export NIK_DATABASE_USER="$DBUSER"
export NIK_DATABASE_PASSWORD="$DBPASS"
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
rm -f "$LOG"
(cd "$ROOT/backend" && setsid nohup ./target/debug/cloudpivot >"$LOG" 2>&1 </dev/null & echo $! >"$PIDFILE")
for _ in $(seq 1 240); do
    if [ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" || true)" = "200" ]; then
        break
    fi
    kill -0 "$(cat "$PIDFILE")" 2>/dev/null || { echo "FAIL: 实例退出了，看 $LOG" >&2; tail -30 "$LOG" >&2; exit 2; }
    sleep 1
done
[ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health")" = "200" ] || {
    echo 'FAIL: 健康检查超时' >&2; tail -30 "$LOG" >&2; exit 2; }
ok '实例已起（空库首启，/health 200）'

assert_eq "$(psql_q "SELECT count(*) FROM information_schema.views WHERE table_schema='public' AND table_name IN ('v_purchase_plan_item_execution','v_purchase_plan_execution')")" "2" '两个执行率视图在位'
assert_eq "$(psql_q "SELECT count(*) FROM information_schema.tables WHERE table_schema='public' AND table_name IN ('biz_purchase_plan','biz_purchase_plan_item')")" "2" '采购计划两张表在位'
assert_eq "$(psql_q "SELECT count(*) FROM biz_flow_def WHERE flow_code='pp_approve' AND deleted_at IS NULL")" "1" 'pp_approve 流程定义 1 条'
stop_server
ok '实例已停（下面用同一演练库跑真库用例）'

# ---------- ④ 真库：同一批用例 SKIP=0 ----------
echo '==> 4. 真库：ERP_DB_URL 指向演练库，整组 purchase_plan 用例必须 SKIP=0'
export ERP_DB_URL="postgres://$DBUSER:$DBPASS@$DBHOST:$DBPORT/$DB"
(cd "$ROOT/backend" && ERP_DB_URL="$ERP_DB_URL" \
    cargo test --lib business::erp::purchase::purchase_plan -- --test-threads=1 --nocapture) \
    >"$OUTDIR/02_module_with_db.out" 2>&1
RC=$?
tail -5 "$OUTDIR/02_module_with_db.out"
DB_PASSED=$(result_field "$OUTDIR/02_module_with_db.out" passed)
DB_FAILED=$(result_field "$OUTDIR/02_module_with_db.out" failed)
DB_SKIPS=$(grep -c 'SKIP: ' "$OUTDIR/02_module_with_db.out" || true)
assert_eq "0" "$RC" '真库整组返回 0'
assert_eq "0" "$DB_FAILED" '真库整组 0 failed'
assert_eq "0" "$DB_SKIPS" '真库整组 SKIP=0（一个都没跳过）'
DB_RAN=$(grep -cE '^test business::erp::purchase::purchase_plan_(db_tests|handler_db_tests|convert_db_tests|execution_db_tests)::' "$OUTDIR/02_module_with_db.out")
assert_eq "$MODULE_DB_TESTS" "$DB_RAN" '四份真库用例 46 条逐条执行（连库后真跑，不是提前 return）'
assert_ge "$DB_PASSED" "$((PURE_PASSED + MODULE_DB_TESTS))" '真库通过数 ≥ 纯逻辑通过数 + 模块真库用例数'

# ---------- ⑤ 两条门禁 ----------
echo '==> 5. 门禁：fmt --check 与 clippy --all-targets'
(cd "$ROOT/backend" && cargo fmt -p cloudpivot -- --check) >"$OUTDIR/03_fmt.out" 2>&1
FMT_RC=$?
assert_eq "$FMT_RC" "0" 'cargo fmt --check rc=0'
(cd "$ROOT/backend" && cargo clippy --all-targets -- -D warnings) >"$OUTDIR/04_clippy_all_targets.out" 2>&1
CLIPPY_RC=$?
CLIPPY_WARN=$(grep -c '^warning' "$OUTDIR/04_clippy_all_targets.out")
assert_eq "$CLIPPY_RC" "0" 'clippy --all-targets rc=0'
assert_eq "$CLIPPY_WARN" "0" 'clippy 0 条 warning'

echo
echo "================ M4-05 单测演练汇总 ================"
echo "纯逻辑：passed=$PURE_PASSED failed=$PURE_FAILED（不连库）"
echo "不连库：passed=$NO_DB_PASSED failed=$NO_DB_FAILED SKIP=$NO_DB_SKIPS（假绿对照）"
echo "真  库：passed=$DB_PASSED failed=$DB_FAILED SKIP=$DB_SKIPS"
printf '断言：PASS=%s FAIL=%s\n' "$PASS" "$FAIL"
if [ "$FAIL" -ne 0 ]; then
    echo 'RESULT: FAIL'
    exit 1
fi
echo 'RESULT: PASS'
exit 0
