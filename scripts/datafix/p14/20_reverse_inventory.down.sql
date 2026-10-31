-- ============================================================================
-- P14 / 20 · 回滚：删掉冲销流水 + 把成本账写回快照里的原值
-- ----------------------------------------------------------------------------
-- 顺序有意为之：
--   ① 先确认快照在（没快照就不许动任何东西）
--   ② 再删冲销流水（固定主键 id = 01a1f140-…-0001，且校验 doc_no 是本批的）
--   ③ 最后把 biz_inventory 按快照写回 qty/amount/avg_cost/last_txn_at/updated_at
--   ⇒ 任意时刻「流水累计 = 库存」（B4）都不破。
--
-- 幂等：删除按固定主键；UPDATE 只写「与快照不同」的行 ⇒ 第二遍 0 行受影响。
-- 安全阀：快照里没有 biz_inventory 那一行 → 拒绝回滚（说明 up 没跑过）。
-- ============================================================================
do $$
declare
    c_batch  constant text := 'P14';
    c_doc_no constant text := 'DATAFIX-20261101-P14';
    c_txn_id constant uuid := '01a1f140-0000-4000-8000-000000000001';
    v_snap int;
    v_del  int;
    v_n    int;
    v_mat  uuid;
    v_wh   uuid;
    v_cum  numeric;
    v_qty  numeric;
begin
    if to_regclass('public.datafix_p14_snapshot') is null then
        raise exception 'P14/20 down: 没有 datafix_p14_snapshot 表 —— up 没跑过，无从回滚';
    end if;

    select count(*) into v_snap
      from datafix_p14_snapshot where batch = c_batch and tbl = 'biz_inventory';
    if v_snap <> 1 then
        raise exception 'P14/20 down: 成本账快照 % 行（预期 1 行）—— 拒绝回滚，请人工核对快照表', v_snap;
    end if;

    -- ① 删冲销流水（固定主键 + doc_no 双重判据，绝不按 doc_no LIKE 删）
    delete from biz_inventory_txn
     where id = c_txn_id and doc_no = c_doc_no;
    get diagnostics v_del = row_count;

    -- ② 成本账按快照写回
    update biz_inventory i
       set qty         = (e.row_data->>'qty')::numeric,
           amount      = (e.row_data->>'amount')::numeric,
           avg_cost    = (e.row_data->>'avg_cost')::numeric,
           last_txn_at = (e.row_data->>'last_txn_at')::timestamptz,
           updated_at  = (e.row_data->>'updated_at')::timestamptz
      from datafix_p14_snapshot e
     where e.batch = c_batch
       and e.tbl = 'biz_inventory'
       and e.pk = i.id
       and (i.qty, i.amount, i.avg_cost) is distinct from (
             (e.row_data->>'qty')::numeric,
             (e.row_data->>'amount')::numeric,
             (e.row_data->>'avg_cost')::numeric);
    get diagnostics v_n = row_count;

    -- ③ 后置断言：B4 仍然成立
    select i.material_id, i.warehouse_id, i.qty into v_mat, v_wh, v_qty
      from biz_inventory i
      join datafix_p14_snapshot e on e.batch = c_batch and e.tbl = 'biz_inventory' and e.pk = i.id;
    select coalesce(sum(case when txn_type = 'IN' then qty else -qty end), 0) into v_cum
      from biz_inventory_txn where material_id = v_mat and warehouse_id = v_wh;
    if v_cum <> v_qty then
        raise exception 'P14/20 down: 回滚后流水累计 % ≠ 库存 % —— 已回滚', v_cum, v_qty;
    end if;

    raise notice 'P14/20 down: 删除冲销流水 % 行 / 成本账写回 % 行（qty=%，流水累计=%，B4 成立）',
        v_del, v_n, v_qty, v_cum;
end
$$;
