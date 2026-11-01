-- ============================================================================
-- FLOWCLEAN / 20（down）：把 6 个流程按快照原样恢复
-- ----------------------------------------------------------------------------
-- 数据来源：datafix_flowclean_snapshot（20_*.up.sql 在删除前用 to_jsonb(row) 存了整行）
--   ⇒ 不是「按 id 重新插一遍」，而是把**删除前那一刻的整行**原样写回
--     （含 created_at / updated_at / create_by / graph_snapshot / graph_hash 等所有列）。
--
-- 顺序（先父后子，与 FK 方向相反）：def → version → node → node_assignee → edge
--   → instance → instance_node → task → cc → 回填 biz_task.flow_instance_id
--
-- 幂等：每张表都是 `on conflict (...) do nothing`，第二遍 0 行。
-- 安全阀：快照表缺失或本批次 0 行 → 直接报错（说明 up 没跑过）。
-- 覆盖写入的列：biz_task.flow_instance_id 只在当前为 NULL 时写回快照值（不覆盖别人后填的值）。
-- ============================================================================

do $$
declare
    c_batch constant text := 'FLOWCLEAN-20261101';
    v_total int;
    v_n     int;
    v_sum   int := 0;
begin
    if to_regclass('public.datafix_flowclean_snapshot') is null then
        raise exception 'FLOWCLEAN/20 down: 没有 datafix_flowclean_snapshot 表 —— up 没跑过，无从回滚';
    end if;

    select count(*) into v_total from datafix_flowclean_snapshot where batch = c_batch;
    if v_total = 0 then
        raise exception 'FLOWCLEAN/20 down: 批次 % 没有快照行 —— up 没跑过，无从回滚', c_batch;
    end if;

    -- ---------- ① 定义 ----------
    insert into biz_flow_def
    select r.* from datafix_flowclean_snapshot e,
           lateral jsonb_populate_record(null::biz_flow_def, e.row_data) r
     where e.batch = c_batch and e.tbl = 'biz_flow_def'
    on conflict (id) do nothing;
    get diagnostics v_n = row_count; v_sum := v_sum + v_n;
    raise notice 'FLOWCLEAN/20 down: biz_flow_def 写回 % 行', v_n;

    -- ---------- ② 版本 ----------
    insert into biz_flow_def_version
    select r.* from datafix_flowclean_snapshot e,
           lateral jsonb_populate_record(null::biz_flow_def_version, e.row_data) r
     where e.batch = c_batch and e.tbl = 'biz_flow_def_version'
    on conflict (id) do nothing;
    get diagnostics v_n = row_count; v_sum := v_sum + v_n;
    raise notice 'FLOWCLEAN/20 down: biz_flow_def_version 写回 % 行', v_n;

    -- ---------- ③ 节点 ----------
    insert into biz_flow_node
    select r.* from datafix_flowclean_snapshot e,
           lateral jsonb_populate_record(null::biz_flow_node, e.row_data) r
     where e.batch = c_batch and e.tbl = 'biz_flow_node'
    on conflict (id) do nothing;
    get diagnostics v_n = row_count; v_sum := v_sum + v_n;
    raise notice 'FLOWCLEAN/20 down: biz_flow_node 写回 % 行', v_n;

    -- ---------- ④ 节点审批人（主键是 node_id+assignee_type+ref_id，没有 id 列）----------
    insert into biz_flow_node_assignee
    select r.* from datafix_flowclean_snapshot e,
           lateral jsonb_populate_record(null::biz_flow_node_assignee, e.row_data) r
     where e.batch = c_batch and e.tbl = 'biz_flow_node_assignee'
    on conflict (node_id, assignee_type, ref_id) do nothing;
    get diagnostics v_n = row_count; v_sum := v_sum + v_n;
    raise notice 'FLOWCLEAN/20 down: biz_flow_node_assignee 写回 % 行', v_n;

    -- ---------- ⑤ 边 ----------
    insert into biz_flow_edge
    select r.* from datafix_flowclean_snapshot e,
           lateral jsonb_populate_record(null::biz_flow_edge, e.row_data) r
     where e.batch = c_batch and e.tbl = 'biz_flow_edge'
    on conflict (id) do nothing;
    get diagnostics v_n = row_count; v_sum := v_sum + v_n;
    raise notice 'FLOWCLEAN/20 down: biz_flow_edge 写回 % 行', v_n;

    -- ---------- ⑥ 实例 ----------
    insert into biz_flow_instance
    select r.* from datafix_flowclean_snapshot e,
           lateral jsonb_populate_record(null::biz_flow_instance, e.row_data) r
     where e.batch = c_batch and e.tbl = 'biz_flow_instance'
    on conflict (id) do nothing;
    get diagnostics v_n = row_count; v_sum := v_sum + v_n;
    raise notice 'FLOWCLEAN/20 down: biz_flow_instance 写回 % 行', v_n;

    -- ---------- ⑦ 实例节点 ----------
    insert into biz_flow_instance_node
    select r.* from datafix_flowclean_snapshot e,
           lateral jsonb_populate_record(null::biz_flow_instance_node, e.row_data) r
     where e.batch = c_batch and e.tbl = 'biz_flow_instance_node'
    on conflict (id) do nothing;
    get diagnostics v_n = row_count; v_sum := v_sum + v_n;
    raise notice 'FLOWCLEAN/20 down: biz_flow_instance_node 写回 % 行', v_n;

    -- ---------- ⑧ 待办 ----------
    insert into biz_flow_task
    select r.* from datafix_flowclean_snapshot e,
           lateral jsonb_populate_record(null::biz_flow_task, e.row_data) r
     where e.batch = c_batch and e.tbl = 'biz_flow_task'
    on conflict (id) do nothing;
    get diagnostics v_n = row_count; v_sum := v_sum + v_n;
    raise notice 'FLOWCLEAN/20 down: biz_flow_task 写回 % 行', v_n;

    -- ---------- ⑨ 抄送 ----------
    insert into biz_flow_cc
    select r.* from datafix_flowclean_snapshot e,
           lateral jsonb_populate_record(null::biz_flow_cc, e.row_data) r
     where e.batch = c_batch and e.tbl = 'biz_flow_cc'
    on conflict (id) do nothing;
    get diagnostics v_n = row_count; v_sum := v_sum + v_n;
    raise notice 'FLOWCLEAN/20 down: biz_flow_cc 写回 % 行', v_n;

    -- ---------- ⑩ 外部引用回填（biz_task 的 flow 三元组 + source）----------
    -- up 里为了绕过 ck_biz_task_flow_triplet / ck_biz_task_source_flow 把四列一起改了，
    -- 这里按快照原样写回；只在 flow_instance_id 仍为空（= 没被别人重新挂流程）时才动。
    update biz_task t
       set flow_instance_id = (e.row_data->>'flow_instance_id')::uuid,
           flow_node_key    = e.row_data->>'flow_node_key',
           flow_round       = (e.row_data->>'flow_round')::int,
           source           = e.row_data->>'source'
      from datafix_flowclean_snapshot e
     where e.batch = c_batch
       and e.tbl = 'biz_task_flow_ref'
       and e.pk = t.id::text
       and t.flow_instance_id is null;
    get diagnostics v_n = row_count; v_sum := v_sum + v_n;
    raise notice 'FLOWCLEAN/20 down: biz_task 的 flow 三元组 / source 回填 % 行', v_n;

    -- ---------- 收口核对 ----------
    select count(*) into v_total
      from biz_flow_def where deleted_at is null and flow_code = any(
           array['acc_tasknode_141800','acc_tn_141831','d5_281866',
                 'd5_282996','e2e_cs_084913','p12_canvas_demo']);
    if v_total <> 6 then
        raise exception 'FLOWCLEAN/20 down: 恢复后 6 个流程只回来 % 条 —— 回滚不完整，人工核对快照表', v_total;
    end if;

    raise notice 'FLOWCLEAN/20 down: 回滚完成，写回 % 行（6 个流程已全部复原）', v_sum;
end
$$;
