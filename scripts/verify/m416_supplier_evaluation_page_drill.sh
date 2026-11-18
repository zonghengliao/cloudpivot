#!/usr/bin/env bash
# ============================================================================
# M4-16 供应商评估「前端页面 = 评估录入 + 排名」· 演练
#
# 卡：t_005d6b65（M4-16【M4·采购】供应商评估：前端页面）
#
# 这一卡交三样东西，脚本逐样验：
#   ① 页面：`frontend/src/views/business/erp/supplier-evaluation/{index.vue,logic.ts}` +
#      `api/business/erp/supplier-evaluation.ts` + `router/index.ts` 路由登记
#   ② 页面需要的**最小接口面**（5 条路由 / 2 个接口码 / 1 个页面显形码 / 3 行菜单，V0457）
#      —— M4-12/13 只交数据层（零路由），页面离开接口就是空壳；先例 M4-11b
#   ③ 真库用例：`supplier_evaluation_handler_db_tests`（打分入口覆盖语义 / 台账分页筛选 /
#      排名排序）—— 本卡新写的两段 SQL（page_evaluations / evaluations_in_period）只能真库验
#
# 用法：
#   bash scripts/verify/m416_supplier_evaluation_page_drill.sh               # 99 → 145（默认）
#   PG_HOST=127.0.0.1 bash scripts/verify/m416_supplier_evaluation_page_drill.sh   # 在 145 上跑
#   KEEP_DB=1 bash ...                                                       # 留库排查
#   SKIP_GATES=1 bash ...                                                    # 只验业务，不跑门禁
#   M416_BAD_URL=1 bash ...                                                  # SKIP 守卫自检（期望报红）
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名含 drill；脚本自己 DROP；
# 连接串口令复用 `$PGPASSWORD`（不写占位符 —— 那样真库用例连不上还打 SKIP 记 ok，是假绿）。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIG_DIR="$ROOT/backend/app/migrations/business"
BACKEND="$ROOT/backend"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="${M416_DB:-cp_m416_sev_page_drill_$$}"
KEEP_DB="${KEEP_DB:-0}"
SKIP_GATES="${SKIP_GATES:-0}"
TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
mkdir -p "$TMP"
LOG="$TMP/m416_drill_$$.log"
TEST_LOG="$TMP/m416_tests_$$.log"
PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }

case "$DB" in
    *drill*|*e2e*|*test*) : ;;
    *) printf '库名 %s 不含 drill/e2e/test，拒绝在它上面跑（rc=2）\n' "$DB"; exit 2 ;;
esac

OWN_DB=1
if [ -n "${M416_DB:-}" ]; then OWN_DB=0; fi

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }
psql_file()  { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$1" >"$LOG" 2>&1; }

assert_eq() { # 期望 实际 说明
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 [$1]，实际 [$2]）"; fi
}

cleanup() {
    if [ "$OWN_DB" = "0" ] || [ "$KEEP_DB" = "1" ]; then
        printf '\n  （库保留 → %s；不删）\n' "$DB"
        return
    fi
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
    rm -f "$LOG" "$TEST_LOG"
}
trap cleanup EXIT

printf '== M4-16 供应商评估页面演练 · PG %s:%s / 库 %s ==\n' "$PG_HOST" "$PG_PORT" "$DB"

# ---------- 0) 模板与克隆 ----------
echo "== 0) 前置：模板库可克隆"
if ! psql_admin "SELECT 1 FROM pg_database WHERE datname = '$TEMPLATE_DB'" | grep -q 1; then
    bad "模板库 $TEMPLATE_DB 不存在（请先在两台机器的 PG 里准备模板库）"
    exit 2
fi
ok "模板库 $TEMPLATE_DB 存在"
if [ "$OWN_DB" = "1" ]; then
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
    if psql_admin "CREATE DATABASE $DB TEMPLATE $TEMPLATE_DB" >/dev/null 2>&1; then
        ok "从模板克隆演练库 $DB"
    else
        bad "克隆演练库失败"
        exit 2
    fi
else
    ok "使用调用方给的演练库 $DB（只跑不清）"
fi

# ---------- 1) 推到仓库当前版本 ----------
echo "== 1) 应用业务迁移 V0405…（含本卡的 V0457）"
BASE_VERSION="$(psql_q "SELECT COALESCE(max(version::int),0) FROM schema_migrations WHERE kind='versioned'")"
if ! printf '%s' "$BASE_VERSION" | grep -qE '^[0-9]+$'; then
    bad "读不出模板库的迁移版本：$BASE_VERSION"
    exit 2
fi
info "模板库版本 = V$BASE_VERSION"
MIG_FAIL=0
APPLIED=0
for f in $(ls "$MIG_DIR" | grep -E '^V[0-9]{4}__.*\.up\.sql$' | sort); do
    num="${f:1:4}"
    if [ "$((10#$num))" -le "$BASE_VERSION" ]; then continue; fi
    APPLIED=$((APPLIED+1))
    if ! psql_file "$MIG_DIR/$f"; then
        MIG_FAIL=$((MIG_FAIL+1))
        bad "迁移失败：$f（tail: $(tail -2 "$LOG" | tr '\n' ' ')）"
    fi
done
if [ "$MIG_FAIL" -eq 0 ]; then
    ok "$APPLIED 条迁移全部应用成功（演练库 = 仓库当前版本）"
fi

# ---------- 2) 幂等 / 可回滚 ----------
echo "== 2) V0457 幂等（重跑 up）与可回滚（down → up）"
if psql_file "$MIG_DIR/V0457__supplier_evaluation_page.up.sql"; then
    ok "V0457 up 重跑一次不报错（ON CONFLICT DO UPDATE 幂等）"
else
    bad "V0457 up 重跑失败：$(tail -2 "$LOG" | tr '\n' ' ')"
fi
assert_eq "3" "$(psql_q "SELECT count(*) FROM sys_menu WHERE id IN ('20000000-0000-0000-0000-000000004495'::uuid,'20000000-0000-0000-0000-000000004496'::uuid,'20000000-0000-0000-0000-000000004497'::uuid) AND deleted_at IS NULL")" \
    "重跑后仍是 3 行菜单（不重复插）"

if psql_file "$MIG_DIR/V0457__supplier_evaluation_page.down.sql"; then
    ok "V0457 down 执行成功"
else
    bad "V0457 down 失败：$(tail -2 "$LOG" | tr '\n' ' ')"
fi
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_menu WHERE id IN ('20000000-0000-0000-0000-000000004495'::uuid,'20000000-0000-0000-0000-000000004496'::uuid,'20000000-0000-0000-0000-000000004497'::uuid)")" \
    "down 后菜单 0 行"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_role_menu WHERE menu_id IN ('20000000-0000-0000-0000-000000004495'::uuid,'20000000-0000-0000-0000-000000004496'::uuid,'20000000-0000-0000-0000-000000004497'::uuid)")" \
    "down 后授权 0 行"
assert_eq "1" "$(psql_q "SELECT (to_regclass('public.biz_supplier_evaluation') IS NOT NULL)::int")" \
    "down 不删表（表与数据属 V0454，回滚页面不带走历史分数）"

if psql_file "$MIG_DIR/V0457__supplier_evaluation_page.up.sql"; then
    ok "V0457 重新 up 成功（回滚可逆）"
else
    bad "V0457 重新 up 失败：$(tail -2 "$LOG" | tr '\n' ' ')"
fi

# ---------- 3) 库层断言（独立于 Rust 与迁移自检） ----------
echo "== 3) 菜单 / 授权 / 权限码形状（psql 侧）"
assert_eq "3" "$(psql_q "SELECT count(*) FROM sys_menu WHERE id IN ('20000000-0000-0000-0000-000000004495'::uuid,'20000000-0000-0000-0000-000000004496'::uuid,'20000000-0000-0000-0000-000000004497'::uuid) AND deleted_at IS NULL")" \
    "3 行菜单（1 页面 + 2 按钮）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu WHERE id='20000000-0000-0000-0000-000000004495'::uuid AND menu_type=2 AND auth_code='business:erp:supplier-evaluation:page' AND path='/business/erp/supplier-evaluation' AND parent_id='20000000-0000-0000-0000-000000000946'::uuid AND component='business/erp/supplier-evaluation/index' AND length(title)<=12 AND icon='chart-bar'")" \
    "页面行形状（显形码 / 父级采购管理 / path / component / 标题 ≤12 / 图标存在）"
assert_eq "2" "$(psql_q "SELECT count(*) FROM sys_menu WHERE parent_id='20000000-0000-0000-0000-000000004495'::uuid AND menu_type=3 AND auth_code IN ('business:erp:supplier-evaluation:list','business:erp:supplier-evaluation:save')")" \
    "2 个接口码按钮行挂在页面行下"
assert_eq "0" "$(psql_q "SELECT count(*) FROM (SELECT sort_order FROM sys_menu WHERE parent_id='20000000-0000-0000-0000-000000000946'::uuid AND deleted_at IS NULL GROUP BY sort_order HAVING count(*)>1) dup")" \
    "采购管理目录下 sort_order 无重复"
assert_eq "3" "$(psql_q "SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id=rm.role_id AND r.deleted_at IS NULL WHERE r.role_code='PURCHASE_MANAGER' AND rm.menu_id IN ('20000000-0000-0000-0000-000000004495'::uuid,'20000000-0000-0000-0000-000000004496'::uuid,'20000000-0000-0000-0000-000000004497'::uuid)")" \
    "PURCHASE_MANAGER 拿到 3 行（N20：:list 至少一个非超管角色）"
assert_eq "2" "$(psql_q "SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id=rm.role_id AND r.deleted_at IS NULL WHERE r.role_code='FINANCE_MANAGER' AND rm.menu_id IN ('20000000-0000-0000-0000-000000004495'::uuid,'20000000-0000-0000-0000-000000004496'::uuid)")" \
    "FINANCE_MANAGER 拿到页面 + 列表（**不给** save：自己打分自己复核 = 自审）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id=rm.role_id AND r.deleted_at IS NULL WHERE r.role_code='FINANCE_MANAGER' AND rm.menu_id='20000000-0000-0000-0000-000000004497'::uuid")" \
    "FINANCE_MANAGER 确实没有 save 行"

echo "== 4) 页面显形码在后端零引用（AGENTS 约定 14）"
# 只数**非注释**行：`//` / `//!` / `///` 里提到这个码是文档（本卡 handler/dto 的头注释有），
# 不算「后端拿它判权」；例外台账（permission_audit_tests）单独放行（N18 需要它）。
PAGE_CODE_REFS="$(grep -rn 'supplier-evaluation:page' "$ROOT/backend/app/src" --include=*.rs \
    | grep -v 'permission_audit_tests.rs' \
    | grep -vE ':[0-9]+: *//' \
    | wc -l | tr -d ' ')"
assert_eq "0" "$PAGE_CODE_REFS" "后端源码（非注释、除例外台账）零引用 business:erp:supplier-evaluation:page"
assert_eq "1" "$(grep -c 'business:erp:supplier-evaluation:page' "$ROOT/backend/app/src/system/menu/permission_audit_tests.rs")" \
    "页面码已登记进 ROUTE_LEVEL_ONLY（N18 持续校验）"

echo "== 5) 前端接线（页面 / API / 路由三件齐）"
assert_eq "1" "$([ -f "$ROOT/frontend/src/views/business/erp/supplier-evaluation/index.vue" ] && echo 1 || echo 0)" \
    "页面文件存在"
assert_eq "1" "$([ -f "$ROOT/frontend/src/api/business/erp/supplier-evaluation.ts" ] && echo 1 || echo 0)" \
    "API 层文件存在"
assert_eq "1" "$(grep -c "path: 'business/erp/supplier-evaluation'" "$ROOT/frontend/src/router/index.ts")" \
    "router/index.ts 注册了该 path（n08：menu_type=2 的 path 必须能解析）"
assert_eq "1" "$(grep -c "permission: 'business:erp:supplier-evaluation:page'" "$ROOT/frontend/src/router/index.ts")" \
    "路由 meta.permission 用的是页面显形码"
assert_eq "5" "$(grep -c '^export function' "$ROOT/frontend/src/api/business/erp/supplier-evaluation.ts")" \
    "API 层导出 5 个调用（page / detail / rank / suggestion / save）"

# ---------- 6) 真库用例 ----------
echo "== 6) 真库用例（本卡新增的两段 SQL 只能这样验）"
if [ -n "${M416_BAD_URL:-}" ]; then
    export ERP_DB_URL="postgres://$PG_USER:***@127.0.0.1:1/nonexistent"
    info "M416_BAD_URL 已设：故意给一个连不上的库（验 SKIP 守卫）"
else
    export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
fi
run_tests() { # 过滤串 说明 期望最少通过数 [env 前缀]
    local filter="$1" label="$2" want="$3"
    ( cd "$BACKEND" && cargo test -p cloudpivot --lib "$filter" -- --test-threads=1 ) >"$TEST_LOG" 2>&1
    local rc=$?
    local line passed failed skipped
    line="$(grep -E '^test result:' "$TEST_LOG" | tail -1)"
    passed="$(printf '%s' "$line" | sed -nE 's/.* ([0-9]+) passed.*/\1/p')"
    failed="$(printf '%s' "$line" | sed -nE 's/.* ([0-9]+) failed.*/\1/p')"
    skipped="$(grep -c 'SKIP:' "$TEST_LOG")"
    passed="${passed:-0}"; failed="${failed:-0}"
    if [ "$rc" -ne 0 ]; then
        bad "$label：cargo test rc=$rc（$(grep -E '^(error|test .* FAILED|thread)' "$TEST_LOG" | head -2 | tr '\n' ' ')）"
        return 1
    fi
    assert_eq "0" "$failed" "$label 失败数"
    if [ "$passed" -ge "$want" ]; then
        ok "$label 通过 $passed 条（要求 >= $want）"
    else
        bad "$label 只通过 $passed 条（要求 >= $want）—— 用例被 SKIP 或没编译进去"
    fi
    assert_eq "0" "$skipped" "$label SKIP 行数（真库用例不许默默跳过）"
}

run_tests "business::erp::purchase::supplier_evaluation_dto::tests" "接口 DTO 纯逻辑（400 分支 + 出参整形）" 13
if [ -z "${M416_BAD_URL:-}" ]; then
    run_tests "business::erp::purchase::supplier_evaluation_handler_db_tests" "页面接口真库（覆盖 / 分页 / 排名 / 清场）" 4
fi

if [ -n "${M416_BAD_URL:-}" ]; then
    echo "== M416_BAD_URL 模式：上面应当出现 FAIL（SKIP 守卫有效）=="
    if [ "$FAIL" -gt 0 ]; then printf '  rc=1（失败 %d 条，符合预期）\n' "$FAIL"; exit 1; fi
    printf '  居然全绿 —— SKIP 守卫失效\n'; exit 0
fi

# ---------- 7) 门禁 ----------
if [ "$SKIP_GATES" = "1" ]; then
    info "SKIP_GATES=1 → 跳过 permission_audit_tests / n08 / menu_invariants.sh"
else
    echo "== 7) 门禁（权限审计 / 菜单-路由对齐 / 菜单 8 条）"
    export SYS_DB_URL="$ERP_DB_URL"
    run_tests "system::menu::permission_audit_tests" "权限审计 N14–N21" 8
    run_tests "system::menu::tests::n08_menu_paths_all_resolve_in_frontend_router" "n08 菜单路径全部能在前端路由解析" 1
    if PGHOST="$PG_HOST" PGPORT="$PG_PORT" PGUSER="$PG_USER" bash "$ROOT/scripts/verify/menu_invariants.sh" "$DB" >"$LOG" 2>&1; then
        ok "menu_invariants.sh 8 条断言全绿"
    else
        bad "menu_invariants.sh 报红：$(grep -E '✗|FAIL|违规' "$LOG" | head -3 | tr '\n' ' ')"
    fi
fi

# ---------- 8) 夹具残留 ----------
echo "== 8) 夹具残留（真库用例跑完必须自带清场）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_supplier_evaluation WHERE supplier_name LIKE 'M416-%' OR remark LIKE 'M416-%'")" \
    "M416- 评估夹具 0 残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_supplier WHERE supplier_code LIKE 'M416-%'")" \
    "M416- 供应商夹具 0 残留"

# ---------- 汇总 ----------
echo
printf '== 汇总：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
if [ "$FAIL" -gt 0 ]; then
    printf '演练失败（rc=1）\n'
    exit 1
fi
printf '演练通过（rc=0）\n'
