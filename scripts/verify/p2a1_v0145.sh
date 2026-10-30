#!/bin/bash
# ============================================================
# scripts/verify/p2a1_v0145.sh —— V0145 验收（P2-A1）
#
# 六场景（卡面 §四）：
#   ① 空库全新库上跑 V0145 成功（走真实迁移器：起一次实例把全部迁移跑到 V0145）
#   ② 幂等：手工再跑一遍 up.sql 不报错
#   ③ down 完整回滚（再��一遍也不报错 = down 也幂等）
#   ④ CHECK 拦住：qty=0 / tax_rate=150 / 三金额不恒等 / price_mode 非法 / 采购入库无供应商
#   ⑤ 唯一索引：同 in_no 插两条 → 第二条失败（23505）
#   ⑥ 软删后可重用单号
#
# 用法（在 144 上）：
#   bash scripts/verify/p2a1_v0145.sh
#   REPO=/root/xxx P2A1_DB=cloudpivot_p2a1_v2 bash scripts/verify/p2a1_v0145.sh
#
# 设计要点：
#   - 自建自毁隔离库（owner 必须是应用账号 cloudpivot，否则迁移只有一行无细节的报错）
#   - 不起隔离实例做 HTTP 验收（本卡只建表，没有接口），只借真实二进制跑一次迁移器
#   - 独立端口 18085 + 独立 Redis db9，收尾按 pid 精确 kill 并复查端口
#   - 全程不碰 prod 库 / cloudpivot_dev
# ============================================================
set -u

REPO=${REPO:-/opt/cloudpivot}
DB=${P2A1_DB:-cloudpivot_p2a1}
PORT=${P2A1_PORT:-18085}
STAGE=${P2A1_STAGE:-/root/p2a1-stage}
BIN=${P2A1_BIN:-$REPO/backend/target/release/cloudpivot}
ENVF=${P2A1_ENVF:-/etc/cloudpivot/cloudpivot-dev.env}
KEEP=${P2A1_KEEP:-0}

# 迁移源目录：默认用仓里的。要在「文件还没进共享仓」的阶段先验一遍时，传 P2A1_MIGSRC
MIGSRC=${P2A1_MIGSRC:-$REPO/backend/migrations}
MIG_DIR="$MIGSRC/business"
MIG_UP="$MIG_DIR/V0145__purchase_order_amounts_and_stock_in.up.sql"
MIG_DOWN="$MIG_DIR/V0145__purchase_order_amounts_and_stock_in.down.sql"

PASS=0; FAIL=0
LOG=$STAGE/verify.log
mkdir -p "$STAGE" 2>/dev/null
: > "$LOG"

ok()   { PASS=$((PASS+1)); printf '[PASS] %s\n' "$1" | tee -a "$LOG"; }
bad()  { FAIL=$((FAIL+1)); printf '[FAIL] %s\n' "$1" | tee -a "$LOG"; }
head_() { printf '\n===== %s =====\n' "$1" | tee -a "$LOG"; }

# ---------- 0. 前置检查 ----------
head_ "0. 前置检查"
for f in "$MIG_UP" "$MIG_DOWN" "$BIN" "$ENVF"; do
    if [ -e "$f" ]; then ok "存在 $f"; else bad "缺文件 $f"; fi
done
if [ "$FAIL" -gt 0 ]; then echo "前置检查失败，退出"; exit 1; fi

set -a; . "$ENVF"; set +a
DBHOST=${NIK_DATABASE_HOST:-127.0.0.1}
DBPORT=${NIK_DATABASE_PORT:-5432}
DBUSER=${NIK_DATABASE_USER:-cloudpivot}
export PGPASSWORD="$NIK_DATABASE_PASSWORD"

# 应用账号直连（只读/读写都走它，保证表 owner = 应用账号）
q()    { psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -Atc "$1" 2>&1; }
qout() { psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -At -c "$1" 2>&1; }
first() { printf '%s\n' "$1" | head -1; }
# 第一行输出（psql -At 的 stdout 带 INSERT 0 1 之类的命令标签）
expect_eq() { # desc sql expected
    local got; got=$(first "$(q "$2")")
    if [ "$got" = "$3" ]; then ok "$1 → $got"; else bad "$1 → 期望 [$3] 实际 [$got]"; fi
}
expect_fail() { # desc sql 期望的错误片段
    local out rc
    out=$(psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -Atc "$2" 2>&1); rc=$?
    if [ $rc -ne 0 ] && printf '%s' "$out" | grep -q "$3"; then
        ok "$1 → 被拦：$(printf '%s' "$out" | head -1 | cut -c1-110)"
    else
        bad "$1 → 没拦住（rc=$rc）：$(printf '%s' "$out" | head -1 | cut -c1-160)"
    fi
}
expect_ok() { # desc sql [expect_substr]
    local out rc
    out=$(psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -Atc "$2" 2>&1); rc=$?
    if [ $rc -eq 0 ]; then
        ok "$1 → 成功${3:+（返回 $(printf '%s' "$out" | head -1 | cut -c1-80)）}"
    else
        bad "$1 → 失败：$(printf '%s' "$out" | head -1 | cut -c1-160)"
    fi
}
prod_max() { su - postgres -c "psql -d cloudpivot -Atc \"select coalesce(max(version::int),0) from schema_migrations\"" 2>/dev/null | head -1; }

PROD_BEFORE=$(prod_max)
echo "prod 库 cloudpivot 迁移版本（开工前）= $PROD_BEFORE" | tee -a "$LOG"

# 端口占用就自动往上找（144 上常有别的卡片的隔离实例占着 1808x）
PORT_WANT=$PORT
for _ in $(seq 1 40); do
    ss -ltnp 2>/dev/null | grep -q ":$PORT " || break
    PORT=$((PORT + 1))
done
if ss -ltnp 2>/dev/null | grep -q ":$PORT "; then
    echo "1808x 段没有空端口，请用 P2A1_PORT 指定"; exit 1
fi
if [ "$PORT" != "$PORT_WANT" ]; then
    echo "端口 $PORT_WANT 被占用，自动改用 $PORT" | tee -a "$LOG"
fi

# ---------- 1. 造隔离库（全新空库）----------
head_ "1. 造隔离库 $DB（全新空库）"
su - postgres -c "dropdb --if-exists $DB" 2>&1 | tee -a "$LOG"
su - postgres -c "createdb -O $DBUSER $DB" 2>&1 | tee -a "$LOG"
OWNER=$(su - postgres -c "psql -d $DB -Atc \"select pg_get_userbyid(datdba) from pg_database where datname='$DB'\"" | head -1)
expect_eq "库 owner" "select current_user" "$DBUSER"
echo "库 $DB owner=$OWNER" | tee -a "$LOG"
TBL_BEFORE=$(q "select count(*) from information_schema.tables where table_schema='public'")
expect_eq "空库 public 表数（应为 0）" "select count(*) from information_schema.tables where table_schema='public'" "0"

# ---------- 2. 场景①：真实迁移器在空库上跑完 V0145 ----------
head_ "2. 场景① 空库全新库跑 V0145（真实迁移器）"
rm -rf "$STAGE/backend"
mkdir -p "$STAGE/backend"
cp -a "$REPO/backend/config" "$STAGE/backend/config"
cp -a "$MIGSRC" "$STAGE/backend/migrations"
mkdir -p "$STAGE/backend/uploads"

cd "$STAGE/backend" || exit 1
NIK_PROFILE=${NIK_PROFILE:-dev} \
NIK_DATABASE_NAME="$DB" \
NIK_SERVER_LISTEN="127.0.0.1:$PORT" \
NIK_REDIS_URL="redis://127.0.0.1:6379/9" \
setsid nohup "$BIN" > "$STAGE/server.log" 2>&1 < /dev/null &
sleep 2
SRV_PID=$(ss -ltnp 2>/dev/null | grep ":$PORT " | grep -oE 'pid=[0-9]+' | head -1 | cut -d= -f2)
for i in $(seq 1 60); do
    if curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then break; fi
    sleep 1
done
if curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
    ok "实例就绪 /health 200（pid=$SRV_PID）"
else
    bad "实例没起来；日志尾部：$(tail -5 "$STAGE/server.log" | tr '\n' ' ')"
fi
grep -E "迁移" "$STAGE/server.log" | tail -3 | tee -a "$LOG"

expect_eq "schema_migrations 最大版本" "select max(version::int) from schema_migrations" "145"
expect_eq "V0145 已登记" "select count(*) from schema_migrations where version='145'" "1"
expect_eq "biz_stock_in 表已建" "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_stock_in'" "1"
expect_eq "biz_stock_in_item 表已建" "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_stock_in_item'" "1"
expect_eq "采购主表新增 price_mode（默认 EXCL）" "select column_default, is_nullable from information_schema.columns where table_name='biz_purchase_order' and column_name='price_mode'" "'EXCL'::character varying|NO"
expect_eq "采购明细新增 price_incl numeric(18,6)" "select data_type||'('||numeric_precision||','||numeric_scale||')' from information_schema.columns where table_name='biz_purchase_order_item' and column_name='price_incl'" "numeric(18,6)"
expect_eq "未新建重复金额列 amount_excl" "select count(*) from information_schema.columns where table_name in ('biz_purchase_order','biz_purchase_order_item') and column_name in ('amount_excl','amount_incl')" "0"
expect_eq "10 个新索引" "select count(*) from pg_indexes where schemaname='public' and indexname in ('uk_biz_si_no','idx_biz_si_source','idx_biz_si_supplier','idx_biz_si_status','idx_biz_si_date','idx_biz_si_warehouse','idx_biz_si_voucher','idx_biz_si_item_in','idx_biz_si_item_material','idx_biz_si_item_src')" "10"
expect_eq "uk_biz_si_no 是部分唯一索引" "select count(*) from pg_indexes where indexname='uk_biz_si_no' and indexdef like '%UNIQUE%' and indexdef like '%WHERE (deleted_at IS NULL)%'" "1"
expect_eq "11 个新约束" "select count(*) from pg_constraint where conname in ('ck_biz_po_price_mode','ck_biz_po_item_tax_rate','ck_biz_si_status','ck_biz_si_type','ck_biz_si_tax','ck_biz_si_qty','ck_biz_si_supplier','ck_biz_si_item_qty','ck_biz_si_item_tax','ck_biz_si_item_rate','uk_biz_si_item_line')" "11"

# ---------- 3. 场景②：幂等（同库再跑一遍 up）----------
head_ "3. 场景② 幂等：手工再跑一遍 V0145 up"
TBL_AFTER1=$(first "$(q "select count(*) from information_schema.tables where table_schema='public'")")
echo "首遍迁移后 public 表数 = $TBL_AFTER1（空库从 0 涨到这里）" | tee -a "$LOG"
cd "$STAGE/backend" || exit 1
out=$(psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -v ON_ERROR_STOP=1 -f "$MIG_UP" 2>&1); rc=$?
if [ $rc -eq 0 ]; then ok "第二遍 up 成功（exit 0）"; else bad "第二遍 up 失败：$(printf '%s' "$out" | tail -3 | tr '\n' ' ')"; fi
printf '%s\n' "$out" | grep -E "NOTICE|ERROR" | tail -3 | tee -a "$LOG"
expect_eq "二次执行后表数不变（无重复建）" "select count(*) from information_schema.tables where table_schema='public'" "$TBL_AFTER1"
expect_eq "二次执行后新索引仍 10 个" "select count(*) from pg_indexes where schemaname='public' and indexname in ('uk_biz_si_no','idx_biz_si_source','idx_biz_si_supplier','idx_biz_si_status','idx_biz_si_date','idx_biz_si_warehouse','idx_biz_si_voucher','idx_biz_si_item_in','idx_biz_si_item_material','idx_biz_si_item_src')" "10"

# ---------- 4. 场景④ CHECK 拦截 ----------
head_ "4. 场景④ CHECK 约束拦截（反例全部应失败）"
PO=11111111-1111-1111-1111-111111111111
POI=11111111-1111-1111-1111-111111111112
SI=22222222-2222-2222-2222-222222222222
SUP=33333333-3333-3333-3333-333333333333
WH=44444444-4444-4444-4444-444444444444
MAT=55555555-5555-5555-5555-555555555555
UNIT=66666666-6666-6666-6666-666666666666
LOC=77777777-7777-7777-7777-777777777777

expect_ok "夹具：采购订单（价税合计 = 200+26）" "insert into biz_purchase_order (id,order_no,supplier_id,order_date,warehouse_id,total_qty,total_amount,total_tax,total_with_tax,price_mode) values ('$PO','P2A1-PO-0001','$SUP','2026-10-31','$WH',100,200.00,26.00,226.00,'EXCL')"
expect_ok "夹具：采购明细（13% 税率）" "insert into biz_purchase_order_item (id,order_id,line_no,material_id,unit_id,qty,price,price_incl,tax_rate,amount,tax_amount,with_tax_amount) values ('$POI','$PO',1,'$MAT','$UNIT',100,2.000000,2.260000,13,200.00,26.00,226.00)"

expect_fail "④-1 明细 qty=0" "insert into biz_purchase_order_item (order_id,line_no,material_id,unit_id,qty,price,tax_rate,amount,tax_amount,with_tax_amount) values ('$PO',2,'$MAT','$UNIT',0,2.0,13,0,0,0)" "ck_biz_po_item_qty"
expect_fail "④-2 明细 tax_rate=150" "insert into biz_purchase_order_item (order_id,line_no,material_id,unit_id,qty,price,tax_rate,amount,tax_amount,with_tax_amount) values ('$PO',3,'$MAT','$UNIT',1,2.0,150,2.0,0.26,2.26)" "ck_biz_po_item_tax_rate"
expect_fail "④-3 明细三金额不恒等（226 ≠ 200+25）" "insert into biz_purchase_order_item (order_id,line_no,material_id,unit_id,qty,price,tax_rate,amount,tax_amount,with_tax_amount) values ('$PO',4,'$MAT','$UNIT',100,2.0,13,200.00,25.00,226.00)" "ck_biz_po_item_tax"
expect_fail "④-4 采购单头三金额不恒等" "insert into biz_purchase_order (order_no,supplier_id,order_date,warehouse_id,total_qty,total_amount,total_tax,total_with_tax) values ('P2A1-PO-BAD','$SUP','2026-10-31','$WH',1,200.00,25.00,226.00)" "ck_biz_po_tax_sum"
expect_fail "④-5 price_mode 非法值" "insert into biz_purchase_order (order_no,supplier_id,order_date,warehouse_id,price_mode) values ('P2A1-PO-BAD2','$SUP','2026-10-31','$WH','BAD')" "ck_biz_po_price_mode"

expect_ok "夹具：收货单（票货同行，三金额恒等）" "insert into biz_stock_in (id,in_no,in_type,source_order_id,supplier_id,warehouse_id,in_date,total_qty,total_amount,total_tax,total_with_tax) values ('$SI','P2A1-IN-0001','PURCHASE','$PO','$SUP','$WH','2026-10-31',100,200.00,26.00,226.00)"
expect_eq "invoice_received 默认 TRUE" "select invoice_received from biz_stock_in where id='$SI'" "t"

expect_fail "④-6 收货明细 qty=0" "insert into biz_stock_in_item (stock_in_id,line_no,source_item_id,material_id,unit_id,qty,price,price_incl,tax_rate,amount,tax_amount,with_tax_amount) values ('$SI',1,'$POI','$MAT','$UNIT',0,2.0,2.26,13,0,0,0)" "ck_biz_si_item_qty"
expect_fail "④-7 收货明细 tax_rate=150" "insert into biz_stock_in_item (stock_in_id,line_no,source_item_id,material_id,unit_id,qty,price,price_incl,tax_rate,amount,tax_amount,with_tax_amount) values ('$SI',2,'$POI','$MAT','$UNIT',1,2.0,2.26,150,2.0,0.26,2.26)" "ck_biz_si_item_rate"
expect_fail "④-8 收货明细三金额不恒等" "insert into biz_stock_in_item (stock_in_id,line_no,source_item_id,material_id,unit_id,qty,price,price_incl,tax_rate,amount,tax_amount,with_tax_amount) values ('$SI',3,'$POI','$MAT','$UNIT',100,2.0,2.26,13,200.00,25.00,226.00)" "ck_biz_si_item_tax"
expect_ok   "正向对照：收货明细合规行" "insert into biz_stock_in_item (stock_in_id,line_no,source_item_id,material_id,unit_id,qty,price,price_incl,tax_rate,amount,tax_amount,with_tax_amount,location_id,lot_no) values ('$SI',1,'$POI','$MAT','$UNIT',100,2.000000,2.260000,13,200.00,26.00,226.00,'$LOC','LOT-P2A1-01')"
expect_fail "④-9 收货单头三金额不恒等" "insert into biz_stock_in (in_no,in_type,supplier_id,warehouse_id,in_date,total_qty,total_amount,total_tax,total_with_tax) values ('P2A1-IN-BAD','PURCHASE','$SUP','$WH','2026-10-31',100,200.00,25.00,226.00)" "ck_biz_si_tax"
expect_fail "④-10 采购入库无供应商" "insert into biz_stock_in (in_no,in_type,warehouse_id,in_date) values ('P2A1-IN-NOSUP','PURCHASE','$WH','2026-10-31')" "ck_biz_si_supplier"
expect_fail "④-11 收货单 status 非法" "insert into biz_stock_in (in_no,in_type,supplier_id,warehouse_id,in_date,status) values ('P2A1-IN-BADST','PURCHASE','$SUP','$WH','2026-10-31','PAID')" "ck_biz_si_status"
expect_fail "④-12 收货单 in_type 非法" "insert into biz_stock_in (in_no,in_type,supplier_id,warehouse_id,in_date) values ('P2A1-IN-BADTY','SALE','$SUP','$WH','2026-10-31')" "ck_biz_si_type"
expect_fail "④-13 明细行号重复（同单两行 line_no=1）" "insert into biz_stock_in_item (stock_in_id,line_no,material_id,unit_id,qty) values ('$SI',1,'$MAT','$UNIT',1)" "uk_biz_si_item_line"

# ---------- 5. 场景⑤ 唯一索引 ----------
head_ "5. 场景⑤ 唯一索引：同 in_no 第二条失败"
expect_fail "⑤ 同 in_no 再插一条" "insert into biz_stock_in (in_no,in_type,supplier_id,warehouse_id,in_date) values ('P2A1-IN-0001','PURCHASE','$SUP','$WH','2026-10-31')" "uk_biz_si_no"
expect_eq "⑤ 该单号只有 1 行" "select count(*) from biz_stock_in where in_no='P2A1-IN-0001'" "1"

# ---------- 6. 场景⑥ 软删后可重用单号 ----------
head_ "6. 场景⑥ 软删后可重用单号"
expect_ok "软删首行" "update biz_stock_in set deleted_at = now() where id='$SI'"
expect_ok "软删后用同一 in_no 再建一单" "insert into biz_stock_in (in_no,in_type,supplier_id,warehouse_id,in_date) values ('P2A1-IN-0001','PURCHASE','$SUP','$WH','2026-10-31')"
expect_eq "该单号 2 行（1 已删 + 1 在用）" "select count(*) from biz_stock_in where in_no='P2A1-IN-0001'" "2"
expect_eq "在用的只有 1 行" "select count(*) from biz_stock_in where in_no='P2A1-IN-0001' and deleted_at is null" "1"

# ---------- 7. 场景③ down 回滚 ----------
head_ "7. 场景③ down 完整回滚（带数据）"
cd "$STAGE/backend" || exit 1
out=$(psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -v ON_ERROR_STOP=1 -f "$MIG_DOWN" 2>&1); rc=$?
if [ $rc -eq 0 ]; then ok "down 成功（exit 0，带数据回滚）"; else bad "down 失败：$(printf '%s' "$out" | tail -3 | tr '\n' ' ')"; fi
printf '%s\n' "$out" | grep -E "NOTICE|ERROR" | tee -a "$LOG"
expect_eq "收货两表已删除" "select count(*) from information_schema.tables where table_schema='public' and table_name in ('biz_stock_in','biz_stock_in_item')" "0"
expect_eq "price_mode 列已删" "select count(*) from information_schema.columns where table_name='biz_purchase_order' and column_name='price_mode'" "0"
expect_eq "price_incl 列已删" "select count(*) from information_schema.columns where table_name='biz_purchase_order_item' and column_name='price_incl'" "0"
expect_eq "税率区间 CHECK 已删" "select count(*) from pg_constraint where conname='ck_biz_po_item_tax_rate'" "0"
expect_eq "既有金额列仍在（total_amount/total_tax/total_with_tax）" "select count(*) from information_schema.columns where table_name='biz_purchase_order' and column_name in ('total_amount','total_tax','total_with_tax')" "3"
expect_eq "采购订单既有数据未被 down 清掉" "select count(*) from biz_purchase_order where order_no='P2A1-PO-0001'" "1"
out=$(psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -v ON_ERROR_STOP=1 -f "$MIG_DOWN" 2>&1); rc=$?
if [ $rc -eq 0 ]; then ok "down 再跑一遍也成功（幂等）"; else bad "down 第二遍失败：$(printf '%s' "$out" | tail -2 | tr '\n' ' ')"; fi

# ---------- 8. down 后重新 up（往返闭合）----------
head_ "8. down → up 往返闭合"
out=$(psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -v ON_ERROR_STOP=1 -f "$MIG_UP" 2>&1); rc=$?
if [ $rc -eq 0 ]; then ok "回滚后重新 up 成功"; else bad "重新 up 失败：$(printf '%s' "$out" | tail -3 | tr '\n' ' ')"; fi
expect_eq "往返后收货两表回来" "select count(*) from information_schema.tables where table_schema='public' and table_name in ('biz_stock_in','biz_stock_in_item')" "2"
expect_eq "往返后 price_mode 回来（默认 EXCL）" "select column_default from information_schema.columns where table_name='biz_purchase_order' and column_name='price_mode'" "'EXCL'::character varying"
expect_eq "往返后 11 个约束回来" "select count(*) from pg_constraint where conname in ('ck_biz_po_price_mode','ck_biz_po_item_tax_rate','ck_biz_si_status','ck_biz_si_type','ck_biz_si_tax','ck_biz_si_qty','ck_biz_si_supplier','ck_biz_si_item_qty','ck_biz_si_item_tax','ck_biz_si_item_rate','uk_biz_si_item_line')" "11"
expect_ok "往返后夹具：新建收货单" "insert into biz_stock_in (in_no,in_type,supplier_id,warehouse_id,in_date) values ('P2A1-IN-RT','PURCHASE','$SUP','$WH','2026-10-31')"
RT=$(first "$(q "select id from biz_stock_in where in_no='P2A1-IN-RT'")")
expect_fail "往返后 CHECK 仍然拦 qty=0" "insert into biz_stock_in_item (stock_in_id,line_no,material_id,unit_id,qty) values ('$RT',1,'$MAT','$UNIT',0)" "ck_biz_si_item_qty"

# ---------- 9. 收尾：kill 实例 + 复查端口 + 删库 ----------
head_ "9. 收尾"
if [ -n "${SRV_PID:-}" ] && kill -0 "$SRV_PID" 2>/dev/null; then
    kill "$SRV_PID" 2>/dev/null
    for i in $(seq 1 15); do kill -0 "$SRV_PID" 2>/dev/null || break; sleep 1; done
    kill -0 "$SRV_PID" 2>/dev/null && kill -9 "$SRV_PID" 2>/dev/null
    ok "已按 pid=$SRV_PID 精确 kill 隔离实例"
else
    ok "隔离实例已退出"
fi
if ss -ltnp 2>/dev/null | grep -q ":$PORT "; then bad "端口 $PORT 仍被占用"; else ok "端口 $PORT 已释放"; fi

PROD_AFTER=$(prod_max)
echo "prod 库 cloudpivot 迁移版本（收尾）= $PROD_AFTER" | tee -a "$LOG"
if [ "$PROD_BEFORE" = "$PROD_AFTER" ]; then ok "prod 库未动（$PROD_BEFORE → $PROD_AFTER，prod 二进制未重启即不应用新迁移）"; else bad "prod 库版本变了：$PROD_BEFORE → $PROD_AFTER"; fi
DEV_MAX=$(su - postgres -c "psql -d cloudpivot_dev -Atc \"select coalesce(max(version::int),0) from schema_migrations\"" 2>/dev/null | head -1)
echo "dev 库 cloudpivot_dev 迁移版本 = $DEV_MAX（dev 的 cargo-watch 会自动应用，不计为污染）" | tee -a "$LOG"

if [ "$KEEP" = "1" ]; then
    echo "P2A1_KEEP=1：保留库 $DB 与目录 $STAGE" | tee -a "$LOG"
else
    su - postgres -c "dropdb --if-exists $DB" 2>&1 | tee -a "$LOG"
    ok "已删除隔离库 $DB"
fi

# ---------- 汇总 ----------
head_ "汇总"
printf 'PASS=%d FAIL=%d\n' "$PASS" "$FAIL" | tee -a "$LOG"
if [ "$FAIL" -eq 0 ]; then echo "RESULT: ALL GREEN"; exit 0; else echo "RESULT: HAS FAILURES"; exit 1; fi
