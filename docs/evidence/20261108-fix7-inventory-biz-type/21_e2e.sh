#!/usr/bin/env bash
# ============================================================================
# FIX-7（t_6207df79）145 取证 · E2E：销售出库 biz_type / 无源入库 in_type / 对账口径
#
# 跑法：ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix7-inventory-biz-type/21_e2e.sh'
# 前置：10_prepare_145.sh + 11_start_instance.sh（8048 + 拷贝库）
#
# 断言全部对着**真接口 + 真库**：HTTP 状态 + 响应字段 + 库内落值 + 对账 SQL + 巡检脚本。
# ============================================================================
set -uo pipefail
EV="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$EV/00_env.sh"

: > "$EV/raw/e2e.log"
exec > >(tee -a "$EV/raw/e2e.log") 2>&1

echo "=== FIX-7 E2E $(date '+%F %T') db=$DBNAME base=$BASE ==="

# ---------- 登录 ----------
ADMIN=$(login admin "$ADMIN_PASS")
if [ -z "$ADMIN" ]; then echo "FATAL: admin 登录失败"; exit 1; fi
TOK="$ADMIN"   # api() 用它；401 时自愈换新
echo "$ADMIN" > "$(tokf admin)"
echo "token ok (len=${#ADMIN})"

# ---------- 0. 迁移落库（存量库升级路径）----------
echo "--- 0) V0330 已应用到拷贝库 + 约束已换 ----------"
check_eq "330" "$(sq "select version from schema_migrations where name='inventory_biz_type_and_sourceless_in_type'")" "V0330 已应用"
check_eq "1" "$(sq "select count(*) from pg_constraint where conname='ck_biz_si_type' and pg_get_constraintdef(oid) like '%SOURCELESS%'")" "ck_biz_si_type 含 SOURCELESS"
check_eq "1" "$(sq "select count(*) from pg_constraint where conname='ck_biz_si_supplier_scope' and pg_get_constraintdef(oid) like '%SOURCELESS%'")" "ck_biz_si_supplier_scope 覆盖 SOURCELESS"

# ---------- 1. 存量回填（acceptance ①⑤）----------
echo "--- 1) 存量回填：销售出库 OTHER_OUT → SALE_OUT；无源单 PURCHASE → SOURCELESS ----------"
PRE="$EV/raw/pre_migration_state.txt"
pre_val() { sed -n "s/^$1|//p" "$PRE" | tr -d '[:space:]'; }
check_eq "0" "$(sq "select count(*) from biz_inventory_txn t join biz_stock_out s on s.id=t.doc_id
  where t.doc_type='STOCK_OUT' and s.source_type='SALES' and t.biz_type='OTHER_OUT'")" \
  "销售出库单下已无 OTHER_OUT 流水"
check_eq "$(pre_val sales_txn_other_out)" "$(sq "select count(*) from biz_inventory_txn t join biz_stock_out s on s.id=t.doc_id
  where t.doc_type='STOCK_OUT' and s.source_type='SALES' and t.biz_type='SALE_OUT'")" \
  "4 条存量流水全部迁成 SALE_OUT（条数 = 迁移前 OTHER_OUT 条数）"
check_eq "$(pre_val manual_other_out_txn)" "$(sq "select count(*) from biz_inventory_txn t join biz_stock_out s on s.id=t.doc_id
  where t.doc_type='STOCK_OUT' and coalesce(s.source_type,'') <> 'SALES' and t.biz_type='OTHER_OUT'")" \
  "手工其他出库的 OTHER_OUT 一条没被误伤"
check_eq "0" "$(sq "select count(*) from biz_stock_in where is_sourceless and in_type='PURCHASE'")" "已无「无源 + PURCHASE」的单"
check_eq "$(pre_val sourceless_docs)" "$(sq "select count(*) from biz_stock_in where in_type='SOURCELESS'")" \
  "存量无源单全部迁成 SOURCELESS（条数 = 迁移前无源单数）"
check_eq "0" "$(sq "select count(*) from biz_stock_in where in_type='SOURCELESS' and supplier_id is null")" \
  "所有 SOURCELESS 单都有供应商（约束口径成立）"

# ---------- 2. 主数据（拷贝库里的既有种子）----------
MAT="01a1b5d9-420f-735d-bdb6-80b22842d3dc"     # DEMO-INV01（serial_required=false）
WH="01a1b5d9-5e84-7173-a4f0-a4744f8d19b9"      # WH-DVA
SUP="01a1b5d2-504f-72f7-92b2-00f884285a2c"     # DEMO-S001
CUS=$(sq "select id from biz_customer where deleted_at is null limit 1")
UNIT=$(sq "select unit_id from biz_material where id='$MAT'::uuid")
echo "mat=$MAT wh=$WH sup=$SUP cus=$CUS unit=$UNIT"

# 新单据的唯一标记（本卡夹具，跑完保留在拷贝库里、随库一起销毁）
TAG="FIX7-$(date +%H%M%S)"

# ---------- 3. 无源入库（HTTP）→ in_type = SOURCELESS（acceptance ④）----------
echo "--- 3) 无源入库单：is_sourceless=true → in_type=SOURCELESS ----------"
BODY=$(api POST '/business/erp/stock-in' "" "{
  \"is_sourceless\": true,
  \"supplier_id\": \"$SUP\",
  \"warehouse_id\": \"$WH\",
  \"remark\": \"$TAG 无源入库（无采购单，有供应商）\",
  \"items\": [{\"material_id\": \"$MAT\", \"qty\": 100, \"price_excl\": 9, \"tax_rate\": 0}]
  }")
check_http "200" "无源入库建单"
IN_ID=$(printf '%s' "$BODY" | jq -r '.data.id // empty')
check_eq "SOURCELESS" "$(printf '%s' "$BODY" | jq -r '.data.in_type // empty')" "建单响应 in_type"
check_eq "SOURCELESS" "$(sq "select in_type from biz_stock_in where id='$IN_ID'::uuid")" "库内 in_type"
check_eq "t" "$(sq "select (is_sourceless and supplier_id='$SUP'::uuid)::text from biz_stock_in where id='$IN_ID'::uuid")" "库内 is_sourceless + 供应商"

api POST "/business/erp/stock-in/$IN_ID/submit-post" "" '{}' > /dev/null
check_http "200" "无源入库提交过账（金额 1000 恰好走到审批线以下则自动过账）"
check_eq "POSTED" "$(sq "select status from biz_stock_in where id='$IN_ID'::uuid")" "无源入库落 POSTED"
check_eq "PURCHASE_IN" "$(sq "select biz_type from biz_inventory_txn where doc_type='STOCK_IN' and doc_id='$IN_ID'::uuid limit 1")" \
  "无源入库的流水 biz_type 仍是 PURCHASE_IN（本次只改单据侧 in_type，已知口径见报告）"

# ---------- 4. 手工出库（HTTP）→ ISSUE_OUT 不受影响（回归）----------
echo "--- 4) 手工领用出库 30 件 → 流水 ISSUE_OUT ----------"
BODY=$(api POST '/business/erp/stock-out' "" "{
  \"out_type\": \"ISSUE\", \"warehouse_id\": \"$WH\", \"reason\": \"$TAG 手工领用（回归：非销售出库不许变 SALE_OUT）\",
  \"items\": [{\"material_id\": \"$MAT\", \"qty\": 30}]
}")
check_http "200" "手工领用出库建单"
SO_ID=$(printf '%s' "$BODY" | jq -r '.data.id // empty')
api PATCH "/business/erp/stock-out/$SO_ID/status" "" '{"status":"POSTED"}' > /dev/null
check_http "200" "手工领用出库过账"
check_eq "ISSUE_OUT" "$(sq "select biz_type from biz_inventory_txn where doc_type='STOCK_OUT' and doc_id='$SO_ID'::uuid limit 1")" \
  "手工出库流水 biz_type（回归：仍 ISSUE_OUT）"

# ---------- 5. 销售出库（HTTP 发货单过账）→ SALE_OUT（acceptance ①②）----------
echo "--- 5) 销售出库：APPROVED 订单（SQL 夹具）→ 建发货单 → 过账 → 流水 SALE_OUT ----------"
SO_UUID=$(sq "select gen_random_uuid()")
ITEM_UUID=$(sq "select gen_random_uuid()")
psqlc -v ON_ERROR_STOP=1 -q <<SQL
INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status, total_amount, total_tax, total_with_tax, remark)
VALUES ('$SO_UUID', '$TAG-SO', '$CUS'::uuid, CURRENT_DATE, 'APPROVED', 50, 0, 50, '$TAG 销售出库 biz_type 夹具');
INSERT INTO biz_sales_order_item (id, so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount)
VALUES ('$ITEM_UUID', '$SO_UUID'::uuid, 1, '$MAT'::uuid, 5, 10, 50, 0, 0);
SQL
BEFORE_SALE_QTY=$(sq "select coalesce(sum(qty),0) from biz_inventory_txn where biz_type='SALE_OUT'")

BODY=$(api POST '/business/erp/shipment' "" "{
  \"so_id\": \"$SO_UUID\", \"warehouse_id\": \"$WH\", \"remark\": \"$TAG 销售出库\"
}")
check_http "200" "建发货单"
SHIP_ID=$(printf '%s' "$BODY" | jq -r '.data.id // empty')
api PATCH "/business/erp/shipment/$SHIP_ID/status" "" '{"target_status":"POSTED"}' > /dev/null
check_http "200" "发货单过账（同一事务里生成并过账销售出库单）"

OUT_ID=$(sq "select id from biz_stock_out where source_id='$SHIP_ID'::uuid and source_type='SALES' and deleted_at is null")
check_eq "SALES" "$(sq "select source_type from biz_stock_out where id='$OUT_ID'::uuid")" "销售出库单 source_type"
check_eq "OTHER" "$(sq "select out_type from biz_stock_out where id='$OUT_ID'::uuid")" "销售出库单 out_type 仍是 OTHER（两维度不变）"
check_eq "SALE_OUT" "$(sq "select biz_type from biz_inventory_txn where doc_type='STOCK_OUT' and doc_id='$OUT_ID'::uuid and txn_type='OUT' limit 1")" \
  "★ 销售出库流水 biz_type = SALE_OUT（改之前是 OTHER_OUT）"
check_eq "1" "$(sq "select count(distinct biz_type) from biz_inventory_txn where doc_type='STOCK_OUT' and doc_id='$OUT_ID'::uuid")" \
  "该单流水只有一种 biz_type"
check_eq "5.0000" "$(sq "select sum(qty) from biz_inventory_txn where biz_type='SALE_OUT' and doc_type='STOCK_OUT' and doc_id='$OUT_ID'::uuid")" \
  "SALE_OUT 流水数量 = 发货 5 件"
NEW_SALE_QTY=$(sq "select coalesce(sum(qty),0) from biz_inventory_txn where biz_type='SALE_OUT'")
check_num "$(awk -v a="$BEFORE_SALE_QTY" 'BEGIN{printf "%.4f", a+5}')" "$NEW_SALE_QTY" \
  "按 biz_type 统计销售出库：统计前 $BEFORE_SALE_QTY + 本次 5 = 统计后 $NEW_SALE_QTY"

# ---------- 6. 对账口径（acceptance ③）----------
echo "--- 6) 对账：库存 vs 流水按 (物料,仓库) 聚合；逐桶只比 成本账 vs 库位账 ----------"
REC1="with inv as (select material_id, warehouse_id, sum(qty) q from biz_inventory group by 1,2),
      txn as (select material_id, warehouse_id, sum(case when txn_type='IN' then qty else -qty end) q from biz_inventory_txn group by 1,2)
      select count(*) from inv full join txn using (material_id, warehouse_id)
      where coalesce(inv.q,0) <> coalesce(txn.q,0)"
REC2="with cost as (select i.material_id, i.warehouse_id, coalesce(il.location_id::text,'(未分配)') bucket, sum(i.qty) q
                      from biz_inventory i left join biz_inventory_location il on il.id=i.location_id group by 1,2,3),
      loc as (select material_id, warehouse_id, coalesce(location_id::text,'(未分配)') bucket, sum(qty) q
                from biz_inventory_location group by 1,2,3)
      select count(*) from cost c full join loc l
        on c.material_id=l.material_id and c.warehouse_id=l.warehouse_id and c.bucket=l.bucket
      where coalesce(c.q,0) <> coalesce(l.q,0)"
REC3="select count(*) from biz_inventory_txn where (txn_type='IN' and after_qty-before_qty <> qty)
        or (txn_type='OUT' and before_qty-after_qty <> qty)"
BASE_REC1=$(sed -n 's/^reconcile_qty_diff|//p' "$PRE" | tr -d '[:space:]')
check_eq "${BASE_REC1:-0}" "$(sq "$REC1")" "库存 vs 流水：(物料,仓库) 聚合差异行数（与迁移前一致）"
check_eq "0" "$(sq "$REC2")" "逐桶：成本账 vs 库位账 差异行数"
check_eq "0" "$(sq "$REC3")" "流水自洽 after_qty-before_qty = ±qty"
echo "  ℹ️ 金额口径：库存金额 vs 流水金额（容许 4 位标度误差，仅记录）"
psqlc -X -P pager=off -c "
  select (select coalesce(sum(amount),0) from biz_inventory) as inv_amount,
         (select coalesce(sum(case when txn_type='IN' then amount else -amount end),0) from biz_inventory_txn) as txn_amount,
         (select coalesce(sum(amount),0) from biz_inventory)
           - (select coalesce(sum(case when txn_type='IN' then amount else -amount end),0) from biz_inventory_txn) as diff"

# ---------- 7. 巡检脚本在同一份数据上跑 ----------
echo "--- 7) 巡检脚本（本卡口径固化的 B1 / B1b / B4）----------"
if command -v python3 >/dev/null 2>&1; then
  NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 NIK_DATABASE_USER=cloudpivot \
  NIK_DATABASE_NAME="$DBNAME" NIK_DATABASE_PASSWORD="$NIK_DATABASE_PASSWORD" \
    python3 /opt/cloudpivot/scripts/verify/erp_invariants.py --group 账账 > "$EV/raw/invariants_acct.log" 2>&1
  RC=$?
  echo "  erp_invariants --group 账账 exit=$RC"
  grep -E "B1 |B1b|B4 " "$EV/raw/invariants_acct.log" | sed 's/^/    /'
  check_eq "0" "$RC" "巡检「账账」组 BLOCKER 全绿（退出码 0）"
else
  echo "  ⚠️ 145 上没有 python3，跳过巡检脚本（对账 SQL 已在 §6 直接跑过）"
fi

# ---------- 收尾 ----------
summary
echo "--- 关键落值快照（供人工复核）---"
psqlc -X -P pager=off -c "
  select s.out_no, s.out_type, s.source_type, t.biz_type, t.txn_type, t.qty
    from biz_stock_out s join biz_inventory_txn t on t.doc_id = s.id and t.doc_type='STOCK_OUT'
   where s.out_no like 'OUT-%' and s.source_type = 'SALES' and s.deleted_at is null
   order by s.created_at desc limit 8"
psqlc -X -P pager=off -c "
  select in_no, in_type, is_sourceless, (supplier_id is not null) as has_supplier
    from biz_stock_in where in_no like 'IN-%' order by created_at desc limit 5"
