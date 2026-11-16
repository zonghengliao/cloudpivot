#!/usr/bin/env bash
# ============================================================================
# M1-13 迁移 V0414 真库往返演练（现金流底稿表 biz_cashflow_worksheet）
#
# 卡：t_31c91cd5
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆一个演练库（库名含 drill，脚本自己 DROP）
#   2. up ×2        —— 第一次建表，第二次证明幂等（重复执行零报错、零残留）
#   3. 结构断言      —— 14 列 / 7 条 CHECK / 4 条索引（2 条必须是部分唯一索引）/ 注释在位
#   4. 行为断言      —— 明细行可写、upsert 幂等（同期间同项目只一行）、负数被拒、
#                      缺活动被拒、非法 line_type/期间被拒、每期间只有一份期初/期末
#   5. 勾稽契约      —— 期初 + 流入 − 流出 = 期末（单表聚合，M1-14 的校验口径）
#   6. down ×2      —— 第一次回滚，第二次证明 down 幂等
#   7. up ×1        —— 证明回滚后可重放
#   8. down 护栏反证 —— 造一行底稿后 down 必须 RAISE EXCEPTION 拒绝（且表不动）
#
# 用法：
#   bash scripts/verify/m113_cashflow_worksheet_drill.sh            # 默认 99 → 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m113_cashflow_worksheet_drill.sh   # 在 145 上跑
#
# 环境变量（默认值 = 开发机 99 连 145 的 PG，见卡面「数据库约束」）：
#   PG_HOST (172.18.10.145) / PG_PORT (5432) / PG_USER (cloudpivot) /
#   PGPASSWORD（默认取 145 开发库口令，仓库内既有脚本同值）/ TEMPLATE_DB (cloudpivot_test)
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名必须含 drill/e2e/test；脚本自己 DROP。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
UP="$ROOT/backend/app/migrations/business/V0414__cashflow_worksheet.up.sql"
DOWN="$ROOT/backend/app/migrations/business/V0414__cashflow_worksheet.down.sql"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="cp_m113_v0414_drill_$$"

TMP="${TMPDIR:-/tmp}"
PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }

psql_q() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
runfile() { # 文件 日志
    psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -1 -f "$1" >"$2" 2>&1
}
assert_eq() { # 期望 实际 说明
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 $1，实际 $2）"; fi
}

cleanup() {
    dropdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" --if-exists "$DB" >/dev/null 2>&1
}
trap cleanup EXIT

echo "== M1-13 / V0414 真库往返演练（现金流底稿表）=="
echo "   PG=${PG_HOST}:${PG_PORT} 模板=${TEMPLATE_DB} 演练库=${DB}"
echo

# ---------- 0) 建演练库 ----------
if ! createdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -T "$TEMPLATE_DB" "$DB" 2>"$TMP/m113_createdb.err"; then
    echo "  ⛔ 建库失败：$(cat "$TMP/m113_createdb.err")"
    exit 1
fi
info "演练库已从 $TEMPLATE_DB 克隆"
assert_eq "0" "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_cashflow_worksheet';")" \
    "克隆库升级前表不存在"

# ---------- 1) up ×2 ----------
echo
echo "== 1) up 第一次 =="
if runfile "$UP" "$TMP/m113_up1.log"; then
    ok "up 第一次执行成功（建表 + 自检块通过）"
    grep -E "NOTICE:.*(前置表齐备|自检通过)" "$TMP/m113_up1.log" | sed 's/^/       /'
else
    bad "up 第一次执行失败"
    tail -20 "$TMP/m113_up1.log"
fi

echo
echo "== 2) up 第二次（幂等）=="
if runfile "$UP" "$TMP/m113_up2.log"; then
    ok "up 重复执行成功（幂等，探针零残留）"
else
    bad "up 重复执行失败"
    tail -20 "$TMP/m113_up2.log"
fi

# ---------- 3) 结构断言 ----------
echo
echo "== 3) 结构断言 =="
assert_eq "14" "$(psql_q "select count(*) from information_schema.columns
   where table_schema='public' and table_name='biz_cashflow_worksheet';")" \
   "列数 = 14"

assert_eq "7" "$(psql_q "select count(*) from information_schema.columns
   where table_schema='public' and table_name='biz_cashflow_worksheet'
     and ((column_name='period_code' and character_maximum_length=7  and is_nullable='NO')
       or (column_name='line_type'   and character_maximum_length=8  and is_nullable='NO')
       or (column_name='item_code'   and character_maximum_length=32 and is_nullable='YES')
       or (column_name='item_name'   and character_maximum_length=64 and is_nullable='NO')
       or (column_name='activity'    and character_maximum_length=12 and is_nullable='YES')
       or (column_name='direction'   and character_maximum_length=8  and is_nullable='YES')
       or (column_name='amount'      and numeric_precision=18 and numeric_scale=4 and is_nullable='NO'));")" \
   "7 个关键列的长度/精度/可空性（amount = NUMERIC(18,4)）"

assert_eq "FLOW" "$(psql_q "select btrim(regexp_replace(column_default,'::.*\$',''), '''')
   from information_schema.columns
   where table_name='biz_cashflow_worksheet' and column_name='line_type';")" \
   "line_type 默认 FLOW"

assert_eq "7" "$(psql_q "select count(*) from pg_constraint where conrelid='biz_cashflow_worksheet'::regclass
   and contype='c' and conname like 'ck_biz_cashflow_ws_%';")" \
   "7 条 CHECK 在位"

assert_eq "4" "$(psql_q "select count(*) from pg_indexes where schemaname='public'
   and tablename='biz_cashflow_worksheet'
   and (indexname like 'uk_biz_cashflow_ws_%' or indexname like 'idx_biz_cashflow_ws_%');")" \
   "4 条索引在位"

assert_eq "2" "$(psql_q "select count(*) from pg_indexes where schemaname='public'
   and tablename='biz_cashflow_worksheet' and indexname like 'uk_biz_cashflow_ws_%'
   and indexdef like '%UNIQUE INDEX%' and indexdef like '%WHERE%';")" \
   "两条唯一索引都是「部分唯一索引」（余额行 item_code 为 NULL，普通唯一约束挡不住重复）"

assert_eq "1" "$(psql_q "select count(*) from pg_description d
   join pg_class c on c.oid=d.objoid
   where c.relname='biz_cashflow_worksheet' and d.objsubid=0 and d.description like '%现金流量表编制底稿行%';")" \
   "表注释在位（说明这是底稿行不是分类配置）"

# ---------- 4) 行为断言 ----------
echo
echo "== 4) 行为断言（约束真的会拦）=="
assert_eq "1" "$(psql_q "insert into biz_cashflow_worksheet
   (period_code,line_type,item_code,item_name,activity,direction,amount,sort_order)
   values ('1900-02','FLOW','PROBE_A','__drill__','OPERATING','INFLOW',100,101);" 2>&1 | grep -c 'INSERT 0 1')" \
   "明细行可写"

# upsert 契约：M1-14 的写法（ON CONFLICT 部分唯一索引）
psql_q "insert into biz_cashflow_worksheet
   (period_code,line_type,item_code,item_name,activity,direction,amount,sort_order)
   values ('1900-02','FLOW','PROBE_A','__drill__','OPERATING','INFLOW',1200,101)
   on conflict (period_code, item_code) where item_code is not null
   do update set amount=excluded.amount;" >/dev/null 2>&1
assert_eq "1200.0000|1" "$(psql_q "select amount||'|'||count(*) over ()
   from biz_cashflow_worksheet where period_code='1900-02' and item_code='PROBE_A';")" \
   "同期间同项目第二次写入是 upsert（一行，金额被覆盖）"

assert_eq "1" "$(psql_q "insert into biz_cashflow_worksheet
   (period_code,line_type,item_code,item_name,activity,direction,amount)
   values ('1900-02','FLOW','PROBE_NEG','__drill__','OPERATING','OUTFLOW',-1);" 2>&1 | grep -c 'ck_biz_cashflow_ws_flow_amount')" \
   "负数明细行被 ck_biz_cashflow_ws_flow_amount 拦下"

assert_eq "1" "$(psql_q "insert into biz_cashflow_worksheet
   (period_code,line_type,item_code,item_name,amount)
   values ('1900-02','FLOW','PROBE_SHAPE','__drill__',10);" 2>&1 | grep -c 'ck_biz_cashflow_ws_flow_shape')" \
   "明细行缺 activity/direction 被 ck_biz_cashflow_ws_flow_shape 拦下"

assert_eq "1" "$(psql_q "insert into biz_cashflow_worksheet
   (period_code,line_type,item_code,item_name,amount)
   values ('1900-02','BOGUS',NULL,'__drill__',1);" 2>&1 | grep -c 'ck_biz_cashflow_ws_line_type')" \
   "非法 line_type 被拦下（item_code/activity/direction 留空，只违反这一条 CHECK）"

assert_eq "1" "$(psql_q "insert into biz_cashflow_worksheet
   (period_code,line_type,item_name,amount)
   values ('2026-13','OPENING','__drill__',1);" 2>&1 | grep -c 'ck_biz_cashflow_ws_period')" \
   "非法期间 2026-13 被拦下（否则这行永远聚合不进任何一期）"

assert_eq "1" "$(psql_q "insert into biz_cashflow_worksheet
   (period_code,line_type,item_name,amount)
   values ('1900-02','OPENING','期初现金及现金等价物余额',5000);" 2>&1 | grep -c 'INSERT 0 1')" \
   "余额行（三要素为空）可写"

assert_eq "1" "$(psql_q "insert into biz_cashflow_worksheet
   (period_code,line_type,item_name,amount)
   values ('1900-02','OPENING','期初现金及现金等价物余额',1);" 2>&1 | grep -c 'uk_biz_cashflow_ws_balance')" \
   "同期间第二个期初余额被 uk_biz_cashflow_ws_balance 拦下（两个期末=两个真相）"

assert_eq "1" "$(psql_q "insert into biz_cashflow_worksheet
   (period_code,line_type,item_name,amount)
   values ('1900-02','CLOSING','期末现金及现金等价物余额',9999);" 2>&1 | grep -c 'INSERT 0 1')" \
   "期末余额可写"

# 余额行允许负数（透支 / 受限资金）
psql_q "insert into biz_cashflow_worksheet
   (period_code,line_type,item_name,amount) values ('1900-03','CLOSING','期末现金及现金等价物余额',-20);" >/dev/null 2>&1
assert_eq "-20.0000" "$(psql_q "select amount from biz_cashflow_worksheet where period_code='1900-03' and line_type='CLOSING';")" \
   "余额行允许负数（银行透支/受限资金录得进）"

# ---------- 5) 勾稽契约（M1-14 口径）----------
echo
echo "== 5) 勾稽契约：期初 + 流入 − 流出 = 期末（单表聚合）=="
psql_q "insert into biz_cashflow_worksheet
   (period_code,line_type,item_code,item_name,activity,direction,amount,sort_order)
   values ('1900-04','FLOW','OPER_SALES_RECEIPTS','销售商品收到的现金','OPERATING','INFLOW',1000,101),
          ('1900-04','FLOW','OPER_PURCHASE_PAYMENTS','购买商品支付的现金','OPERATING','OUTFLOW',300,201),
          ('1900-04','FLOW','INVEST_PURCHASES','购建长期资产支付的现金','INVESTING','OUTFLOW',200,351);
   insert into biz_cashflow_worksheet (period_code,line_type,item_name,amount,sort_order)
   values ('1900-04','OPENING','期初现金及现金等价物余额',500,1),
          ('1900-04','CLOSING','期末现金及现金等价物余额',1000,9999);" >/dev/null 2>&1
assert_eq "0.0000" "$(psql_q "select (max(case when line_type='OPENING' then amount end)
    + sum(case when line_type='FLOW' and direction='INFLOW' then amount else 0 end)
    - sum(case when line_type='FLOW' and direction='OUTFLOW' then amount else 0 end)
    - max(case when line_type='CLOSING' then amount end))::numeric(18,4)
   from biz_cashflow_worksheet where period_code='1900-04';")" \
   "勾稽闭合（500 + 1000 − 500 = 1000）"

assert_eq "OPERATING|1000.0000|300.0000" "$(psql_q "select activity||'|'||
    sum(case when direction='INFLOW' then amount else 0 end)||'|'||
    sum(case when direction='OUTFLOW' then amount else 0 end)
   from biz_cashflow_worksheet where period_code='1900-04' and activity='OPERATING' group by activity;")" \
   "按活动分流入/流出小计（经营活动 1000 / 300）"

# ---------- 6) 清探针行 ----------
psql_q "delete from biz_cashflow_worksheet where period_code like '1900-%';" >/dev/null 2>&1
assert_eq "0" "$(psql_q "select count(*) from biz_cashflow_worksheet;")" \
    "探针行已清空"

# ---------- 7) down ×2 + 重放 ----------
echo
echo "== 6) down ×2（回滚 + 幂等）=="
if runfile "$DOWN" "$TMP/m113_down1.log"; then
    ok "down 第一次执行成功"
else
    bad "down 第一次执行失败"; tail -20 "$TMP/m113_down1.log"
fi
assert_eq "0" "$(psql_q "select count(*) from information_schema.tables
   where table_schema='public' and table_name='biz_cashflow_worksheet';")" \
   "回滚后表已移除"

if runfile "$DOWN" "$TMP/m113_down2.log"; then
    ok "down 重复执行成功（幂等）"
else
    bad "down 重复执行失败"; tail -20 "$TMP/m113_down2.log"
fi

echo
echo "== 7) 回滚后重放 up =="
if runfile "$UP" "$TMP/m113_up3.log"; then
    ok "回滚后重放 up 成功"
else
    bad "回滚后重放 up 失败"; tail -20 "$TMP/m113_up3.log"
fi
assert_eq "14" "$(psql_q "select count(*) from information_schema.columns
   where table_schema='public' and table_name='biz_cashflow_worksheet';")" \
   "重放后 14 列回来了"

# ---------- 8) down 护栏反证 ----------
echo
echo "== 8) down 护栏反证（有底稿行必须拒绝回滚）=="
psql_q "insert into biz_cashflow_worksheet
   (period_code,line_type,item_code,item_name,activity,direction,amount)
   values ('1900-05','FLOW','PROBE_GUARD','__drill__','OPERATING','INFLOW',1);" >/dev/null 2>&1
if runfile "$DOWN" "$TMP/m113_down_guard.log"; then
    bad "down 竟然在有底稿行时成功了（护栏失效）"
else
    if grep -q "V0414 down 拒绝执行" "$TMP/m113_down_guard.log"; then
        ok "down 被护栏拒绝并给出去向说明"
        grep "V0414 down 拒绝执行" "$TMP/m113_down_guard.log" | sed 's/^/       /'
    else
        bad "down 失败但原因不是护栏（$(tail -3 "$TMP/m113_down_guard.log"))"
    fi
fi
assert_eq "14" "$(psql_q "select count(*) from information_schema.columns
   where table_schema='public' and table_name='biz_cashflow_worksheet';")" \
   "拒绝回滚后结构未被破坏（14 列仍在）"
psql_q "delete from biz_cashflow_worksheet where period_code like '1900-%';" >/dev/null 2>&1

# ---------- 汇总 ----------
echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] || exit 1
