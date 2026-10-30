#!/bin/bash
# W1a · V0137 验收 ③④⑥：down 往返一致性 / 有数据时 down 必须中止 / 约束反例 8 条
# 用法：REPO=/opt/cloudpivot bash scripts/verify/w1a_v0137_accept_3_4_6.sh（先跑 accept_1_2）
set -uo pipefail
REPO=${REPO:-/opt/cloudpivot}
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
DB=${W1A_DB:-cloudpivot_w1a}
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PSQL="psql -h 127.0.0.1 -p 5432 -U $NIK_DATABASE_USER -d $DB -At -v ON_ERROR_STOP=1"
MIG=$REPO/backend/migrations/business
UP=$MIG/V0137__erp_master_data_and_warehouse.up.sql
DOWN=$MIG/V0137__erp_master_data_and_warehouse.down.sql
T13="'biz_unit','biz_account','biz_supplier','biz_accounting_period','biz_purchase_order','biz_purchase_order_item','biz_seq','biz_warehouse','biz_material','biz_inventory','biz_inventory_txn','biz_location','biz_inventory_location'"

SEED_HASH_SQL="select md5(string_agg(t,'|' order by t)) from (
  select 'unit:'||id||unit_code||unit_name||is_base||coalesce(base_unit_id::text,'-')||rate from biz_unit
  union all select 'wh:'||id||warehouse_code||level||coalesce(parent_id::text,'-')||warehouse_path||is_leaf from biz_warehouse
  union all select 'loc:'||id||location_code||warehouse_id||is_default||location_type from biz_location
  union all select 'mat:'||id||material_code||unit_id||material_type from biz_material
  union all select 'acct:'||id||account_code||balance_direction from biz_account
  union all select 'sup:'||id||supplier_code||credit_days from biz_supplier
  union all select 'period:'||id||period_code||status from biz_accounting_period
  union all select 'seq:'||id||seq_type||current_no||step from biz_seq
) s(t)"
IDX_HASH_SQL="select md5(string_agg(indexdef,'|' order by indexdef)) from pg_indexes where schemaname='public' and tablename like 'biz_%'"
CON_HASH_SQL="select md5(string_agg(c.relname||'.'||con.conname||':'||pg_get_constraintdef(con.oid),'|' order by c.relname, con.conname)) from pg_constraint con join pg_class c on c.oid=con.conrelid join pg_namespace n on n.oid=c.relnamespace where n.nspname='public' and c.relname like 'biz_%'"

echo "################ ③ down → up 往返：数据一致性 ################"
H1=$($PSQL -c "$SEED_HASH_SQL"); I1=$($PSQL -c "$IDX_HASH_SQL"); C1=$($PSQL -c "$CON_HASH_SQL")
echo "before: seed=$H1"
echo "before: idx =$I1"
echo "before: con =$C1"
echo "--- down ---"
$PSQL -f "$DOWN" 2>&1 | grep -E "NOTICE|ERROR" | sed 's/^/  /'
echo "--- down 后 13 表计数（应为 0）: $($PSQL -c "select count(*) from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname='public' and c.relkind='r' and relname in ($T13)")"
echo "--- up ---"
$PSQL -f "$UP" 2>&1 | grep -E "NOTICE|ERROR" | grep -v "does not exist, skipping" | sed 's/^/  /'
H2=$($PSQL -c "$SEED_HASH_SQL"); I2=$($PSQL -c "$IDX_HASH_SQL"); C2=$($PSQL -c "$CON_HASH_SQL")
echo "after : seed=$H2"
echo "after : idx =$I2"
echo "after : con =$C2"
if [ "$H1" = "$H2" ] && [ "$I1" = "$I2" ] && [ "$C1" = "$C2" ]; then
  echo "[PASS] ③ down→up 往返：种子数据 / 索引 DDL / 约束定义三者完全一致"
else
  echo "[FAIL] ③ 往返后不一致"
fi

echo
echo "################ ④ 有数据时 down 必须中止 ################"
$PSQL -c "insert into biz_warehouse (warehouse_code, warehouse_name, warehouse_path, is_leaf) values ('BUSINESS-001','业务仓库-测试', '/x/', 1)" >/dev/null
$PSQL -c "insert into biz_inventory_txn (material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, qty, period_code) values (gen_random_uuid(), gen_random_uuid(), 'IN','PURCHASE_IN','STOCK_IN', gen_random_uuid(), 'IN-001', 5, '2026-10')" >/dev/null
echo "已插入 2 行业务数据（1 仓库 + 1 流水）"
OUT=$($PSQL -f "$DOWN" 2>&1); RC=$?
echo "down 退出码 = $RC"
echo "$OUT" | grep -vE "^psql:.*NOTICE" | sed 's/^/  /'
echo "down 后 13 表计数（应仍为 13）: $($PSQL -c "select count(*) from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname='public' and c.relkind='r' and relname in ($T13)")"
if [ $RC -ne 0 ] && echo "$OUT" | grep -q "回滚中止"; then echo "[PASS] ④ 有数据时 down 中止，且没有静默丢数据"; else echo "[FAIL] ④ down 未按预期中止"; fi
$PSQL -c "delete from biz_inventory_txn where doc_no='IN-001'" >/dev/null
$PSQL -c "delete from biz_warehouse where warehouse_code='BUSINESS-001'" >/dev/null
echo "--- 清理后再 down（应放行）---"
$PSQL -f "$DOWN" 2>&1 | grep -E "回滚检查通过|回滚完成|回滚中止" | sed 's/^/  /'
$PSQL -f "$UP" 2>&1 | grep -E "自检通过" | sed 's/^/  /'
H3=$($PSQL -c "$SEED_HASH_SQL")
[ "$H1" = "$H3" ] && echo "[PASS] ④ 清理后 down 放行 + up 复原，种子哈希仍为 $H3" || echo "[FAIL] ④ 清理后哈希漂移"

echo
echo "################ ⑥ 约束反例（每条都必须被库拦住）################"
try_fail() { # $1=描述 $2=SQL $3=期望错误特征
  OUT=$($PSQL -c "$2" 2>&1); RC=$?
  if [ $RC -ne 0 ] && echo "$OUT" | grep -qiE "$3"; then
    echo "[PASS] $1 → $(echo "$OUT" | grep -oE 'violates [a-z ]+constraint "[a-z_]+"' | head -1)"
  else
    echo "[FAIL] $1（退出码 $RC）: $OUT"
  fi
}
try_fail "反例1 仓库层级：level=2 但无 parent_id（ck_biz_warehouse_root）" \
  "insert into biz_warehouse (warehouse_code, warehouse_name, level, warehouse_path) values ('NEG-1','孤儿分仓',2,'/x/')" "ck_biz_warehouse_root"
try_fail "反例2 仓库层级：level=4 超上限（ck_biz_warehouse_level）" \
  "insert into biz_warehouse (warehouse_code, warehouse_name, level, warehouse_path, parent_id) values ('NEG-2','四级仓',4,'/x/', gen_random_uuid())" "ck_biz_warehouse_level"
try_fail "反例3 计量单位：is_base=0 却不带基准单位（ck_biz_unit_base）" \
  "insert into biz_unit (unit_code, unit_name, is_base, base_unit_id) values ('NEG-3','坏单位',0,NULL)" "ck_biz_unit_base"
try_fail "反例4 采购单：价税合计≠不含税+税（ck_biz_po_tax_sum）" \
  "insert into biz_purchase_order (order_no, supplier_id, order_date, warehouse_id, total_amount, total_tax, total_with_tax) values ('NEG-4', gen_random_uuid(), CURRENT_DATE, gen_random_uuid(), 100, 13, 999)" "ck_biz_po_tax_sum"
try_fail "反例5 采购单：状态不在枚举内（ck_biz_po_status，VARCHAR 状态列）" \
  "insert into biz_purchase_order (order_no, supplier_id, order_date, warehouse_id, status) values ('NEG-5', gen_random_uuid(), CURRENT_DATE, gen_random_uuid(), 'PAID')" "ck_biz_po_status"
try_fail "反例6 库位：同仓第二个默认库位（uk_biz_location_default）" \
  "insert into biz_location (location_code, warehouse_id, is_default) values ('NEG-6','d1370001-0000-4000-8000-000000000002',1)" "uk_biz_location_default"
try_fail "反例7 库位库存：同物料同仓两条 location_id IS NULL（uk_biz_inv_loc）" \
  "insert into biz_inventory_location (material_id, warehouse_id, location_id, qty) values ('d1370003-0000-4000-8000-000000000001','d1370001-0000-4000-8000-000000000002',NULL,1); insert into biz_inventory_location (material_id, warehouse_id, location_id, qty) values ('d1370003-0000-4000-8000-000000000001','d1370001-0000-4000-8000-000000000002',NULL,2)" "uk_biz_inv_loc"
try_fail "反例8 流水：qty<=0（ck_biz_inv_txn_qty）" \
  "insert into biz_inventory_txn (material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, qty, period_code) values (gen_random_uuid(), gen_random_uuid(),'IN','X','STOCK_IN',gen_random_uuid(),'NEG-8',0,'2026-10')" "ck_biz_inv_txn_qty"
$PSQL -c "delete from biz_inventory_location where material_id='d1370003-0000-4000-8000-000000000001' and location_id is null" >/dev/null
echo "--- 终态：13 表 / 种子行数 ---"
$PSQL -c "select count(*) from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname='public' and c.relkind='r' and relname in ($T13)"
$PSQL -c "select (select count(*) from biz_unit)||'/'||(select count(*) from biz_warehouse)||'/'||(select count(*) from biz_location)||'/'||(select count(*) from biz_material)||'/'||(select count(*) from biz_inventory_location)"
echo "DONE"
