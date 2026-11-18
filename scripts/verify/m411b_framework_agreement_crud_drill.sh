#!/usr/bin/env bash
# ============================================================================
# M4-11b 框架协议维护链路（CRUD + 提交审批）· 演练
#
# 卡：t_f358da98（M4-11b【M4·采购】框架协议维护（CRUD）后端接口 + 页面写入口）
#
# 这一卡交的是**完整纵切**：新增迁移 V0456（5 个接口权限码按钮行 + fw_agreement 审批流程 +
# 授权）+ 后端四件套（dto / model / handler / flow）+ 审批线接线（FWA_APPROVE）+ 前端台账写入口。
# 脚本验的是：
#   1. 从模板库克隆演练库 → 推到仓库当前版本（含 V0456）
#   2. **迁移幂等**：V0456 再跑一遍不报错、菜单 / 流程不长行
#   3. psql 侧独立断言：5 个码挂在 V0452 页面行下 / 同父 sort 唯一 / 授权 / 流程四层 4 节点 3 边 /
#      graph_hash 形状与快照条数 / 夹具 0 残留
#   4. 本卡的 Rust 用例 12 条（纯逻辑 + 真库）：建单形状 / 11 个反向分支 / 就地改单 /
#      状态守卫 / 收紧守卫 / 删单 / 走真审批引擎提交与驳回 / 空范围拒绝 /
#      **全链 建 → 提交 → 审批 → 下单 → 执行量涨** / 种子 hash 自证 / 清场
#      —— 要求 0 failed + 0 SKIP + 用例数对得上（SKIP 就是假绿）
#   5. 回归：M4-07/08/09/10 框架协议全组 + 审批回调注册表对账 + FlowBizType 词表契约
#   6. 门禁：permission_audit_tests + menu_invariants.sh
#
# 用法：
#   bash scripts/verify/m411b_framework_agreement_crud_drill.sh            # 99 → 145（默认）
#   PG_HOST=127.0.0.1 bash scripts/verify/m411b_framework_agreement_crud_drill.sh   # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m411b_framework_agreement_crud_drill.sh  # 留库排查
#   M411B_BAD_URL=1 bash scripts/verify/m411b_framework_agreement_crud_drill.sh     # SKIP 守卫自检（**期望报红 rc=1**）
#   SKIP_GATES=1 bash scripts/verify/m411b_framework_agreement_crud_drill.sh        # 跳过门禁（只验业务）
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名含 drill；脚本自己 DROP；
# 连接串密码一律复用 `$PGPASSWORD`（**不要**在 URL 里写占位符 —— 那样真库用例连不上、
# 打 SKIP 还记 ok，是假绿）。
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
DB="${M411B_DB:-cp_m411b_drill_$$}"
KEEP_DB="${KEEP_DB:-0}"
SKIP_GATES="${SKIP_GATES:-0}"
TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
mkdir -p "$TMP"
LOG="$TMP/m411b_drill_$$.log"
TEST_LOG="$TMP/m411b_tests_$$.log"
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
if [ -n "${M411B_DB:-}" ]; then OWN_DB=0; fi

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }
psql_file()  { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$1" >"$LOG" 2>&1; }

assert_eq() { # 期望 实际 说明
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 [$1]，实际 [$2]）"; fi
}

cleanup() {
    if [ "$OWN_DB" = "0" ] || [ "$KEEP_DB" = "1" ]; then
        printf '\n  （库保留 → %s；不删）\n' "$DB"
        return 0
    fi
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1 || true
    rm -f "$LOG" "$TEST_LOG"
    return 0
}
trap cleanup EXIT

printf '== M4-11b 框架协议维护链路演练 · PG %s:%s / 库 %s ==\n' "$PG_HOST" "$PG_PORT" "$DB"

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
        info "TEMPLATE 克隆失败（模板库有常连）→ pg_dump 兜底"
        psql_admin "CREATE DATABASE $DB" >/dev/null 2>&1
        if pg_dump -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" --no-owner "$TEMPLATE_DB" 2>/dev/null \
            | psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -q >/dev/null 2>&1; then
            ok "pg_dump 兜底克隆 $DB"
        else
            bad "克隆演练库失败（两种方式都不行）"
            exit 2
        fi
    fi
else
    ok "使用调用方给的演练库 $DB（只跑不清）"
fi

# ---------- 1) 推到仓库当前版本 ----------
echo "== 1) 应用业务迁移 V0405…（含本卡 V0456）"
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
assert_eq "0" "$MIG_FAIL" "迁移全部成功（应用 $APPLIED 条）"

# ---------- 2) 迁移幂等 ----------
echo "== 2) 迁移幂等（V0456 再跑一遍）"
MENU_BEFORE="$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code LIKE 'business:erp:framework-agreement:%' AND deleted_at IS NULL")"
FLOW_BEFORE="$(psql_q "SELECT count(*) FROM biz_flow_def WHERE flow_code = 'fw_agreement'")"
if psql_file "$MIG_DIR/V0456__framework_agreement_crud.up.sql"; then
    ok "V0456 重跑不报错"
else
    bad "V0456 重跑失败（幂等破了）：$(tail -3 "$LOG" | tr '\n' ' ')"
fi
assert_eq "$MENU_BEFORE" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code LIKE 'business:erp:framework-agreement:%' AND deleted_at IS NULL")" "重跑后权限码行数不变"
assert_eq "$FLOW_BEFORE" "$(psql_q "SELECT count(*) FROM biz_flow_def WHERE flow_code = 'fw_agreement'")" "重跑后流程定义行数不变"
assert_eq "4" "$(psql_q "SELECT count(*) FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000d06a2'::uuid")" "重跑后节点仍 4 个"

# ---------- 3) psql 侧结构断言 ----------
echo "== 3) 结构断言（菜单 / 授权 / 流程 / 视图）"
assert_eq "7" "$MENU_BEFORE" "框架协议域共 7 个码（页面 + 执行量 + 本卡 5 个）"
assert_eq "5" "$(psql_q "SELECT count(*) FROM sys_menu WHERE parent_id = '20000000-0000-0000-0000-000000004480'::uuid AND menu_type = 3 AND deleted_at IS NULL AND auth_code IN ('business:erp:framework-agreement:list','business:erp:framework-agreement:create','business:erp:framework-agreement:update','business:erp:framework-agreement:delete','business:erp:framework-agreement:submit')")" "5 个接口码按钮全部挂在 V0452 页面行下"
assert_eq "0" "$(psql_q "SELECT count(*) FROM (SELECT sort_order FROM sys_menu WHERE parent_id = '20000000-0000-0000-0000-000000004480'::uuid AND deleted_at IS NULL GROUP BY sort_order HAVING count(*) > 1) d")" "同父 sort_order 无重复（menu_invariants ⑦）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_menu WHERE id IN (SELECT unnest(ARRAY['20000000-0000-0000-0000-000000004490'::uuid,'20000000-0000-0000-0000-000000004491'::uuid,'20000000-0000-0000-0000-000000004492'::uuid,'20000000-0000-0000-0000-000000004493'::uuid,'20000000-0000-0000-0000-000000004494'::uuid])) AND length(title) > 12")" "按钮行 title 均 ≤ 12 字（menu_invariants ⑥）"
assert_eq "5" "$(psql_q "SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL WHERE r.role_code = 'PURCHASE_MANAGER' AND rm.menu_id IN ('20000000-0000-0000-0000-000000004490'::uuid,'20000000-0000-0000-0000-000000004491'::uuid,'20000000-0000-0000-0000-000000004492'::uuid,'20000000-0000-0000-0000-000000004493'::uuid,'20000000-0000-0000-0000-000000004494'::uuid)")" "PURCHASE_MANAGER 拿到 5 个授权"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_flow_def WHERE flow_code = 'fw_agreement' AND biz_type = 'FWA_APPROVE' AND status = 'PUBLISHED' AND is_default AND deleted_at IS NULL")" "fw_agreement 流程已发布且默认"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d06a2'::uuid AND node_count = 4 AND edge_count = 3 AND graph_hash ~ '^[0-9a-f]{64}$'")" "流程版本 4 节点 / 3 边 / hash 形状正确"
assert_eq "3" "$(psql_q "SELECT count(*) FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000d06a2'::uuid")" "边 3 条（采购经理 → 财务经理 串行）"
assert_eq "2" "$(psql_q "SELECT count(*) FROM biz_flow_node_assignee a JOIN biz_flow_node n ON n.id = a.node_id WHERE n.version_id = '20000000-0000-4000-8000-0000000d06a2'::uuid AND a.assignee_type = 'ROLE'")" "两个 ROLE 审批人（采购经理 / 财务经理）"
assert_eq "4" "$(psql_q "SELECT jsonb_array_length(graph_snapshot->'nodes') FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d06a2'::uuid")" "graph_snapshot 里 4 个节点"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_framework_agreement WHERE agreement_name LIKE 'M411B-%'")" "夹具协议 0 残留（跑用例前）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_seq WHERE seq_type = 'FWA' AND deleted_at IS NULL")" "FWA 取号行唯一（V0450 种的，不许被本卡重复种）"

# ---------- 4) 本卡的 Rust 用例 ----------
echo "== 4) 本卡真库用例（framework_agreement_crud_db_tests，12 条）"
TEST_BIN_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
if [ "${M411B_BAD_URL:-0}" = "1" ]; then
    TEST_BIN_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/cp_no_such_db_$$"
    info "M411B_BAD_URL=1：故意用连不上的库（验「SKIP 守卫有牙」）"
fi
(
    cd "$BACKEND" || exit 1
    ERP_DB_URL="$TEST_BIN_URL" timeout 1200 cargo test --lib \
        business::erp::purchase::framework_agreement_crud_db_tests -- --test-threads=1 --nocapture >"$TEST_LOG" 2>&1
)
TEST_RC=$?
PASSED="$(grep -oE '^test result: ok\. [0-9]+ passed' "$TEST_LOG" | head -1 | grep -oE '[0-9]+' | head -1)"
PASSED="${PASSED:-0}"
MARKERS="$(grep -c '\[M411B-DB\]' "$TEST_LOG" || true)"
FAILED_LINES="$(grep -cE '^test .* FAILED|panicked at' "$TEST_LOG" || true)"
assert_eq "12" "$PASSED" "用例通过 12 条"
assert_eq "0" "$FAILED_LINES" "0 条失败"
if [ "${M411B_BAD_URL:-0}" = "1" ]; then
    # 连不上库时用例会提前 return：passed 仍是 12 但**没有任何真库标记**。
    # 有牙自证的口径与 M4-08 一致：**期望这个模式报红**（rc=1 + FAIL=1），
    # 而不是「检测到假绿然后记一条 PASS」—— 后者跑完是 rc=0，容易被当成真绿。
    if [ "$MARKERS" = "0" ]; then
        bad "假绿对照（预期红）：连不上库 → 真库标记=0，passed 数骗不到脚本"
    else
        ok "假绿对照没生效：连不上库却仍有 $MARKERS 个真库标记（守卫失效，必须修）"
    fi
else
    assert_eq "12" "$MARKERS" "12 条用例都真连了库（每条打印一次自证标记）"
    if [ "$TEST_RC" = "0" ]; then ok "cargo test rc=0"; else bad "cargo test rc=$TEST_RC（tail: $(tail -5 "$TEST_LOG" | tr '\n' ' ')）"; fi
fi

# ---------- 5) 回归 ----------
echo "== 5) 回归：框架协议全组 + 审批回调注册表 + 词表契约"
for grp in "business::erp::purchase::framework_agreement" "business::flow::doc_reader_tests" "business::erp::doc_reader_impl_tests"; do
    if (
        cd "$BACKEND" || exit 1
        ERP_DB_URL="$TEST_BIN_URL" timeout 1200 cargo test --lib "$grp" -- --test-threads=1 >"$TEST_LOG" 2>&1
    ); then
        R_PASSED="$(grep -oE '^test result: ok\. [0-9]+ passed' "$TEST_LOG" | head -1 | grep -oE '[0-9]+' | head -1)"
        ok "$grp 全绿（$R_PASSED 条）"
    else
        bad "$grp 有失败（tail: $(tail -5 "$TEST_LOG" | tr '\n' ' ')）"
    fi
done

# ---------- 6) 门禁 ----------
if [ "$SKIP_GATES" = "1" ]; then
    info "SKIP_GATES=1：跳过门禁"
else
    echo "== 6) 门禁：permission_audit_tests + menu_invariants"
    if (
        cd "$BACKEND" || exit 1
        timeout 900 cargo test --lib business::system::menu::permission_audit_tests -- --test-threads=1 >"$TEST_LOG" 2>&1
    ); then
        ok "permission_audit_tests 全绿"
    else
        bad "permission_audit_tests 有失败（tail: $(tail -5 "$TEST_LOG" | tr '\n' ' ')）"
    fi
    if PGPASSWORD="$PGPASSWORD" PGHOST="$PG_HOST" PGPORT="$PG_PORT" PGUSER="$PG_USER" \
        bash "$ROOT/scripts/verify/menu_invariants.sh" "$DB" >"$LOG" 2>&1; then
        ok "menu_invariants 8 条全绿"
    else
        bad "menu_invariants 报红（tail: $(tail -8 "$LOG" | tr '\n' ' ')）"
    fi
fi

# ---------- 收尾 ----------
echo "== 结果"
if [ "$FAIL" = "0" ]; then
    printf '  PASS=%s FAIL=0  RESULT: PASS\n' "$PASS"
    exit 0
fi
printf '  PASS=%s FAIL=%s  RESULT: FAIL\n' "$PASS" "$FAIL"
exit 1
