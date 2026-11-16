#!/usr/bin/env bash
# ============================================================================
# M3-01 迁移 V0432 真库往返演练（销售报价单 biz_quotation + biz_quotation_line）
#
# 卡：t_6a61726c（M3-01【M3·销售】报价单：表结构）
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. up ×2        —— 第一次建表，第二次证明幂等（重复执行零报错、探针零残留）
#   3. 结构断言      —— 17 + 12 列 / 10 条 CHECK / 1 条 CASCADE 外键 / 7 条索引 / 注释
#   4. 行为断言      —— 有效期（早于报价日必拒、当天有效必过）、状态域、价税恒等、
#                      转单回链成对、存活行单号唯一 + 软删后可复用、行号单内唯一、
#                      行数量/税率/金额域、删单头明细级联
#   5. 写入契约      —— QT 取号行唯一且可按天滚动参数就位；一张销售订单只能被一个报价单认领
#                      （M3-03 的转单幂等反向唯一）
#   6. down 护栏反证 —— 有非作废报价单时 down 必须拒绝且结构不动；全部置 CANCELLED 后才允许回滚
#   7. down ×2 / up ×1 —— down 幂等、回滚后可重放
#
# 用法：
#   bash scripts/verify/m301_sales_quotation_drill.sh                       # 99 → 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m301_sales_quotation_drill.sh     # 在 145 上跑
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名含 drill；脚本自己 DROP。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
UP="$ROOT/backend/app/migrations/business/V0432__sales_quotation.up.sql"
DOWN="$ROOT/backend/app/migrations/business/V0432__sales_quotation.down.sql"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="cp_m301_quotation_drill_$$"

TMP="${TMPDIR:-/tmp}"
PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }

psql_q() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }
runfile() { # 文件 日志
    psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -1 -f "$1" >"$2" 2>&1
}
assert_eq() { # 期望 实际 说明
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 $1，实际 $2）"; fi
}
assert_has() { # 文本 子串 说明
    case "$1" in *"$2"*) ok "$3" ;; *) bad "$3（输出里没有 [$2]：$1）" ;; esac
}
# expect_fail <sql> <错误码或文案> <说明>：SQL 必须失败，且报错里带指定片段
expect_fail() {
    local out; out="$(psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -tAc "$1" 2>&1)"
    case "$out" in
        *"$2"*) ok "$3" ;;
        *) bad "$3（期望被拒且含 [$2]，实际输出：$out）" ;;
    esac
}
# expect_ok <sql> <说明>：SQL 必须成功
expect_ok() {
    local out; out="$(psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -tAc "$1" 2>&1)"
    case "$out" in
        *ERROR*) bad "$2（期望成功，实际：$out）" ;;
        *) ok "$2" ;;
    esac
}

cleanup() { psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1; }
trap cleanup EXIT

echo "== M3-01 / V0432 真库往返演练（销售报价单 biz_quotation + biz_quotation_line）=="
echo "   PG=${PG_HOST}:${PG_PORT} 模板=${TEMPLATE_DB} 演练库=${DB}"
echo

# ---------- 0) 建演练库 ----------
if ! createdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -T "$TEMPLATE_DB" "$DB" 2>"$TMP/m301_createdb.err"; then
    echo "  模板库被占用，5 秒后重试一次"
    sleep 5
    createdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -T "$TEMPLATE_DB" "$DB" 2>"$TMP/m301_createdb.err" || {
        echo "  ⛔ 建库失败：$(cat "$TMP/m301_createdb.err")"; exit 1; }
fi
info "演练库已从 $TEMPLATE_DB 克隆"
assert_eq "0" "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name in ('biz_quotation','biz_quotation_line');")" \
    "克隆库升级前两张表都不存在"
assert_eq "1" "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_customer';")" \
    "前置表 biz_customer 在位（V0260 已应用）"
assert_eq "1" "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_seq';")" \
    "前置表 biz_seq 在位（V0137 已应用）"
assert_eq "0" "$(psql_q "select count(*) from biz_seq where seq_type='QT';")" \
    "克隆库里还没有 QT 取号行"

# ---------- 1) up ×2 ----------
echo
echo "== 1) up 第一次 =="
if runfile "$UP" "$TMP/m301_up1.log"; then
    ok "up 第一次执行成功（建表 + 自检块通过）"
    grep -E "NOTICE:.*(前置表齐备|自检通过)" "$TMP/m301_up1.log" | sed 's/^/       /'
else
    bad "up 第一次执行失败"
    tail -20 "$TMP/m301_up1.log"
fi

echo
echo "== 2) up 第二次（幂等）=="
if runfile "$UP" "$TMP/m301_up2.log"; then
    ok "up 重复执行成功（幂等）"
else
    bad "up 重复执行失败"
    tail -20 "$TMP/m301_up2.log"
fi
assert_eq "0|0" "$(psql_q "select (select count(*) from biz_quotation)||'|'||(select count(*) from biz_quotation_line);")" \
    "重放后探针零残留（头 0 行 / 明细 0 行）"
assert_eq "1" "$(psql_q "select count(*) from biz_seq where seq_type='QT';")" \
    "重放后 QT 取号行仍只有 1 行（种子是 WHERE NOT EXISTS）"

# ---------- 3) 结构断言 ----------
echo
echo "== 3) 结构断言 =="
assert_eq "17" "$(psql_q "select count(*) from information_schema.columns where table_schema='public' and table_name='biz_quotation';")" \
    "头表列数 = 17"
assert_eq "12" "$(psql_q "select count(*) from information_schema.columns where table_schema='public' and table_name='biz_quotation_line';")" \
    "行表列数 = 12"

assert_eq "8" "$(psql_q "select count(*) from information_schema.columns
   where table_schema='public' and table_name='biz_quotation'
     and ((column_name='quote_no'       and character_maximum_length=32 and is_nullable='NO')
       or (column_name='customer_id'    and data_type='uuid' and is_nullable='NO')
       or (column_name='quote_date'     and data_type='date' and is_nullable='NO')
       or (column_name='valid_until'    and data_type='date' and is_nullable='NO')
       or (column_name='status'         and character_maximum_length=24 and is_nullable='NO')
       or (column_name='total_amount'   and numeric_precision=18 and numeric_scale=4 and is_nullable='NO')
       or (column_name='total_tax'      and numeric_precision=18 and numeric_scale=4 and is_nullable='NO')
       or (column_name='total_with_tax' and numeric_precision=18 and numeric_scale=4 and is_nullable='NO'));")" \
    "头表 8 个关键列的类型/精度/可空性"

assert_eq "8" "$(psql_q "select count(*) from information_schema.columns
   where table_schema='public' and table_name='biz_quotation_line'
     and ((column_name='quotation_id' and data_type='uuid'    and is_nullable='NO')
       or (column_name='line_no'      and data_type='integer' and is_nullable='NO')
       or (column_name='material_id'  and data_type='uuid'    and is_nullable='NO')
       or (column_name='qty'          and numeric_precision=18 and numeric_scale=4 and is_nullable='NO')
       or (column_name='price'        and numeric_precision=18 and numeric_scale=6 and is_nullable='NO')
       or (column_name='amount'       and numeric_precision=18 and numeric_scale=4 and is_nullable='NO')
       or (column_name='tax_rate'     and numeric_precision=7  and numeric_scale=4 and is_nullable='NO')
       or (column_name='tax_amount'   and numeric_precision=18 and numeric_scale=4 and is_nullable='NO'));")" \
    "行表 8 个关键列的类型/精度/可空性"

assert_eq "DRAFT" "$(psql_q "select replace(split_part(column_default,'::',1),'''','') from information_schema.columns
   where table_schema='public' and table_name='biz_quotation' and column_name='status';")" \
    "status 默认 DRAFT"

assert_eq "5" "$(psql_q "select count(*) from pg_constraint where conrelid='public.biz_quotation'::regclass
   and contype='c' and conname in ('ck_biz_quotation_status','ck_biz_quotation_valid_until',
   'ck_biz_quotation_amount_nonneg','ck_biz_quotation_amount','ck_biz_quotation_converted');")" \
    "头表 5 条 CHECK 在位"
assert_eq "5" "$(psql_q "select count(*) from pg_constraint where conrelid='public.biz_quotation_line'::regclass
   and contype='c' and conname in ('ck_biz_quotation_line_no','ck_biz_quotation_line_qty',
   'ck_biz_quotation_line_price','ck_biz_quotation_line_rate','ck_biz_quotation_line_amount');")" \
    "行表 5 条 CHECK 在位"

assert_eq "fk_biz_quotation_line_quotation|FOREIGN KEY (quotation_id) REFERENCES biz_quotation(id) ON DELETE CASCADE" \
    "$(psql_q "select conname||'|'||pg_get_constraintdef(oid) from pg_constraint where conname='fk_biz_quotation_line_quotation';")" \
    "明细外键 = REFERENCES biz_quotation(id) ON DELETE CASCADE"

assert_eq "7" "$(psql_q "select count(*) from pg_indexes where schemaname='public'
   and tablename in ('biz_quotation','biz_quotation_line')
   and (indexname like 'uk_biz_quotation%' or indexname like 'idx_biz_quotation%');")" \
    "7 条索引（头 5 + 行 2）在位"

assert_has "$(psql_q "select indexdef from pg_indexes where indexname='uk_biz_quotation_no';")" \
    "WHERE (deleted_at IS NULL)" "单号唯一是「存活行唯一」的部分索引"
assert_has "$(psql_q "select indexdef from pg_indexes where indexname='uk_biz_quotation_converted_so';")" \
    "WHERE (converted_so_id IS NOT NULL)" "转单唯一是部分索引（未转单的行不占名额）"
assert_eq "1" "$(psql_q "select count(*) from pg_indexes where indexname='uk_biz_quotation_line_no' and indexdef like '%UNIQUE%';")" \
    "行号唯一索引是 UNIQUE"

assert_eq "1" "$(psql_q "select count(*) from pg_description d join pg_class c on c.oid=d.objoid
   where c.relname='biz_quotation' and d.objsubid=0;")" "头表有表注释"
TBL_COMMENT="$(psql_q "select obj_description('public.biz_quotation'::regclass,'pg_class');")"
assert_has "$TBL_COMMENT" "销售报价单头" "表注释说明是销售报价单"

# ---------- 4) 行为断言（CHECK / 唯一 / 级联）----------
echo
echo "== 4) 行为断言 =="
Q1="'d0432aaa-0000-4000-8000-000000000001'::uuid"
Q2="'d0432aaa-0000-4000-8000-000000000002'::uuid"
SO1="'d0432aaa-0000-4000-8000-000000000003'::uuid"
SO2="'d0432aaa-0000-4000-8000-000000000004'::uuid"
CUS="'d0432aaa-0000-4000-8000-000000000005'::uuid"
MAT="'d0432aaa-0000-4000-8000-000000000006'::uuid"

expect_ok "insert into biz_quotation(id,quote_no,customer_id,quote_date,valid_until)
  values($Q1,'__M301_PROBE_Q1__',$CUS,date '2026-11-17',date '2026-11-30');" \
  "正常报价单可写"
assert_eq "DRAFT|0.0000|0.0000|0.0000" "$(psql_q "select status||'|'||total_amount::text||'|'||total_tax::text||'|'||total_with_tax::text
   from biz_quotation where id=$Q1;")" \
  "默认值 = DRAFT / 三个金额 0"

expect_fail "insert into biz_quotation(quote_no,customer_id,quote_date,valid_until)
  values('__M301_PROBE_X__',$CUS,date '2026-11-17',date '2026-11-16');" \
  "ck_biz_quotation_valid_until" "有效期早于报价日被拒"
expect_ok "insert into biz_quotation(id,quote_no,customer_id,quote_date,valid_until)
  values($Q2,'__M301_PROBE_Q2__',$CUS,date '2026-11-17',date '2026-11-17');" \
  "有效期 = 报价日（当天有效）允许"
expect_fail "insert into biz_quotation(quote_no,customer_id,quote_date,valid_until,status)
  values('__M301_PROBE_X__',$CUS,date '2026-11-17',date '2026-11-30','WIN');" \
  "ck_biz_quotation_status" "状态域外的值（WIN）被拒"
expect_fail "insert into biz_quotation(quote_no,customer_id,quote_date,valid_until,total_amount,total_tax,total_with_tax)
  values('__M301_PROBE_X__',$CUS,date '2026-11-17',date '2026-11-30',100,13,112);" \
  "ck_biz_quotation_amount" "价税合计不等（100+13≠112）被拒"
expect_ok "insert into biz_quotation(quote_no,customer_id,quote_date,valid_until,total_amount,total_tax,total_with_tax)
  values('__M301_PROBE_AMT__',$CUS,date '2026-11-17',date '2026-11-30',100,13,113);" \
  "价税合计相等（100+13=113）允许"
expect_fail "insert into biz_quotation(quote_no,customer_id,quote_date,valid_until,converted_so_id)
  values('__M301_PROBE_X__',$CUS,date '2026-11-17',date '2026-11-30',$SO1);" \
  "ck_biz_quotation_converted" "转单回链只填 id 不填时间被拒"
expect_fail "insert into biz_quotation(quote_no,customer_id,quote_date,valid_until)
  values('__M301_PROBE_Q1__',$CUS,date '2026-11-17',date '2026-11-30');" \
  "uk_biz_quotation_no" "存活行单号重号被拒"

# 软删后单号可复用（部分唯一索引的业务价值）
expect_ok "update biz_quotation set deleted_at=current_timestamp where id=$Q1;" "软删 Q1"
expect_ok "insert into biz_quotation(quote_no,customer_id,quote_date,valid_until)
  values('__M301_PROBE_Q1__',$CUS,date '2026-11-17',date '2026-11-30');" \
  "软删后同单号可复用"
Q1B="$(psql_q "select id from biz_quotation where quote_no='__M301_PROBE_Q1__' and deleted_at is null;")"

# 行明细
expect_fail "insert into biz_quotation_line(quotation_id,line_no,material_id,qty)
  values($Q2,0,$MAT,1);" "ck_biz_quotation_line_no" "行号 0 被拒"
expect_fail "insert into biz_quotation_line(quotation_id,line_no,material_id,qty)
  values($Q2,1,$MAT,0);" "ck_biz_quotation_line_qty" "数量 0 被拒"
expect_fail "insert into biz_quotation_line(quotation_id,line_no,material_id,qty,tax_rate)
  values($Q2,1,$MAT,1,101);" "ck_biz_quotation_line_rate" "税率 101 被拒"
expect_ok "insert into biz_quotation_line(quotation_id,line_no,material_id,qty,price,amount,tax_rate,tax_amount)
  values($Q2,1,$MAT,10,9.5,95,13,12.35);" "正常行明细可写"
expect_fail "insert into biz_quotation_line(quotation_id,line_no,material_id,qty)
  values($Q2,1,$MAT,1);" "uk_biz_quotation_line_no" "同一报价单内行号重号被拒"
expect_ok "insert into biz_quotation_line(quotation_id,line_no,material_id,qty)
  values($Q2,2,$MAT,1);" "同一报价单不同行号可写"

# 级联：删单头 → 明细一起走
expect_ok "insert into biz_quotation(id,quote_no,customer_id,quote_date,valid_until)
  values('d0432aaa-0000-4000-8000-0000000000ff'::uuid,'__M301_PROBE_CAS__',$CUS,date '2026-11-17',date '2026-11-30');" \
  "建级联探针单头"
expect_ok "insert into biz_quotation_line(quotation_id,line_no,material_id,qty)
  values('d0432aaa-0000-4000-8000-0000000000ff'::uuid,1,$MAT,3);" "建级联探针明细"
assert_eq "1" "$(psql_q "select count(*) from biz_quotation_line where quotation_id='d0432aaa-0000-4000-8000-0000000000ff'::uuid;")" \
  "级联探针明细在位"
expect_ok "delete from biz_quotation where id='d0432aaa-0000-4000-8000-0000000000ff'::uuid;" "删单头"
assert_eq "0" "$(psql_q "select count(*) from biz_quotation_line where quotation_id='d0432aaa-0000-4000-8000-0000000000ff'::uuid;")" \
  "删单头后明细被级联删除（ON DELETE CASCADE）"

# ---------- 5) 写入契约（下游 M3-02 / M3-03 依赖）----------
echo
echo "== 5) 写入契约 =="
assert_eq "QT|yyyyMMdd|1|-|1|报价单号|1" \
  "$(psql_q "select seq_type||'|'||date_pattern||'|'||step||'|'||separator||'|'||status||'|'||seq_name||'|'||(remark like '报价单单号：QT-yyyyMMdd-000001%')::int
   from biz_seq where seq_type='QT';")" \
  "QT 取号行：QT-yyyyMMdd-000001 / 按天滚动 / 启用"

# 一张销售订单只能被一个报价单认领（M3-03 转单的反向唯一）
expect_ok "update biz_quotation set converted_so_id=$SO1, converted_at=current_timestamp where id=$Q2;" \
  "报价单认领 SO1（转单回链成对写入）"
expect_fail "update biz_quotation set converted_so_id=$SO1, converted_at=current_timestamp where id in (select id from biz_quotation where quote_no='__M301_PROBE_Q1__' and deleted_at is null);" \
  "uk_biz_quotation_converted_so" "第二张报价单认领同一张 SO 被拒"
expect_ok "update biz_quotation set converted_so_id=$SO2, converted_at=current_timestamp where id in (select id from biz_quotation where quote_no='__M301_PROBE_Q1__' and deleted_at is null);" \
  "另一张报价单认领不同的 SO 允许"

# ---------- 6) down 护栏反证 ----------
echo
echo "== 6) down 护栏反证 =="
OUT="$(psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -1 -f "$DOWN" 2>&1)"
assert_has "$OUT" "V0432 down 拒绝执行" "有非作废报价单时 down 被拒"
assert_eq "1|1" "$(psql_q "select (select count(*) from information_schema.tables where table_name='biz_quotation')||'|'||(select count(*) from information_schema.tables where table_name='biz_quotation_line');")" \
  "被拒后两张表结构不动"

expect_ok "update biz_quotation set status='CANCELLED';" "全部置 CANCELLED"
if runfile "$DOWN" "$TMP/m301_down1.log"; then
    ok "CANCELLED 后 down 执行成功"
else
    bad "down 执行失败"
    tail -20 "$TMP/m301_down1.log"
fi
assert_eq "0|0|0" "$(psql_q "select (select count(*) from information_schema.tables where table_schema='public' and table_name in ('biz_quotation','biz_quotation_line'))||'|'||(select count(*) from pg_indexes where tablename in ('biz_quotation','biz_quotation_line'))||'|'||(select count(*) from biz_seq where seq_type='QT');")" \
  "down 后表/索引/QT 取号行全部移除"

echo
echo "== 7) down 幂等 + 重放 =="
if runfile "$DOWN" "$TMP/m301_down2.log"; then
    ok "down 第二次执行成功（幂等）"
else
    bad "down 第二次执行失败"
    tail -20 "$TMP/m301_down2.log"
fi
if runfile "$UP" "$TMP/m301_up3.log"; then
    ok "回滚后可重放（up 第三次成功）"
else
    bad "回滚后重放失败"
    tail -20 "$TMP/m301_up3.log"
fi
assert_eq "17|12|1" "$(psql_q "select (select count(*) from information_schema.columns where table_name='biz_quotation')||'|'||(select count(*) from information_schema.columns where table_name='biz_quotation_line')||'|'||(select count(*) from biz_seq where seq_type='QT');")" \
  "重放后结构/取号行完整"

echo
echo "== 结果：PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] || exit 1
