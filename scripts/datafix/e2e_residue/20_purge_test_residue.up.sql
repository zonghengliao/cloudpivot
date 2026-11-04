-- ============================================================================
-- E2ERES / 20（up）：清掉生产库里的 e2e 测试残留
-- ----------------------------------------------------------------------------
-- 范围（**唯一判据**，与 biz_type_domain.py::is_test_residue 逐字一致）：
--     biz_type 以 'E2E_' 开头  OR  biz_type = 'D2_SMOKE'
--
--   ⚠ 判据不是「不在内置枚举里」—— biz_type 允许客户自定义，那样会把真流程删掉。
--     本脚本一个 LIKE 'E2E%' 的模糊匹配都不写：只认前缀/等值这两种精确形式。
--
-- 顺序（先子后父，与 FK 方向一致）：
--   biz_flow_task → biz_flow_cc → biz_flow_instance_node → biz_flow_instance
--   → biz_flow_edge → biz_flow_node_assignee → biz_flow_node
--   → biz_flow_def_version → biz_flow_def
--
--   （biz_flow_instance.version_id → biz_flow_def_version 是 RESTRICT，所以实例必须先走；
--     biz_flow_def.current_version_id 没有外键，版本删完再删定义即可）
--
-- biz_status_log：**一行都不删**。它是审计证据表，且实测命中测试残留口径 0 行；
--   本脚本只在结尾断言「行数与基线一致」（防止误删/连带删除）。
--
-- 硬校验：每张表受影响行数必须 == datafix_e2e_residue_guard 里的 residue_* 基线，
--         任一条不符 → RAISE EXCEPTION，整个事务回滚，一行都不动。
-- 幂等：残留已是 0 行 → NOTICE 跳过；半截残留（例如只删了实例）→ 报错停，不硬上。
-- 回滚：bash apply.sh --rollback（20 的 down 按快照整行写回）。
-- ============================================================================

\set ON_ERROR_STOP on

do $$
declare
    c_batch constant text := 'E2ERES-20261104';
    v_defs int; v_vers int; v_nodes int; v_edges int; v_asg int;
    v_inst int; v_inode int; v_tasks int; v_cc int;
    v_exp_defs int; v_exp_vers int; v_exp_nodes int; v_exp_edges int; v_exp_asg int;
    v_exp_inst int; v_exp_inode int; v_exp_tasks int; v_exp_cc int;
    v_slog_before int; v_slog_exp int;
    v_n int;
    v_def_ids uuid[]; v_ver_ids uuid[]; v_node_ids uuid[]; v_inst_ids uuid[]; v_inode_ids uuid[];
begin
    ------------------------------------------------------------------ 基线
    if to_regclass('public.datafix_e2e_residue_guard') is null then
        raise exception 'E2ERES/20 up: 没有 datafix_e2e_residue_guard —— 先跑 10_precheck.sql';
    end if;
    select val::int into v_exp_defs  from datafix_e2e_residue_guard where batch=c_batch and item='residue_biz_flow_def';
    select val::int into v_exp_vers  from datafix_e2e_residue_guard where batch=c_batch and item='residue_biz_flow_def_version';
    select val::int into v_exp_nodes from datafix_e2e_residue_guard where batch=c_batch and item='residue_biz_flow_node';
    select val::int into v_exp_edges from datafix_e2e_residue_guard where batch=c_batch and item='residue_biz_flow_edge';
    select val::int into v_exp_asg   from datafix_e2e_residue_guard where batch=c_batch and item='residue_biz_flow_node_assignee';
    select val::int into v_exp_inst  from datafix_e2e_residue_guard where batch=c_batch and item='residue_biz_flow_instance';
    select val::int into v_exp_inode from datafix_e2e_residue_guard where batch=c_batch and item='residue_biz_flow_instance_node';
    select val::int into v_exp_tasks from datafix_e2e_residue_guard where batch=c_batch and item='residue_biz_flow_task';
    select val::int into v_exp_cc    from datafix_e2e_residue_guard where batch=c_batch and item='residue_biz_flow_cc';
    select val::int into v_slog_exp  from datafix_e2e_residue_guard where batch=c_batch and item='biz_status_log_total';

    if v_exp_defs is null or v_exp_inst is null then
        raise exception 'E2ERES/20 up: 基线缺失（10_precheck.sql 没跑或跑失败）';
    end if;

    ------------------------------------------------------------------ 现算 id 集
    select coalesce(array_agg(id), '{}') into v_def_ids
      from biz_flow_def where left(biz_type,4) = 'E2E_' or biz_type = 'D2_SMOKE';
    select coalesce(array_agg(id), '{}') into v_ver_ids
      from biz_flow_def_version where def_id = any(v_def_ids);
    select coalesce(array_agg(id), '{}') into v_node_ids
      from biz_flow_node where version_id = any(v_ver_ids);
    select coalesce(array_agg(id), '{}') into v_inst_ids
      from biz_flow_instance where left(biz_type,4) = 'E2E_' or biz_type = 'D2_SMOKE';
    select coalesce(array_agg(id), '{}') into v_inode_ids
      from biz_flow_instance_node where instance_id = any(v_inst_ids);

    select count(*) into v_vers  from biz_flow_def_version where def_id = any(v_def_ids);
    select count(*) into v_nodes from biz_flow_node        where version_id = any(v_ver_ids);
    select count(*) into v_edges from biz_flow_edge        where version_id = any(v_ver_ids);
    select count(*) into v_asg   from biz_flow_node_assignee where node_id = any(v_node_ids);
    select count(*) into v_inode from biz_flow_instance_node where instance_id = any(v_inst_ids);
    select count(*) into v_tasks from biz_flow_task        where instance_id = any(v_inst_ids);
    select count(*) into v_cc    from biz_flow_cc          where instance_id = any(v_inst_ids);
    v_defs := coalesce(array_length(v_def_ids, 1), 0);
    v_inst := coalesce(array_length(v_inst_ids, 1), 0);

    -- 幂等：上次已经删干净了 → 直接退出（必须在「与基线比对」之前判断，
    -- 否则第二次跑会拿「实测 0 行 vs 基线 85 行」报错）
    if (v_defs, v_vers, v_nodes, v_edges, v_asg, v_inst, v_inode, v_tasks, v_cc)
       = (0, 0, 0, 0, 0, 0, 0, 0, 0) then
        raise notice 'E2ERES/20 up: 测试残留已是 0 行，无需清理（幂等）';
        return;
    end if;

    if (v_defs, v_vers, v_nodes, v_edges, v_asg, v_inst, v_inode, v_tasks, v_cc)
       is distinct from (v_exp_defs, v_exp_vers, v_exp_nodes, v_exp_edges, v_exp_asg,
                         v_exp_inst, v_exp_inode, v_exp_tasks, v_exp_cc) then
        raise exception
            'E2ERES/20 up: 现状与基线不符（有人动过数据或上次删了一半），停。%'
            ' 实测 def=% ver=% node=% edge=% asg=% inst=% inode=% task=% cc=% / 基线 % % % % % % % % %',
            E'\n', v_defs, v_vers, v_nodes, v_edges, v_asg, v_inst, v_inode, v_tasks, v_cc,
            v_exp_defs, v_exp_vers, v_exp_nodes, v_exp_edges, v_exp_asg,
            v_exp_inst, v_exp_inode, v_exp_tasks, v_exp_cc;
    end if;

    -- 在飞审批绝不删（10 已拦过，这里再拦一次：10 与 20 之间可能有人发起）
    select count(*) into v_n from biz_flow_instance
     where id = any(v_inst_ids) and status = 'PENDING';
    if v_n > 0 then
        raise exception 'E2ERES/20 up: 有 % 条残留实例处于 PENDING（10 之后有人发起了），停', v_n;
    end if;

    ------------------------------------------------------------------ 快照（整行）
    insert into datafix_e2e_residue_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_def', t.id::text, to_jsonb(t) from biz_flow_def t where t.id = any(v_def_ids)
    on conflict do nothing;
    insert into datafix_e2e_residue_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_def_version', t.id::text, to_jsonb(t) from biz_flow_def_version t where t.id = any(v_ver_ids)
    on conflict do nothing;
    insert into datafix_e2e_residue_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_node', t.id::text, to_jsonb(t) from biz_flow_node t where t.id = any(v_node_ids)
    on conflict do nothing;
    insert into datafix_e2e_residue_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_node_assignee',
           t.node_id::text||'|'||t.assignee_type||'|'||t.ref_id::text, to_jsonb(t)
      from biz_flow_node_assignee t where t.node_id = any(v_node_ids)
    on conflict do nothing;
    insert into datafix_e2e_residue_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_edge', t.id::text, to_jsonb(t) from biz_flow_edge t where t.version_id = any(v_ver_ids)
    on conflict do nothing;
    insert into datafix_e2e_residue_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_instance', t.id::text, to_jsonb(t) from biz_flow_instance t where t.id = any(v_inst_ids)
    on conflict do nothing;
    insert into datafix_e2e_residue_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_instance_node', t.id::text, to_jsonb(t) from biz_flow_instance_node t where t.id = any(v_inode_ids)
    on conflict do nothing;
    insert into datafix_e2e_residue_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_task', t.id::text, to_jsonb(t) from biz_flow_task t where t.instance_id = any(v_inst_ids)
    on conflict do nothing;
    insert into datafix_e2e_residue_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_cc', t.id::text, to_jsonb(t) from biz_flow_cc t where t.instance_id = any(v_inst_ids)
    on conflict do nothing;

    select count(*) into v_n from datafix_e2e_residue_snapshot where batch = c_batch;
    raise notice 'E2ERES/20 up: 快照 % 行（回滚底气）', v_n;
    if v_n <> (v_defs+v_vers+v_nodes+v_asg+v_edges+v_inst+v_inode+v_tasks+v_cc) then
        raise exception 'E2ERES/20 up: 快照行数 % 与命中总数 % 不符 —— 快照不全，不许删',
            v_n, (v_defs+v_vers+v_nodes+v_asg+v_edges+v_inst+v_inode+v_tasks+v_cc);
    end if;

    ------------------------------------------------------------------ 删除（先子后父）
    -- ① 待办
    delete from biz_flow_task where instance_id = any(v_inst_ids);
    get diagnostics v_n = row_count;
    if v_n <> v_tasks then raise exception 'E2ERES/20 ①: biz_flow_task 删了 % 行，期望 %', v_n, v_tasks; end if;
    insert into datafix_e2e_residue_guard (batch,item,val) values (c_batch,'deleted_biz_flow_task',v_n::text)
      on conflict (batch,item) do update set val = excluded.val;

    -- ② 抄送
    delete from biz_flow_cc where instance_id = any(v_inst_ids);
    get diagnostics v_n = row_count;
    if v_n <> v_cc then raise exception 'E2ERES/20 ②: biz_flow_cc 删了 % 行，期望 %', v_n, v_cc; end if;
    insert into datafix_e2e_residue_guard (batch,item,val) values (c_batch,'deleted_biz_flow_cc',v_n::text)
      on conflict (batch,item) do update set val = excluded.val;

    -- ③ 实例节点
    delete from biz_flow_instance_node where instance_id = any(v_inst_ids);
    get diagnostics v_n = row_count;
    if v_n <> v_inode then raise exception 'E2ERES/20 ③: biz_flow_instance_node 删了 % 行，期望 %', v_n, v_inode; end if;
    insert into datafix_e2e_residue_guard (batch,item,val) values (c_batch,'deleted_biz_flow_instance_node',v_n::text)
      on conflict (batch,item) do update set val = excluded.val;

    -- ④ 实例
    delete from biz_flow_instance where id = any(v_inst_ids);
    get diagnostics v_n = row_count;
    if v_n <> v_inst then raise exception 'E2ERES/20 ④: biz_flow_instance 删了 % 行，期望 %', v_n, v_inst; end if;
    insert into datafix_e2e_residue_guard (batch,item,val) values (c_batch,'deleted_biz_flow_instance',v_n::text)
      on conflict (batch,item) do update set val = excluded.val;

    -- ⑤ 边
    delete from biz_flow_edge where version_id = any(v_ver_ids);
    get diagnostics v_n = row_count;
    if v_n <> v_edges then raise exception 'E2ERES/20 ⑤: biz_flow_edge 删了 % 行，期望 %', v_n, v_edges; end if;
    insert into datafix_e2e_residue_guard (batch,item,val) values (c_batch,'deleted_biz_flow_edge',v_n::text)
      on conflict (batch,item) do update set val = excluded.val;

    -- ⑥ 节点审批人
    delete from biz_flow_node_assignee where node_id = any(v_node_ids);
    get diagnostics v_n = row_count;
    if v_n <> v_asg then raise exception 'E2ERES/20 ⑥: biz_flow_node_assignee 删了 % 行，期望 %', v_n, v_asg; end if;
    insert into datafix_e2e_residue_guard (batch,item,val) values (c_batch,'deleted_biz_flow_node_assignee',v_n::text)
      on conflict (batch,item) do update set val = excluded.val;

    -- ⑦ 节点
    delete from biz_flow_node where id = any(v_node_ids);
    get diagnostics v_n = row_count;
    if v_n <> v_nodes then raise exception 'E2ERES/20 ⑦: biz_flow_node 删了 % 行，期望 %', v_n, v_nodes; end if;
    insert into datafix_e2e_residue_guard (batch,item,val) values (c_batch,'deleted_biz_flow_node',v_n::text)
      on conflict (batch,item) do update set val = excluded.val;

    -- ⑧ 版本
    delete from biz_flow_def_version where id = any(v_ver_ids);
    get diagnostics v_n = row_count;
    if v_n <> v_vers then raise exception 'E2ERES/20 ⑧: biz_flow_def_version 删了 % 行，期望 %', v_n, v_vers; end if;
    insert into datafix_e2e_residue_guard (batch,item,val) values (c_batch,'deleted_biz_flow_def_version',v_n::text)
      on conflict (batch,item) do update set val = excluded.val;

    -- ⑨ 定义
    delete from biz_flow_def where id = any(v_def_ids);
    get diagnostics v_n = row_count;
    if v_n <> v_defs then raise exception 'E2ERES/20 ⑨: biz_flow_def 删了 % 行，期望 %', v_n, v_defs; end if;
    insert into datafix_e2e_residue_guard (batch,item,val) values (c_batch,'deleted_biz_flow_def',v_n::text)
      on conflict (batch,item) do update set val = excluded.val;

    ------------------------------------------------------------------ 审计表：一行都没动
    select count(*) into v_slog_before from biz_status_log;
    if v_slog_before <> v_slog_exp then
        raise exception 'E2ERES/20: biz_status_log 行数 % != 基线 % —— 审计表被动过，回滚排查', v_slog_before, v_slog_exp;
    end if;

    raise notice 'E2ERES/20 up 完成：def=% ver=% node=% edge=% asg=% inst=% inode=% task=% cc=%；biz_status_log 未动（% 行）',
        v_defs, v_vers, v_nodes, v_edges, v_asg, v_inst, v_inode, v_tasks, v_cc, v_slog_before;
end $$;

-- 人眼可读的删除结果
select item, val from datafix_e2e_residue_guard
 where batch = 'E2ERES-20261104' and item like 'deleted_%' order by item;
