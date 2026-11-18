#!/usr/bin/env bash
# ============================================================================
# M4-17 采购订单挂合同字段（`contract_id`）· 演练
#
# 卡：t_566fc2dc（M4-17【M4·采购】采购订单加 contract_id 字段 —— 迁移 + DTO）
#
# # 这一卡为什么没有新迁移（重要，别当漏做）
#
# 卡面要求的「迁移 + DTO」在仓库里**已经完整落地**，来源是 ARCH-2 合同勾稽批次的
# `V0198__erp_po_contract_bridge`（本次核对时该迁移已在 335 条版本化迁移之内，
# 模板库 `cloudpivot_test` 里四列/三约束/一索引全在）。再补一条同义迁移会：
#   ① 与 AGENTS 约定 6「跨目录全局唯一、先落地者定号」相冲突，白占号；
#   ② 加不出任何新约束（现有 `ck_biz_po_contract_pair` 已是双向钉死）；
#   ③ 让「字段到底谁负责」变成两处真相。
#
# 所以本卡交的是**这个字段的验收**：一条能反复跑的命令，把
#   「库结构 → 约束反证 → 代码接线 → 真库语义 → 夹具残留」
# 五段钉住，将来谁重构采购单 DTO / 模型层，掉一截就会在这里红。
#
# 脚本验的五件事（逐条对应卡面「迁移 + DTO」）：
#   ① 迁移来源：V0198 已在 schema_migrations 里 executed，且记录的 checksum 与仓库文件 sha256
#      一致（指纹纪律）；up/down 的重复执行守卫是 existence-guard 形状（静态可判）。
#      四列 / 两约束（+入库单那条）/ 一**部分**索引都在；`contract_id` 上**没有外键**
#      （设计 DEC-02：弱引用，别顺手补 FK）。
#      ⚠ 实测发现：V0198 的 up 在**已升到 HEAD 的库**上重跑会被自己的 S2-① 前置体检拦下
#      （后来的迁移把 SOURCELESS 也允许带供应商，与 V0198 时的假设冲突）。已应用迁移不可改，
#      所以本脚本不拿「重跑 up」当验收面 —— 详见第 2 段输出与 README 的「已知事实」。
#   ② 约束反证（写库负断言）：CONTRACT 缺 contract_id / 缺合同号快照 / SPOT 带 contract_id /
#      枚举拼错，四种都必须被库拦下；合法 CONTRACT 行必须插得进（否则是「拦过头」）
#   ③ 接线：模型列清单 / 行映射 / 分页筛选走部分索引 / DTO 入参出参 / 写库 NULLIF 归一 /
#      合同额度现算（`committed_amount`）/ handler 解析与额度守卫两处调用 / 前端只发 contract_id
#   ④ 真库语义：`purchase_handler::tests` 里 13 条合同相关用例（CONTRACT 派生供应商+双快照 /
#      SPOT 拒绝 / 合同不可用 / 有效期 / 额度超限 / 分页下发快照列 / 无类型改单保持）
#   ⑤ 夹具残留：本脚本自己把用例夹具按固定 id 清掉，并断言回到跑测试前的基线
#
# 用法：
#   bash scripts/verify/m417_purchase_order_contract_id_drill.sh                  # 99 → 145（默认）
#   PG_HOST=127.0.0.1 bash scripts/verify/m417_purchase_order_contract_id_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash ...                                                            # 留库排查
#   SKIP_TESTS=1 bash ...                                                         # 只验库+接线，不跑 cargo
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名含 drill；脚本自己 DROP；
# 口令复用 `$PGPASSWORD`（不写占位符 —— 那样真库用例连不上还打 SKIP 记 ok，是假绿）。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIG_DIR="$ROOT/backend/app/migrations/business"
BACKEND="$ROOT/backend"
V0198_UP="$MIG_DIR/V0198__erp_po_contract_bridge.up.sql"
V0198_DOWN="$MIG_DIR/V0198__erp_po_contract_bridge.down.sql"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="${M417_DB:-cp_m417_po_contract_drill_$$}"
KEEP_DB="${KEEP_DB:-0}"
SKIP_TESTS="${SKIP_TESTS:-0}"
TMP="${TMPDIR:-/tmp}"
mkdir -p "$TMP"
LOG="$TMP/m417_drill_$$.log"
TEST_LOG="$TMP/m417_tests_$$.log"
PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }

# 夹具 id（与 purchase_handler::tests 的 FIXTURES 逐字一致；脚本只清自己的）
F_SUPPLIER='d2a20001-0000-4000-8000-000000000001'
F_CONTRACTS="'d2a20001-0000-4000-8000-0000000000c1'::uuid,'d2a20001-0000-4000-8000-0000000000c2'::uuid,'d2a20001-0000-4000-8000-0000000000c3'::uuid"

case "$DB" in
    *drill*|*e2e*|*test*) : ;;
    *) printf '库名 %s 不含 drill/e2e/test，拒绝在它上面跑（rc=2）\n' "$DB"; exit 2 ;;
esac

OWN_DB=1
if [ -n "${M417_DB:-}" ]; then OWN_DB=0; fi

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }
psql_file()  { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$1" >"$LOG" 2>&1; }
# 期望失败：返回 0 = 被库拦下了；且必须是被**指定的那条约束**拦下（防「语法错也算拦下」的假绿）
# 期望约束名支持用 `|` 写多个（同一行可能同时违反两条 CHECK，PG 只报先命中的那条）
psql_expect_fail() { # sql 期望约束名(可 |) 说明
    if psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -c "$1" >"$LOG" 2>&1; then
        bad "$3（库**没有**拦下 —— 约束失效）"
    elif grep -Eq "violates check constraint \"($2)\"" "$LOG"; then
        ok "$3（拦下：$(grep -oE 'violates check constraint "[a-z_]+"' "$LOG" | head -1 | sed 's/.*"\(.*\)"/\1/')）"
    else
        bad "$3（失败原因不是 $2：$(head -2 "$LOG" | tr '\n' ' ')）"
    fi
}
psql_expect_ok() { # sql 说明
    if psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -c "$1" >"$LOG" 2>&1; then
        ok "$2"
    else
        bad "$2（被拒：$(tail -2 "$LOG" | tr '\n' ' ')）"
    fi
}
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

printf '== M4-17 采购订单 contract_id 字段演练 · PG %s:%s / 库 %s ==\n' "$PG_HOST" "$PG_PORT" "$DB"

# ---------- 0) 前置：模板库 + 克隆 ----------
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

# ---------- 1) 推到仓库当前版本（本卡不改迁移，但要证明「HEAD 上这四列在」） ----------
echo "== 1) 应用业务迁移到仓库当前版本"
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

# ---------- 2) V0198：迁移来源 + 指纹 + 为什么不在 HEAD 上重跑 ----------
echo "== 2) V0198（本字段的来源迁移）：已应用记录 / 指纹 / 回滚脚本形状"
assert_eq "1" "$(psql_q "SELECT count(*) FROM schema_migrations WHERE kind='versioned' AND version='198' AND success AND name='erp_po_contract_bridge'")" \
    "schema_migrations 记录 V0198 = executed/success"
assert_eq "$(sha256sum "$V0198_UP" | cut -d' ' -f1)" \
    "$(psql_q "SELECT checksum FROM schema_migrations WHERE kind='versioned' AND version='198'")" \
    "已应用 V0198 的 checksum 与仓库文件 sha256 一致（AGENTS 约定 6：已应用迁移不可改）"
# down 必须每条 DROP 都带 IF EXISTS / existence-guard（重跑 no-op 的形状，静态可判）
DOWN_DROPS="$(grep -cE '^\s*(ALTER TABLE .*DROP CONSTRAINT|DROP INDEX)' "$V0198_DOWN")"
DOWN_GUARDED="$(grep -cE '^\s*ALTER TABLE .*DROP CONSTRAINT IF EXISTS|^\s*DROP INDEX IF EXISTS' "$V0198_DOWN")"
assert_eq "$DOWN_DROPS" "$DOWN_GUARDED" "down 的 $DOWN_DROPS 条 DROP 全部带 IF EXISTS（可重跑）"
assert_eq "1" "$(grep -c "IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_po_contract_pair')" "$V0198_UP")" \
    "up 加约束走 existence-guard（重复执行 no-op）"
assert_eq "1" "$(grep -c 'ADD COLUMN IF NOT EXISTS contract_id' "$V0198_UP")" \
    "up 加列走 ADD COLUMN IF NOT EXISTS（重复执行 no-op）"
info "不在 HEAD 上重跑 V0198 up/down 的原因（实测发现，写进 README）："
info "  up 的 S2-① 前置体检要求「非 PURCHASE 入库不得带供应商」，而后来的迁移把"
info "  ck_biz_si_supplier_scope 放宽到 SOURCELESS 也可带供应商（现库里有 12 行 SOURCELESS 带供应商）。"
info "  于是 down（删四列 + 删该约束）之后再 up 会被 S2-① 拦下 —— 这条迁移只对 ≤V0198 的库可逆；"
info "  已应用迁移按约定 6 不可改，所以本脚本不拿「重跑 up」当验收面，改验结构与真库语义。"

# ---------- 3) 库结构断言（独立于迁移自检与 Rust） ----------
echo "== 3) 库结构：列 / 约束 / 部分索引 / 无外键 / 注释"
assert_eq "uuid|YES|" "$(psql_q "SELECT data_type||'|'||is_nullable||'|'||COALESCE(column_default,'') FROM information_schema.columns WHERE table_name='biz_purchase_order' AND column_name='contract_id'")" \
    "contract_id：uuid / 可空 / 无默认值（SPOT 单就是 NULL）"
assert_eq "character varying|NO|'SPOT'::character varying" "$(psql_q "SELECT data_type||'|'||is_nullable||'|'||column_default FROM information_schema.columns WHERE table_name='biz_purchase_order' AND column_name='purchase_type'")" \
    "purchase_type：varchar / NOT NULL / 默认 'SPOT'（存量单口径）"
assert_eq "2" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_name='biz_purchase_order' AND ((column_name='contract_no_snapshot' AND character_maximum_length=64) OR (column_name='supplier_name_snapshot' AND character_maximum_length=128))")" \
    "两个快照列长度分别 64 / 128"
assert_eq "1" "$(psql_q "SELECT count(*) FROM pg_constraint WHERE conname='ck_biz_po_contract_pair' AND contype='c'")" \
    "双向钉死约束 ck_biz_po_contract_pair 存在"
assert_eq "1" "$(psql_q "SELECT count(*) FROM pg_constraint WHERE conname='ck_biz_po_purchase_type' AND contype='c'")" \
    "枚举约束 ck_biz_po_purchase_type 存在"
assert_eq "1" "$(psql_q "SELECT count(*) FROM pg_indexes WHERE tablename='biz_purchase_order' AND indexname='idx_biz_po_contract' AND indexdef LIKE '%WHERE (contract_id IS NOT NULL)%'")" \
    "idx_biz_po_contract 是**部分**索引（WHERE contract_id IS NOT NULL）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM pg_constraint c JOIN pg_class t ON t.oid=c.conrelid WHERE t.relname='biz_purchase_order' AND c.contype='f'")" \
    "采购单表零外键（ARCH-2 DEC-02：contract_id / supplier_id 都是弱引用，别顺手补 FK）"
assert_eq "4" "$(psql_q "SELECT count(*) FROM pg_description d JOIN pg_class t ON t.oid=d.objoid JOIN pg_attribute a ON a.attrelid=t.oid AND a.attnum=d.objsubid WHERE t.relname='biz_purchase_order' AND a.attname IN ('contract_id','purchase_type','contract_no_snapshot','supplier_name_snapshot')")" \
    "4 列都有 COMMENT（口径写在库里，不是只写在代码注释里）"
# 部分索引真的能被这个筛选用到（小表默认会选 seqscan，这里关掉它看规划器有没有别的路）
assert_eq "1" "$(psql_q "SET enable_seqscan=off; EXPLAIN (COSTS OFF) SELECT id FROM biz_purchase_order WHERE contract_id = 'd2a20001-0000-4000-8000-0000000000c1'::uuid" | grep -c 'idx_biz_po_contract')" \
    "按 contract_id 筛选可走 idx_biz_po_contract"

# ---------- 4) 约束反证（写库负断言） ----------
echo "== 4) 约束反证：四种非法组合必须被库拦下，合法行必须插得进"
PSQL_BASE="INSERT INTO biz_purchase_order
   (order_no, supplier_id, order_date, warehouse_id, status,
    total_qty, total_amount, total_tax, total_with_tax,
    purchase_type, contract_id, contract_no_snapshot)
   VALUES ('M417-%s', '$F_SUPPLIER'::uuid, DATE '2026-06-01',
           'd2a20001-0000-4000-8000-000000000002'::uuid, 'DRAFT',
           0, 0, 0, 0, %s)"

psql_expect_fail "$(printf "$PSQL_BASE" 'bad1' "'CONTRACT', NULL, 'HT-M417-1'")" "ck_biz_po_contract_pair" \
    "CONTRACT 但 contract_id 为空 → 拦"
psql_expect_fail "$(printf "$PSQL_BASE" 'bad2' "'CONTRACT', 'd2a20001-0000-4000-8000-0000000000c1'::uuid, NULL")" "ck_biz_po_contract_pair" \
    "CONTRACT 但合同号快照为空 → 拦（列表不 JOIN 合同表的前提）"
psql_expect_fail "$(printf "$PSQL_BASE" 'bad3' "'SPOT', 'd2a20001-0000-4000-8000-0000000000c1'::uuid, 'HT-M417-3'")" "ck_biz_po_contract_pair" \
    "SPOT 但带着 contract_id → 拦（零星采购没有合同）"
psql_expect_fail "$(printf "$PSQL_BASE" 'bad4' "'CONTRCAT', NULL, NULL")" "ck_biz_po_purchase_type|ck_biz_po_contract_pair" \
    "purchase_type 拼成 CONTRCAT → 拦（CASE … ELSE FALSE，NULL 判真被堵死）"
psql_expect_ok "$(printf "$PSQL_BASE" 'ok1' "'CONTRACT', 'd2a20001-0000-4000-8000-0000000000c1'::uuid, 'HT-M417-OK'")" \
    "合法 CONTRACT 行（id + 合同号快照齐）→ 插得进（约束没拦过头）"
psql_expect_ok "DELETE FROM biz_purchase_order WHERE order_no LIKE 'M417-%'" \
    "清掉本段的探针行"

# ---------- 5) 代码接线（静态断言：列清单 / DTO / 写库 / 守卫 / 前端） ----------
echo "== 5) 代码接线（模型 / DTO / handler / 前端逐段）"
P="$ROOT/backend/app/src/business/erp"
assert_eq "1" "$(grep -c 'purchase_type, contract_id, contract_no_snapshot, supplier_name_snapshot' "$P/model_order.rs")" \
    "模型列清单 PURCHASE_ORDER_COLUMNS 带上 contract_id 与快照"
assert_eq "1" "$(grep -c 'contract_id: row.get_typed("contract_id")?' "$P/model_order.rs")" \
    "行映射读 contract_id（漏读 = 详情永远 null）"
assert_eq "1" "$(grep -c 'AND contract_id = ?' "$P/model_order.rs")" \
    "分页筛选支持 contract_id"
assert_eq "1" "$(grep -c 'idx_biz_po_contract' "$P/model_order.rs")" \
    "筛选处标注走哪条索引（注释与迁移对得上）"
assert_eq "2" "$(grep -c 'pub contract_id: Option<String>,' "$P/dto.rs")" \
    "DTO 入参筛选项 + 出参各一处 contract_id"
assert_eq "1" "$(grep -c 'purchase_type = CONTRACT 必须给 contract_id' "$P/purchase/purchase_dto.rs")" \
    "入参校验：CONTRACT 必填 contract_id"
assert_eq "1" "$(grep -c 'purchase_type = SPOT 不能给 contract_id' "$P/purchase/purchase_dto.rs")" \
    "入参校验：SPOT 必须为空（与库约束同口径）"
assert_eq "1" "$(grep -c "contract_id = NULLIF(" "$P/purchase/purchase_model.rs")" \
    "改单写库用 NULLIF 归一空串 → NULL（否则 SPOT 改单会撞 uuid 解析）"
assert_eq "1" "$(grep -c 'pub async fn committed_amount' "$P/purchase/purchase_model.rs")" \
    "合同累计已下单额现算 committed_amount（M4-18 的判据就靠它，缓存必漂移）"
assert_eq "1" "$(grep -c 'async fn resolve_contract' "$P/purchase/purchase_handler.rs")" \
    "handler 有合同解析（存在 / 采购类 / 可用状态 / 有效期四步）"
assert_eq "2" "$(grep -c 'guard_contract_amount(&tx, contract, totals.with_tax' "$P/purchase/purchase_handler.rs")" \
    "额度守卫在建单 + 改单两处都调用（锁合同行后现算）"
assert_eq "1" "$(grep -c 'contract_id: form.contract_id || null' "$ROOT/frontend/src/views/business/erp/purchase/purchase-logic.ts")" \
    "前端合同采购只发 contract_id（供应商由服务端从乙方派生）"
assert_eq "1" "$(grep -c 'contract_id: string' "$ROOT/frontend/src/views/business/erp/purchase/purchase-logic.ts")" \
    "前端表单状态里有 contract_id"

# ---------- 6) 真库用例 ----------
if [ "$SKIP_TESTS" = "1" ]; then
    info "SKIP_TESTS=1 → 跳过真库用例与 cargo 编译"
else
    echo "== 6) 真库用例（合同相关 13 条）"
    # 基线：跑之前先记一遍（跑完要回到这里）
    BASE_PO="$(psql_q "SELECT count(*) FROM biz_purchase_order")"
    BASE_HT="$(psql_q "SELECT count(*) FROM biz_contract")"
    export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"

    run_tests() { # 过滤串 说明 期望通过数
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

    run_tests "business::erp::purchase::purchase_handler::tests::contract" \
        "合同采购：派生 / 必填 / 不可用 / 额度 / 有效期 / 分页快照" 8
    run_tests "business::erp::purchase::purchase_handler::tests::spot_order" \
        "零星采购：带 contract_id 拒绝 / 快照保持 NULL" 2
    run_tests "business::erp::purchase::purchase_handler::tests::legacy_spot_order_still_readable" \
        "存量单（V0198 之前建的）落默认 SPOT / 快照为 NULL 仍可读" 1
    run_tests "business::erp::purchase::purchase_handler::tests::update_without_type_keeps_contract_type" \
        "改单不带 purchase_type 不得把合同单洗成 SPOT" 1
    run_tests "business::erp::purchase::purchase_handler::tests::page_exposes_contract_snapshot_columns" \
        "分页出参下发 contract_id / 合同号快照" 1

    # ---------- 7) 夹具残留（脚本自己收，回到跑测试前的基线） ----------
    echo "== 7) 夹具残留（按固定 id 清场 → 回到基线）"
    psql_q "DELETE FROM biz_status_log WHERE entity_type = 'PURCHASE_ORDER' AND entity_id NOT IN (SELECT id FROM biz_purchase_order)" >/dev/null 2>&1
    psql_q "DELETE FROM biz_purchase_order WHERE supplier_id = '$F_SUPPLIER'::uuid" >/dev/null 2>&1
    psql_q "DELETE FROM biz_contract WHERE id IN ($F_CONTRACTS)" >/dev/null 2>&1
    assert_eq "$BASE_PO" "$(psql_q "SELECT count(*) FROM biz_purchase_order")" "采购单回到跑测试前的基线"
    assert_eq "$BASE_HT" "$(psql_q "SELECT count(*) FROM biz_contract")" "合同回到跑测试前的基线"
    assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_purchase_order WHERE order_no LIKE 'M417-%'")" "探针行 0 残留"
fi

# ---------- 汇总 ----------
echo
printf '== 汇总：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
if [ "$FAIL" -gt 0 ]; then
    printf '演练失败（rc=1）\n'
    exit 1
fi
printf '演练通过（rc=0）\n'
