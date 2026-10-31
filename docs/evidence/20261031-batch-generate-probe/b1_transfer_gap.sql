-- 2026-10-31 · 架构侧取证：调拨结算不写库位账 ⇒ B1（BLOCKER）必红
-- 依据：docs/技术方案-20261031-批量生成无来源入库.md §11 R3
-- 只读脚本：**不 BEGIN/COMMIT、不写任何表**，可对任何库（含生产副本）直接跑。
-- 用法：
--   cp docs/evidence/20261031-batch-generate-probe/b1_transfer_gap.sql /tmp/archspec/
--   chmod 644 /tmp/archspec/b1_transfer_gap.sql
--   su postgres -c "psql -X -d cloudpivot_test -f /tmp/archspec/b1_transfer_gap.sql"

\echo '===== ① B1 判据原样（scripts/verify/erp_invariants.py:173-189）====='
with cost as (
    select material_id, warehouse_id, sum(qty) as q
    from biz_inventory
    group by 1, 2
), loc as (
    select material_id, warehouse_id, sum(qty) as q
    from biz_inventory_location
    group by 1, 2
)
select coalesce(c.material_id, l.material_id)::text as material_id,
       coalesce(c.warehouse_id, l.warehouse_id)::text as warehouse_id,
       coalesce(c.q,0)::text as cost_qty,
       coalesce(l.q,0)::text as loc_qty,
       (coalesce(c.q,0) - coalesce(l.q,0))::text as diff
  from cost c full outer join loc l
    on c.material_id = l.material_id and c.warehouse_id = l.warehouse_id
 where abs(coalesce(c.q,0) - coalesce(l.q,0)) > 0.0001
 order by 1, 2;

\echo '===== ② 上述差额行的物料是否只有调拨流水（收货流水=0 才能归因到调拨路径）====='
with cost as (
    select material_id, warehouse_id, sum(qty) as q from biz_inventory group by 1,2
), loc as (
    select material_id, warehouse_id, sum(qty) as q from biz_inventory_location group by 1,2
), bad as (
    select coalesce(c.material_id, l.material_id) as material_id,
           coalesce(c.warehouse_id, l.warehouse_id) as warehouse_id
      from cost c full outer join loc l
        on c.material_id = l.material_id and c.warehouse_id = l.warehouse_id
     where abs(coalesce(c.q,0) - coalesce(l.q,0)) > 0.0001
)
select m.material_code,
       w.warehouse_name,
       count(*) filter (where t.doc_type = 'STOCK_TRANSFER')      as 调拨流水,
       count(*) filter (where t.doc_type <> 'STOCK_TRANSFER')     as 非调拨流水
  from bad b
  join biz_material  m on m.id = b.material_id
  join biz_warehouse w on w.id = b.warehouse_id
  left join biz_inventory_txn t
         on t.material_id = b.material_id and t.warehouse_id = b.warehouse_id
 group by 1, 2
 order by 1, 2;

\echo '===== ③ 库位账行数（为 0 说明该仓压根没有库位账）====='
select count(*) as inventory_location_rows from biz_inventory_location;
