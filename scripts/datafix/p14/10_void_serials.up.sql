-- ============================================================================
-- P14 / 10：软删「批量生成」造出来的 10 个序列号
-- ----------------------------------------------------------------------------
-- 目标数据（老板 2026-11-01 00:16 确认是测试用、无实际作用）：
--   物料 M-W1A-004 / 仓库 WH01 / 10 件 / 成本 0
--   序列号 SN-20261031-000011 ~ SN-20261031-000020
--   source_type = 'PRODUCE'、source_id IS NULL、create_by = henry.liao
--   created_at = 2026-10-31 16:17:24.048839+08（同一批，同一条 INSERT）
--
-- 为什么是「软删」而不是物理删：
--   ① `biz_stock_transfer_item_serial.serial_id` 是**硬外键**指向 biz_material_serial，
--      物理删万一有引用就是 FK 违约；软删永远安全。
--   ② `uk_biz_material_serial_no` 是 `WHERE deleted_at IS NULL` 的部分唯一索引
--      ⇒ 软删后同号可以重新发号，不会挡未来的正常业务。
--   ③ 行保留 = 审计不丢（谁在什么时候生成过这 10 个号，以后还查得到）。
--
-- 判据（严进）：四个条件同时成立才动
--   source_type='PRODUCE' AND source_id IS NULL AND create_by=henry.liao.id
--   AND created_at ∈ [2026-10-31 16:17:00+08, 2026-10-31 16:18:00+08)
--   另外**叠加形状复核**：必须全部是 M-W1A-004 / WH01 / IN_STOCK / 无库位 / 号段 SN-20261031-*，
--   一条不符就 RAISE EXCEPTION 停下（宁可不动，不许误伤）。
--
-- 反例表（这几类**不在**本脚本范围，命中即停）：
--   * `source_type='PURCHASE' AND source_id IS NULL` 的 21 条（历史夹具，已全部软删，不动）
--   * 有 source_id 的（收货/调拨真业务）—— 不匹配判据，天然不动
--   * 同窗口但 create_by 不是 henry.liao 的 —— 不动
--
-- 幂等：命中的 10 行全部 deleted_at 已置位时只 NOTICE 跳过（第二遍 0 行受影响）。
--       只有「命中总数 ≠ 10」或「存活数既不是 10 也不是 0」才抛异常。
--
-- 回滚：10_void_serials.down.sql 按 datafix_p14_snapshot 的整行快照精确写回 deleted_at。
-- ============================================================================

-- 整行快照表：down 精确写回用（P10 数据修复踩过的教训 —— 只记「删了什么 id」不够，
-- 要能还原成删除前的**一模一样**，包括 updated_at）。
create table if not exists datafix_p14_snapshot (
    batch       text        not null,
    tbl         text        not null,
    pk          uuid        not null,
    row_data    jsonb       not null,
    captured_at timestamptz not null default now(),
    primary key (batch, tbl, pk)
);
comment on table datafix_p14_snapshot is
    'P14 数据修复整行快照（batch=P14）。down 脚本按此精确写回；清理完成后可整表 DROP。';

do $$
declare
    c_batch constant text := 'P14';
    c_from  constant timestamptz := timestamptz '2026-10-31 16:17:00+08';
    c_to    constant timestamptz := timestamptz '2026-10-31 16:18:00+08';
    v_user     uuid;
    v_mat      uuid;
    v_wh       uuid;
    v_total    int;
    v_alive    int;
    v_offshape int;
    v_link     int;
    v_snapped  int;
    v_n        int;
begin
    -- ---------- 0) 前置：关键对象必须存在 ----------
    select id into v_user from sys_user where username = 'henry.liao' and deleted_at is null;
    if v_user is null then
        raise exception 'P14/10: 找不到用户 henry.liao（或已软删）—— 停下人工核对，不自动清';
    end if;

    select id into v_mat from biz_material
     where material_code = 'M-W1A-004' and deleted_at is null
     order by created_at limit 1;
    if v_mat is null then
        raise exception 'P14/10: 找不到物料 M-W1A-004 —— 停下人工核对，不自动清';
    end if;

    select id into v_wh from biz_warehouse where warehouse_code = 'WH01';
    if v_wh is null then
        raise exception 'P14/10: 找不到仓库 WH01 —— 停下人工核对，不自动清';
    end if;

    -- ---------- 1) 命中总数与存活数（硬约束：必须恰好 10 行） ----------
    select count(*), count(*) filter (where deleted_at is null)
      into v_total, v_alive
      from biz_material_serial
     where source_type = 'PRODUCE'
       and source_id is null
       and create_by = v_user
       and created_at >= c_from
       and created_at <  c_to;

    if v_total <> 10 then
        raise exception 'P14/10: 判据命中 % 行（含已软删），预期恰好 10 行 —— 停下人工核对，不自动清', v_total;
    end if;

    if v_alive = 0 then
        raise notice 'P14/10: 10 个序列号已全部软删 —— 幂等跳过（本步 0 行受影响）';
        return;
    elsif v_alive <> 10 then
        raise exception 'P14/10: 存活 % 行（预期 10 行全新 或 0 行已清）—— 部分已删说明情况变了，停下人工核对', v_alive;
    end if;

    -- ---------- 2) 形状复核（不许误伤） ----------
    select count(*) into v_offshape
      from biz_material_serial
     where source_type = 'PRODUCE'
       and source_id is null
       and create_by = v_user
       and created_at >= c_from
       and created_at <  c_to
       and deleted_at is null
       and (material_id <> v_mat
            or warehouse_id <> v_wh
            or status <> 'IN_STOCK'
            or location_id is not null
            or serial_no !~ '^SN-20261031-0000(1[1-9]|20)$');
    if v_offshape > 0 then
        raise exception 'P14/10: 有 % 行形状不符（应为 M-W1A-004/WH01/IN_STOCK/无库位/SN-20261031-000011~000020）—— 停下人工核对', v_offshape;
    end if;

    -- ---------- 3) 不得被调拨明细引用（有引用就该走作废流程，不是软删） ----------
    select count(*) into v_link
      from biz_stock_transfer_item_serial x
     where x.serial_id in (
             select id from biz_material_serial
              where source_type = 'PRODUCE' and source_id is null and create_by = v_user
                and created_at >= c_from and created_at < c_to);
    if v_link > 0 then
        raise exception 'P14/10: 这批号被 % 条调拨明细引用 —— 应走调拨作废，不得直接软删，已中止', v_link;
    end if;

    -- ---------- 4) 整行快照（down 精确写回用） ----------
    insert into datafix_p14_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_material_serial', s.id, to_jsonb(s)
      from biz_material_serial s
     where s.source_type = 'PRODUCE'
       and s.source_id is null
       and s.create_by = v_user
       and s.created_at >= c_from
       and s.created_at <  c_to
    on conflict (batch, tbl, pk) do nothing;
    get diagnostics v_snapped = row_count;

    -- ---------- 5) 软删 ----------
    update biz_material_serial
       set deleted_at = now(),
           updated_at = now()
     where source_type = 'PRODUCE'
       and source_id is null
       and create_by = v_user
       and created_at >= c_from
       and created_at <  c_to
       and deleted_at is null;
    get diagnostics v_n = row_count;

    if v_n <> 10 then
        raise exception 'P14/10: 实际软删 % 行（预期 10）—— 整条语句已回滚，请人工核对', v_n;
    end if;

    raise notice 'P14/10: 软删 % 个序列号（SN-20261031-000011~000020），快照新增 % 行', v_n, v_snapped;
end
$$;
