-- ============================================================================
-- P14 / 10 · 回滚：把 10 个序列号恢复成「未软删」原样
-- ----------------------------------------------------------------------------
-- 数据来源：datafix_p14_snapshot（10_void_serials.up.sql 在软删之前用 to_jsonb(s) 存了整行）
--   ⇒ 不是「把 deleted_at 无条件置 NULL」，而是**写回快照里那一刻的值**
--     （原行 deleted_at 本来就是 NULL，但快照口径能防「up 前后被别人改过」这类事故）。
--
-- 幂等：只更新「当前仍处于软删」的行；第二遍 0 行。
-- 安全阀：快照不是 10 行 → 拒绝回滚（说明 up 没干净跑过，或者快照表被别的批次污染）。
-- 顺序：本脚本只解软删，不碰成本账/流水；成本账回滚见 20_reverse_inventory.down.sql
--       （apply.sh --rollback 会先跑 20 的 down 再跑本脚本，保证「流水累计 = 库存」始终成立）。
-- ============================================================================
do $$
declare
    c_batch constant text := 'P14';
    v_snap  int;
    v_n     int;
begin
    if to_regclass('public.datafix_p14_snapshot') is null then
        raise exception 'P14/10 down: 没有 datafix_p14_snapshot 表 —— up 没跑过，无从回滚';
    end if;

    select count(*) into v_snap
      from datafix_p14_snapshot
     where batch = c_batch and tbl = 'biz_material_serial';
    if v_snap <> 10 then
        raise exception 'P14/10 down: 快照 % 行 ≠ 10 —— 拒绝回滚，请人工核对快照表', v_snap;
    end if;

    update biz_material_serial s
       set deleted_at = (e.row_data->>'deleted_at')::timestamptz,
           updated_at = (e.row_data->>'updated_at')::timestamptz
      from datafix_p14_snapshot e
     where e.batch = c_batch
       and e.tbl = 'biz_material_serial'
       and e.pk = s.id
       and s.deleted_at is not null;
    get diagnostics v_n = row_count;

    raise notice 'P14/10 down: 已还原 % 个序列号的 deleted_at（第二遍跑会报 0）', v_n;
end
$$;
