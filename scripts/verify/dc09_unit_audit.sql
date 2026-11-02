-- DC-09 计量单位止血：存量排查（**只读**，全 SELECT）
--
-- 用途：上闸前先看清「有没有存量单据用了折算单位」。命中 → 不自动改数，
-- 出清单交业务确认后再补「数据修正卡」（卡面 §4）。
--
-- 用法：
--   runuser -u postgres -- psql -d <库> -f scripts/verify/dc09_unit_audit.sql
--   psql "postgres://user:pass@host:5432/<库>" -f scripts/verify/dc09_unit_audit.sql
--
-- 判据说明（与代码 unit_guard::ensure_base_unit 同一口径）：
--   · 采购 / 收货明细自己有 unit_id ⇒ 直接按它判；
--   · 调拨明细（biz_stock_transfer_item）**没有 unit_id** ⇒ 按物料的基础单位判。

\echo === ① 非基本单位清单（is_base = 0）
select unit_code, unit_name, rate, base_unit_id is not null as has_base
  from biz_unit
 where deleted_at is null and is_base = 0
 order by unit_code;

\echo === ② 挂在非基本单位上的物料（这些物料建任何单据都会被拒）
select m.material_code, m.material_name, u.unit_code, u.unit_name, u.rate
  from biz_material m
  join biz_unit u on u.id = m.unit_id
 where m.deleted_at is null and u.is_base = 0
 order by m.material_code;

\echo === ③ 采购 / 收货明细命中（卡面 SQL，逐行）
SELECT 'po' AS src, i.id, m.material_code, u.unit_name, u.rate, i.qty
  FROM biz_purchase_order_item i
  JOIN biz_material m ON m.id = i.material_id
  JOIN biz_unit u ON u.id = i.unit_id
 WHERE u.is_base = 0
UNION ALL
SELECT 'si', i.id, m.material_code, u.unit_name, u.rate, i.qty
  FROM biz_stock_in_item i
  JOIN biz_material m ON m.id = i.material_id
  JOIN biz_unit u ON u.id = i.unit_id
 WHERE u.is_base = 0;

\echo === ④ 调拨明细命中（明细无 unit_id，按物料的基础单位判）
SELECT 'tr' AS src, i.id, m.material_code, u.unit_name, u.rate, i.qty
  FROM biz_stock_transfer_item i
  JOIN biz_material m ON m.id = i.material_id
  JOIN biz_unit u ON u.id = m.unit_id
 WHERE u.is_base = 0;

\echo === ⑤ 基数（对账用：命中数 / 总行数）
select 'po_items' as k, count(*) as rows from biz_purchase_order_item
union all select 'si_items', count(*) from biz_stock_in_item
union all select 'tr_items', count(*) from biz_stock_transfer_item;
