#!/usr/bin/env bash
# ============================================================================
# M4-19 合同执行量统计（**已执行 / 合同额**）· 演练
#
# 卡：t_08ae874c（M4-19【M4·采购】合同执行量统计）
# 上游：t_b90d4761（M4-18 采购单合同额度守卫）—— 本卡的口径必须与它同源，
#       否则会出现「报表说还剩 10、下单却被拒」的争议。
#
# 这一卡交了什么（四件套 + 视图两枚 + 菜单一行 + 两个只读接口）：
#   · `V0458__erp_contract_execution.{up,down}.sql`：两个视图 + 执行量按钮 + 三角色授权
#   · `contract_execution_{dto,model,handler}.rs`：纯判据 / 只读取数 / 两个接口
#   · `contract_execution_db_tests.rs`：12 条真库用例（真入口造单，作废 / 删除走真实链路）
#
# 本脚演练的七段（任何一段红就 FAIL）：
#   0) 模板库克隆 → 回放 V0405…HEAD（模板停在 V0404，不回放的话用例会在「列不存在」上假红）
#   1) 迁移结构：两个视图 / 关键列 / 按钮行 / 三角色授权 / 页面行仍在
#   2) 视图口径（**SQL 直查**，不经 Rust）：采购累加 / 作废释放 / 软删释放 / 销售侧 /
#      服务租赁无下游 / 软删合同不可见 / 币种串了显形 / 执行率精度 / 单头=单据级合计
#   3) 幂等与回滚：up 重跑 no-op；down 撤干净；再 up 复原
#   4) 真库用例：12 条（断言 `12 passed`，SKIP 假绿会被第 4 段判红）
#   5) 合同模块回归：整个 `business::erp::contract::`（109 条，含权限码/菜单对拍）
#   6) 门禁：cargo fmt --check / cargo clippy --lib -D warnings
#   7) 残留：演练库自己 DROP，`cp_m419%` 0 行
#
# 用法：
#   bash scripts/verify/m419_contract_execution_drill.sh              # 99 → 145（默认）
#   PG_HOST=127.0.0.1 bash scripts/verify/m419_contract_execution_drill.sh   # 在 145 上跑
#   M419_BAD_URL=1 bash scripts/verify/m419_contract_execution_drill.sh      # 有牙自检（期望 rc=1）
#   KEEP_DB=1 ...      # 留库排查
#   SKIP_GATES=1 ...   # 跳过 fmt/clippy（只在已单独跑过时用）
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145、库名含 drill/test/e2e、脚本自己 DROP；
# 口令复用 `$PGPASSWORD`（不写占位符 —— 那样真库用例连不上还打 SKIP 记 ok，是假绿）。
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
DB="${M419_DB:-cp_m419_exec_drill_$$}"
KEEP_DB="${KEEP_DB:-0}"
SKIP_GATES="${SKIP_GATES:-0}"
BAD_URL="${M419_BAD_URL:-0}"
TMP="${TMPDIR:-/tmp}"
mkdir -p "$TMP"
LOG="$TMP/m419_$$.log"
PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }

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
    else
        psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
    fi
    rm -f "$LOG"
}
trap cleanup EXIT

# 断言：SQL 结果必须等于期望值（trim 后逐字符比）
check() { # label sql expect
    local label="$1" sql="$2" expect="$3" got
    got="$(psql_q "$sql" | tr -d '[:space:]')"
    if [ "$got" = "$expect" ]; then
        ok "$label = $expect"
    else
        bad "$label：期望 [$expect] 实际 [$got]"
    fi
}

printf '== M4-19 合同执行量统计演练 · PG %s:%s / 库 %s ==\n' "$PG_HOST" "$PG_PORT" "$DB"

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
    ok "回放 V0405…HEAD 共 $APPLIED 条迁移，0 失败"
else
    printf '\n  迁移失败 %d 条，后续段无意义 → 提前退出\n' "$MIG_FAIL"
    exit 1
fi

# ---------- 1) 迁移结构 ----------
echo "== 1) 迁移结构：两个视图 / 按钮行 / 三角色授权"
check "视图 v_contract_execution 存在" \
      "SELECT count(*) FROM pg_views WHERE schemaname='public' AND viewname='v_contract_execution'" "1"
check "视图 v_contract_execution_order 存在" \
      "SELECT count(*) FROM pg_views WHERE schemaname='public' AND viewname='v_contract_execution_order'" "1"
check "单头视图列数" \
      "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='v_contract_execution'" "37"
check "单据视图列数" \
      "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='v_contract_execution_order'" "12"
check "关键列齐（executed_amount / remaining_amount / exec_state / downstream_type / currency_mismatch_doc_count）" \
      "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='v_contract_execution' AND column_name IN ('executed_amount','remaining_amount','exec_state','downstream_type','currency_mismatch_doc_count')" "5"
check "按钮行（…4498 / menu_type=3 / 父=合同管理 / sort=50 / 标题≤12）" \
      "SELECT count(*) FROM sys_menu WHERE id='20000000-0000-0000-0000-000000004498' AND menu_type=3 AND auth_code='business:erp:contract:execution' AND parent_id='20000000-0000-0000-0000-000000001100' AND sort_order=50 AND length(title)<=12 AND deleted_at IS NULL" "1"
check "合同管理页面行仍在（V0197 的资产，down 不许连带删）" \
      "SELECT count(*) FROM sys_menu WHERE id='20000000-0000-0000-0000-000000001100' AND deleted_at IS NULL" "1"
check "三角色授权（采购经理 / 销售主管 / 财务经理）" \
      "SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id=rm.role_id AND r.deleted_at IS NULL WHERE rm.menu_id='20000000-0000-0000-0000-000000004498' AND r.role_code IN ('PURCHASE_MANAGER','SALES_SUPERVISOR','FINANCE_MANAGER')" "3"
check "其它角色没被顺手授权（授权行总数 = 3）" \
      "SELECT count(*) FROM sys_role_menu WHERE menu_id='20000000-0000-0000-0000-000000004498'" "3"

# ---------- 2) 视图口径（SQL 直查，不经 Rust） ----------
echo "== 2) 视图口径：SQL 直查（累加 / 释放 / 销售侧 / 无下游 / 币种 / 精度）"
# 夹具：3 张合同 + 4 张单据（都固定 UUID，段末清场）
psql_q "DELETE FROM biz_purchase_order WHERE contract_id IN ('00000000-0000-4000-8000-0000041900f1'::uuid,'00000000-0000-4000-8000-0000041900f2'::uuid,'00000000-0000-4000-8000-0000041900f3'::uuid)" >/dev/null
psql_q "DELETE FROM biz_sales_order WHERE contract_id IN ('00000000-0000-4000-8000-0000041900f2'::uuid)" >/dev/null
psql_q "DELETE FROM biz_contract WHERE contract_no LIKE 'M419D-%'" >/dev/null

FIXTURE_SQL="$TMP/m419_fixture_$$.sql"
cat > "$FIXTURE_SQL" <<'SQL'
INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type, party_b_id,
                          party_b_name, amount, currency_code, sign_date, start_date, end_date, status)
VALUES
  ('00000000-0000-4000-8000-0000041900f1'::uuid, 'M419D-HT-PUR', 'M419 演练采购合同', 'PURCHASE', 'SUPPLIER',
   gen_random_uuid(), '演练供应商', 1000.0000, 'CNY', CURRENT_DATE - 30, CURRENT_DATE - 30, CURRENT_DATE + 30, 'ACTIVE'),
  ('00000000-0000-4000-8000-0000041900f2'::uuid, 'M419D-HT-SAL', 'M419 演练销售合同', 'SALES', 'CUSTOMER',
   gen_random_uuid(), '演练客户', 500.0000, 'CNY', CURRENT_DATE - 30, CURRENT_DATE - 30, CURRENT_DATE + 30, 'ACTIVE'),
  ('00000000-0000-4000-8000-0000041900f3'::uuid, 'M419D-HT-SRV', 'M419 演练服务合同', 'SERVICE', 'SUPPLIER',
   gen_random_uuid(), '演练供应商', 100.0000, 'CNY', CURRENT_DATE - 30, CURRENT_DATE - 30, CURRENT_DATE + 30, 'ACTIVE');

INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status, currency_code,
                                total_amount, total_tax, total_with_tax, purchase_type, contract_id, contract_no_snapshot)
VALUES
  ('00000000-0000-4000-8000-0000041900e1'::uuid, 'M419D-PO-ALIVE', gen_random_uuid(), CURRENT_DATE - 5,
   gen_random_uuid(), 'APPROVED', 'CNY', 177, 23, 200, 'CONTRACT',
   '00000000-0000-4000-8000-0000041900f1'::uuid, 'M419D-HT-PUR'),
  ('00000000-0000-4000-8000-0000041900e2'::uuid, 'M419D-PO-CANCEL', gen_random_uuid(), CURRENT_DATE - 4,
   gen_random_uuid(), 'CANCELLED', 'CNY', 265.4867, 34.5133, 300, 'CONTRACT',
   '00000000-0000-4000-8000-0000041900f1'::uuid, 'M419D-HT-PUR'),
  ('00000000-0000-4000-8000-0000041900e3'::uuid, 'M419D-PO-DELETED', gen_random_uuid(), CURRENT_DATE - 3,
   gen_random_uuid(), 'APPROVED', 'CNY', 353.9823, 46.0177, 400, 'CONTRACT',
   '00000000-0000-4000-8000-0000041900f1'::uuid, 'M419D-HT-PUR');

UPDATE biz_purchase_order SET deleted_at = now() WHERE id = '00000000-0000-4000-8000-0000041900e3'::uuid;

-- 币种串了的单据：USD 单挂 CNY 合同（建单入口不收币种，只能靠改库 / 导入造出来）
INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status, currency_code,
                                total_amount, total_tax, total_with_tax, purchase_type, contract_id, contract_no_snapshot)
VALUES ('00000000-0000-4000-8000-0000041900e4'::uuid, 'M419D-PO-USD', gen_random_uuid(), CURRENT_DATE - 2,
        gen_random_uuid(), 'DRAFT', 'USD', 88.4956, 11.5044, 100, 'CONTRACT',
        '00000000-0000-4000-8000-0000041900f1'::uuid, 'M419D-HT-PUR');

INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status, currency_code,
                             total_amount, total_tax, total_with_tax, contract_id)
VALUES ('00000000-0000-4000-8000-0000041900d1'::uuid, 'M419D-SO-1', gen_random_uuid(), CURRENT_DATE - 1,
        'APPROVED', 'CNY', 442.4779, 57.5221, 500,
        '00000000-0000-4000-8000-0000041900f2'::uuid);

-- 软删合同：不许出现在报表里
INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type, party_b_id,
                          party_b_name, amount, currency_code, sign_date, start_date, end_date, status, deleted_at)
VALUES ('00000000-0000-4000-8000-0000041900f9'::uuid, 'M419D-HT-DEL', 'M419 演练已删合同', 'PURCHASE', 'SUPPLIER',
        gen_random_uuid(), '演练供应商', 700.0000, 'CNY', CURRENT_DATE - 30, CURRENT_DATE - 30,
        CURRENT_DATE + 30, 'ACTIVE', now());
SQL
if psql_file "$FIXTURE_SQL"; then
    ok "演练夹具造好（3 张合同 + 4 张采购单 + 1 张销售单 + 1 张软删合同）"
else
    bad "演练夹具造失败：$(tail -3 "$LOG" | tr '\n' ' ')"
fi
rm -f "$FIXTURE_SQL"

PUR="'00000000-0000-4000-8000-0000041900f1'::uuid"
SAL="'00000000-0000-4000-8000-0000041900f2'::uuid"
SRV="'00000000-0000-4000-8000-0000041900f3'::uuid"
DEL="'00000000-0000-4000-8000-0000041900f9'::uuid"

# 已执行额 = 存活 200 + USD 100（作废 300 / 软删 400 不算）
check "采购合同已执行额（作废/软删不算，USD 照加）" \
      "SELECT executed_amount FROM v_contract_execution WHERE contract_id=$PUR" "300.0000"
check "采购合同已执行净额（不含税）" \
      "SELECT executed_amount_net FROM v_contract_execution WHERE contract_id=$PUR" "265.4956"
check "采购合同剩余额（1000 − 300）" \
      "SELECT remaining_amount FROM v_contract_execution WHERE contract_id=$PUR" "700.0000"
check "采购合同执行率（6 位小数）" \
      "SELECT exec_amount_rate FROM v_contract_execution WHERE contract_id=$PUR" "0.300000"
check "采购合同执行状态" \
      "SELECT exec_state FROM v_contract_execution WHERE contract_id=$PUR" "PARTIAL"
check "计入单据数（2 张）" \
      "SELECT order_doc_count FROM v_contract_execution WHERE contract_id=$PUR" "2"
check "释放单据数（作废 1 + 软删 1）" \
      "SELECT released_doc_count FROM v_contract_execution WHERE contract_id=$PUR" "2"
check "币种串了的计入单据数" \
      "SELECT currency_mismatch_doc_count FROM v_contract_execution WHERE contract_id=$PUR" "1"
check "采购单计入数 / 销售单计入数" \
      "SELECT purchase_doc_count || '|' || sales_doc_count FROM v_contract_execution WHERE contract_id=$PUR" "2|0"
check "下游类型按合同类型分派（采购）" \
      "SELECT downstream_type FROM v_contract_execution WHERE contract_id=$PUR" "PURCHASE_ORDER"

# 销售合同：恰好用满（500/500）⇒ DONE（不是 OVER）
check "销售合同已执行额" "SELECT executed_amount FROM v_contract_execution WHERE contract_id=$SAL" "500.0000"
check "销售合同剩余额" "SELECT remaining_amount FROM v_contract_execution WHERE contract_id=$SAL" "0.0000"
check "销售合同执行率" "SELECT exec_amount_rate FROM v_contract_execution WHERE contract_id=$SAL" "1.000000"
check "销售合同执行状态（边界：等于合同额 = DONE）" \
      "SELECT exec_state FROM v_contract_execution WHERE contract_id=$SAL" "DONE"
check "销售合同下游类型" "SELECT downstream_type FROM v_contract_execution WHERE contract_id=$SAL" "SALES_ORDER"

# 服务合同：没有下游单据
check "服务合同执行状态" "SELECT exec_state FROM v_contract_execution WHERE contract_id=$SRV" "NO_DOWNSTREAM"
check "服务合同剩余 = 合同额原样" "SELECT remaining_amount FROM v_contract_execution WHERE contract_id=$SRV" "100.0000"
check "服务合同 downstream_type 为空" \
      "SELECT COALESCE(downstream_type,'NULL') FROM v_contract_execution WHERE contract_id=$SRV" "NULL"

# 软删合同不可见
check "已软删合同不进报表" "SELECT count(*) FROM v_contract_execution WHERE contract_id=$DEL" "0"

# 单据级视图：counted 判定 + 币种标记
check "单据级：作废单 counted=false" \
      "SELECT counted FROM v_contract_execution_order WHERE order_no='M419D-PO-CANCEL'" "f"
check "单据级：软删单 counted=false" \
      "SELECT counted FROM v_contract_execution_order WHERE order_no='M419D-PO-DELETED'" "f"
check "单据级：USD 单 currency_matched=false 但 counted=true" \
      "SELECT currency_matched::text || '|' || counted::text FROM v_contract_execution_order WHERE order_no='M419D-PO-USD'" "false|true"

# 单头合计 = 单据级 counted 之和（两套聚合不许分叉）
check "单头合计 = 单据级 counted 合计" \
      "SELECT ((SELECT COALESCE(SUM(total_with_tax),0) FROM v_contract_execution_order WHERE contract_id=$PUR AND counted) = (SELECT executed_amount FROM v_contract_execution WHERE contract_id=$PUR))::text" "true"

# 清场（夹具只动自己造的，按固定 UUID / 合同号前缀）
psql_q "DELETE FROM biz_purchase_order WHERE contract_id IN ($PUR,$SAL,$SRV,$DEL)" >/dev/null
psql_q "DELETE FROM biz_sales_order WHERE contract_id IN ($PUR,$SAL,$SRV,$DEL)" >/dev/null
psql_q "DELETE FROM biz_contract WHERE contract_no LIKE 'M419D-%'" >/dev/null
check "演练夹具清场 0 残留（合同）" "SELECT count(*) FROM biz_contract WHERE contract_no LIKE 'M419D-%'" "0"
check "演练夹具清场 0 残留（单据）" \
      "SELECT count(*) FROM v_contract_execution_order WHERE order_no LIKE 'M419D-%'" "0"

# ---------- 3) 幂等与回滚 ----------
echo "== 3) 幂等与回滚：up 重跑 / down 撤干净 / 再 up 复原"
MIG_UP="$MIG_DIR/V0458__erp_contract_execution.up.sql"
MIG_DOWN="$MIG_DIR/V0458__erp_contract_execution.down.sql"
if psql_file "$MIG_UP"; then ok "up 重跑（幂等）通过"; else bad "up 重跑失败：$(tail -2 "$LOG" | tr '\n' ' ')"; fi
check "重跑后视图仍在" "SELECT count(*) FROM pg_views WHERE schemaname='public' AND viewname IN ('v_contract_execution','v_contract_execution_order')" "2"
if psql_file "$MIG_DOWN"; then ok "down 执行通过"; else bad "down 失败：$(tail -2 "$LOG" | tr '\n' ' ')"; fi
check "down 之后两个视图都没了" \
      "SELECT count(*) FROM pg_views WHERE schemaname='public' AND viewname IN ('v_contract_execution','v_contract_execution_order')" "0"
check "down 之后按钮行没了" \
      "SELECT count(*) FROM sys_menu WHERE id='20000000-0000-0000-0000-000000004498'" "0"
check "down 之后授权没了" \
      "SELECT count(*) FROM sys_role_menu WHERE menu_id='20000000-0000-0000-0000-000000004498'" "0"
check "down 之后合同管理页面行仍在（不是本卡的资产）" \
      "SELECT count(*) FROM sys_menu WHERE id='20000000-0000-0000-0000-000000001100' AND deleted_at IS NULL" "1"
if psql_file "$MIG_UP"; then ok "再 up 复原通过"; else bad "再 up 失败：$(tail -2 "$LOG" | tr '\n' ' ')"; fi
check "复原后视图在位" "SELECT count(*) FROM pg_views WHERE schemaname='public' AND viewname IN ('v_contract_execution','v_contract_execution_order')" "2"

# ---------- 4) 真库用例（12 条） ----------
echo "== 4) 真库用例：contract_execution_db_tests（12 条）"
if [ "$BAD_URL" = "1" ]; then
    TEST_URL="postgres://cloudpivot:wrong@127.0.0.1:1/definitely_missing"
    info "有牙自检：故意用坏连接串跑用例（期望这一整轮 rc=1）"
else
    TEST_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
fi
TESTOUT="$TMP/m419_tests_$$.out"
( cd "$BACKEND" && ERP_DB_URL="$TEST_URL" cargo test --lib \
    business::erp::contract::contract_execution_db_tests -- --test-threads=1 --nocapture ) >"$TESTOUT" 2>&1
TEST_RC=$?
TESTS_PASSED="$(grep -oE 'ok\. [0-9]+ passed' "$TESTOUT" | grep -oE '[0-9]+' | head -1)"
# 有牙判据：每条用例都打印一行 `[M4-19] …`，**数标记**才防得住「静默 SKIP 记 ok」的假绿。
# 连接串写错时 cargo 仍会报 `12 passed`（用例 read 了 else return），只有标记数会掉到 0。
MARKERS="$(grep -o '\[M4-19\]' "$TESTOUT" | wc -l | tr -d ' ')"
if [ "$TEST_RC" -eq 0 ] && [ "${TESTS_PASSED:-0}" = "12" ] && [ "${MARKERS:-0}" = "12" ]; then
    ok "真库用例 12 passed / 12 条正向标记（0 failed / 0 ignored）"
else
    bad "真库用例未达标：rc=$TEST_RC passed=${TESTS_PASSED:-0} markers=${MARKERS:-0}（期望 12/12）。尾部输出："
    tail -25 "$TESTOUT" | sed 's/^/      /'
fi
if [ "$BAD_URL" = "1" ]; then
    rm -f "$TESTOUT"
    printf '\n有牙自检结束：FAIL=%d（期望 > 0 且 rc=1）\n' "$FAIL"
    [ "$FAIL" -gt 0 ] && exit 1 || exit 0
fi
rm -f "$TESTOUT"

# ---------- 5) 合同模块回归 ----------
echo "== 5) 合同模块回归：business::erp::contract::"
REGOUT="$TMP/m419_reg_$$.out"
( cd "$BACKEND" && ERP_DB_URL="$TEST_URL" cargo test --lib business::erp::contract:: -- --test-threads=1 ) >"$REGOUT" 2>&1
REG_RC=$?
REG_PASSED="$(grep -oE 'ok\. [0-9]+ passed' "$REGOUT" | grep -oE '[0-9]+' | head -1)"
REG_FAILED="$(grep -oE '[0-9]+ failed' "$REGOUT" | grep -oE '[0-9]+' | head -1)"
if [ "$REG_RC" -eq 0 ] && [ "${REG_FAILED:-1}" = "0" ]; then
    ok "合同模块回归全绿（passed=${REG_PASSED:-?} failed=0）"
else
    bad "合同模块回归有红：rc=$REG_RC passed=${REG_PASSED:-?} failed=${REG_FAILED:-?}"
    tail -25 "$REGOUT" | sed 's/^/      /'
fi
rm -f "$REGOUT"

# ---------- 6) 门禁 ----------
echo "== 6) 门禁：fmt / clippy"
if [ "$SKIP_GATES" = "1" ]; then
    info "SKIP_GATES=1：本轮不跑 fmt/clippy"
else
    if ( cd "$BACKEND" && cargo fmt --check ) >"$LOG" 2>&1; then
        ok "cargo fmt --check rc=0"
    else
        bad "cargo fmt --check 不干净：$(tail -3 "$LOG" | tr '\n' ' ')"
    fi
    if ( cd "$BACKEND" && cargo clippy --lib -- -D warnings ) >"$LOG" 2>&1; then
        ok "cargo clippy --lib -D warnings rc=0"
    else
        bad "clippy 有告警/错误：$(tail -5 "$LOG" | tr '\n' ' ')"
    fi
fi

# ---------- 7) 残留 ----------
echo "== 7) 残留检查"
check "演练库夹具 0 残留（合同）" "SELECT count(*) FROM biz_contract WHERE contract_no LIKE 'M419D-%' OR contract_no LIKE 'M419-%'" "0"

printf '\n===== M4-19 演练结果：PASS=%d FAIL=%d =====\n' "$PASS" "$FAIL"
if [ "$FAIL" -gt 0 ]; then
    printf '库 %s 保留以便排查（KEEP_DB=1 时），否则随 trap 删除\n' "$DB"
    exit 1
fi
exit 0
