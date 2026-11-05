-- ============================================================================
-- E2ERES2 / 20（up）：清掉「36 条软删测试定义 + 16 条孤儿实例」
-- ----------------------------------------------------------------------------
-- 删的是 10_precheck.sql 里**钉住**的 id 集（datafix_e2eres2_target, batch=E2ERES2-20261104），
-- 不重算判据 —— 这样 precheck 之后新冒出来的 e2e_* 定义不会被顺带删掉（只会让 30 报 WARN）。
--
-- 判据（单一来源 = 视图 datafix_e2eres2_v_target_def / _inst）：
--   SET A-1 def  ：deleted_at is not null
--                  and not (left(biz_type,4)='E2E_' or biz_type='D2_SMOKE')   -- 上一张卡的口径，本卡不碰
--                  and flow_code ~ '^(e2e|diag|diag2|vrf|d5|acc|tn)_'
--   SET A-2 inst ：deleted_at is null and def_id in (SET A-1)                 -- 孤儿实例
--
--   ⚠ 前缀只是「候选」，能删是因为逐条判过性质（create_by / flow_name / created_at /
--     11 处业务单据引用全 0）。判决表见 docs/evidence/20261104-e2eres2/README.md。
--
-- 顺序（先子后父，与 FK 方向一致）：
--   biz_flow_task → biz_flow_cc → biz_flow_instance_node → biz_flow_instance
--   → biz_flow_edge → biz_flow_node_assignee → biz_flow_node
--   → biz_flow_def_version → biz_flow_def
--
--   外键现状（pg_constraint 实测）：
--     version.def_id → def            CASCADE   （版本先删，行数才对得上）
--     edge.version_id → version        CASCADE
--     node.version_id → version        CASCADE
--     node_assignee.node_id → node     CASCADE
--     instance.def_id → def            RESTRICT  ⇒ 实例必须先走
--     instance.version_id → version    RESTRICT  ⇒ 实例必须先走
--     instance_node.instance_id → instance  CASCADE
--     task.instance_id → instance           CASCADE
--     task.instance_node_id → instance_node CASCADE（task 先删）
--     instance_node.node_id → node          SET NULL（实例节点先删）
--     biz_task.flow_instance_id → instance  SET NULL ⇒ 闸⑥ 已断言 0 条
--
-- biz_status_log：**一行都不删**（审计证据表）。脚本结尾断言行数没变、pin 前那段指纹没变。
--
-- 硬校验：每张表受影响行数必须 == datafix_e2eres2_guard 的 target_* 基线，
--         任一条不符 → RAISE EXCEPTION，整个事务回滚，一行都不动。
-- 幂等：钉住的目标行已是 0 → NOTICE 跳过；半截残留（只删了实例）→ 报错停，不硬上。
-- 回滚：bash apply.sh --rollback（20 的 down 按快照整行写回）。
-- ============================================================================

\set ON_ERROR_STOP on

do $$
declare
    c_batch constant text := 'E2ERES2-20261104';
    v_defs int; v_vers int; v_nodes int; v_edges int; v_asg int;
    v_inst int; v_inode int; v_tasks int; v_cc int;
    v_exp_defs int; v_exp_vers int; v_exp_nodes int; v_exp_edges int; v_exp_asg int;
    v_exp_inst int; v_exp_inode int; v_exp_tasks int; v_exp_cc int;
    v_slog_before int; v_slog_exp int; v_slog_fp_before text; v_slog_fp_exp text;
    v_pin_ts timestamptz;
    v_n int;
    v_def_ids uuid[]; v_ver_ids uuid[]; v_node_ids uuid[]; v_inst_ids uuid[]; v_inode_ids uuid[];
begin
    ------------------------------------------------------------------ 基线
    if to_regclass('public.datafix_e2eres2_guard') is null
       or to_regclass('public.datafix_e2eres2_target') is null then
        raise exception 'E2ERES2/20 up: 没有 datafix_e2eres2_guard / _target —— 先跑 10_precheck.sql';
    end if;
    select val::int into v_exp_defs  from datafix_e2eres2_guard where batch=c_batch and item='target_biz_flow_def';
    select val::int into v_exp_vers  from datafix_e2eres2_guard where batch=c_batch and item='target_biz_flow_def_version';
    select val::int into v_exp_nodes from datafix_e2eres2_guard where batch=c_batch and item='target_biz_flow_node';
    select val::int into v_exp_edges from datafix_e2eres2_guard where batch=c_batch and item='target_biz_flow_edge';
    select val::int into v_exp_asg   from datafix_e2eres2_guard where batch=c_batch and item='target_biz_flow_node_assignee';
    select val::int into v_exp_inst  from datafix_e2eres2_guard where batch=c_batch and item='target_biz_flow_instance';
    select val::int into v_exp_inode from datafix_e2eres2_guard where batch=c_batch and item='target_biz_flow_instance_node';
    select val::int into v_exp_tasks from datafix_e2eres2_guard where batch=c_batch and item='target_biz_flow_task';
    select val::int into v_exp_cc    from datafix_e2eres2_guard where batch=c_batch and item='target_biz_flow_cc';
    select val::int into v_slog_exp  from datafix_e2eres2_guard where batch=c_batch and item='biz_status_log_total';
    select val     into v_slog_fp_exp from datafix_e2eres2_guard where batch=c_batch and item='biz_status_log_fingerprint';
    select val::timestamptz into v_pin_ts from datafix_e2eres2_guard where batch=c_batch and item='biz_status_log_pin_ts';

    if v_exp_defs is null or v_exp_inst is null then
        raise exception 'E2ERES2/20 up: 基线缺失（10_precheck.sql 没跑或跑失败）';
    end if;

    ------------------------------------------------------------------ 钉住的 id 集
    select coalesce(array_agg(id), '{}') into v_def_ids
      from datafix_e2eres2_target where batch=c_batch and kind='def';
    select coalesce(array_agg(id), '{}') into v_inst_ids
      from datafix_e2eres2_target where batch=c_batch and kind='inst';

    if coalesce(array_length(v_def_ids,1),0) = 0 and coalesce(array_length(v_inst_ids,1),0) = 0 then
        -- 首次 precheck 就是 clean 状态 ⇒ 没钉表 ⇒ 无事可做
        raise notice 'E2ERES2/20 up: 没有钉住的目标集（首次 precheck 时已是 0 行），无需清理（幂等）';
        return;
    end if;

    ------------------------------------------------------------------ 现算子行（只按钉住的 id 走外键）
    select coalesce(array_agg(id), '{}') into v_ver_ids
      from biz_flow_def_version where def_id = any(v_def_ids);
    select coalesce(array_agg(id), '{}') into v_node_ids
      from biz_flow_node where version_id = any(v_ver_ids);
    select coalesce(array_agg(id), '{}') into v_inode_ids
      from biz_flow_instance_node where instance_id = any(v_inst_ids);

    v_defs := coalesce(array_length(v_def_ids, 1), 0);
    v_inst := coalesce(array_length(v_inst_ids, 1), 0);
    -- ⚠ 必须是「现存的钉住行数」，不是「钉住数组的长度」——
    --    否则删完之后第二遍会算成 36/16 而不是 0/0，幂等分支永远走不到。
    select count(*) into v_defs from biz_flow_def      where id = any(v_def_ids);
    select count(*) into v_inst from biz_flow_instance where id = any(v_inst_ids);
    select count(*) into v_vers  from biz_flow_def_version where id = any(v_ver_ids);
    select count(*) into v_nodes from biz_flow_node        where id = any(v_node_ids);
    select count(*) into v_edges from biz_flow_edge        where version_id = any(v_ver_ids);
    select count(*) into v_asg   from biz_flow_node_assignee where node_id = any(v_node_ids);
    select count(*) into v_inode from biz_flow_instance_node where id = any(v_inode_ids);
    select count(*) into v_tasks from biz_flow_task        where instance_id = any(v_inst_ids);
    select count(*) into v_cc    from biz_flow_cc          where instance_id = any(v_inst_ids);

    -- 幂等：上次已经删干净了 → 直接退出（必须在「与基线比对」之前判断）
    if (v_defs, v_vers, v_nodes, v_edges, v_asg, v_inst, v_inode, v_tasks, v_cc)
       = (0, 0, 0, 0, 0, 0, 0, 0, 0) then
        raise notice 'E2ERES2/20 up: 钉住的目标行已是 0 行，无需清理（幂等）';
        return;
    end if;

    if (v_defs, v_vers, v_nodes, v_edges, v_asg, v_inst, v_inode, v_tasks, v_cc)
       is distinct from (v_exp_defs, v_exp_vers, v_exp_nodes, v_exp_edges, v_exp_asg,
                         v_exp_inst, v_exp_inode, v_exp_tasks, v_exp_cc) then
        raise exception
            'E2ERES2/20 up: 现状与基线不符（有人动过数据或上次删了一半），停。%'
            ' 实测 def=% ver=% node=% edge=% asg=% inst=% inode=% task=% cc=% / 基线 % % % % % % % % %',
            E'\n', v_defs, v_vers, v_nodes, v_edges, v_asg, v_inst, v_inode, v_tasks, v_cc,
            v_exp_defs, v_exp_vers, v_exp_nodes, v_exp_edges, v_exp_asg,
            v_exp_inst, v_exp_inode, v_exp_tasks, v_exp_cc;
    end if;

    -- 在飞审批绝不删（10 已拦过，这里再拦一次：10 与 20 之间可能有人发起）
    select count(*) into v_n from biz_flow_instance
     where id = any(v_inst_ids) and status = 'PENDING';
    if v_n > 0 then
        raise exception 'E2ERES2/20 up: 有 % 条目标实例处于 PENDING（10 之后有人发起了），停', v_n;
    end if;
    select count(*) into v_n from biz_flow_instance
     where def_id = any(v_def_ids) and status = 'PENDING';
    if v_n > 0 then
        raise exception 'E2ERES2/20 up: 有 % 条在飞审批挂在目标定义上，停', v_n;
    end if;

    ------------------------------------------------------------------ 快照（整行）
    insert into datafix_e2eres2_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_def', t.id::text, to_jsonb(t) from biz_flow_def t where t.id = any(v_def_ids)
    on conflict do nothing;
    insert into datafix_e2eres2_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_def_version', t.id::text, to_jsonb(t) from biz_flow_def_version t where t.id = any(v_ver_ids)
    on conflict do nothing;
    insert into datafix_e2eres2_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_node', t.id::text, to_jsonb(t) from biz_flow_node t where t.id = any(v_node_ids)
    on conflict do nothing;
    insert into datafix_e2eres2_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_node_assignee',
           t.node_id::text||'|'||t.assignee_type||'|'||t.ref_id::text, to_jsonb(t)
      from biz_flow_node_assignee t where t.node_id = any(v_node_ids)
    on conflict do nothing;
    insert into datafix_e2eres2_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_edge', t.id::text, to_jsonb(t) from biz_flow_edge t where t.version_id = any(v_ver_ids)
    on conflict do nothing;
    insert into datafix_e2eres2_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_instance', t.id::text, to_jsonb(t) from biz_flow_instance t where t.id = any(v_inst_ids)
    on conflict do nothing;
    insert into datafix_e2eres2_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_instance_node', t.id::text, to_jsonb(t) from biz_flow_instance_node t where t.id = any(v_inode_ids)
    on conflict do nothing;
    insert into datafix_e2eres2_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_task', t.id::text, to_jsonb(t) from biz_flow_task t where t.instance_id = any(v_inst_ids)
    on conflict do nothing;
    insert into datafix_e2eres2_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_cc', t.id::text, to_jsonb(t) from biz_flow_cc t where t.instance_id = any(v_inst_ids)
    on conflict do nothing;

    select count(*) into v_n from datafix_e2eres2_snapshot where batch = c_batch;
    raise notice 'E2ERES2/20 up: 快照 % 行（回滚底气）', v_n;
    if v_n <> (v_defs+v_vers+v_nodes+v_asg+v_edges+v_inst+v_inode+v_tasks+v_cc) then
        raise exception 'E2ERES2/20 up: 快照行数 % 与命中总数 % 不符 —— 快照不全，不许删',
            v_n, (v_defs+v_vers+v_nodes+v_asg+v_edges+v_inst+v_inode+v_tasks+v_cc);
    end if;

    ------------------------------------------------------------------ 删除（先子后父）
    -- ① 待办
    delete from biz_flow_task where instance_id = any(v_inst_ids);
    get diagnostics v_n = row_count;
    if v_n <> v_tasks then raise exception 'E2ERES2/20 ①: biz_flow_task 删了 % 行，期望 %', v_n, v_tasks; end if;
    insert into datafix_e2eres2_guard (batch,item,val) values (c_batch,'deleted_biz_flow_task',v_n::text)
      on conflict (batch,item) do update set val = excluded.val;

    -- ② 抄送
    delete from biz_flow_cc where instance_id = any(v_inst_ids);
    get diagnostics v_n = row_count;
    if v_n <> v_cc then raise exception 'E2ERES2/20 ②: biz_flow_cc 删了 % 行，期望 %', v_n, v_cc; end if;
    insert into datafix_e2eres2_guard (batch,item,val) values (c_batch,'deleted_biz_flow_cc',v_n::text)
      on conflict (batch,item) do update set val = excluded.val;

    -- ③ 实例节点
    delete from biz_flow_instance_node where instance_id = any(v_inst_ids);
    get diagnostics v_n = row_count;
    if v_n <> v_inode then raise exception 'E2ERES2/20 ③: biz_flow_instance_node 删了 % 行，期望 %', v_n, v_inode; end if;
    insert into datafix_e2eres2_guard (batch,item,val) values (c_batch,'deleted_biz_flow_instance_node',v_n::text)
      on conflict (batch,item) do update set val = excluded.val;

    -- ④ 实例
    delete from biz_flow_instance where id = any(v_inst_ids);
    get diagnostics v_n = row_count;
    if v_n <> v_inst then raise exception 'E2ERES2/20 ④: biz_flow_instance 删了 % 行，期望 %', v_n, v_inst; end if;
    insert into datafix_e2eres2_guard (batch,item,val) values (c_batch,'deleted_biz_flow_instance',v_n::text)
      on conflict (batch,item) do update set val = excluded.val;

    -- ⑤ 边
    delete from biz_flow_edge where version_id = any(v_ver_ids);
    get diagnostics v_n = row_count;
    if v_n <> v_edges then raise exception 'E2ERES2/20 ⑤: biz_flow_edge 删了 % 行，期望 %', v_n, v_edges; end if;
    insert into datafix_e2eres2_guard (batch,item,val) values (c_batch,'deleted_biz_flow_edge',v_n::text)
      on conflict (batch,item) do update set val = excluded.val;

    -- ⑥ 节点审批人
    delete from biz_flow_node_assignee where node_id = any(v_node_ids);
    get diagnostics v_n = row_count;
    if v_n <> v_asg then raise exception 'E2ERES2/20 ⑥: biz_flow_node_assignee 删了 % 行，期望 %', v_n, v_asg; end if;
    insert into datafix_e2eres2_guard (batch,item,val) values (c_batch,'deleted_biz_flow_node_assignee',v_n::text)
      on conflict (batch,item) do update set val = excluded.val;

    -- ⑦ 节点
    delete from biz_flow_node where id = any(v_node_ids);
    get diagnostics v_n = row_count;
    if v_n <> v_nodes then raise exception 'E2ERES2/20 ⑦: biz_flow_node 删了 % 行，期望 %', v_n, v_nodes; end if;
    insert into datafix_e2eres2_guard (batch,item,val) values (c_batch,'deleted_biz_flow_node',v_n::text)
      on conflict (batch,item) do update set val = excluded.val;

    -- ⑧ 版本
    delete from biz_flow_def_version where id = any(v_ver_ids);
    get diagnostics v_n = row_count;
    if v_n <> v_vers then raise exception 'E2ERES2/20 ⑧: biz_flow_def_version 删了 % 行，期望 %', v_n, v_vers; end if;
    insert into datafix_e2eres2_guard (batch,item,val) values (c_batch,'deleted_biz_flow_def_version',v_n::text)
      on conflict (batch,item) do update set val = excluded.val;

    -- ⑨ 定义
    delete from biz_flow_def where id = any(v_def_ids);
    get diagnostics v_n = row_count;
    if v_n <> v_defs then raise exception 'E2ERES2/20 ⑨: biz_flow_def 删了 % 行，期望 %', v_n, v_defs; end if;
    insert into datafix_e2eres2_guard (batch,item,val) values (c_batch,'deleted_biz_flow_def',v_n::text)
      on conflict (batch,item) do update set val = excluded.val;

    ------------------------------------------------------------------ 审计表：一行都没动
    select count(*) into v_slog_before from biz_status_log;
    if v_slog_before < v_slog_exp then
        raise exception 'E2ERES2/20: biz_status_log 行数 % < 基线 % —— 审计表少了行，回滚排查', v_slog_before, v_slog_exp;
    end if;
    select md5(string_agg(x, E'\n' order by x)) into v_slog_fp_before
      from (select id::text||'|'||entity_type||'|'||coalesce(entity_no,'-')||'|'
                   ||coalesce(from_status,'-')||'|'||to_status||'|'||created_at as x
              from biz_status_log where created_at <= v_pin_ts) t;
    if v_slog_fp_before is distinct from v_slog_fp_exp then
        raise exception 'E2ERES2/20: biz_status_log pin 前那段指纹变了（本脚本没删它，是别的东西动过）—— 回滚排查';
    end if;

    raise notice 'E2ERES2/20 up 完成：def=% ver=% node=% edge=% asg=% inst=% inode=% task=% cc=%；biz_status_log 未动（% 行）',
        v_defs, v_vers, v_nodes, v_edges, v_asg, v_inst, v_inode, v_tasks, v_cc, v_slog_before;
end $$;

-- 人眼可读的删除结果
select item, val from datafix_e2eres2_guard
 where batch = 'E2ERES2-20261104' and item like 'deleted_%' order by item;
