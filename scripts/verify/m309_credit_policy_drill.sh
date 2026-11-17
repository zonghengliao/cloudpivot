#!/usr/bin/env bash
# ============================================================================
# M3-09 客户信用「额度字段 + 超限策略配置」· 真库演练
#
# 卡：t_464b7265（M3-09【M3·销售】客户信用：额度字段 + 校验配置）
# 上游：t_8d301195（M3-08 报价单转订单单测，已把 V0404 之前的库结构坐实）
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. 补模板落后的业务迁移 V0405…V0437（本卡新增 V0437__customer_credit_policy）
#   3. **迁移三连**：up → down → up，证明 V0437 两个方向都幂等、且回滚不动别人的列
#   4. 断言列 / 约束 / 默认值就位（结构问题与环境问题在这里一眼分开）
#   5. 真库用例（--test-threads=1）：
#        · 本卡 credit 域（策略判定纯函数 + 策略读写 + 警示台账）
#        · customer 域（CRUD + 策略默认/归一/400）
#        · sales 域（订单提交守卫 BLOCK/WARN/PASS 三态）
#      **SKIP=0** 必须断言（真库用例被「无 ERP_DB_URL」打成 SKIP 就是假绿）
#   6. 断言夹具清场：CR38- / SO5HT- / SO2T- / M309- 与警示台账 0 残留
#   7. 打印 PASS/FAIL 汇总；任一不符退出码 1
#
# 用法：
#   bash scripts/verify/m309_credit_policy_drill.sh                    # 99 → 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m309_credit_policy_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m309_credit_policy_drill.sh          # 留库排查
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名含 drill；脚本自己 DROP。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIG_DIR="$ROOT/backend/app/migrations/business"
APP_DIR="$ROOT/backend/app"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="cp_m309_credit_drill_$$"
KEEP_DB="${KEEP_DB:-0}"

TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
LOG="$TMP/m309_drill_$$.log"
PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }
psql_file()  { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$1" >"$LOG" 2>&1; }
assert_eq() { # 期望 实际 说明
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 [$1]，实际 [$2]）"; fi
}
assert_ge() { # 下限 实际 说明
    if [ "${2:-0}" -ge "$1" ] 2>/dev/null; then ok "$3（=$2）"; else bad "$3（期望 ≥ $1，实际 [$2]）"; fi
}
assert_le() { # 上限 实际 说明
    if [ "${2:-0}" -le "$1" ] 2>/dev/null; then ok "$3（=$2 ≤ 基线 $1）"; else bad "$3（期望 ≤ $1，实际 [$2]）"; fi
}

cleanup() {
    if [ "$KEEP_DB" = "1" ]; then
        printf '\n  （KEEP_DB=1：演练库保留 → %s）\n' "$DB"
        return
    fi
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
    rm -f "$LOG"
}
trap cleanup EXIT

printf '== M3-09 客户信用额度 + 超限策略 · 演练（PG %s:%s / 库 %s）==\n' "$PG_HOST" "$PG_PORT" "$DB"

# ---------- 0) 模板与前置 ----------
echo "== 0) 前置：模板库可克隆"
if ! psql_admin "SELECT 1 FROM pg_database WHERE datname = '$TEMPLATE_DB'" | grep -q 1; then
    bad "模板库 $TEMPLATE_DB 不存在（请先在两台机器的 PG 里准备模板库）"
    exit 1
fi
ok "模板库 $TEMPLATE_DB 存在"

psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
if psql_admin "CREATE DATABASE $DB TEMPLATE $TEMPLATE_DB" >/dev/null 2>&1; then
    ok "从模板克隆演练库 $DB"
else
    bad "克隆演练库失败"
    exit 1
fi

PSQL_MIG="psql -h $PG_HOST -p $PG_PORT -U $PG_USER -d $DB -v ON_ERROR_STOP=1 -q"

# ---------- 1) 补齐迁移 V0405…V0437 ----------
echo "== 1) 补齐业务迁移 V0405…V0437（幂等，逐条 ON_ERROR_STOP）"
MIG_FAIL=0
APPLIED=0
for f in $(ls "$MIG_DIR" | grep -E '^V04(0[5-9]|[1-3][0-9])__.*\.up\.sql$' | sort); do
    APPLIED=$((APPLIED+1))
    if ! psql_file "$MIG_DIR/$f"; then
        MIG_FAIL=$((MIG_FAIL+1))
        bad "迁移失败：$f（tail: $(tail -2 "$LOG" | tr '\n' ' ')）"
    fi
done
if [ "$MIG_FAIL" -eq 0 ]; then
    ok "$APPLIED 条业务迁移全部应用成功"
fi

# ---------- 2) V0437 三连：up → down → up（幂等 + 回滚不动别人） ----------
echo "== 2) V0437 迁移幂等：重复 up / down / 再 up"
if $PSQL_MIG -f "$MIG_DIR/V0437__customer_credit_policy.up.sql" >"$LOG" 2>&1; then
    ok "V0437 重复 up 成功（0 行影响、不报重复约束）"
else
    bad "V0437 重复 up 失败：$(tail -2 "$LOG" | tr '\n' ' ')"
fi
assert_eq "credit_policy" "$(psql_q "SELECT column_name FROM information_schema.columns WHERE table_name='biz_customer' AND column_name='credit_policy'")" "列仍在（重复 up 没把表搞坏）"

if $PSQL_MIG -f "$MIG_DIR/V0437__customer_credit_policy.down.sql" >"$LOG" 2>&1; then
    ok "V0437 down 成功"
else
    bad "V0437 down 失败：$(tail -2 "$LOG" | tr '\n' ' ')"
fi
assert_eq "0" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_name='biz_customer' AND column_name='credit_policy'")" "down 后 credit_policy 列已移除"
assert_eq "3" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_name='biz_customer' AND column_name IN ('credit_limit','credit_used','credit_days')")" "down 没顺手删掉额度口径三列"

if $PSQL_MIG -f "$MIG_DIR/V0437__customer_credit_policy.up.sql" >"$LOG" 2>&1; then
    ok "V0437 down 之后再 up 成功（两个方向都幂等）"
else
    bad "V0437 再 up 失败：$(tail -2 "$LOG" | tr '\n' ' ')"
fi

# ---------- 3) 结构断言 ----------
echo "== 3) 结构断言（列 / 类型 / NOT NULL / 默认值 / CHECK 域）"
assert_eq "character varying|10|NO" \
    "$(psql_q "SELECT data_type||'|'||character_maximum_length||'|'||is_nullable FROM information_schema.columns WHERE table_name='biz_customer' AND column_name='credit_policy'")" \
    "credit_policy = varchar(10) NOT NULL"
assert_eq "1" "$(psql_q "SELECT count(*) FROM pg_constraint WHERE conname='ck_biz_customer_credit_policy'")" "CHECK 约束 ck_biz_customer_credit_policy 在位"
assert_eq "1" "$(psql_q "SELECT count(*) FROM pg_constraint WHERE conname='ck_biz_customer_credit_policy' AND pg_get_constraintdef(oid) LIKE '%BLOCK%' AND pg_get_constraintdef(oid) LIKE '%WARN%' AND pg_get_constraintdef(oid) LIKE '%PASS%'")" "CHECK 域值含 BLOCK / WARN / PASS"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_customer WHERE credit_policy NOT IN ('BLOCK','WARN','PASS')")" "存量行全部在域内"
# 默认值：省略该列的 INSERT 必须落 BLOCK（插一条读回再删掉，结尾断言 0 残留）
DEFAULT_PROBE="$(psql_q "INSERT INTO biz_customer(code,name) VALUES ('__M309_PROBE__','探针') RETURNING credit_policy" | head -1)"
assert_eq "BLOCK" "$DEFAULT_PROBE" "省略 credit_policy 的 INSERT 落默认 BLOCK"
psql_q "DELETE FROM biz_customer WHERE code LIKE '__M309_PROBE%'" >/dev/null
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_customer WHERE code LIKE '__M309_PROBE%'")" "探针行 0 残留"
# 越界值必须被拒
NEG="$(psql_q "BEGIN; INSERT INTO biz_customer(code,name,credit_policy) VALUES ('__M309_BAD__','探针','BOGUS'); ROLLBACK;" | tr '\n' ' ')"
if printf '%s' "$NEG" | grep -qi "ck_biz_customer_credit_policy\|check"; then
    ok "越界策略被 CHECK 拒绝（不是靠应用层兜）"
else
    bad "越界策略没被库层拒绝：$NEG"
fi

# ---------- 4) 真库用例 ----------
export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
export SYS_DB_URL="$ERP_DB_URL"
echo "== 4) 真库用例（--test-threads=1）"

run_group() { # 模块路径 最少用例数 说明
    local module="$1" min="$2" label="$3" out line passed failed skipped
    out="$TMP/m309_$$_${module//[:/]/_}.out"
    ( cd "$APP_DIR" && cargo test --lib "$module" -- --test-threads=1 ) >"$out" 2>&1
    line="$(grep -E '^test result: ' "$out" | head -1)"
    passed="$(printf '%s' "$line" | sed -n 's/.*ok\. \([0-9]*\) passed.*/\1/p')"
    failed="$(printf '%s' "$line" | sed -n 's/.* \([0-9]*\) failed.*/\1/p')"
    skipped="$(grep -c '^SKIP' "$out" || true)"
    [ -z "${passed:-}" ] && passed=0
    [ -z "${failed:-}" ] && failed=0
    [ -z "${skipped:-}" ] && skipped=0
    if [ "${failed:-0}" -eq 0 ]; then
        assert_ge "$min" "$passed" "$label：$module 全绿（$passed 条）"
    else
        bad "$label：$module 有 $failed 条失败（见 $out）"
    fi
    if [ "$skipped" -eq 0 ]; then
        ok "$label：本组 SKIP=0（真库用例没被打成假绿）"
    else
        bad "$label：本组出现 $skipped 行 SKIP（真库用例被跳过 = 假绿，见 $out）"
    fi
}

run_group "business::erp::credit" 20 "本卡信用域（策略判定 + 策略读写 + 警示台账）"
run_group "business::erp::customer" 20 "客户档案域（CRUD + 策略默认/归一/400 不被打破）"
run_group "business::erp::sales" 200 "销售域整组（订单提交守卫三态 + 报价单 / 退货 / 审批桥）"

# ---------- 5) 夹具清场 ----------
echo "== 5) 夹具清场（不允许把本卡夹具带出演练库）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_customer WHERE code LIKE 'CR38-%'")" "biz_customer 无 CR38- 夹具客户（credit 域）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_customer WHERE code LIKE 'SO5HT-%'")" "biz_customer 无 SO5HT- 夹具客户（订单守卫）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_customer WHERE code LIKE 'SO2T-%'")" "biz_customer 无 SO2T- 夹具客户（客户档案）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_customer WHERE code LIKE '__V0437_PROBE%' OR code LIKE '__M309_%'")" "迁移 / 演练探针 0 残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_sales_order o LEFT JOIN biz_customer c ON c.id = o.customer_id WHERE c.id IS NULL AND o.so_no LIKE 'SO38-%'")" "无 SO38- 孤儿订单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_credit_log l LEFT JOIN biz_customer c ON c.id = l.customer_id WHERE c.code LIKE 'CR38-%' OR c.code LIKE 'SO5HT-%'")" "警示台账无夹具客户残留行"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_credit_log WHERE cause = 'OVER_LIMIT_WARN' AND ref_no LIKE 'SO38-%'")" "无 SO38- 警示台账残留"

# ---------- 汇总 ----------
printf '\n== 汇总：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "ALL PASS"
    exit 0
fi
echo "有失败项，见上面的 FAIL 行"
exit 1
