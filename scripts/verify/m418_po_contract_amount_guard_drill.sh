#!/usr/bin/env bash
# ============================================================================
# M4-18 采购订单挂合同额度守卫（**金额不超合同 · 超限拒绝**）· 演练
#
# 卡：t_b90d4761（M4-18【M4·采购】订单挂合同校验（金额不超合同）—— 超限拒绝）
# 上游：t_566fc2dc（M4-17 contract_id 字段）在收尾注释里点名「M4-18 也已实现，先核验再动手」。
#
# # 这一卡为什么没有改业务代码（重要，别当漏做）
#
# 守卫本体在 HEAD 上已经完整落地，且语义正确：
#   · `purchase_handler::guard_contract_amount`（事务内 `contract_lock_full` = FOR UPDATE 锁合同行
#     → `purchase_model::committed_amount` 现算 → `> 合同额` 报 409 + 66019）；
#   · 两个入口各一处：`create_purchase_order`（建单）/ `update_purchase_order`（改单，`exclude` 自己）；
#   · 口径 `status <> 'CANCELLED' AND deleted_at IS NULL`（作废 / 软删释放额度），**不做缓存列**；
#   · 采购单**唯一写入口**：`purchase_model::create` / `update` 只被这两个 handler 调，
#     连「采购申请 → 订单」（`purchase_requisition_handler` 872 行）也是走 `create_purchase_order`。
# 本脚不重复实现同一道守卫（那只会变成两处真相），改为把它的每一个**可被放宽的条件**钉死：
#
#   ① 累计超限（第二张顶破）        ② 边界（正好等于放行 / 差一分拒绝）
#   ③ 改单超限 + 整单回滚           ④ 改单**排除自己**（用满的合同也改得动）
#   ⑤ 作废释放额度                  ⑥ 软删释放额度
#   ⑦ 跨合同不串额                  ⑧ **并发只准一张过**（FOR UPDATE 是唯一保证）
#
# 而且这 8 条要**能红**才有意义 —— 所以第 ④ 段用变异反证（MUTATE=1）逐条证明：
#   M1 去掉 `exclude` → 恰好 ④ 红；M2 去掉 `status <> 'CANCELLED'` → 恰好 ⑤ 红；
#   M3 去掉 `FOR UPDATE` → ⑧ 红（连跑 2 轮都红，不是偶发）。
#
# 用法：
#   bash scripts/verify/m418_po_contract_amount_guard_drill.sh                 # 99 → 145（默认）
#   PG_HOST=127.0.0.1 bash scripts/verify/m418_po_contract_amount_guard_drill.sh  # 在 145 上跑
#   MUTATE=1 bash scripts/verify/m418_po_contract_amount_guard_drill.sh       # 加跑变异反证（多 ~15 分钟）
#   SKIP_MODULE=1 ...    # 只跑 m418 八条，不跑采购整套件
#   KEEP_DB=1 ...        # 留库排查
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145、库名含 drill/test/e2e、脚本自己 DROP；
# 口令复用 `$PGPASSWORD`（不写占位符 —— 那样真库用例连不上还打 SKIP 记 ok，是假绿）。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIG_DIR="$ROOT/backend/app/migrations/business"
BACKEND="$ROOT/backend"
HANDLER="$BACKEND/app/src/business/erp/purchase/purchase_handler.rs"
PMODEL="$BACKEND/app/src/business/erp/purchase/purchase_model.rs"
CMODEL="$BACKEND/app/src/business/erp/contract/contract_model.rs"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="${M418_DB:-cp_m418_guard_drill_$$}"
KEEP_DB="${KEEP_DB:-0}"
MUTATE="${MUTATE:-0}"
SKIP_MODULE="${SKIP_MODULE:-0}"
TMP="${TMPDIR:-/tmp}"
mkdir -p "$TMP"
LOG="$TMP/m418_$$.log"
PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }

# 夹具 id（与 purchase_handler::tests 的 FIXTURES 逐字一致）
F_SUPPLIER='d2a20001-0000-4000-8000-000000000001'
F_GUARD_CONTRACTS="'d2a20001-0000-4000-8000-0000000000d1'::uuid,'d2a20001-0000-4000-8000-0000000000d2'::uuid,'d2a20001-0000-4000-8000-0000000000d3'::uuid,'d2a20001-0000-4000-8000-0000000000d4'::uuid"

case "$DB" in
    *drill*|*e2e*|*test*) : ;;
    *) printf '库名 %s 不含 drill/e2e/test，拒绝在它上面跑（rc=2）\n' "$DB"; exit 2 ;;
esac

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }
psql_file()  { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$1" >"$LOG" 2>&1; }

cleanup() {
    if [ "$KEEP_DB" = "1" ]; then
        printf '\n  （库保留 → %s；不删）\n' "$DB"
        return
    fi
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
    rm -f "$LOG"
}
trap cleanup EXIT

printf '== M4-18 采购单合同额度守卫演练 · PG %s:%s / 库 %s ==\n' "$PG_HOST" "$PG_PORT" "$DB"

# ---------- 0) 前置：模板库 + 克隆 + 回放到仓库当前版本 ----------
echo "== 0) 前置：模板库可克隆，演练库推到仓库当前版本"
if ! psql_admin "SELECT 1 FROM pg_database WHERE datname = '$TEMPLATE_DB'" | grep -q 1; then
    bad "模板库 $TEMPLATE_DB 不存在（请先在两台机器的 PG 里准备模板库）"
    exit 2
fi
ok "模板库 $TEMPLATE_DB 存在"
if ! psql_admin "CREATE DATABASE $DB TEMPLATE $TEMPLATE_DB" >/dev/null 2>&1; then
    bad "克隆演练库失败（库名 $DB）"
    exit 2
fi
ok "从模板克隆演练库 $DB"

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

# 模板自带的历史残留：`zz_teardown_*` 里有「全库不剩 PENDING 采购审批实例」的断言（顶唯一索引），
# 而模板库里 2026-11-10 就躺着一行别的模块留下的 PENDING 实例（供应商 b1b00005-…，与本夹具无关）。
# 它不是本次改动造成的、也不归采购模块清（清别人的夹具会把兄弟模块的中间态砸了），
# 所以演练在**跑用例之前**把「此刻已存在」的这类行清掉并打印出来 —— 之后若有 PENDING 残留，
# 只可能来自本次跑的动作，断言仍然有效。
echo "== 0b) 模板自带的历史残留（跑用例前清理，逐行打印）"
STALE="$(psql_q "SELECT instance_no FROM biz_flow_instance WHERE biz_type='PO_APPROVE' AND status='PENDING' AND deleted_at IS NULL")"
if [ -z "$STALE" ]; then
    ok "模板里没有历史 PENDING 采购审批实例"
else
    info "清掉跑前就存在的 PENDING 实例：$(printf '%s' "$STALE" | tr '\n' ' ')"
    psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q \
        -c "DELETE FROM biz_flow_task WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE biz_type='PO_APPROVE' AND status='PENDING' AND deleted_at IS NULL);
            DELETE FROM biz_flow_instance WHERE biz_type='PO_APPROVE' AND status='PENDING' AND deleted_at IS NULL;" >"$LOG" 2>&1
    left="$(psql_q "SELECT count(*) FROM biz_flow_instance WHERE biz_type='PO_APPROVE' AND status='PENDING' AND deleted_at IS NULL")"
    if [ "$left" = "0" ]; then ok "历史残留已清空（清理后 PENDING = 0）"; else bad "历史残留清理后仍有 $left 行"; fi
fi

# 基线：夹具供应商的采购单张数（跑完要回到这个数，证明用例自清）
BASELINE_ORDERS="$(psql_q "SELECT count(*) FROM biz_purchase_order WHERE supplier_id = '$F_SUPPLIER'::uuid")"
info "基线：夹具供应商名下采购单 = $BASELINE_ORDERS 张"

run_tests() { # 过滤串 日志文件
    ( cd "$BACKEND" && ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB" \
        cargo test --lib "$1" -- --test-threads=1 ) >"$2" 2>&1
}

# ---------- 1) 代码接线：守卫到底在哪、由谁触发 ----------
echo "== 1) 代码接线（守卫本体 + 两个入口 + 现算口径 + 行锁）"
grep -q 'async fn guard_contract_amount(' "$HANDLER" \
    && ok "守卫本体 guard_contract_amount 在 purchase_handler.rs" \
    || bad "找不到 guard_contract_amount"
if grep -q 'guard_contract_amount(&tx, contract, totals.with_tax, None)' "$HANDLER"; then
    ok "建单入口调用守卫（exclude = None）"
else
    bad "建单入口没有调用守卫"
fi
if grep -q 'guard_contract_amount(&tx, contract, totals.with_tax, Some(id))' "$HANDLER"; then
    ok "改单入口调用守卫（exclude = Some(id)，自己不算两遍）"
else
    bad "改单入口没有调用守卫"
fi
grep -q 'contract_model::contract_lock_full' "$HANDLER" \
    && ok "守卫先锁合同行（contract_lock_full → FOR UPDATE）" \
    || bad "守卫没有锁合同行"
grep -q 'FOR UPDATE' "$CMODEL" \
    && ok "contract_lock_full 的 SQL 里确实有 FOR UPDATE" \
    || bad "contract_lock_full 丢了 FOR UPDATE（并发下会双双放行）"
if grep -q "contract_id = ?::uuid AND status <> 'CANCELLED' AND deleted_at IS NULL" "$PMODEL"; then
    ok "现算口径：contract_id 精确匹配 + 排除作废 + 排除软删"
else
    bad "committed_amount 的口径三条件缺项"
fi
grep -q 'sql.push_str(" AND id <> ?::uuid")' "$PMODEL" \
    && ok "改单排除自己（exclude → AND id <> ?）" \
    || bad "exclude 分支没有拼 id <> ?"
grep -q 'if after > locked.amount' "$HANDLER" \
    && ok "判据是 after > 合同额（正好用满放行，不是 >=）" \
    || bad "判据不是 after > locked.amount"
grep -q 'ERP_CONTRACT_AMOUNT_EXCEEDED' "$HANDLER" \
    && ok "超限用专用错误码 ERP_CONTRACT_AMOUNT_EXCEEDED（66019）" \
    || bad "超限没有用专用错误码"
# 唯一写入口：采购单的写入只经 `purchase_model::create` / `update`，而它俩各自只有
# `purchase_handler.rs` 一个调用方 —— 所以「采购申请 → 订单」这类旁路也绕不开守卫。
CREATORS="$(grep -rn 'purchase_model::create(' --include=*.rs "$BACKEND/app/src" | grep -v '_tests\.rs' | wc -l)"
UPDATERS="$(grep -rn 'purchase_model::update(' --include=*.rs "$BACKEND/app/src" | grep -v '_tests\.rs' | wc -l)"
if [ "$CREATORS" = "1" ] && [ "$UPDATERS" = "1" ]; then
    ok "唯一写入口：create/update 各只有 1 个调用方（都在 purchase_handler.rs 的守卫之后）"
else
    bad "写入口不唯一：create 调用方 $CREATORS 处、update $UPDATERS 处（可能绕过守卫）"
fi

# ---------- 2) 真库：M4-18 八条用例 ----------
echo "== 2) 真库用例：M4-18 八条（累计/边界/改单/排除自己/作废/软删/跨合同/并发）"
T8="$TMP/m418_t8_$$.log"
run_tests 'business::erp::purchase::purchase_handler::tests::m418' "$T8"
if grep -qE 'test result: ok\. 8 passed; 0 failed' "$T8"; then
    ok "8 条全绿（0 failed，0 ignored：没有 SKIP 假绿）"
else
    bad "8 条没有全绿：$(grep -E 'test result:' "$T8" | head -1)"
    grep -E '^test .* FAILED|panicked at' "$T8" | head -10 | sed 's/^/       /'
fi
if grep -qE 'SKIP: 未设置 ERP_DB_URL|SKIP: ERP_DB_URL' "$T8"; then
    bad "用例打了 SKIP（= 假绿）：$T8"
fi

# ---------- 3) 整套件回归：采购单 handler 全量 ----------
if [ "$SKIP_MODULE" = "1" ]; then
    info "SKIP_MODULE=1，跳过采购整套件"
else
    echo "== 3) 整套件回归：purchase_handler::tests 全量（确认新夹具没踩坏兄弟用例）"
    TM="$TMP/m418_tm_$$.log"
    run_tests 'business::erp::purchase::purchase_handler::tests' "$TM"
    RESULT="$(grep -E 'test result:' "$TM" | head -1)"
    if printf '%s' "$RESULT" | grep -qE '0 failed'; then
        ok "整套件 0 failed：$RESULT"
    else
        bad "整套件有红：$RESULT"
        grep -E '^test .* FAILED' "$TM" | head -10 | sed 's/^/       /'
    fi
fi

# ---------- 4) 夹具残留：跑完必须回到基线 ----------
echo "== 4) 夹具残留（用例自清）"
ORDERS_NOW="$(psql_q "SELECT count(*) FROM biz_purchase_order WHERE supplier_id = '$F_SUPPLIER'::uuid")"
if [ "$ORDERS_NOW" = "$BASELINE_ORDERS" ]; then
    ok "夹具供应商采购单回到基线（$BASELINE_ORDERS 张）"
else
    bad "夹具供应商采购单未回到基线：基线 $BASELINE_ORDERS，现在 $ORDERS_NOW"
fi
GUARD_C="$(psql_q "SELECT count(*) FROM biz_contract WHERE id IN ($F_GUARD_CONTRACTS)")"
if [ "$GUARD_C" = "0" ]; then
    ok "四张额度夹具合同已清（P2A3-HT-CAP30/FULL/SHORT/OTHER 残留 0）"
else
    bad "四张额度夹具合同残留 $GUARD_C 行"
fi

# ---------- 5) 变异反证：这 8 条真能红 ----------
if [ "$MUTATE" != "1" ]; then
    echo "== 5) 变异反证：MUTATE 未开，跳过（MUTATE=1 可跑，约 15 分钟）"
else
    echo "== 5) 变异反证（改 1 处 → 恰好对应用例红 → 还原 sha256 一致）"
    PM_SHA="$(sha256sum "$PMODEL" | awk '{print $1}')"
    CM_SHA="$(sha256sum "$CMODEL" | awk '{print $1}')"
    cp "$PMODEL" "$TMP/pm_$$.bak"
    cp "$CMODEL" "$TMP/cm_$$.bak"

    expect_red() { # 日志 期望红的用例名 说明
        local file="$1" target="$2" label="$3"
        local reds greens
        reds="$(grep -E '^test .* FAILED' "$file" | sed 's/^test //; s/ \.\.\. FAILED//' | tr '\n' ' ')"
        greens="$(grep -cE '^test .* ok$' "$file")"
        if printf '%s' "$reds" | grep -q "$target" && [ "$greens" = "7" ]; then
            ok "$label → 恰好 1 条红（$target），其余 7 条绿"
        else
            bad "$label → 期望恰好 $target 红 + 7 绿，实得：红 [$reds] 绿 $greens"
        fi
    }

    # M1：去掉「改单排除自己」
    python3 - "$PMODEL" <<'PY'
import re, sys
p = sys.argv[1]
s = open(p, encoding='utf-8').read()
s = s.replace('        sql.push_str(" AND id <> ?::uuid");\n        params.push(Value::from(&exclude_id));\n',
              '        let _ = exclude_id; // M418-M1\n')
open(p, 'w', encoding='utf-8').write(s)
PY
    T1="$TMP/m418_m1_$$.log"
    run_tests 'business::erp::purchase::purchase_handler::tests::m418' "$T1"
    expect_red "$T1" 'm418_update_excludes_self_so_a_full_contract_stays_editable' "M1 去掉 exclude"
    cp "$TMP/pm_$$.bak" "$PMODEL"

    # M2：去掉「作废不计入」
    python3 - "$PMODEL" <<'PY'
import sys
p = sys.argv[1]
s = open(p, encoding='utf-8').read()
s = s.replace("WHERE contract_id = ?::uuid AND status <> 'CANCELLED' AND deleted_at IS NULL",
              "WHERE contract_id = ?::uuid AND deleted_at IS NULL")
open(p, 'w', encoding='utf-8').write(s)
PY
    T2="$TMP/m418_m2_$$.log"
    run_tests 'business::erp::purchase::purchase_handler::tests::m418' "$T2"
    expect_red "$T2" 'm418_cancelled_order_frees_quota' "M2 去掉 status <> 'CANCELLED'"
    cp "$TMP/pm_$$.bak" "$PMODEL"

    # M3：去掉行锁（并发唯一保证）
    python3 - "$CMODEL" <<'PY'
import sys
p = sys.argv[1]
s = open(p, encoding='utf-8').read()
s = s.replace("FROM biz_contract c WHERE c.id = ?::uuid AND c.deleted_at IS NULL FOR UPDATE",
              "FROM biz_contract c WHERE c.id = ?::uuid AND c.deleted_at IS NULL")
open(p, 'w', encoding='utf-8').write(s)
PY
    M3_RED=0
    for round in 1 2; do
        T3="$TMP/m418_m3_${round}_$$.log"
        run_tests 'business::erp::purchase::purchase_handler::tests::m418_concurrent_creates_only_one_wins' "$T3"
        if grep -qE '^test .* FAILED' "$T3"; then M3_RED=$((M3_RED+1)); fi
    done
    if [ "$M3_RED" = "2" ]; then
        ok "M3 去掉 FOR UPDATE → 并发用例连跑 2 轮全红（不是偶发）"
    else
        bad "M3 去掉 FOR UPDATE → 2 轮里只红了 $M3_RED 轮（并发护栏不可靠）"
    fi
    cp "$TMP/cm_$$.bak" "$CMODEL"

    if [ "$(sha256sum "$PMODEL" | awk '{print $1}')" = "$PM_SHA" ]; then
        ok "purchase_model.rs 还原后 sha256 一致（$PM_SHA）"
    else
        bad "purchase_model.rs 还原后 sha256 不一致！"
    fi
    if [ "$(sha256sum "$CMODEL" | awk '{print $1}')" = "$CM_SHA" ]; then
        ok "contract_model.rs 还原后 sha256 一致（$CM_SHA）"
    else
        bad "contract_model.rs 还原后 sha256 不一致！"
    fi
    rm -f "$TMP/pm_$$.bak" "$TMP/cm_$$.bak"
fi

# ---------- 汇总 ----------
printf '\n== 汇总：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
