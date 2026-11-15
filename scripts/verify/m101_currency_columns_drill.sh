#!/usr/bin/env bash
# ============================================================================
# M1-01 迁移 V0406 真库往返演练（biz_account.currency_code + biz_voucher.currency_code/exchange_rate）
#
# 卡：t_76e55ffd
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆一个演练库（库名含 drill，脚本自己 DROP）
#   2. up  ×2   —— 第一次建列，第二次证明幂等（重复执行零报错、零残留）
#   3. 结构断言 —— 三列类型/精度/默认值/NOT NULL、3 条 CHECK、2 条索引、存量回填全 CNY
#   4. down ×2  —— 第一次回滚，第二次证明 down 幂等
#   5. up  ×1   —— 证明回滚后可重放
#   6. down 护栏反证 —— 先造一条外币科目，down 必须 RAISE EXCEPTION 拒绝（且结构不动）
#
# 用法：
#   bash scripts/verify/m101_currency_columns_drill.sh
#
# 环境变量（默认值 = 开发机 99 连 145 的 PG）：
#   PG_HOST (172.18.10.145) / PG_PORT (5432) / PG_USER (cloudpivot) /
#   PGPASSWORD（默认取 145 开发库口令，仓库内既有脚本同值）/ TEMPLATE_DB (cloudpivot_test)
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名必须含 drill/e2e/test；脚本自己 DROP。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
UP="$ROOT/backend/app/migrations/business/V0406__account_voucher_currency.up.sql"
DOWN="$ROOT/backend/app/migrations/business/V0406__account_voucher_currency.down.sql"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="cp_m101_v0406_drill_$$"

PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }

psql_q() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
assert_eq() { # 期望 实际 说明
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 $1，实际 $2）"; fi
}

cleanup() {
    dropdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" --if-exists "$DB" >/dev/null 2>&1
}
trap cleanup EXIT

echo "== M1-01 / V0406 真库往返演练 =="
echo "   PG=${PG_HOST}:${PG_PORT} 模板=${TEMPLATE_DB} 演练库=${DB}"
echo

# ---------- 0) 建演练库 ----------
if ! createdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -T "$TEMPLATE_DB" "$DB" 2>/tmp/m101_createdb.err; then
    echo "  ⛔ 建库失败：$(cat /tmp/m101_createdb.err)"
    exit 1
fi
info "演练库已从 $TEMPLATE_DB 克隆"
BASE_ACC=$(psql_q "select count(*) from biz_account;")
info "存量 biz_account 行数 = $BASE_ACC（回填对象）"

# ---------- 1) up ×2 ----------
echo
echo "== 1) up 第一次 =="
if psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -1 -f "$UP" >/tmp/m101_up1.log 2>&1; then
    ok "up 第一次执行成功（自检块通过）"
    grep -E "NOTICE:.*(回填|自检通过)" /tmp/m101_up1.log | sed 's/^/       /'
else
    bad "up 第一次执行失败"
    tail -20 /tmp/m101_up1.log
fi

echo
echo "== 2) up 第二次（幂等）=="
if psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -1 -f "$UP" >/tmp/m101_up2.log 2>&1; then
    ok "up 重复执行成功（幂等）"
else
    bad "up 重复执行失败"
    tail -20 /tmp/m101_up2.log
fi

# ---------- 3) 结构断言 ----------
echo
echo "== 3) 结构 / 回填断言 =="
assert_eq "3" "$(psql_q "select count(*) from information_schema.columns
   where (table_name='biz_account' and column_name='currency_code')
      or (table_name='biz_voucher' and column_name in ('currency_code','exchange_rate'));")" \
   "三列齐"

assert_eq "8" "$(psql_q "select character_maximum_length from information_schema.columns
   where table_name='biz_account' and column_name='currency_code';")" \
   "biz_account.currency_code 长度"

assert_eq "18,8" "$(psql_q "select numeric_precision||','||numeric_scale from information_schema.columns
   where table_name='biz_voucher' and column_name='exchange_rate';")" \
   "biz_voucher.exchange_rate 精度"

assert_eq "NO|'CNY'" "$(psql_q "select is_nullable||'|'||regexp_replace(column_default,'::.*\$','')
   from information_schema.columns where table_name='biz_voucher' and column_name='currency_code';")" \
   "biz_voucher.currency_code NOT NULL 且默认 CNY"

assert_eq "1" "$(psql_q "select column_default from information_schema.columns
   where table_name='biz_voucher' and column_name='exchange_rate';")" \
   "biz_voucher.exchange_rate 默认 1"

assert_eq "3" "$(psql_q "select count(*) from pg_constraint where conname in
   ('ck_biz_account_currency_code','ck_biz_voucher_currency_code','ck_biz_voucher_exchange_rate');")" \
   "3 条 CHECK 在位"

assert_eq "2" "$(psql_q "select count(*) from pg_indexes where indexname in
   ('idx_biz_account_currency','idx_biz_voucher_currency');")" \
   "2 条索引在位"

assert_eq "1" "$(psql_q "select count(*) from pg_indexes where indexname='idx_biz_voucher_currency'
   and indexdef like '%WHERE%CNY%';")" \
   "凭证索引是「只收外币」的部分索引"

assert_eq "0" "$(psql_q "select count(*) from biz_account where currency_code is null or currency_code <> 'CNY';")" \
   "存量科目全部回填为 CNY"

assert_eq "0" "$(psql_q "select count(*) from biz_voucher where currency_code is null or exchange_rate is null;")" \
   "存量凭证币种/汇率无空值"

# 约束真的会拦（行为反证）
assert_eq "1" "$(psql_q "insert into biz_account (account_code,account_name,level,account_category,balance_direction,currency_code)
   values ('V0406DRILL','__drill__',1,'ASSET','DEBIT','usd');" | grep -c 'ck_biz_account_currency_code')" \
   "小写币种被 ck_biz_account_currency_code 拦下"
assert_eq "1" "$(psql_q "insert into biz_voucher (voucher_word,voucher_no,period,voucher_date,total_debit,total_credit,entry_count,currency_code,exchange_rate)
   values ('记','V0406DRILL','1900-01',DATE '1900-01-01',1,1,2,'USD',0);" | grep -c 'ck_biz_voucher_exchange_rate')" \
   "汇率 0 被 ck_biz_voucher_exchange_rate 拦下"
assert_eq "1" "$(psql_q "insert into biz_voucher (voucher_word,voucher_no,period,voucher_date,total_debit,total_credit,entry_count,currency_code,exchange_rate)
   values ('记','V0406DRILL','1900-01',DATE '1900-01-01',1,1,2,'US',1);" | grep -c 'ck_biz_voucher_currency_code')" \
   "两位币种被 ck_biz_voucher_currency_code 拦下"

# ---------- 4) down ×2 ----------
echo
echo "== 4) down ×2（回滚 + 幂等）=="
if psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -1 -f "$DOWN" >/tmp/m101_down1.log 2>&1; then
    ok "down 第一次执行成功"
else
    bad "down 第一次执行失败"; tail -20 /tmp/m101_down1.log
fi
assert_eq "0" "$(psql_q "select count(*) from information_schema.columns
   where (table_name='biz_account' and column_name='currency_code')
      or (table_name='biz_voucher' and column_name in ('currency_code','exchange_rate'));")" \
   "回滚后三列已移除"

if psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -1 -f "$DOWN" >/tmp/m101_down2.log 2>&1; then
    ok "down 重复执行成功（幂等）"
else
    bad "down 重复执行失败"; tail -20 /tmp/m101_down2.log
fi

# ---------- 5) 回滚后重放 up ----------
echo
echo "== 5) 回滚后重放 up =="
if psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -1 -f "$UP" >/tmp/m101_up3.log 2>&1; then
    ok "回滚后重放 up 成功"
else
    bad "回滚后重放 up 失败"; tail -20 /tmp/m101_up3.log
fi
assert_eq "3" "$(psql_q "select count(*) from information_schema.columns
   where (table_name='biz_account' and column_name='currency_code')
      or (table_name='biz_voucher' and column_name in ('currency_code','exchange_rate'));")" \
   "重放后三列回来了"

# ---------- 6) down 护栏反证 ----------
echo
echo "== 6) down 护栏反证（有外币数据必须拒绝回滚）=="
psql_q "insert into biz_account (account_code,account_name,level,account_category,balance_direction,currency_code)
        values ('V0406GUARD','__drill__',1,'ASSET','DEBIT','USD');" >/dev/null
if psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -1 -f "$DOWN" >/tmp/m101_down_guard.log 2>&1; then
    bad "down 竟然在有外币科目时成功了（护栏失效）"
else
    if grep -q "V0406 down 拒绝执行" /tmp/m101_down_guard.log; then
        ok "down 被护栏拒绝并给出去向说明"
        grep "V0406 down 拒绝执行" /tmp/m101_down_guard.log | sed 's/^/       /'
    else
        bad "down 失败但原因不是护栏（$(tail -3 /tmp/m101_down_guard.log))"
    fi
fi
assert_eq "3" "$(psql_q "select count(*) from information_schema.columns
   where (table_name='biz_account' and column_name='currency_code')
      or (table_name='biz_voucher' and column_name in ('currency_code','exchange_rate'));")" \
   "拒绝回滚后结构未被破坏（三列仍在）"
psql_q "delete from biz_account where account_code='V0406GUARD';" >/dev/null

# ---------- 汇总 ----------
echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] || exit 1
