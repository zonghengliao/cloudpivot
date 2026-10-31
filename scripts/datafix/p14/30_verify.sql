-- ============================================================================
-- P14 / 30：清完自检（必须全绿）
-- ----------------------------------------------------------------------------
-- 用法（apply.sh 会自动跑；也可单独跑）：
--   psql -h … -U … -d … -At -F'|' -v ON_ERROR_STOP=1 -f 30_verify.sql
-- 输出：每行 `状态|编号 名称|实测值`，状态 ∈ PASS / FAIL / INFO。
--       任何一行 FAIL ⇒ apply.sh 退出码 1（全绿才放行）。
--
-- 判据口径与 `scripts/verify/erp_invariants.py` 一致（B1/B4/C3/C5/B2），
-- 区别只是把范围收紧到「本批 + 全库复核」，便于清理当场看结论。
-- ============================================================================
with cfg as (
    select (select id from biz_material
             where material_code = 'M-W1A-004' and deleted_at is null
             order by created_at limit 1) as mat,
           (select id from biz_warehouse where warehouse_code = 'WH01') as wh,
           (select id from sys_user where username = 'henry.liao' and deleted_at is null) as usr
),
cn as (
    select
        -- ① 序列号存活数
        (select count(*) from biz_material_serial s, cfg
          where s.source_type = 'PRODUCE' and s.source_id is null
            and s.create_by = cfg.usr
            and s.created_at >= timestamptz '2026-10-31 16:17:00+08'
            and s.created_at <  timestamptz '2026-10-31 16:18:00+08'
            and s.deleted_at is null) as alive_serials,
        -- ② 该组成本账（0 行 / 非零 / 已归零 三态要分开）
        (select count(*) from biz_inventory i, cfg
          where i.material_id = cfg.mat and i.warehouse_id = cfg.wh) as inv_rows,
        (select count(*) from biz_inventory i, cfg
          where i.material_id = cfg.mat and i.warehouse_id = cfg.wh
            and i.qty = 0 and i.amount = 0 and i.avg_cost = 0) as inv_zero_rows,
        -- ③ 冲销流水
        (select count(*) from biz_inventory_txn t
          where t.doc_no = 'DATAFIX-20261101-P14' and t.txn_type = 'OUT'
            and t.qty = 10 and t.amount = 0) as revert_txn,
        -- ④ 库位账（该组）
        (select coalesce(sum(l.qty), 0) from biz_inventory_location l, cfg
          where l.material_id = cfg.mat and l.warehouse_id = cfg.wh) as loc_qty,
        -- ⑤ 流水累计（该组）
        (select coalesce(sum(case when t.txn_type = 'IN' then t.qty else -t.qty end), 0)
           from biz_inventory_txn t, cfg
          where t.material_id = cfg.mat and t.warehouse_id = cfg.wh) as txn_cum,
        -- ⑥ 凭证
        (select count(*) from biz_voucher v
          where v.source_no like '%SN-20261031-000011%') as vouchers,
        -- ⑦ 全库 C3（有货但零成本）
        (select count(*) from biz_inventory i where i.qty > 0 and i.avg_cost = 0) as db_zero_cost,
        -- ⑧ 全库 C5（一物一码：在库件数 = 帐面数量，仅 serial_required 物料）
        (select count(*) from (
            with serial as (
                select material_id, warehouse_id, count(*)::numeric as q
                  from biz_material_serial
                 where status = 'IN_STOCK' and deleted_at is null
                 group by 1, 2
            ), inv as (
                select material_id, warehouse_id, sum(qty) as q from biz_inventory group by 1, 2
            )
            select 1
              from serial s
              full outer join inv v
                on v.material_id = s.material_id
               and v.warehouse_id is not distinct from s.warehouse_id
              join biz_material m on m.id = coalesce(s.material_id, v.material_id)
             where m.serial_required = true
               and abs(coalesce(s.q,0) - coalesce(v.q,0)) > 0.0001
        ) x) as db_serial_gap,
        -- ⑨ 全库 B1（库位账合计 vs 成本账 qty）
        (select count(*) from (
            with cost as (select material_id, warehouse_id, sum(qty) as q from biz_inventory group by 1,2),
                 loc  as (select material_id, warehouse_id, sum(qty) as q from biz_inventory_location group by 1,2)
            select 1 from cost c full outer join loc l
                on c.material_id = l.material_id and c.warehouse_id = l.warehouse_id
             where abs(coalesce(c.q,0) - coalesce(l.q,0)) > 0.0001
        ) y) as db_b1_gap,
        -- ⑩ 全库 B2（库存金额 = 1405 余额）
        (select coalesce(sum(amount),0) from biz_inventory) as inv_amount,
        (select coalesce(sum(coalesce(e.debit_amount,0) - coalesce(e.credit_amount,0)), 0)
           from biz_voucher_entry e
           join biz_voucher v on v.id = e.voucher_id and v.deleted_at is null
           join biz_account a on a.id = e.account_id
          where a.account_code like '1405%') as gl_1405
)
select status, label, detail from (
    select case when alive_serials = 0 then 'PASS' else 'FAIL' end as status,
           '① 10 个序列号已全部软删（存活数）' as label,
           alive_serials::text as detail from cn
    union all
    select case when inv_rows = 1 and inv_zero_rows = 1 then 'PASS' else 'FAIL' end,
           '② 成本账 M-W1A-004/WH01 归零（行数=1 且 qty/amount/avg_cost 全 0）',
           inv_rows::text || ' 行 / 其中归零 ' || inv_zero_rows::text || ' 行' from cn
    union all
    select case when revert_txn = 1 then 'PASS' else 'FAIL' end,
           '③ 冲销流水存在（doc_no=DATAFIX-20261101-P14 / OUT / qty=10 / 金额 0）',
           revert_txn::text || ' 条' from cn
    union all
    select case when loc_qty = 0 then 'PASS' else 'FAIL' end,
           '④ 库位账保持 0 行（B1 分子）',
           loc_qty::text || ' 件' from cn
    union all
    select case when txn_cum = 0 then 'PASS' else 'FAIL' end,
           '⑤ 流水累计 = 库存（B4）',
           txn_cum::text || ' vs 0.0000' from cn
    union all
    select case when vouchers = 0 then 'PASS' else 'FAIL' end,
           '⑥ 无对应凭证（金额 0 → 不需要红冲）',
           vouchers::text || ' 张' from cn
    union all
    select case when db_zero_cost = 0 then 'PASS' else 'FAIL' end,
           '⑦ 全库 C3「有库存则成本价非零」',
           db_zero_cost::text || ' 行违规' from cn
    union all
    select case when db_serial_gap = 0 then 'PASS' else 'FAIL' end,
           '⑧ 全库 C5「在库序列号件数 = 库存数量」',
           db_serial_gap::text || ' 组不符' from cn
    union all
    select case when db_b1_gap = 0 then 'PASS' else 'FAIL' end,
           '⑨ 全库 B1「库位账合计 = 成本账 qty」',
           db_b1_gap::text || ' 组不符' from cn
    union all
    select case when abs(inv_amount - gl_1405) <= 0.0001 then 'PASS' else 'FAIL' end,
           '⑩ 全库 B2「库存金额 = 存货科目 1405 余额」',
           inv_amount::text || ' vs ' || gl_1405::text from cn
    union all
    select case when alive_serials = 0 and inv_zero_rows = 1 then 'INFO' else 'FAIL' end,
           '—— 汇总：P14 清理完成（① 与 ② 同时成立）',
           '' from cn
) t
order by case status when 'FAIL' then 0 when 'PASS' then 1 else 2 end, label;
