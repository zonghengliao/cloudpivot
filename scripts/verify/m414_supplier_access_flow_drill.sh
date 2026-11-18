#!/usr/bin/env bash
# ============================================================================
# M4-14 供应商准入 / 黑名单流程 · 演练
#
# 卡：t_583852b1（M4-14【M4·采购】供应商准入/黑名单流程「走 flow」）
#
# 这一卡交的是**完整的后端纵切**：新增迁移 V0455（申请单表 + SAC 取号 +
# supplier_access 审批流程 + 5 行菜单 + 授权）+ 四件套 + 审批回调落地 + 接口 4 个权限码
# + 采购订单建单的供应商启停闸。脚本验的是：
#   1. 从模板库克隆演练库 → 推到仓库当前版本（含 V0455）
#   2. **迁移幂等**：V0455 再跑一遍不报错、不长行
#   3. 本卡的 Rust 用例：`supplier_access` 全组（纯逻辑 + 真库，含走真审批引擎的端到端）
#      —— 要求 0 failed + 0 SKIP + 用例数对得上（SKIP 就是假绿）
#   4. 回归：M4-13 打分/评级、流程注册表对账、FlowBizType 词表契约
#   5. psql 侧独立断言：表/约束/取号/流程四层与 hash 形状/菜单与授权/夹具 0 残留
#   6. 不夹带：迁移只多 V0455 一对、前端只改镜像（doc-kinds.ts + 镜像单测）、无 Vue 页面
#   7. 门禁：permission_audit_tests + menu_invariants.sh
#
# 用法：
#   bash scripts/verify/m414_supplier_access_flow_drill.sh                  # 99 → 145（默认）
#   PG_HOST=127.0.0.1 bash scripts/verify/m414_supplier_access_flow_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m414_supplier_access_flow_drill.sh        # 留库排查
#   M414_BAD_URL=1 bash scripts/verify/m414_supplier_access_flow_drill.sh   # SKIP 守卫自检（期望报红）
#   SKIP_GATES=1 bash scripts/verify/m414_supplier_access_flow_drill.sh     # 跳过门禁（只验业务）
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
DB="${M414_DB:-cp_m414_sa_drill_$$}"
KEEP_DB="${KEEP_DB:-0}"
SKIP_GATES="${SKIP_GATES:-0}"
TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
mkdir -p "$TMP"
LOG="$TMP/m414_drill_$$.log"
TEST_LOG="$TMP/m414_tests_$$.log"
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
if [ -n "${M414_DB:-}" ]; then OWN_DB=0; fi

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }
psql_file()  { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$1" >"$LOG" 2>&1; }

assert_eq() { # 期望 实际 说明
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 [$1]，实际 [$2]）"; fi
}

rejects_c() { # sql 约束名 说明
    local out
    out="$(psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -q -X -c "$1" 2>&1)"
    if ! printf '%s' "$out" | grep -q 'ERROR'; then
        bad "$2 —— 库层居然接受了（约束失效）"
        return
    fi
    case "$out" in
        *"invalid input syntax"*|*"does not exist"*|*"cannot cast"*)
            bad "$2 —— 被拒了但**不是约束**（SQL 自己写错：$(printf '%s' "$out" | head -1)）"
            ;;
        *)
            if printf '%s' "$out" | grep -q "\"$3\""; then
                ok "$2（库层拒了：$3）"
            else
                bad "$2 —— 报错但不是 $3 拦的：$(printf '%s' "$out" | head -1)"
            fi
            ;;
    esac
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

printf '== M4-14 供应商准入 / 黑名单流程演练 · PG %s:%s / 库 %s ==\n' "$PG_HOST" "$PG_PORT" "$DB"

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
echo "== 1) 应用业务迁移 V0405…（含本卡 V0455）"
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
assert_eq "1" "$(psql_q "SELECT (to_regclass('public.biz_supplier_access') IS NOT NULL)::int")" \
    "申请单表存在（V0455 已落地）"

echo "== 1.1) 迁移幂等：V0455 再跑一遍"
BEFORE_ROWS="$(psql_q "SELECT count(*) FROM biz_supplier_access")"
BEFORE_MENU="$(psql_q "SELECT count(*) FROM sys_menu WHERE id IN ('20000000-0000-0000-0000-000000004484'::uuid,'20000000-0000-0000-0000-000000004485'::uuid,'20000000-0000-0000-0000-000000004486'::uuid,'20000000-0000-0000-0000-000000004487'::uuid,'20000000-0000-0000-0000-000000004488'::uuid)")"
if psql_file "$MIG_DIR/V0455__supplier_access.up.sql"; then
    ok "V0455 第二次执行成功（幂等：建表/索引/取号/流程/菜单全部 IF NOT EXISTS / ON CONFLICT）"
else
    bad "V0455 第二次执行失败：$(tail -3 "$LOG" | tr '\n' ' ')"
fi
assert_eq "$BEFORE_ROWS" "$(psql_q "SELECT count(*) FROM biz_supplier_access")" "重跑后申请单行数不变"
assert_eq "$BEFORE_MENU" "$(psql_q "SELECT count(*) FROM sys_menu WHERE id IN ('20000000-0000-0000-0000-000000004484'::uuid,'20000000-0000-0000-0000-000000004485'::uuid,'20000000-0000-0000-0000-000000004486'::uuid,'20000000-0000-0000-0000-000000004487'::uuid,'20000000-0000-0000-0000-000000004488'::uuid)")" "重跑后菜单行数不变（=5）"

# ---------- 2) 本卡 Rust 用例 ----------
echo "== 2) 本卡 Rust 用例（纯逻辑 + 真库 + 走真审批引擎）"
if [ -n "${M414_BAD_URL:-}" ]; then
    export ERP_DB_URL="postgres://$PG_USER:***@127.0.0.1:1/nonexistent"
    info "M414_BAD_URL 已设：故意给一个连不上的库（验 SKIP 守卫）"
else
    export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
fi
run_tests() { # 过滤串 说明 期望最少通过数
    local filter="$1" label="$2" want="$3"
    ( cd "$BACKEND" && cargo test -p cloudpivot --lib "$filter" -- --test-threads=1 ) >"$TEST_LOG" 2>&1
    local rc=$?
    local line
    line="$(grep -E '^test result:' "$TEST_LOG" | tail -1)"
    local passed failed
    passed="$(printf '%s' "$line" | sed -nE 's/.* ([0-9]+) passed.*/\1/p')"
    failed="$(printf '%s' "$line" | sed -nE 's/.* ([0-9]+) failed.*/\1/p')"
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
    local skipped
    skipped="$(grep -c 'SKIP:' "$TEST_LOG")"
    assert_eq "0" "$skipped" "$label SKIP 行数（真库用例不许默默跳过）"
    if [ -n "${M414_EVIDENCE:-}" ]; then
        {
            printf '\n===== %s（filter=%s）=====\n' "$label" "$filter"
            cat "$TEST_LOG"
        } >>"$M414_EVIDENCE"
    fi
}

run_tests_pkg() { # 包 过滤串 说明 期望最少通过数
    local pkg="$1" filter="$2" label="$3" want="$4"
    ( cd "$BACKEND" && cargo test -p "$pkg" --lib "$filter" -- --test-threads=1 ) >"$TEST_LOG" 2>&1
    local rc=$?
    local line
    line="$(grep -E '^test result:' "$TEST_LOG" | tail -1)"
    local passed failed
    passed="$(printf '%s' "$line" | sed -nE 's/.* ([0-9]+) passed.*/\1/p')"
    failed="$(printf '%s' "$line" | sed -nE 's/.* ([0-9]+) failed.*/\1/p')"
    passed="${passed:-0}"; failed="${failed:-0}"
    if [ "$rc" -ne 0 ]; then
        bad "$label：cargo test rc=$rc（$(grep -E '^(error|test .* FAILED|thread)' "$TEST_LOG" | head -2 | tr '\n' ' ')）"
        return 1
    fi
    assert_eq "0" "$failed" "$label 失败数"
    if [ "$passed" -ge "$want" ]; then
        ok "$label 通过 $passed 条（要求 >= $want）"
    else
        bad "$label 只通过 $passed 条（要求 >= $want）"
    fi
}

run_tests "business::erp::purchase::supplier_access" "准入/黑名单（M4-14）" 20
if [ -z "${M414_BAD_URL:-}" ]; then
    run_tests "business::erp::purchase::supplier_evaluation_grading" "打分+评级回归（M4-13）" 16
    run_tests "business::flow::doc_reader_tests" "流程注册表对账" 4
    run_tests "business::erp::flow_biz_type_contract_tests" "审批类型契约" 2
fi

if [ -n "${M414_BAD_URL:-}" ]; then
    echo "== M414_BAD_URL 模式：上面应当出现 FAIL（SKIP 守卫有效）=="
    if [ "$FAIL" -gt 0 ]; then printf '  rc=1（失败 %d 条，符合预期）\n' "$FAIL"; exit 1; fi
    printf '  居然全绿 —— SKIP 守卫失效\n'; exit 0
fi

# ---------- 3) psql 侧独立断言 ----------
echo "== 3) 库层不变量（psql 侧，独立于 Rust）"
assert_eq "21" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='biz_supplier_access'")" \
    "申请单表 21 列"
assert_eq "1" "$(psql_q "SELECT count(*) FROM pg_constraint WHERE conrelid='biz_supplier_access'::regclass AND conname='ck_biz_sa_grade_only_blacklist'")" \
    "等级快照只能出现在黑名单单上（CHECK 在）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_seq WHERE seq_type='SAC' AND prefix='SAC' AND deleted_at IS NULL")" \
    "取号行 SAC 在且唯一"

# 坏数据逐条被拒（约束真的在管）
rejects_c "INSERT INTO biz_supplier_access (access_no, supplier_id, supplier_code, supplier_name, access_type) VALUES ('SAC-19990101-914001', gen_random_uuid(), 'M414-X', 'x', 'DELETE')" \
    "类型域外的 DELETE 被拒" "ck_biz_sa_type"
rejects_c "INSERT INTO biz_supplier_access (access_no, supplier_id, supplier_code, supplier_name, access_type, grade_snapshot) VALUES ('SAC-19990101-914002', gen_random_uuid(), 'M414-X', 'x', 'ADMIT', 'D')" \
    "准入单带等级快照被拒（不造摆设列）" "ck_biz_sa_grade_only_blacklist"
rejects_c "INSERT INTO biz_supplier_access (access_no, supplier_id, supplier_code, supplier_name, access_type, status, reason, approval_instance_id) VALUES ('SAC-19990101-914003', gen_random_uuid(), 'M414-X', 'x', 'BLACKLIST', 'SUBMITTED', '短', gen_random_uuid())" \
    "提交后理由不足 5 字被拒" "ck_biz_sa_reason_on_submit"
rejects_c "INSERT INTO biz_supplier_access (access_no, supplier_id, supplier_code, supplier_name, access_type, status, reason, approval_instance_id) VALUES ('SAC-19990101-914004', gen_random_uuid(), 'M414-X', 'x', 'BLACKLIST', 'APPROVED', '理由够五个字', gen_random_uuid())" \
    "APPROVED 但没有结论人/时间被拒" "ck_biz_sa_decided"
rejects_c "INSERT INTO biz_supplier_access (access_no, supplier_id, supplier_code, supplier_name, access_type) VALUES ('SAC-19990101-914005', '20000000-0000-4000-8000-0000000e4140'::uuid, 'M414-DUP', 'dup-a', 'BLACKLIST'); \
           INSERT INTO biz_supplier_access (access_no, supplier_id, supplier_code, supplier_name, access_type) VALUES ('SAC-19990101-914006', '20000000-0000-4000-8000-0000000e4140'::uuid, 'M414-DUP', 'dup-b', 'ADMIT')" \
    "同一供应商第二张未结单被拒" "uk_biz_sa_supplier_open"

# 流程四层
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_flow_def WHERE flow_code='supplier_access' AND biz_type='SUPPLIER_ACCESS' AND status='PUBLISHED' AND is_default AND deleted_at IS NULL")" \
    "supplier_access 流程定义（已发布 + 默认 + 唯一）"
assert_eq "5" "$(psql_q "SELECT node_count FROM biz_flow_def_version v JOIN biz_flow_def d ON d.current_version_id=v.id WHERE d.flow_code='supplier_access'")" \
    "节点数 5"
assert_eq "5" "$(psql_q "SELECT edge_count FROM biz_flow_def_version v JOIN biz_flow_def d ON d.current_version_id=v.id WHERE d.flow_code='supplier_access'")" \
    "边数 5"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_flow_edge e JOIN biz_flow_def d ON d.current_version_id=e.version_id WHERE d.flow_code='supplier_access' AND e.edge_key='e_type_blk' AND e.condition_expr->>'field'='form.access_type' AND e.condition_expr->>'value'='BLACKLIST' AND NOT e.is_default")" \
    "拉黑条件边落在 form.access_type = BLACKLIST（分级在流程定义里，不在代码里）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_flow_def_version v JOIN biz_flow_def d ON d.current_version_id=v.id WHERE d.flow_code='supplier_access' AND v.graph_hash ~ '^[0-9a-f]{64}$' AND v.graph_hash <> repeat('0',64)")" \
    "graph_hash 是真算出来的 64 位十六进制（不是占位）"

# 菜单与授权
assert_eq "5" "$(psql_q "SELECT count(*) FROM sys_menu WHERE id IN ('20000000-0000-0000-0000-000000004484'::uuid,'20000000-0000-0000-0000-000000004485'::uuid,'20000000-0000-0000-0000-000000004486'::uuid,'20000000-0000-0000-0000-000000004487'::uuid,'20000000-0000-0000-0000-000000004488'::uuid) AND deleted_at IS NULL")" \
    "菜单 5 行（1 页面 + 4 按钮）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu WHERE id='20000000-0000-0000-0000-000000004484'::uuid AND menu_type=2 AND auth_code='business:erp:supplier-access:page' AND parent_id='20000000-0000-0000-0000-000000000946'::uuid AND length(title)<=12")" \
    "页面行：显形码 + 挂在采购管理下 + 标题 ≤ 12 字"
assert_eq "4" "$(psql_q "SELECT count(*) FROM sys_menu WHERE parent_id='20000000-0000-0000-0000-000000004484'::uuid AND menu_type=3 AND auth_code IN ('business:erp:supplier-access:list','business:erp:supplier-access:create','business:erp:supplier-access:update','business:erp:supplier-access:submit')")" \
    "4 个接口码挂在页面行下（页面码 ≠ 接口码）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM (SELECT sort_order FROM sys_menu WHERE parent_id='20000000-0000-0000-0000-000000004484'::uuid AND deleted_at IS NULL GROUP BY sort_order HAVING count(*)>1) dup")" \
    "同父 sort_order 无重复"
assert_eq "5" "$(psql_q "SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id=rm.role_id AND r.deleted_at IS NULL WHERE r.role_code='PURCHASE_MANAGER' AND rm.menu_id IN ('20000000-0000-0000-0000-000000004484'::uuid,'20000000-0000-0000-0000-000000004485'::uuid,'20000000-0000-0000-0000-000000004486'::uuid,'20000000-0000-0000-0000-000000004487'::uuid,'20000000-0000-0000-0000-000000004488'::uuid)")" \
    "PURCHASE_MANAGER 拿到 5 行（N20：除超管外至少一个角色能用）"

# 夹具残留
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_supplier_access WHERE supplier_code LIKE 'M414-%'")" \
    "本卡申请单夹具 0 残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_supplier WHERE supplier_code LIKE 'M414-%'")" \
    "本卡供应商夹具 0 残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_instance WHERE biz_type='SUPPLIER_ACCESS'")" \
    "本卡审批实例 0 残留"

# ---------- 4) 不夹带 ----------
echo "== 4) 不夹带：迁移只多一对、前端只改镜像、无 Vue 页面"
# 状态无关断言：本卡在迁移目录里只允许出现 V0455 的 up/down（提交前=2 个未跟踪，提交后=0；
# 别人并行改的迁移不在本卡范围，所以不数总数）
assert_eq "0" "$(git -C "$ROOT" status --porcelain backend/app/migrations | grep -E 'V0455__supplier_access' | grep -vcE '^\?\? backend/app/migrations/business/V0455__supplier_access\.(up|down)\.sql$' || true)" \
    "迁移目录里没有 V0455 之外的改动属于本卡"
assert_eq "0" "$(git -C "$ROOT" status --porcelain frontend/src/views/business/erp/supplier-access 2>/dev/null | wc -l | tr -d ' ')" \
    "supplier-access 页面目录 0 改动（页面属 M4-16）"
assert_eq "0" "$(ls "$ROOT/frontend/src/views/business/erp/supplier-access" 2>/dev/null | wc -l | tr -d ' ')" \
    "supplier-access 页面目录不存在（本卡不画页面）"
# 前端只允许「镜像同步」这两处：FlowBizType 词表 + 它的逐字比对单测
assert_eq "1" "$(grep -c "'SUPPLIER_ACCESS'," "$ROOT/frontend/src/api/business/erp/doc-kinds.ts")" \
    "前端词表镜像里有 SUPPLIER_ACCESS"
assert_eq "1" "$(grep -c "SUPPLIER_ACCESS: 'supplier_access'," "$ROOT/frontend/src/api/business/erp/doc-kinds.ts")" \
    "前端 flow_code 镜像里有 supplier_access"
assert_eq "1" "$(grep -c "解析到.*个字面量\|assert.equal(rust.length, 27" "$ROOT/frontend/tests/unit/flow-biz-link.test.ts")" \
    "镜像单测已同步到 27 个字面量"

# ---------- 5) 门禁 ----------
if [ "$SKIP_GATES" = "1" ]; then
    info "SKIP_GATES=1 → 跳过 permission_audit_tests / menu_invariants.sh"
else
    echo "== 5) 门禁"
    run_tests "system::menu::permission_audit_tests" "权限审计" 8
    run_tests_pkg "cloudpivot-kernel" "error_code::tests::erp_error_codes_are_stable" "错误码表（66xxx）" 1
    run_tests "business::erp::doc_reader_impl_tests" "注册表契约（19 受控 + 调拨单）" 16
    MENU_OUT="$(cd "$ROOT" && PGHOST="$PG_HOST" PGPORT="$PG_PORT" PGUSER="$PG_USER" bash scripts/verify/menu_invariants.sh "$DB" 2>&1)"
    MENU_RC=$?
    if [ "$MENU_RC" -eq 0 ]; then
        ok "menu_invariants.sh 8 条全绿（$(printf '%s' "$MENU_OUT" | grep -cE '^ *(✓|PASS)' || true) 条输出行）"
    else
        bad "menu_invariants.sh rc=$MENU_RC：$(printf '%s' "$MENU_OUT" | grep -E '✗|FAIL|违规' | head -3 | tr '\n' ' ')"
    fi
fi

# ---------- 收尾 ----------
echo
if [ "$FAIL" -eq 0 ]; then
    printf '== 结果：断言通过 %d 条 / 失败 0 条 · rc=0 ==\n' "$PASS"
    exit 0
fi
printf '== 结果：断言通过 %d 条 / 失败 %d 条 · rc=1 ==\n' "$PASS" "$FAIL"
exit 1
