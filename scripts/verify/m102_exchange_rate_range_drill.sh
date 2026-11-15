#!/usr/bin/env bash
# ============================================================================
# M1-02 迁移 V0407 真库往返演练（biz_exchange_rate 生效区间 effective_to + 区间排除约束）
#
# 卡：t_438a4c0d（M1-02【M1·财务】多币种：迁移 biz_exchange_rate 加生效区间 + 唯一键）
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. 预置 5 行汇率（3 组：USD/SPOT 三行、USD/MONTH_END 一行、EUR/SPOT 一行），
#      用来验证「回填把时间轴切成首尾相接的段」这件事在**多行**上真的对
#   3. up ×1 —— 建列 + 回填 + 两条约束 + 自检块（7 组行为探针）
#   4. 结构断言 —— 列型 / CHECK / EXCLUDE（含 DEFERRABLE）/ V0272 的 uk 未被拆
#   5. 回填断言 —— 每一行的 effective_to 逐行对拍（不是只看「非空」）
#   6. up ×2 —— 幂等：第二次执行后整表指纹与第一次完全一致
#   7. 行为反证 —— 重叠插入被 ex 拒 / 反向区间被 ck 拒 / 同起点被 uk 拒 / 相邻放行
#   8. down ×2 —— 回滚 + 幂等；回滚后 5 行业务数据原样还在（派生列丢了不算丢数据）
#   9. up ×1 —— 回滚后重放，回填结果与第一次逐行相同
#
# 用法：
#   bash scripts/verify/m102_exchange_rate_range_drill.sh
#
# 环境变量（默认 = 开发机 99 连 145 的 PG）：
#   PG_HOST (172.18.10.145) / PG_PORT (5432) / PG_USER (cloudpivot) /
#   PGPASSWORD（默认取 145 开发库口令，仓库内既有脚本同值）/ TEMPLATE_DB (cloudpivot_test)
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名必须含 drill/e2e/test；脚本自己 DROP。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
UP="$ROOT/backend/app/migrations/business/V0407__exchange_rate_effective_range.up.sql"
DOWN="$ROOT/backend/app/migrations/business/V0407__exchange_rate_effective_range.down.sql"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="cp_m102_v0407_drill_$$"
ERRDIR="${ERRDIR:-$(mktemp -d)}"

PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }

psql_q() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
assert_eq() { # 期望 实际 说明
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 $1，实际 $2）"; fi
}
run_sqlfile() { # 文件 日志
    psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -1 -f "$1" >"$2" 2>&1
}
# 整表指纹：区间一旦算错，指纹立刻变
fingerprint() {
    psql_q "select coalesce(md5(string_agg(from_currency||to_currency||rate_type||effective_date::text||
                     coalesce(effective_to::text,'NULL')||rate::text, '|' order by from_currency,to_currency,rate_type,effective_date)),'-')
             from biz_exchange_rate;"
}
to_edges() { # 期望 from,to,type,effective_date : 打印 effective_to
    psql_q "select coalesce(effective_to::text,'NULL') from biz_exchange_rate
             where from_currency='$1' and to_currency='$2' and rate_type='$3' and effective_date=DATE '$4';"
}

cleanup() { dropdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" --if-exists "$DB" >/dev/null 2>&1; }
trap cleanup EXIT

echo "== M1-02 / V0407 真库往返演练 =="
echo "   PG=${PG_HOST}:${PG_PORT} 模板=${TEMPLATE_DB} 演练库=${DB}"
echo

# ---------- 0) 建演练库 ----------
if ! createdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -T "$TEMPLATE_DB" "$DB" 2>"$ERRDIR/createdb.err"; then
    echo "  ⛔ 建库失败：$(cat "$ERRDIR/createdb.err")"
    exit 1
fi
info "演练库已从 $TEMPLATE_DB 克隆"

# ---------- 0.5) 预置多行汇率（回填对象） ----------
# 模板库自带 USD→CNY SPOT 2026-11-01 / 2026-11-15 两行；再补三行凑出「三行同组」与「单行组」
psql_q "insert into biz_exchange_rate (from_currency,to_currency,rate,rate_type,effective_date,remark)
        values ('USD','CNY',7.05,'SPOT',DATE '2026-12-01','__M102_SEED__'),
               ('USD','CNY',7.02,'MONTH_END',DATE '2026-11-30','__M102_SEED__'),
               ('EUR','CNY',7.65,'SPOT',DATE '2026-11-10','__M102_SEED__');" >/dev/null
BASE_ROWS=$(psql_q "select count(*) from biz_exchange_rate;")
BASE_FP=$(fingerprint)
info "预置后存量行数 = $BASE_ROWS（USD/SPOT 3 行、USD/MONTH_END 1 行、EUR/SPOT 1 行）"

# ---------- 1) up 第一次 ----------
echo
echo "== 1) up 第一次（建列 + 回填 + 双约束 + 自检）=="
if run_sqlfile "$UP" "$ERRDIR/up1.log"; then
    ok "up 第一次执行成功（含自检块）"
    grep -E "NOTICE:.*(V0407|M1-02)" "$ERRDIR/up1.log" | sed 's/^/       /'
else
    bad "up 第一次执行失败"; tail -25 "$ERRDIR/up1.log"
fi

# ---------- 2) 结构断言 ----------
echo
echo "== 2) 结构断言 =="
assert_eq "date|YES" "$(psql_q "select data_type||'|'||is_nullable from information_schema.columns
   where table_name='biz_exchange_rate' and column_name='effective_to';")" \
   "effective_to 是可空 DATE"

assert_eq "1" "$(psql_q "select count(*) from pg_constraint
   where conrelid='public.biz_exchange_rate'::regclass and conname='ck_biz_exchange_rate_dates' and contype='c';")" \
   "ck_biz_exchange_rate_dates 在位"

assert_eq "1" "$(psql_q "select count(*) from pg_constraint
   where conrelid='public.biz_exchange_rate'::regclass
     and conname='ex_biz_exchange_rate_period' and contype='x';")" \
   "ex_biz_exchange_rate_period 排除约束在位"

assert_eq "1" "$(psql_q "select count(*) from pg_constraint
   where conrelid='public.biz_exchange_rate'::regclass and conname='ex_biz_exchange_rate_period'
     and pg_get_constraintdef(oid) like '%daterange%'
     and pg_get_constraintdef(oid) like '%&&%'
     and pg_get_constraintdef(oid) like '%DEFERRABLE%'
     and pg_get_constraintdef(oid) like '%rate_type%';")" \
   "排除约束是 (币对+类型, daterange &&) 且 DEFERRABLE"

assert_eq "1" "$(psql_q "select count(*) from pg_indexes
   where schemaname='public' and indexname='uk_biz_exchange_rate' and indexdef like 'CREATE UNIQUE INDEX%';")" \
   "V0272 的 uk_biz_exchange_rate 仍在（upsert 目标不许拆）"

assert_eq "1" "$(psql_q "select count(*) from pg_extension where extname='btree_gist';")" \
   "btree_gist 扩展在位"

# ---------- 3) 回填断言（逐行对拍） ----------
echo
echo "== 3) 回填断言（时间轴首尾相接）=="
assert_eq "$BASE_ROWS" "$(psql_q "select count(*) from biz_exchange_rate;")" "回填不增删行"
assert_eq "2026-11-14" "$(to_edges USD CNY SPOT 2026-11-01)" "USD/SPOT 11-01 的失效日 = 下一行前一天"
assert_eq "2026-11-30" "$(to_edges USD CNY SPOT 2026-11-15)" "USD/SPOT 11-15 的失效日 = 下一行前一天"
assert_eq "NULL"       "$(to_edges USD CNY SPOT 2026-12-01)" "USD/SPOT 末行保持 NULL（长期有效）"
assert_eq "NULL"       "$(to_edges USD CNY MONTH_END 2026-11-30)" "USD/MONTH_END 单行组保持 NULL"
assert_eq "NULL"       "$(to_edges EUR CNY SPOT 2026-11-10)" "EUR/SPOT 单行组保持 NULL"
assert_eq "0" "$(psql_q "select count(*) from biz_exchange_rate a join biz_exchange_rate b
     on a.id<>b.id and a.from_currency=b.from_currency and a.to_currency=b.to_currency
        and a.rate_type=b.rate_type
        and daterange(a.effective_date,a.effective_to,'[]') && daterange(b.effective_date,b.effective_to,'[]');")" \
   "全表零重叠对"

# ---------- 4) up 第二次（幂等） ----------
echo
echo "== 4) up 第二次（幂等）=="
FP_AFTER_UP1=$(fingerprint)
if run_sqlfile "$UP" "$ERRDIR/up2.log"; then
    ok "up 重复执行成功"
else
    bad "up 重复执行失败"; tail -25 "$ERRDIR/up2.log"
fi
assert_eq "$FP_AFTER_UP1" "$(fingerprint)" "第二次执行后整表指纹不变（回填不覆盖已派生值）"

# ---------- 5) 行为反证 ----------
echo
echo "== 5) 行为反证（库层真的会拦）=="
# 5.1 重叠：USD/SPOT 11-15 起是开放区间，插 11-20 必须被拒
assert_eq "1" "$(psql_q "insert into biz_exchange_rate
   (from_currency,to_currency,rate,rate_type,effective_date,remark)
   values ('USD','CNY',7.3,'SPOT',DATE '2026-11-20','__M102_PROBE__');" | grep -c 'ex_biz_exchange_rate_period')" \
   "插入落在开放区间内的新起点被 ex 拒"

# 5.2 反向区间：止早于起 → ck 拒
assert_eq "1" "$(psql_q "insert into biz_exchange_rate
   (from_currency,to_currency,rate,rate_type,effective_date,effective_to,remark)
   values ('USD','CNY',7.3,'SPOT',DATE '2026-11-20',DATE '2026-11-19','__M102_PROBE__');" \
   | grep -c 'ck_biz_exchange_rate_dates')" \
   "effective_to < effective_date 被 ck 拒"

# 5.3 同起点：uk 拒
assert_eq "1" "$(psql_q "insert into biz_exchange_rate
   (from_currency,to_currency,rate,rate_type,effective_date,remark)
   values ('USD','CNY',7.3,'SPOT',DATE '2026-11-15','__M102_PROBE__');" | grep -c 'uk_biz_exchange_rate')" \
   "同起点重复被 uk 拒"

# 5.4 相邻放行：先把末行收口到 12-31，再插 2027-01-01 —— 必须成功
psql_q "update biz_exchange_rate set effective_to=DATE '2026-12-31'
         where from_currency='USD' and to_currency='CNY' and rate_type='SPOT'
           and effective_date=DATE '2026-12-01';" >/dev/null
ADJ=$(psql_q "insert into biz_exchange_rate
   (from_currency,to_currency,rate,rate_type,effective_date,remark)
   values ('USD','CNY',7.4,'SPOT',DATE '2027-01-01','__M102_PROBE__');" )
assert_eq "INSERT 0 1" "$ADJ" "相邻区间（12-31 失效 / 01-01 生效）放行"

# 5.5 清理探针行，恢复原状
psql_q "delete from biz_exchange_rate where remark='__M102_PROBE__';" >/dev/null
psql_q "update biz_exchange_rate set effective_to=NULL
         where from_currency='USD' and to_currency='CNY' and rate_type='SPOT'
           and effective_date=DATE '2026-12-01';" >/dev/null
assert_eq "$FP_AFTER_UP1" "$(fingerprint)" "探针清理后回到 up 完成态"

# ---------- 6) down ×2 ----------
echo
echo "== 6) down ×2（回滚 + 幂等）=="
if run_sqlfile "$DOWN" "$ERRDIR/down1.log"; then
    ok "down 第一次执行成功"
else
    bad "down 第一次执行失败"; tail -20 "$ERRDIR/down1.log"
fi
assert_eq "0" "$(psql_q "select count(*) from information_schema.columns
   where table_name='biz_exchange_rate' and column_name='effective_to';")" "回滚后 effective_to 已移除"
assert_eq "0" "$(psql_q "select count(*) from pg_constraint
   where conrelid='public.biz_exchange_rate'::regclass
     and conname in ('ex_biz_exchange_rate_period','ck_biz_exchange_rate_dates');")" "回滚后两条约束已移除"
assert_eq "$BASE_ROWS" "$(psql_q "select count(*) from biz_exchange_rate;")" "回滚后业务行一行不少"
assert_eq "1" "$(psql_q "select count(*) from pg_indexes where indexname='uk_biz_exchange_rate';")" \
   "回滚后 uk_biz_exchange_rate 仍在"

if run_sqlfile "$DOWN" "$ERRDIR/down2.log"; then
    ok "down 重复执行成功（幂等）"
else
    bad "down 重复执行失败"; tail -20 "$ERRDIR/down2.log"
fi

# ---------- 7) 回滚后重放 up ----------
echo
echo "== 7) 回滚后重放 up（回填必须逐行复现）=="
if run_sqlfile "$UP" "$ERRDIR/up3.log"; then
    ok "回滚后重放 up 成功"
else
    bad "回滚后重放 up 失败"; tail -25 "$ERRDIR/up3.log"
fi
assert_eq "$FP_AFTER_UP1" "$(fingerprint)" "重放后整表指纹与第一次完全一致"

# ---------- 汇总 ----------
echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] || exit 1
