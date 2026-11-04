-- ============================================================================
-- E2ERES / 20（down）：按快照把删掉的测试残留整行写回
-- ----------------------------------------------------------------------------
-- 数据来源：datafix_e2e_residue_snapshot（up 在删除前用 to_jsonb(row) 存了整行）
--   ⇒ 不是「按 id 重新造一遍」，而是把**删除前那一刻的整行**原样写回
--     （含 created_at / updated_at / create_by / graph_snapshot / graph_hash / status 等所有列）。
--
-- 顺序（先父后子，与 FK 方向相反）：
--   def → version → node → node_assignee → edge → instance → instance_node → task → cc
--
-- 幂等：每张表 on conflict do nothing，第二遍 0 行。
-- 安全阀：快照表缺失 / 本批次 0 行 → 直接报错（说明 up 没跑过，无从回滚）。
-- 注意：biz_status_log 全程没删，回滚不需要动它（脚本会断言行数没变）。
-- ============================================================================

\set ON_ERROR_STOP on

do $$
declare
    c_batch constant text := 'E2ERES-20261104';
    v_total int;
    v_n int;
    v_sum int := 0;
    v_slog int;
    v_slog_exp int;
begin
    if to_regclass('public.datafix_e2e_residue_snapshot') is null then
        raise exception 'E2ERES/20 down: 没有 datafix_e2e_residue_snapshot 表 —— up 没跑过，无从回滚';
    end if;
    select count(*) into v_total from datafix_e2e_residue_snapshot where batch = c_batch;
    if v_total = 0 then
        raise exception 'E2ERES/20 down: 批次 % 没有快照行 —— up 没跑过，无从回滚', c_batch;
    end if;

    -- ① 定义
    insert into biz_flow_def
    select r.* from datafix_e2e_residue_snapshot e,
           lateral jsonb_populate_record(null::biz_flow_def, e.row_data) r
     where e.batch = c_batch and e.tbl = 'biz_flow_def'
    on conflict (id) do nothing;
    get diagnostics v_n = row_count; v_sum := v_sum + v_n;
    raise notice 'E2ERES/20 down ①: biz_flow_def 写回 % 行', v_n;

    -- ② 版本
    insert into biz_flow_def_version
    select r.* from datafix_e2e_residue_snapshot e,
           lateral jsonb_populate_record(null::biz_flow_def_version, e.row_data) r
     where e.batch = c_batch and e.tbl = 'biz_flow_def_version'
    on conflict (id) do nothing;
    get diagnostics v_n = row_count; v_sum := v_sum + v_n;
    raise notice 'E2ERES/20 down ②: biz_flow_def_version 写回 % 行', v_n;

    -- ③ 节点
    insert into biz_flow_node
    select r.* from datafix_e2e_residue_snapshot e,
           lateral jsonb_populate_record(null::biz_flow_node, e.row_data) r
     where e.batch = c_batch and e.tbl = 'biz_flow_node'
    on conflict (id) do nothing;
    get diagnostics v_n = row_count; v_sum := v_sum + v_n;
    raise notice 'E2ERES/20 down ③: biz_flow_node 写回 % 行', v_n;

    -- ④ 节点审批人（主键 node_id + assignee_type + ref_id，没有 id 列）
    insert into biz_flow_node_assignee
    select r.* from datafix_e2e_residue_snapshot e,
           lateral jsonb_populate_record(null::biz_flow_node_assignee, e.row_data) r
     where e.batch = c_batch and e.tbl = 'biz_flow_node_assignee'
    on conflict (node_id, assignee_type, ref_id) do nothing;
    get diagnostics v_n = row_count; v_sum := v_sum + v_n;
    raise notice 'E2ERES/20 down ④: biz_flow_node_assignee 写回 % 行', v_n;

    -- ⑤ 边
    insert into biz_flow_edge
    select r.* from datafix_e2e_residue_snapshot e,
           lateral jsonb_populate_record(null::biz_flow_edge, e.row_data) r
     where e.batch = c_batch and e.tbl = 'biz_flow_edge'
    on conflict (id) do nothing;
    get diagnostics v_n = row_count; v_sum := v_sum + v_n;
    raise notice 'E2ERES/20 down ⑤: biz_flow_edge 写回 % 行', v_n;

    -- ⑥ 实例
    insert into biz_flow_instance
    select r.* from datafix_e2e_residue_snapshot e,
           lateral jsonb_populate_record(null::biz_flow_instance, e.row_data) r
     where e.batch = c_batch and e.tbl = 'biz_flow_instance'
    on conflict (id) do nothing;
    get diagnostics v_n = row_count; v_sum := v_sum + v_n;
    raise notice 'E2ERES/20 down ⑥: biz_flow_instance 写回 % 行', v_n;

    -- ⑦ 实例节点
    insert into biz_flow_instance_node
    select r.* from datafix_e2e_residue_snapshot e,
           lateral jsonb_populate_record(null::biz_flow_instance_node, e.row_data) r
     where e.batch = c_batch and e.tbl = 'biz_flow_instance_node'
    on conflict (id) do nothing;
    get diagnostics v_n = row_count; v_sum := v_sum + v_n;
    raise notice 'E2ERES/20 down ⑦: biz_flow_instance_node 写回 % 行', v_n;

    -- ⑧ 待办
    insert into biz_flow_task
    select r.* from datafix_e2e_residue_snapshot e,
           lateral jsonb_populate_record(null::biz_flow_task, e.row_data) r
     where e.batch = c_batch and e.tbl = 'biz_flow_task'
    on conflict (id) do nothing;
    get diagnostics v_n = row_count; v_sum := v_sum + v_n;
    raise notice 'E2ERES/20 down ⑧: biz_flow_task 写回 % 行', v_n;

    -- ⑨ 抄送
    insert into biz_flow_cc
    select r.* from datafix_e2e_residue_snapshot e,
           lateral jsonb_populate_record(null::biz_flow_cc, e.row_data) r
     where e.batch = c_batch and e.tbl = 'biz_flow_cc'
    on conflict (id) do nothing;
    get diagnostics v_n = row_count; v_sum := v_sum + v_n;
    raise notice 'E2ERES/20 down ⑨: biz_flow_cc 写回 % 行', v_n;

    -- ⑩ 审计表没被碰过（本卡全程不删 biz_status_log）
    select val::int into v_slog_exp from datafix_e2e_residue_guard
     where batch = c_batch and item = 'biz_status_log_total';
    select count(*) into v_slog from biz_status_log;
    if v_slog_exp is not null and v_slog <> v_slog_exp then
        raise exception 'E2ERES/20 down: biz_status_log 行数 % != 基线 %（不是本脚本造成的，人工核对）', v_slog, v_slog_exp;
    end if;

    raise notice 'E2ERES/20 down 完成：写回 % 行，快照共 % 行（幂等：重复执行写回 0 行）', v_sum, v_total;
end $$;

select item, val from datafix_e2e_residue_guard
 where batch = 'E2ERES-20261104' and item like 'deleted_%' order by item;
