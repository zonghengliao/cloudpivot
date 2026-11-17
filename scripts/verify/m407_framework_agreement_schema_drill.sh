#!/usr/bin/env bash
# ============================================================================
# M4-07 框架协议表结构（有效期 + 价格条款 + 数量上限）· 演练
#
# 卡：t_7eb1c440（M4-07【M4·采购】框架协议：表结构（有效期 + 价格条款 + 数量上限））
#
# 这一卡**只交表结构**，所以脚本验的也是结构本身（不是业务链路）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. 把演练库推到仓库上一代（V0405…V0449），再应用 V0450 —— **连跑两遍**证明幂等
#   3. 结构断言（psql 侧，独立于 Rust）：表 / 列数 29+13 / 7 索引 / 外键级联 /
#      部分唯一索引 / 取号行 FWA
#   4. 约束行为断言：卡面三块（有效期 / 价格条款 / 数量上限）+ 供应商与物料范围，
#      坏数据逐条必须被库层拒；「已执行量超上限」反过来必须**写得进去**
#   5. 本卡**不许夹带**：框架协议权限码 0 个、菜单行 0 行、采购订单表 0 个新列
#   6. 空库全链重放：`scripts/verify/mig_replay_fresh.sh` 把全部版本化迁移按版本号
#      在空库上重跑一遍（V0450 不许依赖更晚的迁移 / 不许和别人的号撞车）
#   7. 回滚三段：有数据 → 拒；清空 → 成功；再 up → 复原
#   8. Rust 用例：本卡 20 条（8 纯逻辑 + 12 真库），**SKIP=0 + 正向标记**双保险
#   9. 门禁：permission_audit_tests 全绿 + menu_invariants.sh 8 条（本卡零菜单，
#      跑一遍证明没顶破 540 上限）
#  10. 夹具残留：`M407-` 协议与物料范围 0 条
#
# 用法：
#   bash scripts/verify/m407_framework_agreement_schema_drill.sh                    # 99 → 145（默认）
#   PG_HOST=127.0.0.1 bash scripts/verify/m407_framework_agreement_schema_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m407_framework_agreement_schema_drill.sh          # 留库排查
#   M407_BAD_URL=1 bash scripts/verify/m407_framework_agreement_schema_drill.sh     # SKIP 守卫自检（期望报红）
#   SKIP_REPLAY=1 bash scripts/verify/m407_framework_agreement_schema_drill.sh      # 跳过空库全链重放
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名含 drill；脚本自己 DROP；
# 连接串密码一律复用 `$PGPASSWORD`（**不要**在 URL 里写占位符 —— 那样真库用例会
# 连不上、打 SKIP 还记 ok，是假绿）。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIG_DIR="$ROOT/backend/app/migrations/business"
BACKEND="$ROOT/backend"
MIG="$MIG_DIR/V0450__framework_agreement.up.sql"
DOWN="$MIG_DIR/V0450__framework_agreement.down.sql"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="${M407_DB:-cp_m407_fa_drill_$$}"
KEEP_DB="${KEEP_DB:-0}"
SKIP_REPLAY="${SKIP_REPLAY:-0}"
TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
mkdir -p "$TMP"
LOG="$TMP/m407_drill_$$.log"
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
if [ -n "${M407_DB:-}" ]; then OWN_DB=0; fi

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }
psql_file()  { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$1" >"$LOG" 2>&1; }
psql_sql()   { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f - >"$LOG" 2>&1; }

assert_eq() { # 期望 实际 说明
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 [$1]，实际 [$2]）"; fi
}

# 期望这条 SQL 被库层拒，且**必须是被指定约束拒的**。
#
# ⚠ 只断言「有 ERROR」会放过「SQL 语法/类型写错了」的假绿 —— 实测踩过：协议号写成
#   `'gen_random_uuid()'::uuid` 时报的是 invalid input syntax，10 条反证全部「通过」，
#   其实一条约束都没验到。所以这里同时断言错误文本里有约束名，并显式排除语法/类型错误。
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

accepts() { # sql 说明
    local out
    out="$(psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -q -X -v ON_ERROR_STOP=1 -c "$1" 2>&1)"
    if [ $? -eq 0 ]; then ok "$2"; else bad "$2 —— 被拒了：$(printf '%s' "$out" | head -2 | tr '\n' ' ')"; fi
}

cleanup() {
    if [ "$OWN_DB" = "0" ] || [ "$KEEP_DB" = "1" ]; then
        printf '\n  （库保留 → %s；不删）\n' "$DB"
        return
    fi
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
    rm -f "$LOG"
}
trap cleanup EXIT

printf '== M4-07 框架协议表结构演练 · PG %s:%s / 库 %s ==\n' "$PG_HOST" "$PG_PORT" "$DB"

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

# ---------- 1) 把演练库推到 V0449，再上 V0450 ----------
echo "== 1) 补齐业务迁移 V0405…V0449 + 再上 V0450"
MIG_FAIL=0
APPLIED=0
for f in $(ls "$MIG_DIR" | grep -E '^V[0-9]{4}__.*\.up\.sql$' | sort); do
    num="${f:1:4}"
    if [ "$num" -le 404 ]; then continue; fi
    if [ "$num" -ge 450 ]; then continue; fi   # V0450 单独上（要单测幂等）
    APPLIED=$((APPLIED+1))
    if ! psql_file "$MIG_DIR/$f"; then
        MIG_FAIL=$((MIG_FAIL+1))
        bad "迁移失败：$f（tail: $(tail -2 "$LOG" | tr '\n' ' ')）"
    fi
done
if [ "$MIG_FAIL" -eq 0 ]; then
    ok "$APPLIED 条 V0405…V0449 迁移全部应用成功（演练库推到 V0449）"
fi
MENU_BEFORE="$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")"
assert_eq "GONE" "$(psql_q "SELECT COALESCE(to_regclass('public.biz_framework_agreement')::text,'GONE')")" \
    "上 V0450 之前框架协议表还不存在（下面每一条断言才说明是 V0450 建的）"

echo "== 1b) 应用 V0450（连跑两遍验幂等）"
if psql_file "$MIG"; then ok "第一遍应用成功"; else
    bad "第一遍应用失败：$(tail -3 "$LOG" | tr '\n' ' ')"; exit 2
fi
if psql_file "$MIG"; then ok "第二遍应用成功（幂等：只该出 NOTICE）"; else
    bad "第二遍应用失败（幂等破了）：$(tail -3 "$LOG" | tr '\n' ' ')"
fi
if grep -q 'ERROR' "$LOG"; then
    bad "第二遍输出里出现 ERROR：$(grep 'ERROR' "$LOG" | head -2 | tr '\n' ' ')"
else
    ok "第二遍输出无 ERROR（CREATE ... IF NOT EXISTS 生效）"
fi
MENU_AFTER="$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")"
assert_eq "$MENU_BEFORE" "$MENU_AFTER" "V0450 一条菜单都没加（上迁移前后菜单总数不变；本卡只做表结构）"

# ---------- 2) 结构断言 ----------
echo "== 2) 结构断言"
assert_eq "biz_framework_agreement" \
    "$(psql_q "SELECT COALESCE(to_regclass('public.biz_framework_agreement')::text,'')")" "协议单头表存在"
assert_eq "biz_framework_agreement_item" \
    "$(psql_q "SELECT COALESCE(to_regclass('public.biz_framework_agreement_item')::text,'')")" "物料范围表存在"
assert_eq "29" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='biz_framework_agreement'")" "单头 29 列"
assert_eq "13" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='biz_framework_agreement_item'")" "明细 13 列"
assert_eq "CASCADE" "$(psql_q "SELECT delete_rule FROM information_schema.referential_constraints WHERE constraint_name='fk_biz_fwai_agreement'")" "明细外键 ON DELETE CASCADE"
assert_eq "7" "$(psql_q "SELECT count(DISTINCT indexname) FROM pg_indexes WHERE schemaname='public' AND indexname IN ('uk_biz_framework_agreement_no','idx_biz_fwa_supplier','idx_biz_fwa_period','idx_biz_fwa_status','uk_biz_fwai_material','uk_biz_fwai_line','idx_biz_fwai_material')")" "7 个索引全在"
assert_eq "1" "$(psql_q "SELECT count(*) FROM pg_indexes WHERE schemaname='public' AND indexname='uk_biz_framework_agreement_no' AND indexdef LIKE '%WHERE (deleted_at IS NULL)%'")" "协议号唯一索引是部分索引（软删后可重建同号）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_seq WHERE seq_type='FWA' AND deleted_at IS NULL AND prefix='FWA' AND separator='-'")" "取号规则 FWA 一行"
assert_eq "3" "$(psql_q "SELECT count(*) FROM pg_indexes WHERE schemaname='public' AND tablename='biz_framework_agreement' AND indexname IN ('idx_biz_fwa_supplier','idx_biz_fwa_period','idx_biz_fwa_status')")" "三个单头查询索引都挂在框架协议表上（不占用固定资产已用的 idx_biz_fa_* 命名）"
assert_eq "1" "$(psql_q "SELECT (count(*) > 180)::int FROM information_schema.tables WHERE table_schema='public'")" "模板库克隆完整（public 表 > 180 张）"

# ---------- 3) 约束行为：坏数据必须被拒 ----------
echo "== 3) 约束行为（卡面三块 + 供应商与物料范围）"
rejects_c "INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date) VALUES ('FA-20260101-000001','旧前缀',gen_random_uuid(),'2026-01-01','2026-12-31')" \
    "旧前缀 FA-yyyyMMdd 的号被拒" "ck_biz_fwa_no_fmt"
rejects_c "INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date) VALUES ('FWA-19990101-900901','   ',gen_random_uuid(),'2026-01-01','2026-12-31')" \
    "空白协议名被拒" "ck_biz_fwa_name"
rejects_c "INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date) VALUES ('FWA-19990101-900902','起止颠倒',gen_random_uuid(),'2026-12-31','2026-01-01')" \
    "有效期起晚于止被拒" "ck_biz_fwa_period"
rejects_c "INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, price_mode) VALUES ('FWA-19990101-900903','浮动无规则',gen_random_uuid(),'2026-01-01','2026-12-31','FLOATING')" \
    "FLOATING 但没有调价规则被拒" "ck_biz_fwa_adjust"
rejects_c "INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, price_mode, price_adjust_rule) VALUES ('FWA-19990101-900904','浮动空白规则',gen_random_uuid(),'2026-01-01','2026-12-31','FLOATING','   ')" \
    "FLOATING 的调价规则是空白串被拒（空串陷阱）" "ck_biz_fwa_adjust"
rejects_c "INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, price_currency) VALUES ('FWA-19990101-900905','小写币种',gen_random_uuid(),'2026-01-01','2026-12-31','rmb')" \
    "币种不是 ISO 三位大写被拒" "ck_biz_fwa_currency"
rejects_c "INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, price_tolerance) VALUES ('FWA-19990101-900906','容差过大',gen_random_uuid(),'2026-01-01','2026-12-31',1.5)" \
    "容差 1.5 被拒（0–1）" "ck_biz_fwa_tolerance"
rejects_c "INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, total_max_qty) VALUES ('FWA-19990101-900907','零上限',gen_random_uuid(),'2026-01-01','2026-12-31',0)" \
    "总量上限写 0 被拒（0 不是「不限」，不限要写 NULL）" "ck_biz_fwa_caps"
rejects_c "INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, status) VALUES ('FWA-19990101-900908','死状态',gen_random_uuid(),'2026-01-01','2026-12-31','EXPIRED')" \
    "状态域外的 EXPIRED 被拒（到期是时间的函数，不落状态）" "ck_biz_fwa_status"
rejects_c "INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, status) VALUES ('FWA-19990101-900909','无实例',gen_random_uuid(),'2026-01-01','2026-12-31','SUBMITTED')" \
    "SUBMITTED 没有审批实例被拒" "ck_biz_fwa_instance"

# 正样本：一份 5% 容差的浮动协议 + 两行物料（一行限量、一行不限量）
psql_sql <<'SQL'
INSERT INTO biz_framework_agreement
    (id, agreement_no, agreement_name, supplier_id, supplier_name, start_date, end_date, sign_date,
     price_mode, price_adjust_rule, price_includes_tax, price_currency, price_tolerance, total_max_amount, status)
VALUES ('00000000-0000-4000-8000-000000407001'::uuid, 'FWA-19990101-900950', 'M407-演练框架协议',
        gen_random_uuid(), 'M407-演练供应商', '2026-01-01', '2026-12-31', '2025-12-20',
        'FLOATING', '随钢材指数月度均价上浮，不超过 3%', FALSE, 'CNY', 0.05, 500000, 'APPROVED');
INSERT INTO biz_framework_agreement_item
    (agreement_id, line_no, material_id, unit_id, agreed_price, tax_rate, max_qty)
VALUES ('00000000-0000-4000-8000-000000407001'::uuid, 1, gen_random_uuid(), gen_random_uuid(), 10.000000, 0.13, 1000);
INSERT INTO biz_framework_agreement_item
    (agreement_id, line_no, material_id, unit_id, agreed_price, tax_rate)
VALUES ('00000000-0000-4000-8000-000000407001'::uuid, 2, gen_random_uuid(), gen_random_uuid(), 20.000000, 0.13);
SQL
if [ $? -eq 0 ]; then ok "正样本：FLOATING + 5% 容差 + 总额上限 + 两行物料（限量 / 不限量）都能插"; else
    bad "正样本插不进去：$(tail -2 "$LOG" | tr '\n' ' ')"; fi

assert_eq "false" "$(psql_q "SELECT price_includes_tax::text FROM biz_framework_agreement WHERE id='00000000-0000-4000-8000-000000407001'::uuid")" "协议价默认不含税"
assert_eq "true" "$(psql_q "SELECT (total_executed_qty = 0 AND total_executed_amount = 0)::text FROM biz_framework_agreement WHERE id='00000000-0000-4000-8000-000000407001'::uuid")" "单头两个已执行合计默认 0（派生列，客户端不许传）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_framework_agreement_item WHERE agreement_id='00000000-0000-4000-8000-000000407001'::uuid AND (executed_qty <> 0 OR executed_amount <> 0)")" "明细两个已执行列默认 0"
accepts "INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, status) VALUES ('FWA-19990101-900951','M407-同供应商第二份', (SELECT supplier_id FROM biz_framework_agreement WHERE id='00000000-0000-4000-8000-000000407001'::uuid),'2026-01-01','2026-12-31','APPROVED')" \
    "同一供应商的第二份协议能插（库层不做 supplier 唯一；免审 APPROVED 无实例也合法）"
rejects_c "INSERT INTO biz_framework_agreement_item (agreement_id, line_no, material_id, unit_id, agreed_price) SELECT agreement_id, 3, material_id, unit_id, 11 FROM biz_framework_agreement_item WHERE agreement_id='00000000-0000-4000-8000-000000407001'::uuid AND line_no=1" \
    "同协议「同物料」第二行被拒（否则会出现两个协议价）" "uk_biz_fwai_material"
rejects_c "INSERT INTO biz_framework_agreement_item (agreement_id, line_no, material_id, unit_id, agreed_price) VALUES ('00000000-0000-4000-8000-000000407001'::uuid, 3, gen_random_uuid(), gen_random_uuid(), -1)" \
    "负协议价被拒" "ck_biz_fwai_price"
rejects_c "INSERT INTO biz_framework_agreement_item (agreement_id, line_no, material_id, unit_id, agreed_price, max_qty) VALUES ('00000000-0000-4000-8000-000000407001'::uuid, 3, gen_random_uuid(), gen_random_uuid(), 1, 0)" \
    "物料上限写 0 被拒" "ck_biz_fwai_cap"
rejects_c "INSERT INTO biz_framework_agreement_item (agreement_id, line_no, material_id, unit_id, agreed_price, tax_rate) VALUES ('00000000-0000-4000-8000-000000407001'::uuid, 3, gen_random_uuid(), gen_random_uuid(), 1, 1.5)" \
    "税率 1.5 被拒（0–1）" "ck_biz_fwai_tax"
accepts "UPDATE biz_framework_agreement_item SET executed_qty = 1200 WHERE agreement_id='00000000-0000-4000-8000-000000407001'::uuid AND line_no=1" \
    "已执行量超上限必须写得进去（库层刻意不拦；超限拒绝属 M4-08）"
rejects_c "UPDATE biz_framework_agreement_item SET executed_qty = -1 WHERE agreement_id='00000000-0000-4000-8000-000000407001'::uuid AND line_no=1" \
    "负已执行量被拒" "ck_biz_fwai_exec"
assert_eq "true" "$(psql_q "SELECT (executed_qty = 1200)::text FROM biz_framework_agreement_item WHERE agreement_id='00000000-0000-4000-8000-000000407001'::uuid AND line_no=1")" "超上限的值原样落库（没有被 CHECK 改写/截断）"

# ---------- 4) 本卡不许夹带：权限码 0 / 菜单 0 / 订单表 0 新列 ----------
echo "== 4) 本卡范围守卫（不夹带权限码、菜单、订单表列）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code LIKE 'business:erp:framework-agreement:%' AND deleted_at IS NULL")" "框架协议权限码 0 行（M4-11 才落）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_menu WHERE path LIKE '/business/erp/framework-agreement%' AND deleted_at IS NULL")" "框架协议菜单 0 行（M4-11 才落）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name IN ('biz_purchase_order','biz_purchase_order_item') AND column_name LIKE '%framework_agreement%'")" "采购订单（明细）表 0 个框架协议列（引用字段属 M4-08）"
SRC_HITS="$(grep -rl "framework-agreement:" "$BACKEND/app/src/business/erp/purchase" 2>/dev/null | wc -l)"
assert_eq "0" "$SRC_HITS" "源码里 0 个 framework-agreement 权限码（连常量都不定义）"
ROUTE_HITS="$(grep -rn "framework_agreement" "$BACKEND/app/src/business/erp/purchase/mod.rs" | grep -c "#\[nik::" || true)"
assert_eq "0" "$ROUTE_HITS" "本卡 0 条路由（表结构卡不接线）"

# ---------- 5) 空库全链重放 ----------
echo "== 5) 空库全链重放（mig_replay_fresh.sh，独立库）"
if [ "$SKIP_REPLAY" = "1" ]; then
    info "SKIP_REPLAY=1：跳过（本地快速迭代用；提交前必须跑全）"
else
    REPLAY_OUT="$TMP/m407_$$_replay.out"
    if DBHOST="$PG_HOST" DBPORT="$PG_PORT" DBUSER="$PG_USER" DBPASS="$PGPASSWORD" DBOWNER="$PG_USER" \
       bash "$ROOT/scripts/verify/mig_replay_fresh.sh" "m407$$" >"$REPLAY_OUT" 2>&1; then
        ok "空库按版本号升序重放全链通过（$(grep -oE '[0-9]+/[0-9]+ 条版本化迁移[^ ]*' "$REPLAY_OUT" | tail -1)）"
    else
        bad "空库重放失败（见 $REPLAY_OUT）"
        grep -E '✗ 失败|ERROR|== 结果' "$REPLAY_OUT" | head -6 | sed 's/^/        /'
    fi
fi

# ---------- 6) 回滚三段 ----------
echo "== 6) 回滚三段（数据护栏 / 成功 / 复原）"
if psql_file "$DOWN"; then
    bad "有数据时 down 居然成功了（数据护栏失效）"
else
    if grep -q '回滚被拒绝' "$LOG"; then ok "有数据时 down 被拒（数据护栏生效）"; else
        bad "down 失败但不是因为数据护栏：$(tail -2 "$LOG" | tr '\n' ' ')"; fi
fi
psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -q -X \
    -c "DELETE FROM biz_framework_agreement WHERE agreement_name LIKE 'M407-%'" >/dev/null 2>&1
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_framework_agreement_item WHERE agreement_id='00000000-0000-4000-8000-000000407001'::uuid")" "删单头连带删物料范围（ON DELETE CASCADE）"
if psql_file "$DOWN"; then ok "清空后 down 成功"; else bad "清空后 down 失败：$(tail -2 "$LOG" | tr '\n' ' ')"; fi
assert_eq "" "$(psql_q "SELECT COALESCE(to_regclass('public.biz_framework_agreement')::text,'')")" "down 后单头表已删"
assert_eq "" "$(psql_q "SELECT COALESCE(to_regclass('public.biz_framework_agreement_item')::text,'')")" "down 后物料范围表已删"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_seq WHERE seq_type='FWA'")" "down 后取号行已删"
if psql_file "$MIG"; then ok "再 up 复原成功"; else bad "再 up 失败：$(tail -3 "$LOG" | tr '\n' ' ')"; fi
assert_eq "biz_framework_agreement" "$(psql_q "SELECT COALESCE(to_regclass('public.biz_framework_agreement')::text,'')")" "up 复原后表回来了"
assert_eq "29" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='biz_framework_agreement'")" "up 复原后列数仍是 29"

# ---------- 7) Rust 用例 ----------
echo "== 7) 真库用例（--test-threads=1 --nocapture）"
export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
export SYS_DB_URL="$ERP_DB_URL"
if [ "${M407_BAD_URL:-}" = "1" ]; then
    export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/no_such_db_skip_guard"
    export SYS_DB_URL="$ERP_DB_URL"
    info "SKIP 自检模式（M407_BAD_URL=1）：连接串故意写错，期望 SKIP>0"
fi

OUT="$TMP/m407_$$_tests.out"
( cd "$BACKEND" && cargo test -p cloudpivot --lib business::erp::purchase::framework_agreement -- --test-threads=1 --nocapture ) >"$OUT" 2>&1
LINE="$(grep -E '^test result: ' "$OUT" | tail -1)"
PASSED="$(printf '%s' "$LINE" | sed -n 's/.*ok\. \([0-9]*\) passed.*/\1/p')"
FAILED="$(printf '%s' "$LINE" | sed -n 's/.* \([0-9]*\) failed.*/\1/p')"
SKIPPED="$(grep -c 'SKIP:' "$OUT" || true)"
[ -z "${PASSED:-}" ] && PASSED=0
[ -z "${FAILED:-}" ] && FAILED=0
[ -z "${SKIPPED:-}" ] && SKIPPED=0
if [ "$FAILED" -eq 0 ] && [ "$PASSED" -ge 20 ]; then
    ok "本卡用例全绿（$PASSED 条：8 纯逻辑 + 12 真库）"
elif [ "$FAILED" -ne 0 ]; then
    bad "本卡用例有 $FAILED 条失败（见 $OUT）"
    grep -E 'panicked at|assertion|left:|right:' -A 2 "$OUT" | head -30 | sed 's/^/        /'
else
    bad "本卡用例只跑了 $PASSED 条（期望 ≥ 20，见 $OUT）"
fi
if [ "$SKIPPED" -eq 0 ]; then ok "本组 SKIP=0（真库用例没被「连不上」打成假绿）"; else
    bad "本组出现 $SKIPPED 行 SKIP（假绿）"; grep 'SKIP:' "$OUT" | head -5 | sed 's/^/        /'
fi
if grep -qF '[M4-07] 表形状' "$OUT" && grep -qF '[M4-07] 有效期' "$OUT" \
   && grep -qF '[M4-07] 数量上限' "$OUT" && grep -qF '[M4-07] 物料范围' "$OUT"; then
    ok "正向标记在位（[M4-07] 表形状 / 有效期 / 数量上限 / 物料范围）"
else
    bad "缺正向标记 —— 用例没真跑（被跳过 / 被过滤）"
fi

# ---------- 8) 门禁：权限审计 + 菜单结构 ----------
echo "== 8) 门禁（permission_audit_tests + menu_invariants.sh）"
AUDIT_OUT="$TMP/m407_$$_audit.out"
( cd "$BACKEND" && cargo test -p cloudpivot --lib system::menu::permission_audit_tests -- --test-threads=1 ) >"$AUDIT_OUT" 2>&1
AUDIT_LINE="$(grep -E '^test result: ' "$AUDIT_OUT" | tail -1)"
if printf '%s' "$AUDIT_LINE" | grep -qE 'test result: ok\.' && ! printf '%s' "$AUDIT_LINE" | grep -qE '[1-9][0-9]* failed'; then
    ok "permission_audit_tests 全绿（本卡没动权限码，也不许打破审计）"
else
    bad "permission_audit_tests 不是全绿：${AUDIT_LINE:-无输出}（见 $AUDIT_OUT）"
fi
if PGHOST="$PG_HOST" PGPORT="$PG_PORT" PGUSER="$PG_USER" bash "$ROOT/scripts/verify/menu_invariants.sh" "$DB" >"$TMP/m407_$$_menu.out" 2>&1; then
    ok "menu_invariants.sh 8 条断言全绿"
else
    bad "menu_invariants.sh 报红（见 $TMP/m407_$$_menu.out）"
    tail -15 "$TMP/m407_$$_menu.out" | sed 's/^/        /'
fi
info "菜单总数：$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")（门禁区间 400-540；本卡零新增）"

# ---------- 9) 夹具残留 ----------
echo "== 9) 夹具残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_framework_agreement WHERE agreement_name LIKE 'M407-%'")" "M407- 协议 0 残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_framework_agreement_item it JOIN biz_framework_agreement a ON a.id=it.agreement_id WHERE a.agreement_name LIKE 'M407-%'")" "M407- 物料范围 0 残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_framework_agreement WHERE agreement_no LIKE 'FWA-19990101-9%'")" "自检探针号段 0 残留"

# ---------- 10) 汇总 ----------
echo
if [ "$FAIL" -eq 0 ]; then
    printf '\033[32m== M4-07 演练结果：PASS=%s  FAIL=0 —— 框架协议表结构（有效期 + 价格条款 + 数量上限 · 供应商 + 物料范围）就绪\033[0m\n' "$PASS"
    exit 0
else
    printf '\033[31m== M4-07 演练结果：PASS=%s  FAIL=%s —— 见上面红行\033[0m\n' "$PASS" "$FAIL"
    exit 1
fi
