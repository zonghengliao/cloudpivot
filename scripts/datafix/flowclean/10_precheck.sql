-- ============================================================================
-- FLOWCLEAN / 10：前置校验（硬闸）+ 快照/守卫表 + 基线
-- ----------------------------------------------------------------------------
-- 背景：生产库 12 条存活流程定义里，混着 6 条「测试/演示」流程。
--       本数据修复只做一件事：把这 6 条按 flow_code **精确匹配**清掉，保留 6 条真业务流。
--
--   删（6）:
--     acc_tasknode_141800  验收-TASK_NODE-141800   DRAFT     0 实例
--     acc_tn_141831        验收TASK节点-141831     PUBLISHED 1 实例
--     d5_281866            D5 校验发布演示          PUBLISHED 3 实例
--     d5_282996            D5 报销审批演示          DRAFT     0 实例
--     e2e_cs_084913        E2E会签测试              PUBLISHED 1 实例
--     p12_canvas_demo      P12 画布验收演示         PUBLISHED 0 实例
--   留（6，一个字节都不许动）:
--     task_accept / stock_transfer / stock_in_cancel / payable_pay / po_approve / stock_in_post
--
-- 本脚本只读业务表，只写两张 datafix_* 辅助表：
--   datafix_flowclean_snapshot  整行快照（down 精确写回用）
--   datafix_flowclean_guard     基线（保留流程指纹 / 历史孤儿数），verify 对账用
--
-- 闸门（任一不过 → RAISE EXCEPTION，整脚本回滚，什么都不动）：
--   ① 待清理流程必须正好 6 条（0 条 = 已清理过，幂等放行）
--   ② 保留的 6 条必须存在且 status='PUBLISHED'
--   ③ 存活流程里不许出现计划外的 flow_code（12 条全集之外的一律停）
--
-- 幂等：guard / snapshot 都是 `on conflict do nothing`，重复跑不会改写基线。
-- ============================================================================

-- 整行快照表：down 精确写回用（P10/P14 的教训 —— 只记 id 不够，要能还原成一模一样）
create table if not exists datafix_flowclean_snapshot (
    batch       text        not null,
    tbl         text        not null,
    pk          text        not null,
    row_data    jsonb       not null,
    captured_at timestamptz not null default now(),
    primary key (batch, tbl, pk)
);
comment on table datafix_flowclean_snapshot is
    'FLOWCLEAN 数据修复整行快照（batch=FLOWCLEAN-20261101）。down 脚本按此精确写回；确认无需回滚后整表 DROP。';

-- 基线表：保留流程的指纹 + 删除前的历史孤儿数（verify 对账用）
create table if not exists datafix_flowclean_guard (
    batch       text        not null,
    item        text        not null,
    val         text        not null,
    captured_at timestamptz not null default now(),
    primary key (batch, item)
);
comment on table datafix_flowclean_guard is
    'FLOWCLEAN 基线（batch=FLOWCLEAN-20261101）：保留流程行指纹 / 子表行数 / 删除前历史孤儿数。只增不改。';

do $$
declare
    c_batch  constant text   := 'FLOWCLEAN-20261101';
    c_del    constant text[] := array['acc_tasknode_141800','acc_tn_141831','d5_281866',
                                      'd5_282996','e2e_cs_084913','p12_canvas_demo'];
    c_keep   constant text[] := array['task_accept','stock_transfer','stock_in_cancel',
                                      'payable_pay','po_approve','stock_in_post'];
    v_del    int;
    v_keep   int;
    v_extra  text;
    v_n      int;
begin
    if to_regclass('public.biz_flow_def') is null then
        raise exception 'FLOWCLEAN/10: 当前库里没有 biz_flow_def —— 连错库了，停下';
    end if;

    -- ---------- 闸①：待清理必须 6 条（0 = 已清理，幂等放行）----------
    select count(*) into v_del
      from biz_flow_def
     where deleted_at is null and flow_code = any(c_del);

    if v_del = 0 then
        raise notice 'FLOWCLEAN/10: 6 个待清理流程已不存在 —— 幂等放行（本步不改任何数据）';
    elsif v_del <> 6 then
        raise exception 'FLOWCLEAN/10: 预期 6 个待清理流程，实际 % 个 —— 停下人工核对', v_del;
    end if;

    -- ---------- 闸②：保留的 6 条必须存在且 PUBLISHED ----------
    select count(*) into v_keep
      from biz_flow_def
     where deleted_at is null and status = 'PUBLISHED' and flow_code = any(c_keep);

    if v_keep <> 6 then
        raise exception 'FLOWCLEAN/10: 保留的 6 个业务流有缺失或未发布（实际 % 个）—— 停下', v_keep;
    end if;

    -- ---------- 闸③：存活流程只允许是「12 条全集」的子集 ----------
    select string_agg(flow_code, ',' order by flow_code) into v_extra
      from biz_flow_def
     where deleted_at is null
       and not (flow_code = any(c_del) or flow_code = any(c_keep));

    if v_extra is not null then
        raise exception 'FLOWCLEAN/10: 存活流程里出现计划外条目（%）—— 情况与卡面不符，停下人工核对', v_extra;
    end if;

    -- ---------- 基线 A：保留流程的行指纹（verify 必须逐字节一致）----------
    insert into datafix_flowclean_guard (batch, item, val)
    select c_batch, 'def_md5:' || d.flow_code, md5(to_jsonb(d)::text)
      from biz_flow_def d
     where d.deleted_at is null and d.flow_code = any(c_keep)
    on conflict (batch, item) do nothing;

    insert into datafix_flowclean_guard (batch, item, val)
    select c_batch, 'versions_md5', md5(coalesce(string_agg(to_jsonb(v)::text, '' order by v.id), ''))
      from biz_flow_def_version v
      join biz_flow_def d on d.id = v.def_id
     where d.deleted_at is null and d.flow_code = any(c_keep)
    on conflict (batch, item) do nothing;

    insert into datafix_flowclean_guard (batch, item, val)
    select c_batch, 'nodes_md5', md5(coalesce(string_agg(to_jsonb(n)::text, '' order by n.id), ''))
      from biz_flow_node n
      join biz_flow_def_version v on v.id = n.version_id
      join biz_flow_def d on d.id = v.def_id
     where d.deleted_at is null and d.flow_code = any(c_keep)
    on conflict (batch, item) do nothing;

    insert into datafix_flowclean_guard (batch, item, val)
    select c_batch, 'edges_md5', md5(coalesce(string_agg(to_jsonb(e)::text, '' order by e.id), ''))
      from biz_flow_edge e
      join biz_flow_def_version v on v.id = e.version_id
      join biz_flow_def d on d.id = v.def_id
     where d.deleted_at is null and d.flow_code = any(c_keep)
    on conflict (batch, item) do nothing;

    insert into datafix_flowclean_guard (batch, item, val)
    select c_batch, 'instances_md5', md5(coalesce(string_agg(to_jsonb(i)::text, '' order by i.id), ''))
      from biz_flow_instance i
     where i.flow_code = any(c_keep)
    on conflict (batch, item) do nothing;

    insert into datafix_flowclean_guard (batch, item, val)
    select c_batch, 'tasks_md5', md5(coalesce(string_agg(to_jsonb(t)::text, '' order by t.id), ''))
      from biz_flow_task t
      join biz_flow_instance i on i.id = t.instance_id
     where i.flow_code = any(c_keep)
    on conflict (batch, item) do nothing;

    -- ---------- 基线 B：子表行数（快照，便于人工核对）----------
    insert into datafix_flowclean_guard (batch, item, val)
    select c_batch, 'keep_defs', count(*)::text from biz_flow_def
     where deleted_at is null and flow_code = any(c_keep)
    on conflict (batch, item) do nothing;

    -- ---------- 基线 C：删除前的历史孤儿（不在本卡范围，verify 只要求「别变多」）----------
    insert into datafix_flowclean_guard (batch, item, val)
    select c_batch, 'preexisting_orphan_instances', count(*)::text
      from biz_flow_instance i
     where i.deleted_at is null
       and not exists (select 1 from biz_flow_def d
                        where d.flow_code = i.flow_code and d.deleted_at is null)
    on conflict (batch, item) do nothing;

    insert into datafix_flowclean_guard (batch, item, val)
    select c_batch, 'preexisting_orphan_pending_tasks', count(*)::text
      from biz_flow_task ft
     where ft.status = 'PENDING'
       and not exists (select 1 from biz_flow_instance i
                         join biz_flow_def d on d.flow_code = i.flow_code and d.deleted_at is null
                        where i.id = ft.instance_id)
    on conflict (batch, item) do nothing;

    select count(*) into v_n from datafix_flowclean_guard where batch = c_batch;
    raise notice 'FLOWCLEAN/10: 前置校验通过（待清理 % 条 / 保留 % 条），基线 % 项已就位',
                 v_del, v_keep, v_n;
end
$$;

-- 打印基线，人工留档
select item, val from datafix_flowclean_guard where batch = 'FLOWCLEAN-20261101' order by item;
