#!/usr/bin/env bash
# ============================================================================
# M4-12 供应商评估表结构（质量/交期/价格/服务 四维打分 + 权重）· 演练
#
# 卡：t_3e550d38（M4-12【M4·采购】供应商评估：表结构（质量/交期/价格/服务四维））
#
# 这一卡**只交表结构**，所以脚本验的也是结构本身（不是业务链路）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. 把演练库推到仓库上一代（V0405…V0453），再应用 V0454 —— **连跑两遍**证明幂等
#   3. 结构断言（psql 侧，独立于 Rust）：表 / 25 列 / 4 索引 / 两个部分唯一索引 /
#      total_score 是 STORED 生成列（表达式含四维权重与 round）/ 取号行 SEV
#   4. 约束行为断言：卡面四块（四维打分 / 权重 / 期间 / 供应商）逐条反证，
#      坏数据必须被库层拒；「总分」反过来必须**由库算出来**且**显式写被拒**
#   5. 本卡**不许夹带**：评估权限码 0 个、菜单行 0 行、biz_supplier 0 个新列
#   6. 空库全链重放：`scripts/verify/mig_replay_fresh.sh` 把全部版本化迁移按版本号
#      在空库上重跑一遍（V0454 不许依赖更晚的迁移 / 不许和别人的号撞车）
#   7. 回滚三段：有数据 → 拒；清空 → 成功；再 up → 复原
#   8. Rust 用例：本卡 25 条（13 纯逻辑 + 12 真库），**SKIP=0 + 正向标记**双保险
#   9. 门禁：permission_audit_tests 全绿 + menu_invariants.sh 8 条（本卡零菜单，
#      跑一遍证明没顶破 540 上限）
#  10. 夹具残留：`M412-` 评估 0 条
#
# 用法：
#   bash scripts/verify/m412_supplier_evaluation_schema_drill.sh                    # 99 → 145（默认）
#   PG_HOST=127.0.0.1 bash scripts/verify/m412_supplier_evaluation_schema_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m412_supplier_evaluation_schema_drill.sh          # 留库排查
#   M412_BAD_URL=1 bash scripts/verify/m412_supplier_evaluation_schema_drill.sh     # SKIP 守卫自检（期望报红）
#   SKIP_REPLAY=1 bash scripts/verify/m412_supplier_evaluation_schema_drill.sh      # 跳过空库全链重放
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名含 drill；脚本自己 DROP；
# 连接串密码一律复用 `$PGPASSWORD`（**不要**在 URL 里写占位符 —— 那样真库用例会
# 连不上、打 SKIP 还记 ok，是假绿）。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIG_DIR="$ROOT/backend/app/migrations/business"
BACKEND="$ROOT/backend"
MIG="$MIG_DIR/V0454__supplier_evaluation.up.sql"
DOWN="$MIG_DIR/V0454__supplier_evaluation.down.sql"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="${M412_DB:-cp_m412_sev_drill_$$}"
KEEP_DB="${KEEP_DB:-0}"
SKIP_REPLAY="${SKIP_REPLAY:-0}"
TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
mkdir -p "$TMP"
LOG="$TMP/m412_drill_$$.log"
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
if [ -n "${M412_DB:-}" ]; then OWN_DB=0; fi

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }
psql_file()  { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$1" >"$LOG" 2>&1; }
psql_sql()   { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f - >"$LOG" 2>&1; }

assert_eq() { # 期望 实际 说明
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 [$1]，实际 [$2]）"; fi
}

# 期望这条 SQL 被库层拒，且**必须是被指定约束拒的**。
#
# ⚠ 只断言「有 ERROR」会放过「SQL 语法/类型写错了」的假绿 —— 实测踩过：单号写成
#   `'gen_random_uuid()'::uuid` 时报的是 invalid input syntax，反证全部「通过」，
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

# 期望被拒，且错误文本里出现指定片段（用于列宽 / 生成列这类没有约束名的拒绝）。
rejects_msg() { # sql 固定片段 说明
    local out
    out="$(psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -q -X -c "$1" 2>&1)"
    if ! printf '%s' "$out" | grep -q 'ERROR'; then
        bad "$3 —— 库层居然接受了"
        return
    fi
    if printf '%s' "$out" | grep -qiF -- "$2"; then
        ok "$3（库层拒了：$(printf '%s' "$out" | head -1 | cut -c1-90)）"
    else
        bad "$3 —— 报错文本不含期望片段 [$2]：$(printf '%s' "$out" | head -1)"
    fi
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

printf '== M4-12 供应商评估表结构演练 · PG %s:%s / 库 %s ==\n' "$PG_HOST" "$PG_PORT" "$DB"

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

# ---------- 1) 把演练库推到 V0453，再上 V0454 ----------
echo "== 1) 补齐业务迁移 V0405…V0453 + 再上 V0454"
MIG_FAIL=0
APPLIED=0
for f in $(ls "$MIG_DIR" | grep -E '^V[0-9]{4}__.*\.up\.sql$' | sort); do
    num="${f:1:4}"
    if [ "$num" -le 404 ]; then continue; fi
    if [ "$num" -ge 454 ]; then continue; fi   # V0454 单独上（要单测幂等）
    APPLIED=$((APPLIED+1))
    if ! psql_file "$MIG_DIR/$f"; then
        MIG_FAIL=$((MIG_FAIL+1))
        bad "迁移失败：$f（tail: $(tail -2 "$LOG" | tr '\n' ' ')）"
    fi
done
if [ "$MIG_FAIL" -eq 0 ]; then
    ok "$APPLIED 条 V0405…V0453 迁移全部应用成功（演练库推到 V0453）"
fi
MENU_BEFORE="$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")"
assert_eq "GONE" "$(psql_q "SELECT COALESCE(to_regclass('public.biz_supplier_evaluation')::text,'GONE')")" \
    "上 V0454 之前供应商评估表还不存在（下面每一条断言才说明是 V0454 建的）"

echo "== 1b) 应用 V0454（连跑两遍验幂等）"
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
assert_eq "$MENU_BEFORE" "$MENU_AFTER" "V0454 一条菜单都没加（上迁移前后菜单总数不变；本卡只做表结构）"

# ---------- 2) 结构断言 ----------
echo "== 2) 结构断言"
assert_eq "biz_supplier_evaluation" \
    "$(psql_q "SELECT COALESCE(to_regclass('public.biz_supplier_evaluation')::text,'')")" "评估表存在"
assert_eq "25" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='biz_supplier_evaluation'")" "评估表 25 列"
assert_eq "4" "$(psql_q "SELECT count(DISTINCT indexname) FROM pg_indexes WHERE schemaname='public' AND indexname IN ('uk_biz_sev_no','uk_biz_sev_supplier_period','idx_biz_sev_period','idx_biz_sev_grade')")" "4 个索引全在"
assert_eq "2" "$(psql_q "SELECT count(*) FROM pg_indexes WHERE schemaname='public' AND indexname IN ('uk_biz_sev_no','uk_biz_sev_supplier_period') AND indexdef LIKE '%WHERE (deleted_at IS NULL)%'")" "两个唯一索引都是部分索引（软删后可复用）"
assert_eq "ALWAYS" "$(psql_q "SELECT is_generated FROM information_schema.columns WHERE table_schema='public' AND table_name='biz_supplier_evaluation' AND column_name='total_score'")" "total_score 是 GENERATED ALWAYS 列（任何写入路径都写不进）"
assert_eq "1" "$(psql_q "SELECT (generation_expression LIKE '%quality_weight%' AND generation_expression LIKE '%service_score%' AND generation_expression LIKE 'round%')::int FROM information_schema.columns WHERE table_schema='public' AND table_name='biz_supplier_evaluation' AND column_name='total_score'")" "生成表达式是 round(Σ 四维权重 × 分数)"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_seq WHERE seq_type='SEV' AND deleted_at IS NULL AND prefix='SEV' AND separator='-'")" "取号规则 SEV 一行"
assert_eq "1" "$(psql_q "SELECT (count(*) > 180)::int FROM information_schema.tables WHERE table_schema='public'")" "模板库克隆完整（public 表 > 180 张）"
# 四维分数/权重列一个都不能少（改名时这里先炸）
assert_eq "4" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='biz_supplier_evaluation' AND column_name IN ('quality_score','delivery_score','price_score','service_score')")" "四维分数列（质量/交期/价格/服务）都在"
assert_eq "4" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='biz_supplier_evaluation' AND column_name IN ('quality_weight','delivery_weight','price_weight','service_weight')")" "四维权重列都在"

# ---------- 3) 约束行为：坏数据必须被拒 ----------
echo "== 3) 约束行为（卡面四维 + 权重 + 期间 + 供应商）"
rejects_c "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end) VALUES ('SEV-2026-1',gen_random_uuid(),'2026-01-01','2026-03-31')" \
    "旧形状 SEV-2026-1 的单号被拒" "ck_biz_sev_no_fmt"
rejects_c "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end) VALUES ('SV-19990101-900901',gen_random_uuid(),'2026-01-01','2026-03-31')" \
    "错前缀 SV-yyyyMMdd 的单号被拒" "ck_biz_sev_no_fmt"
rejects_c "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end) VALUES ('SEV-19990101-900902',gen_random_uuid(),'2026-03-31','2026-01-01')" \
    "评估期间起晚于止被拒" "ck_biz_sev_period"
rejects_c "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end, eval_date) VALUES ('SEV-19990101-900903',gen_random_uuid(),'2026-01-01','2026-03-31','2025-12-31')" \
    "评估日早于期间起点被拒（期间没开始就评完）" "ck_biz_sev_eval_date"
rejects_c "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end, quality_weight, delivery_weight, price_weight, service_weight) VALUES ('SEV-19990101-900904',gen_random_uuid(),'2026-01-01','2026-03-31',0.4,0.3,0.2,0.2)" \
    "四维权重之和 1.1 被拒" "ck_biz_sev_weights"
rejects_c "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end, quality_weight, delivery_weight, price_weight) VALUES ('SEV-19990101-900905',gen_random_uuid(),'2026-01-01','2026-03-31',0.4,0.3,0.2)" \
    "只写三个权重（第四项取默认 0.25 → 和 1.15）被拒" "ck_biz_sev_weights"
rejects_c "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end, quality_weight, delivery_weight, price_weight, service_weight) VALUES ('SEV-19990101-900906',gen_random_uuid(),'2026-01-01','2026-03-31',1.5,-0.5,0,0)" \
    "权重 1.5 / -0.5 被拒（和仍是 1，但单项越界）" "ck_biz_sev_weights"
rejects_c "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end, quality_score) VALUES ('SEV-19990101-900907',gen_random_uuid(),'2026-01-01','2026-03-31',100.01)" \
    "质量得分 100.01 被拒（百分制上限 100）" "ck_biz_sev_scores"
rejects_c "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end, service_score) VALUES ('SEV-19990101-900908',gen_random_uuid(),'2026-01-01','2026-03-31',-1)" \
    "负的服务得分被拒" "ck_biz_sev_scores"
rejects_c "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end, grade) VALUES ('SEV-19990101-900909',gen_random_uuid(),'2026-01-01','2026-03-31','E')" \
    "等级域外的 E 被拒" "ck_biz_sev_grade"
rejects_c "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end, grade) VALUES ('SEV-19990101-900910',gen_random_uuid(),'2026-01-01','2026-03-31','a')" \
    "小写等级 a 被拒" "ck_biz_sev_grade"
# 'UNRATED' 先撞列宽（VARCHAR(2) → 22001），走不到 CHECK —— 只要拒就行，别断言约束名
rejects_msg "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end, grade) VALUES ('SEV-19990101-900911',gen_random_uuid(),'2026-01-01','2026-03-31','UNRATED')" \
    "value too long" "自造的 UNRATED 等级被拒（未评级只许 NULL）"

# 正样本：四等分权重 + 80/90/70/60 的评估
psql_sql <<'SQL'
INSERT INTO biz_supplier_evaluation
    (id, evaluation_no, supplier_id, supplier_name, period_start, period_end, eval_date)
VALUES ('00000000-0000-4000-8000-000000412001'::uuid, 'SEV-19990101-900950', gen_random_uuid(),
        'M412-演练供应商', '2026-01-01', '2026-03-31', '2026-04-02');
SQL
if [ $? -eq 0 ]; then ok "正样本：默认四等分权重 + 期间 + 快照都能插"; else
    bad "正样本插不进去：$(tail -2 "$LOG" | tr '\n' ' ')"; fi

assert_eq "0.2500|0.2500|0.2500|0.2500" \
    "$(psql_q "SELECT quality_weight||'|'||delivery_weight||'|'||price_weight||'|'||service_weight FROM biz_supplier_evaluation WHERE id='00000000-0000-4000-8000-000000412001'::uuid")" \
    "四维权重默认值 = 四等分 0.25"
assert_eq "NULL" "$(psql_q "SELECT COALESCE(grade,'NULL') FROM biz_supplier_evaluation WHERE id='00000000-0000-4000-8000-000000412001'::uuid")" "grade 默认 NULL（未评级，不是字符串）"
assert_eq "0.00" "$(psql_q "SELECT total_score FROM biz_supplier_evaluation WHERE id='00000000-0000-4000-8000-000000412001'::uuid")" "四维全 0 分时总分为 0.00"

accepts "UPDATE biz_supplier_evaluation SET quality_weight=0.4, delivery_weight=0.3, price_weight=0.2, service_weight=0.1, quality_score=80, delivery_score=90, price_score=70, service_score=60 WHERE id='00000000-0000-4000-8000-000000412001'::uuid" \
    "40/30/20/10 权重 + 80/90/70/60 分数能落库"
assert_eq "79.00" "$(psql_q "SELECT total_score FROM biz_supplier_evaluation WHERE id='00000000-0000-4000-8000-000000412001'::uuid")" "加权总分由库算出 79.00（32+27+14+6）"
accepts "UPDATE biz_supplier_evaluation SET delivery_score=100 WHERE id='00000000-0000-4000-8000-000000412001'::uuid" \
    "改一维分数能落库"
assert_eq "82.00" "$(psql_q "SELECT total_score FROM biz_supplier_evaluation WHERE id='00000000-0000-4000-8000-000000412001'::uuid")" "改分数后总分自动重算 82.00（生成列）"
rejects_msg "UPDATE biz_supplier_evaluation SET total_score=1 WHERE id='00000000-0000-4000-8000-000000412001'::uuid" \
    "can only be updated to DEFAULT" "UPDATE 显式写 total_score 被拒（生成列）"
rejects_msg "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end, total_score) VALUES ('SEV-19990101-900951',gen_random_uuid(),'2026-04-01','2026-06-30',99)" \
    "cannot insert" "INSERT 显式写 total_score 被拒（生成列）"

# 唯一键：同号 / 同供应商同期间
rejects_c "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end) VALUES ('SEV-19990101-900950',gen_random_uuid(),'2025-01-01','2025-03-31')" \
    "同编号的第二份存活评估被拒" "uk_biz_sev_no"
rejects_c "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end) SELECT 'SEV-19990101-900952', supplier_id, period_start, period_end FROM biz_supplier_evaluation WHERE id='00000000-0000-4000-8000-000000412001'::uuid" \
    "同供应商同期间的第二份评估被拒" "uk_biz_sev_supplier_period"
accepts "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end) SELECT 'SEV-19990101-900953', supplier_id, '2026-04-01', '2026-06-30' FROM biz_supplier_evaluation WHERE id='00000000-0000-4000-8000-000000412001'::uuid" \
    "同供应商换期间（Q2）能插"
accepts "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end) VALUES ('SEV-19990101-900954',gen_random_uuid(),'2026-01-01','2026-03-31')" \
    "换供应商（同期间）能插"

# ---------- 4) 本卡不许夹带：权限码 0 / 菜单 0 / 供应商主数据 0 新列 ----------
echo "== 4) 本卡范围守卫（不夹带权限码、菜单、供应商主数据列）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code LIKE 'business:erp:supplier-evaluation:%' AND deleted_at IS NULL")" "评估权限码 0 行（M4-16 才落）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_menu WHERE path LIKE '%supplier-evaluation%' AND deleted_at IS NULL")" "评估菜单 0 行（M4-16 才落）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='biz_supplier' AND column_name LIKE '%evaluation%'")" "biz_supplier 0 个评估列（供应商主数据不动）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='biz_supplier' AND column_name LIKE '%grade%'")" "biz_supplier 0 个等级列（等级落在评估单上，不写回主数据）"
SRC_HITS="$(grep -rl "supplier-evaluation:" "$BACKEND/app/src/business/erp/purchase" 2>/dev/null | wc -l | tr -d ' ')"
assert_eq "0" "$SRC_HITS" "源码里 0 个 supplier-evaluation 权限码（连常量都不定义）"
ROUTE_HITS="$(grep -rn "supplier_evaluation" "$BACKEND/app/src/business/erp/purchase/mod.rs" | grep -c '#\[nik::' || true)"
assert_eq "0" "$ROUTE_HITS" "本卡 0 条路由（表结构卡不接线）"

# ---------- 5) 空库全链重放 ----------
echo "== 5) 空库全链重放（mig_replay_fresh.sh，独立库）"
if [ "$SKIP_REPLAY" = "1" ]; then
    info "SKIP_REPLAY=1：跳过（本地快速迭代用；提交前必须跑全）"
else
    REPLAY_OUT="$TMP/m412_$$_replay.out"
    if DBHOST="$PG_HOST" DBPORT="$PG_PORT" DBUSER="$PG_USER" DBPASS="$PGPASSWORD" DBOWNER="$PG_USER" \
       bash "$ROOT/scripts/verify/mig_replay_fresh.sh" "m412$$" >"$REPLAY_OUT" 2>&1; then
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
    -c "DELETE FROM biz_supplier_evaluation WHERE evaluation_no LIKE 'SEV-19990101-9%'" >/dev/null 2>&1
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_supplier_evaluation")" "清空后评估表真的空了（down 的数据护栏才会放行）"
if psql_file "$DOWN"; then ok "清空后 down 成功"; else bad "清空后 down 失败：$(tail -2 "$LOG" | tr '\n' ' ')"; fi
assert_eq "" "$(psql_q "SELECT COALESCE(to_regclass('public.biz_supplier_evaluation')::text,'')")" "down 后评估表已删"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_seq WHERE seq_type='SEV'")" "down 后取号行已删"
if psql_file "$MIG"; then ok "再 up 复原成功"; else bad "再 up 失败：$(tail -3 "$LOG" | tr '\n' ' ')"; fi
assert_eq "biz_supplier_evaluation" "$(psql_q "SELECT COALESCE(to_regclass('public.biz_supplier_evaluation')::text,'')")" "up 复原后表回来了"
assert_eq "25" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='biz_supplier_evaluation'")" "up 复原后列数仍是 25"

# ---------- 7) Rust 用例 ----------
echo "== 7) 真库用例（--test-threads=1 --nocapture）"
export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
export SYS_DB_URL="$ERP_DB_URL"
if [ "${M412_BAD_URL:-}" = "1" ]; then
    export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/no_such_db_skip_guard"
    export SYS_DB_URL="$ERP_DB_URL"
    info "SKIP 自检模式（M412_BAD_URL=1）：连接串故意写错，期望 SKIP>0"
fi

OUT="$TMP/m412_$$_tests.out"
( cd "$BACKEND" && cargo test -p cloudpivot --lib business::erp::purchase::supplier_evaluation -- --test-threads=1 --nocapture ) >"$OUT" 2>&1
LINE="$(grep -E '^test result: ' "$OUT" | tail -1)"
PASSED="$(printf '%s' "$LINE" | sed -n 's/.*ok\. \([0-9]*\) passed.*/\1/p')"
FAILED="$(printf '%s' "$LINE" | sed -n 's/.* \([0-9]*\) failed.*/\1/p')"
SKIPPED="$(grep -c 'SKIP:' "$OUT" || true)"
[ -z "${PASSED:-}" ] && PASSED=0
[ -z "${FAILED:-}" ] && FAILED=0
[ -z "${SKIPPED:-}" ] && SKIPPED=0
if [ "$FAILED" -eq 0 ] && [ "$PASSED" -ge 25 ]; then
    ok "本卡用例全绿（$PASSED 条：13 纯逻辑 + 12 真库）"
elif [ "$FAILED" -ne 0 ]; then
    bad "本卡用例有 $FAILED 条失败（见 $OUT）"
    grep -E 'panicked at|assertion|left:|right:' -A 2 "$OUT" | head -30 | sed 's/^/        /'
else
    bad "本卡用例只跑了 $PASSED 条（期望 ≥ 25，见 $OUT）"
fi
if [ "$SKIPPED" -eq 0 ]; then ok "本组 SKIP=0（真库用例没被「连不上」打成假绿）"; else
    bad "本组出现 $SKIPPED 行 SKIP（假绿）"; grep 'SKIP:' "$OUT" | head -5 | sed 's/^/        /'
fi
if grep -qF '[M4-12] 表形状' "$OUT" && grep -qF '[M4-12] 权重' "$OUT" \
   && grep -qF '[M4-12] 总分' "$OUT" && grep -qF '[M4-12] 对拍' "$OUT" \
   && grep -qF '[M4-12] 等级' "$OUT"; then
    ok "正向标记在位（[M4-12] 表形状 / 权重 / 总分 / 对拍 / 等级）"
else
    bad "缺正向标记 —— 用例没真跑（被跳过 / 被过滤）"
fi

# ---------- 8) 门禁：权限审计 + 菜单结构 ----------
echo "== 8) 门禁（permission_audit_tests + menu_invariants.sh）"
AUDIT_OUT="$TMP/m412_$$_audit.out"
( cd "$BACKEND" && cargo test -p cloudpivot --lib system::menu::permission_audit_tests -- --test-threads=1 ) >"$AUDIT_OUT" 2>&1
AUDIT_LINE="$(grep -E '^test result: ' "$AUDIT_OUT" | tail -1)"
if printf '%s' "$AUDIT_LINE" | grep -qE 'test result: ok\.' && ! printf '%s' "$AUDIT_LINE" | grep -qE '[1-9][0-9]* failed'; then
    ok "permission_audit_tests 全绿（本卡没动权限码，也不许打破审计）"
else
    bad "permission_audit_tests 不是全绿：${AUDIT_LINE:-无输出}（见 $AUDIT_OUT）"
fi
if PGHOST="$PG_HOST" PGPORT="$PG_PORT" PGUSER="$PG_USER" bash "$ROOT/scripts/verify/menu_invariants.sh" "$DB" >"$TMP/m412_$$_menu.out" 2>&1; then
    ok "menu_invariants.sh 8 条断言全绿"
else
    bad "menu_invariants.sh 报红（见 $TMP/m412_$$_menu.out）"
    tail -15 "$TMP/m412_$$_menu.out" | sed 's/^/        /'
fi
info "菜单总数：$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")（门禁区间 400-540；本卡零新增）"

# ---------- 9) 夹具残留 ----------
echo "== 9) 夹具残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_supplier_evaluation WHERE supplier_name LIKE 'M412-%' OR remark LIKE 'M412-%'")" "M412- 评估 0 残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_supplier_evaluation WHERE evaluation_no LIKE 'SEV-19990101-9%'")" "自检探针号段 0 残留"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_seq WHERE seq_type='SEV' AND deleted_at IS NULL")" "取号行复原后仍是 1 行"

# ---------- 10) 汇总 ----------
echo
if [ "$FAIL" -eq 0 ]; then
    printf '\033[32m== M4-12 演练结果：PASS=%s  FAIL=0 —— 供应商评估表结构（质量/交期/价格/服务 四维打分 + 权重 · 生成列总分）就绪\033[0m\n' "$PASS"
    exit 0
else
    printf '\033[31m== M4-12 演练结果：PASS=%s  FAIL=%s —— 见上面红行\033[0m\n' "$PASS" "$FAIL"
    exit 1
fi
