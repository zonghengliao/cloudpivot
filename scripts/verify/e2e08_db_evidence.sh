#!/usr/bin/env bash
# E2E-08 · DB 层证据 + 勾稽（psql 直查，只读）
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/e2e08_master_common.sh"
OUT=${OUT:-/tmp/e2e08/db-evidence.txt}
: > "$OUT"
W() { printf '%s\n' "$*" | tee -a "$OUT"; }

W "===== E2E-08 主数据域 · DB 证据（$(date '+%F %T %Z')） ====="
W
W "-- 1. 取号器六条规则（纯流水 / 不计日期 / 永不重置）"
PSQLQ -c "select seq_type, prefix, separator, pad_width, date_pattern, period_key, current_no \
          from biz_seq where seq_type in ('SUP','CUS','MAT','WH','AC','ET') order by seq_type" | tee -a "$OUT"
W
W "-- 2. 本卡演练行（E2E08 前缀）存活计数"
PSQLQ -c "select 'biz_customer' t, count(*) from biz_customer where code ilike 'E2E08%' and deleted_at is null
 union all select 'biz_supplier', count(*) from biz_supplier where supplier_code ilike 'E2E08%' and deleted_at is null
 union all select 'biz_material', count(*) from biz_material where material_code ilike 'E2E08%' and deleted_at is null
 union all select 'biz_warehouse', count(*) from biz_warehouse where warehouse_code ilike 'E2E08%' and deleted_at is null
 union all select 'biz_asset_category', count(*) from biz_asset_category where category_code ilike 'E2E08%' and deleted_at is null
 union all select 'biz_expense_type', count(*) from biz_expense_type where type_code ilike 'E2E08%' and deleted_at is null
 union all select 'biz_unit', count(*) from biz_unit where unit_code ilike 'E2E08%' and deleted_at is null" | tee -a "$OUT"
W
W "-- 3. 编码唯一性（六类主数据，全库存活行）"
PSQLQ -c "select 'supplier' k, count(*) dup from (select supplier_code from biz_supplier where deleted_at is null group by 1 having count(*)>1) s
 union all select 'customer', count(*) from (select code from biz_customer where deleted_at is null group by 1 having count(*)>1) s
 union all select 'material', count(*) from (select material_code from biz_material where deleted_at is null group by 1 having count(*)>1) s
 union all select 'warehouse', count(*) from (select warehouse_code from biz_warehouse where deleted_at is null group by 1 having count(*)>1) s
 union all select 'asset_category', count(*) from (select category_code from biz_asset_category where deleted_at is null group by 1 having count(*)>1) s
 union all select 'expense_type', count(*) from (select type_code from biz_expense_type where deleted_at is null group by 1 having count(*)>1) s" | tee -a "$OUT"
W
W "-- 4. 自动编码形态校验（六类当前最大号与位数；不符合 <前缀>-<数字> 的存活行应为 0）"
PSQLQ -c "select 'SUP' pfx, count(*) filter (where supplier_code !~ '^SUP-[0-9]+\$') bad_shape, max(supplier_code) max_code from biz_supplier where deleted_at is null
 union all select 'CUS', count(*) filter (where code !~ '^CUS-[0-9]+\$'), max(code) from biz_customer where deleted_at is null
 union all select 'MAT', count(*) filter (where material_code !~ '^MAT-[0-9]+\$'), max(material_code) from biz_material where deleted_at is null
 union all select 'WH',  count(*) filter (where warehouse_code !~ '^WH-[0-9]+\$'), max(warehouse_code) from biz_warehouse where deleted_at is null
 union all select 'AC',  count(*) filter (where category_code !~ '^AC-[0-9]+\$'), max(category_code) from biz_asset_category where deleted_at is null
 union all select 'ET',  count(*) filter (where type_code !~ '^ET-[0-9]+\$'), max(type_code) from biz_expense_type where deleted_at is null" | tee -a "$OUT"
W "  （bad_shape 非 0 是历史自有编码体系（如 SO6HT-CUST / W3C-M-L3）；本卡自己造的编码形态由 e2e08_master_a.sh ⑥ 断言）"
W
W "-- 5. 价格表：本卡物料的存活价格行与区间"
PSQLQ -c "select 'agreed' k, customer_id::text, material_id::text, min_qty, price, effective_from, effective_to, status
            from biz_customer_price where material_id in (select id from biz_material where material_code ilike 'E2E08%') and deleted_at is null
 union all
 select 'tier-'||tier_type, coalesce(customer_id::text,'(通用)'), material_id::text, min_qty, price, effective_from, effective_to, status
            from biz_price_tier where material_id in (select id from biz_material where material_code ilike 'E2E08%') and deleted_at is null" | tee -a "$OUT"
W
W "-- 6. 阶梯价区间重叠检查（同物料/客户/类型/生效期，qty 区间相交且都 ACTIVE 的对数）"
PSQLQ -c "select count(*) as overlap_pairs from biz_price_tier a join biz_price_tier b
             on a.id < b.id and a.material_id = b.material_id
            and a.customer_id is not distinct from b.customer_id
            and a.tier_type = b.tier_type
            and a.effective_from <= coalesce(b.effective_to,'9999-12-31')
            and b.effective_from <= coalesce(a.effective_to,'9999-12-31')
            and a.min_qty < coalesce(b.max_qty,'1e18') and b.min_qty < coalesce(a.max_qty,'1e18')
          where a.deleted_at is null and b.deleted_at is null and a.status='ACTIVE' and b.status='ACTIVE'
            and a.material_id in (select id from biz_material where material_code ilike 'E2E08%')" | tee -a "$OUT"
W "  （全库视角：下方为全库重叠对数，仅供观察，可能是他人历史数据）"
PSQLQ -c "select count(*) as overlap_pairs_all from biz_price_tier a join biz_price_tier b
             on a.id < b.id and a.material_id = b.material_id
            and a.customer_id is not distinct from b.customer_id
            and a.tier_type = b.tier_type
            and a.effective_from <= coalesce(b.effective_to,'9999-12-31')
            and b.effective_from <= coalesce(a.effective_to,'9999-12-31')
            and a.min_qty < coalesce(b.max_qty,'1e18') and b.min_qty < coalesce(a.max_qty,'1e18')
          where a.deleted_at is null and b.deleted_at is null and a.status='ACTIVE' and b.status='ACTIVE'" | tee -a "$OUT"
W
W "-- 7. 一物一码：本卡物料 serial_required 与序列号行数（逐物料）"
PSQLQ -c "select m.material_code, m.serial_required, count(s.id) serial_rows
            from biz_material m left join biz_material_serial s on s.material_id = m.id and s.deleted_at is null
           where m.material_code ilike 'E2E08%' and m.deleted_at is null group by 1,2 order by 1" | tee -a "$OUT"
W "-- 7c. 软删物料下的序列号是否保留（审计口径：软删不级联）"
PSQLQ -c "select m.material_code, m.deleted_at is not null as material_deleted, count(s.id) serial_rows
            from biz_material m join biz_material_serial s on s.material_id = m.id and s.deleted_at is null
           where m.material_code ilike 'E2E08%' group by 1,2 order by 1" | tee -a "$OUT"
W
W "-- 7d. 价格行全量（含软删：演练实际建过哪些价、最后如何收口）"
PSQLQ -c "select 'agreed' k, a.price, a.min_qty, a.status, a.effective_from, a.effective_to, a.deleted_at is not null as soft_deleted
            from biz_customer_price a where a.material_id in (select id from biz_material where material_code ilike 'E2E08%')
 union all
 select 'tier-'||b.tier_type, b.price, b.min_qty, b.status, b.effective_from, b.effective_to, b.deleted_at is not null
            from biz_price_tier b where b.material_id in (select id from biz_material where material_code ilike 'E2E08%')
 order by 1,7" | tee -a "$OUT"
W "-- 7b. 序列号状态分布（本卡物料）"
PSQLQ -c "select status, count(*) from biz_material_serial
           where material_id in (select id from biz_material where material_code ilike 'E2E08%') and deleted_at is null
           group by 1 order by 1" | tee -a "$OUT"
W
W "-- 8. 附件上传人姓名快照（本卡单据）"
PSQLQ -c "select a.doc_type, c.code customer_code, a.uploader_name,
                 (a.uploader_name is not null and a.uploader_name <> '' and a.uploader_name !~ '^[0-9a-f-]{36}\$') snapshot_ok,
                 u.real_name current_real_name
            from biz_doc_attachment a
            left join biz_customer c on c.id = a.doc_id and a.doc_type = 'CUSTOMER'
            left join sys_user u on u.id = a.uploaded_by
           where c.code ilike 'E2E08%' and a.deleted_at is null" | tee -a "$OUT"
W
W "-- 9. 悬空引用扫描：物料默认仓指向已软删仓库"
PSQLQ -c "select 'E2E08(含软删物料)' scope, count(*) from biz_material m join biz_warehouse w on w.id = m.default_warehouse_id
           where w.deleted_at is not null and m.material_code ilike 'E2E08%'
 union all
 select 'ALL(存活物料)', count(*) from biz_material m join biz_warehouse w on w.id = m.default_warehouse_id
           where m.deleted_at is null and w.deleted_at is not null" | tee -a "$OUT"
W "  （第一条 = 已软删物料的默认仓指向已删仓库：物料已删不计入引用闸，属预期；第二条是当前线上影响面）"
W
W "-- 10. 编码策略参数（演练后应恢复 true）"
PSQLQ -c "select config_key, config_value, status from sys_config where config_key = 'erp.master_data.allow_manual_code'" | tee -a "$OUT"
W
W "===== 证据结束 ====="
say "DB 证据已写入 $OUT"
