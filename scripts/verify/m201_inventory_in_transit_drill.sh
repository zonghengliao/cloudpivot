#!/usr/bin/env bash
# ============================================================================
# M2-01 迁移 V0420 真库往返演练（在途库存账 biz_inventory_in_transit）
#
# 卡：t_7ba5b0e2（M2-01【M2·仓库】在途库存：表结构）
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. up ×2        —— 第一次建表，第二次证明幂等（重复执行零报错、零残留）
#   3. 结构断言      —— 15 列 / 生成列 / 4 条 CHECK / 1 条 CASCADE 外键 / 4 条索引 / 注释
#   4. 行为断言      —— 生成列由 PG 算、直接写生成列被拒、唯一键拦重号、数量与状态
#                      CHECK 会拦脏数据、外键拦悬空 shipment_id
#   5. 写入契约      —— M2-02 的 upsert（同键累加 shipped_qty，只一行）；
#                      M2-03 的一条 UPDATE 推进 received_qty + status（分两句写被拦）
#   6. 对账恒等      —— 在途 + 已签收 = 发货量（M2-05 口径），且与独立 SQL 聚合一致
#   7. down ×2      —— 第一次回滚，第二次证明 down 幂等
#   8. up ×1        —— 证明回滚后可重放
#   9. down 护栏反证 —— 有在途/已签收行时 down 必须拒绝且结构不动；
#                      全部置成 CANCELLED 后才允许回滚
#
# 用法：
#   bash scripts/verify/m201_inventory_in_transit_drill.sh                       # 99 → 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m201_inventory_in_transit_drill.sh     # 在 145 上跑
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名含 drill；脚本自己 DROP。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
UP="$ROOT/backend/app/migrations/business/V0420__inventory_in_transit.up.sql"
DOWN="$ROOT/backend/app/migrations/business/V0420__inventory_in_transit.down.sql"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="cp_m201_transit_drill_$$"

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

cleanup() {
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
}
trap cleanup EXIT

echo "== M2-01 / V0420 真库往返演练（在途库存账 biz_inventory_in_transit）=="
echo "   PG=${PG_HOST}:${PG_PORT} 模板=${TEMPLATE_DB} 演练库=${DB}"
echo

# ---------- 0) 建演练库 ----------
if ! createdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -T "$TEMPLATE_DB" "$DB" 2>"$TMP/m201_createdb.err"; then
    echo "  模板库被占用，5 秒后重试一次"
    sleep 5
    createdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -T "$TEMPLATE_DB" "$DB" 2>"$TMP/m201_createdb.err" || {
        echo "  ⛔ 建库失败：$(cat "$TMP/m201_createdb.err")"; exit 1; }
fi
info "演练库已从 $TEMPLATE_DB 克隆"
assert_eq "0" "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_inventory_in_transit';")" \
    "克隆库升级前表不存在"
assert_eq "1" "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_shipment';")" \
    "前置表 biz_shipment 在位（V0283 已应用）"

# ---------- 1) up ×2 ----------
echo
echo "== 1) up 第一次 =="
if runfile "$UP" "$TMP/m201_up1.log"; then
    ok "up 第一次执行成功（建表 + 自检块通过）"
    grep -E "NOTICE:.*(前置表齐备|自检通过)" "$TMP/m201_up1.log" | sed 's/^/       /'
else
    bad "up 第一次执行失败"
    tail -20 "$TMP/m201_up1.log"
fi

echo
echo "== 2) up 第二次（幂等）=="
if runfile "$UP" "$TMP/m201_up2.log"; then
    ok "up 重复执行成功（幂等，探针零残留）"
else
    bad "up 重复执行失败"
    tail -20 "$TMP/m201_up2.log"
fi

# ---------- 3) 结构断言 ----------
echo
echo "== 3) 结构断言 =="
assert_eq "15" "$(psql_q "select count(*) from information_schema.columns
   where table_schema='public' and table_name='biz_inventory_in_transit';")" \
   "列数 = 15"

assert_eq "7" "$(psql_q "select count(*) from information_schema.columns
   where table_schema='public' and table_name='biz_inventory_in_transit'
     and ((column_name='shipment_id'  and data_type='uuid' and is_nullable='NO')
       or (column_name='material_id'  and data_type='uuid' and is_nullable='NO')
       or (column_name='warehouse_id' and data_type='uuid' and is_nullable='NO')
       or (column_name='ship_date'    and data_type='date' and is_nullable='NO')
       or (column_name='shipped_qty'  and numeric_precision=18 and numeric_scale=4 and is_nullable='NO')
       or (column_name='received_qty' and numeric_precision=18 and numeric_scale=4 and is_nullable='NO')
       or (column_name='status'       and character_maximum_length=24 and is_nullable='NO'));")" \
   "7 个关键列的类型/精度/可空性"

assert_eq "IN_TRANSIT" "$(psql_q "select replace(split_part(column_default,'::',1),'''','')
   from information_schema.columns
   where table_name='biz_inventory_in_transit' and column_name='status';")" \
   "status 默认 IN_TRANSIT"

assert_eq "ALWAYS|1" "$(psql_q "select is_generated||'|'||(case when generation_expression like '%shipped_qty%' and generation_expression like '%received_qty%' then 1 else 0 end)
   from information_schema.columns
   where table_name='biz_inventory_in_transit' and column_name='qty_in_transit';")" \
   "qty_in_transit 是 GENERATED ALWAYS 且表达式含 shipped_qty/received_qty"

assert_eq "4" "$(psql_q "select count(*) from pg_constraint where conrelid='biz_inventory_in_transit'::regclass
   and contype='c' and conname like 'ck_biz_inv_transit_%';")" \
   "4 条 CHECK 在位"

assert_eq "fk_biz_inv_transit_shipment|FOREIGN KEY (shipment_id) REFERENCES biz_shipment(id) ON DELETE CASCADE" "$(psql_q "
   select conname||'|'||pg_get_constraintdef(oid) from pg_constraint
    where conname='fk_biz_inv_transit_shipment';")" \
   "发货单外键 = REFERENCES biz_shipment(id) ON DELETE CASCADE"

assert_eq "4" "$(psql_q "select count(*) from pg_indexes where schemaname='public'
   and tablename='biz_inventory_in_transit'
   and (indexname like 'uk_biz_inv_transit_%' or indexname like 'idx_biz_inv_transit_%');")" \
   "4 条索引在位"

assert_eq "1" "$(psql_q "select count(*) from pg_indexes where schemaname='public'
   and tablename='biz_inventory_in_transit' and indexname='uk_biz_inv_transit_key'
   and indexdef like '%UNIQUE INDEX%' and indexdef not like '%WHERE%';")" \
   "唯一键是完整三列、不带谓词（本表无软删）"

assert_eq "5" "$(psql_q "select count(*) from pg_indexes where schemaname='public'
   and tablename='biz_inventory_in_transit';")" \
   "索引总数 5 = 1 唯一 + 3 普通 + 1 主键"

assert_eq "1" "$(psql_q "select count(*) from pg_description d join pg_class c on c.oid=d.objoid
   where c.relname='biz_inventory_in_transit' and d.objsubid=0
     and d.description like '%在途库存账%';")" \
   "表注释在位"

assert_eq "0" "$(psql_q "select count(*) from biz_inventory_in_transit;")" \
   "升级后表内 0 行（自检探针已自清场）"

# ---------- 4) 行为断言：约束真的会拦 ----------
echo
echo "== 4) 行为断言（脏数据一律拦下）=="
# 备一张探针订单 + 两张探针发货单（在途账的 shipment_id 是硬外键，必须有真单头）
psql_q "insert into biz_sales_order (id, so_no, customer_id, order_date)
        values ('d0420aaa-0000-4000-8000-000000000001','__M201_DRILL_SO__',gen_random_uuid(),date '2026-11-16');" >/dev/null 2>&1
psql_q "insert into biz_shipment (id, ship_no, so_id, customer_id, warehouse_id, ship_date)
        values ('d0420aaa-0000-4000-8000-000000000002','__M201_DRILL_SH1__','d0420aaa-0000-4000-8000-000000000001',gen_random_uuid(),'d0420aaa-0000-4000-8000-00000000000a',date '2026-11-16'),
               ('d0420aaa-0000-4000-8000-000000000003','__M201_DRILL_SH2__','d0420aaa-0000-4000-8000-000000000001',gen_random_uuid(),'d0420aaa-0000-4000-8000-00000000000a',date '2026-11-16');" >/dev/null 2>&1
assert_eq "2" "$(psql_q "select count(*) from biz_shipment where ship_no like '__M201_DRILL%';")" \
   "探针发货单就位"

MAT='d0420aaa-0000-4000-8000-0000000000b1'
WH='d0420aaa-0000-4000-8000-00000000000a'
SH1='d0420aaa-0000-4000-8000-000000000002'
SH2='d0420aaa-0000-4000-8000-000000000003'

assert_eq "1" "$(psql_q "insert into biz_inventory_in_transit
   (shipment_id,material_id,warehouse_id,ship_date,shipped_qty,received_qty)
   values ('$SH1','$MAT','$WH',date '2026-11-16',100,30);" 2>&1 | grep -c 'INSERT 0 1')" \
   "正常在途行可写（发货 100 / 已签 30）"

assert_eq "70.0000|IN_TRANSIT" "$(psql_q "select qty_in_transit||'|'||status from biz_inventory_in_transit
   where shipment_id='$SH1' and material_id='$MAT';")" \
   "在途量由 PG 算出来 = 100 − 30 = 70"

assert_eq "1" "$(psql_q "insert into biz_inventory_in_transit
   (shipment_id,material_id,warehouse_id,ship_date,shipped_qty)
   values ('$SH1','$MAT','$WH',date '2026-11-16',5);" 2>&1 | grep -c 'uk_biz_inv_transit_key')" \
   "同 (物料, 发货仓, 发货单) 重号被唯一键拦下（同一批在途不能算两遍）"

assert_eq "1" "$(psql_q "insert into biz_inventory_in_transit
   (shipment_id,material_id,warehouse_id,ship_date,shipped_qty,qty_in_transit)
   values ('$SH2','$MAT','$WH',date '2026-11-16',10,999);" 2>&1 | grep -c 'cannot insert a non-DEFAULT value')" \
   "直接给生成列 qty_in_transit 赋值被拒（口径不可绕过）"

assert_eq "1" "$(psql_q "insert into biz_inventory_in_transit
   (shipment_id,material_id,warehouse_id,ship_date,shipped_qty)
   values ('$SH2','$MAT','$WH',date '2026-11-16',0);" 2>&1 | grep -c 'ck_biz_inv_transit_qty')" \
   "shipped_qty = 0 被 ck_biz_inv_transit_qty 拦下"

assert_eq "1" "$(psql_q "insert into biz_inventory_in_transit
   (shipment_id,material_id,warehouse_id,ship_date,shipped_qty,received_qty)
   values ('$SH2','$MAT','$WH',date '2026-11-16',10,11);" 2>&1 | grep -c 'ck_biz_inv_transit_qty')" \
   "received_qty > shipped_qty 被拦下（否则在途变负数）"

assert_eq "1" "$(psql_q "insert into biz_inventory_in_transit
   (shipment_id,material_id,warehouse_id,ship_date,shipped_qty,received_qty,status)
   values ('$SH2','$MAT','$WH',date '2026-11-16',10,10,'IN_TRANSIT');" 2>&1 | grep -c 'ck_biz_inv_transit_state')" \
   "签满仍是 IN_TRANSIT 被 ck_biz_inv_transit_state 拦下"

assert_eq "1" "$(psql_q "insert into biz_inventory_in_transit
   (shipment_id,material_id,warehouse_id,ship_date,shipped_qty,status)
   values ('$SH2','$MAT','$WH',date '2026-11-16',10,'SHIPPED');" 2>&1 | grep -cE 'ck_biz_inv_transit_(status|state)')" \
   "非法状态 SHIPPED 被拦下（状态域 CHECK）"

assert_eq "1" "$(psql_q "insert into biz_inventory_in_transit
   (shipment_id,material_id,warehouse_id,ship_date,shipped_qty)
   values ('00000000-0000-4000-8000-0000ffffffff','$MAT','$WH',date '2026-11-16',1);" 2>&1 | grep -c 'fk_biz_inv_transit_shipment')" \
   "悬空 shipment_id 被外键拦下"

# ---------- 5) 写入契约：M2-02 upsert / M2-03 一条 UPDATE ----------
echo
echo "== 5) 写入契约（M2-02 upsert / M2-03 单语句推进）=="
# M2-02：同一发货单同一物料再发一批 → 累加 shipped_qty，仍然只有一行
psql_q "insert into biz_inventory_in_transit
   (shipment_id,material_id,warehouse_id,ship_date,shipped_qty,received_qty)
   values ('$SH1','$MAT','$WH',date '2026-11-16',20,0)
   on conflict (material_id, warehouse_id, shipment_id)
   do update set shipped_qty = biz_inventory_in_transit.shipped_qty + excluded.shipped_qty;" >/dev/null 2>&1
assert_eq "120.0000|30.0000|90.0000|1" "$(psql_q "select shipped_qty||'|'||received_qty||'|'||qty_in_transit||'|'||count(*) over ()
   from biz_inventory_in_transit where shipment_id='$SH1' and material_id='$MAT';")" \
   "M2-02 upsert：发货量累加到 120、仍只有 1 行、在途 90"

# M2-03 反证：只改 received_qty 而不改 status，签满那一刻必须被拒
assert_eq "1" "$(psql_q "update biz_inventory_in_transit set received_qty = shipped_qty
   where shipment_id='$SH1' and material_id='$MAT';" 2>&1 | grep -c 'ck_biz_inv_transit_state')" \
   "M2-03 反证：只推 received_qty 不动 status 被拒（不许出现「签满仍 IN_TRANSIT」的中间态）"

# M2-03 正写：一条 UPDATE 同时推进 received_qty + status
psql_q "update biz_inventory_in_transit
   set received_qty = shipped_qty,
       status = 'RECEIVED',
       received_at = now()
 where shipment_id='$SH1' and material_id='$MAT';" >/dev/null 2>&1
assert_eq "0.0000|RECEIVED|1" "$(psql_q "select qty_in_transit||'|'||status||'|'||(received_at is not null)::int
   from biz_inventory_in_transit where shipment_id='$SH1' and material_id='$MAT';")" \
   "M2-03 单语句签满：在途归 0、状态 RECEIVED、签收时间有值"

# 部分签收：仍是在途，余量就是差值
psql_q "insert into biz_inventory_in_transit
   (shipment_id,material_id,warehouse_id,ship_date,shipped_qty,received_qty)
   values ('$SH2','$MAT','$WH',date '2026-11-16',50,20);" >/dev/null 2>&1
assert_eq "30.0000|IN_TRANSIT" "$(psql_q "select qty_in_transit||'|'||status
   from biz_inventory_in_transit where shipment_id='$SH2' and material_id='$MAT';")" \
   "部分签收（50 发 / 20 签）仍算在途 30"

# 作废行：不参与在途，但留痕
psql_q "insert into biz_inventory_in_transit
   (shipment_id,material_id,warehouse_id,ship_date,shipped_qty,received_qty,status,remark)
   values ('$SH2','d0420aaa-0000-4000-8000-0000000000c2','$WH',date '2026-11-16',80,0,'CANCELLED','drill：作废');" >/dev/null 2>&1
assert_eq "1" "$(psql_q "select count(*) from biz_inventory_in_transit
   where shipment_id='$SH2' and material_id='d0420aaa-0000-4000-8000-0000000000c2' and status='CANCELLED';")" \
   "作废行可写（CANCELLED 不受数量一致性约束）"

# ---------- 6) 对账恒等（M2-05 口径）----------
echo
echo "== 6) 对账恒等：在途 + 已签收 = 发货量（M2-05）=="
assert_eq "0.0000" "$(psql_q "select (sum(shipped_qty) - sum(received_qty) - sum(qty_in_transit))::numeric(18,4)
   from biz_inventory_in_transit where status <> 'CANCELLED';")" \
   "非作废行：发货量 = 已签收 + 在途（生成列保证的恒等式）"

assert_eq "30.0000|140.0000" "$(psql_q "select
     (select coalesce(sum(qty_in_transit),0) from biz_inventory_in_transit
       where status='IN_TRANSIT' and material_id='$MAT' and warehouse_id='$WH')||'|'||
     (select coalesce(sum(received_qty),0) from biz_inventory_in_transit
       where status<>'CANCELLED' and material_id='$MAT' and warehouse_id='$WH');")" \
   "按物料+仓库汇总：在途 30 / 已签收 140"

assert_eq "$(psql_q "select coalesce(sum(qty_in_transit),0) from biz_inventory_in_transit
   where status='IN_TRANSIT' and material_id='$MAT' and warehouse_id='$WH';")" \
   "$(psql_q "select coalesce(sum(shipped_qty - received_qty),0) from biz_inventory_in_transit
   where status='IN_TRANSIT' and material_id='$MAT' and warehouse_id='$WH';")" \
   "在途汇总 = 独立 SQL（shipped − received）聚合，两处逐位相同"

assert_eq "30.0000" "$(psql_q "select coalesce(sum(qty_in_transit),0) from biz_inventory_in_transit
   where status <> 'CANCELLED' and material_id='$MAT';")" \
   "作废行不参与在途汇总（80 件作废量没被算进 30）"

# ---------- 7) 级联 ----------
echo
echo "== 7) 级联：物理删发货单 → 在途行一起消失 =="
psql_q "delete from biz_shipment where id='$SH2';" >/dev/null 2>&1
assert_eq "0" "$(psql_q "select count(*) from biz_inventory_in_transit where shipment_id='$SH2';")" \
   "删发货单 $SH2 后其在途行已级联删除"

# ---------- 8) down 护栏反证 ----------
echo
echo "== 8) down 护栏反证（还有在途/已签收行必须拒绝回滚）=="
assert_eq "1" "$(psql_q "select count(*) from biz_inventory_in_transit where status<>'CANCELLED';")" \
   "此刻表里还有 1 行已签收（SH1）"
if runfile "$DOWN" "$TMP/m201_down_guard.log"; then
    bad "down 竟然在有在途/已签收行时成功了（护栏失效）"
else
    if grep -q "V0420 down 拒绝执行" "$TMP/m201_down_guard.log"; then
        ok "down 被护栏拒绝并给出去向说明"
        grep "V0420 down 拒绝执行" "$TMP/m201_down_guard.log" | sed 's/^/       /'
    else
        bad "down 失败但原因不是护栏（$(tail -3 "$TMP/m201_down_guard.log")）"
    fi
fi
assert_eq "15" "$(psql_q "select count(*) from information_schema.columns
   where table_schema='public' and table_name='biz_inventory_in_transit';")" \
   "拒绝回滚后结构未被破坏（15 列仍在）"

# ---------- 9) 清探针 → down ×2 → 重放 ----------
echo
echo "== 9) 清探针 → down ×2 → 重放 up =="
psql_q "delete from biz_inventory_in_transit where material_id like 'd0420aaa-%';" >/dev/null 2>&1
psql_q "delete from biz_shipment where ship_no like '__M201_DRILL%';" >/dev/null 2>&1
psql_q "delete from biz_sales_order where so_no like '__M201_DRILL%';" >/dev/null 2>&1
assert_eq "0|0|0" "$(psql_q "select
     (select count(*) from biz_inventory_in_transit)||'|'||
     (select count(*) from biz_shipment where ship_no like '__M201_DRILL%')||'|'||
     (select count(*) from biz_sales_order where so_no like '__M201_DRILL%');")" \
   "探针行三表全部清空"

if runfile "$DOWN" "$TMP/m201_down1.log"; then
    ok "down 第一次执行成功（空表放行）"
else
    bad "down 第一次执行失败"; tail -20 "$TMP/m201_down1.log"
fi
assert_eq "0" "$(psql_q "select count(*) from information_schema.tables
   where table_schema='public' and table_name='biz_inventory_in_transit';")" \
   "回滚后表已移除"

if runfile "$DOWN" "$TMP/m201_down2.log"; then
    ok "down 重复执行成功（幂等）"
else
    bad "down 重复执行失败"; tail -20 "$TMP/m201_down2.log"
fi

if runfile "$UP" "$TMP/m201_up3.log"; then
    ok "回滚后重放 up 成功"
else
    bad "回滚后重放 up 失败"; tail -20 "$TMP/m201_up3.log"
fi
assert_eq "15" "$(psql_q "select count(*) from information_schema.columns
   where table_schema='public' and table_name='biz_inventory_in_transit';")" \
   "重放后 15 列回来了"

# ---------- 汇总 ----------
echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] || exit 1
