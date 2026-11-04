-- ============================================================================
-- E2ERES / 10：前置校验（硬闸）+ 快照表 / 基线表
-- ----------------------------------------------------------------------------
-- 背景：生产库（144 / cloudpivot）里躺着「e2e 脚本跑在生产库上」留下的测试残留。
--       本数据修复只清**测试残留**，口径与 scripts/verify/biz_type_domain.py::is_test_residue
--       逐字一致：
--
--           biz_type 以 'E2E_' 开头   或   biz_type = 'D2_SMOKE'
--
--       这不是「不在内置枚举里就算垃圾」—— biz_type 允许客户自定义，判据只有
--       「这是测试脚本写进来的类型名」。两者口径不同，别混。
--
-- 2026-11-04 生产实测基线（本脚本把数字做成硬闸，对不上就停）：
--   biz_flow_def        85 行（82 个 E2E_* + 3 个 D2_SMOKE，**全部已软删**）
--   biz_flow_def_version 85 行 / biz_flow_node 330 行 / biz_flow_edge 245 行 /
--   biz_flow_node_assignee 3 行
--   biz_flow_instance   37 行（**全部活行、全部非 PENDING**；E2E_AP_* 36 + 1 个 D2_SMOKE 已随
--                       D2 冒烟一起被删，见下）
--   biz_flow_instance_node 107 / biz_flow_task 37 / biz_flow_cc 0
--   外部引用：biz_task.flow_instance_id 指向这些实例 = 0；prev_instance_id 互指 = 0
--   保留（一个字节都不许动）：11 条存活流程定义，全部 PUBLISHED
--   biz_status_log：113 行，其中命中测试残留口径 = 0 行（**审计证据表，本卡一行不删**）
--
-- 闸门（任一不过 → RAISE EXCEPTION，整脚本回滚，一行都不动）：
--   ① 9 张 flow 表都在（连错库立刻停）
--   ② 残留行数必须等于基线（0/0 = 已清理过，幂等放行）
--   ③ 残留实例里不许有 PENDING（在飞审批不删）
--   ④ 残留 def 不许被非残留实例引用（防串行删到真流程）
--   ⑤ 残留实例不许被非残留实例 prev_instance_id 引用
--   ⑥ biz_task 不许有 flow_instance_id 指向残留实例（DELETE SET NULL 会动业务表，先人工确认）
--   ⑦ 存活流程定义必须正好 11 条、全部 PUBLISHED、且没有一条命中残留口径
--
-- 只写两张 datafix_* 辅助表：
--   datafix_e2e_residue_snapshot  整行快照（down 精确写回）
--   datafix_e2e_residue_guard     基线（存活流程指纹 / 各表总量 / 命中量 / 审计表行数）
-- ============================================================================

\set ON_ERROR_STOP on
\pset pager off

create table if not exists datafix_e2e_residue_snapshot (
    batch       text        not null,
    tbl         text        not null,
    pk          text        not null,
    row_data    jsonb       not null,
    captured_at timestamptz not null default now(),
    primary key (batch, tbl, pk)
);
comment on table datafix_e2e_residue_snapshot is
    'E2ERES 数据修复整行快照（batch=E2ERES-20261104）。down 脚本按此整行写回；确认无需回滚后可整表 DROP。';

create table if not exists datafix_e2e_residue_guard (
    batch       text        not null,
    item        text        not null,
    val         text        not null,
    captured_at timestamptz not null default now(),
    primary key (batch, item)
);
comment on table datafix_e2e_residue_guard is
    'E2ERES 基线（batch=E2ERES-20261104）：存活流程指纹 / 各表总量与命中量 / 审计表行数。只增不改。';

do $$
declare
    c_batch   constant text := 'E2ERES-20261104';
    -- 与 biz_type_domain.py::is_test_residue 同口径
    c_residue constant text := '(left(biz_type, 4) = ''E2E_'' OR biz_type = ''D2_SMOKE'')';
    -- 2026-11-04 生产实测基线
    c_defs    constant int := 85;
    c_vers    constant int := 85;
    c_nodes   constant int := 330;
    c_edges   constant int := 245;
    c_asg     constant int := 3;
    c_inst    constant int := 37;
    c_inode   constant int := 107;
    c_tasks   constant int := 37;
    c_cc      constant int := 0;
    c_keep    constant int := 11;
    v_defs int; v_vers int; v_nodes int; v_edges int; v_asg int;
    v_inst int; v_inode int; v_tasks int; v_cc int;
    v_pending int; v_badref int; v_prevref int; v_btask int;
    v_keep int; v_keep_bad int; v_keep_unpub int;
    v_total int; v_slog_total int; v_slog_res int;
    v_fp text;
    v_clean boolean;
begin
    -- ---------- 闸①：表都在 ----------
    if to_regclass('public.biz_flow_def') is null
       or to_regclass('public.biz_flow_instance') is null
       or to_regclass('public.biz_flow_task') is null
       or to_regclass('public.biz_status_log') is null then
        raise exception 'E2ERES/10: 当前库没有 biz_flow_* / biz_status_log —— 连错库了，停下';
    end if;

    -- ---------- 命中量（实数） ----------
    execute format('select count(*) from biz_flow_def where %s', c_residue) into v_defs;
    execute format('select count(*) from biz_flow_instance where %s', c_residue) into v_inst;

    select count(*) into v_vers  from biz_flow_def_version
     where def_id in (select id from biz_flow_def where left(biz_type,4)='E2E_' or biz_type='D2_SMOKE');
    select count(*) into v_nodes from biz_flow_node
     where version_id in (select id from biz_flow_def_version
                           where def_id in (select id from biz_flow_def
                                             where left(biz_type,4)='E2E_' or biz_type='D2_SMOKE'));
    select count(*) into v_edges from biz_flow_edge
     where version_id in (select id from biz_flow_def_version
                           where def_id in (select id from biz_flow_def
                                             where left(biz_type,4)='E2E_' or biz_type='D2_SMOKE'));
    select count(*) into v_asg   from biz_flow_node_assignee
     where node_id in (select id from biz_flow_node
                        where version_id in (select id from biz_flow_def_version
                                              where def_id in (select id from biz_flow_def
                                                                where left(biz_type,4)='E2E_' or biz_type='D2_SMOKE')));
    select count(*) into v_inode from biz_flow_instance_node
     where instance_id in (select id from biz_flow_instance where left(biz_type,4)='E2E_' or biz_type='D2_SMOKE');
    select count(*) into v_tasks from biz_flow_task
     where instance_id in (select id from biz_flow_instance where left(biz_type,4)='E2E_' or biz_type='D2_SMOKE');
    select count(*) into v_cc    from biz_flow_cc
     where instance_id in (select id from biz_flow_instance where left(biz_type,4)='E2E_' or biz_type='D2_SMOKE');

    raise notice 'E2ERES/10 命中量：def=% ver=% node=% edge=% asg=% / inst=% inode=% task=% cc=%',
        v_defs, v_vers, v_nodes, v_edges, v_asg, v_inst, v_inode, v_tasks, v_cc;

    -- ---------- 闸②：等于基线，或全 0（幂等） ----------
    v_clean := (v_defs = 0 and v_inst = 0 and v_vers = 0 and v_nodes = 0
                and v_edges = 0 and v_asg = 0 and v_inode = 0 and v_tasks = 0 and v_cc = 0);
    if v_clean then
        raise notice 'E2ERES/10: 测试残留已是 0 行（清理过或本来就干净），幂等放行';
    else
        if (v_defs, v_vers, v_nodes, v_edges, v_asg, v_inst, v_inode, v_tasks, v_cc)
           is distinct from (c_defs, c_vers, c_nodes, c_edges, c_asg, c_inst, c_inode, c_tasks, c_cc) then
            raise exception
                'E2ERES/10 闸② 不过：命中行数与 2026-11-04 基线不符，停。%'
                ' 实测 def=% ver=% node=% edge=% asg=% inst=% inode=% task=% cc=%'
                ' / 基线 def=% ver=% node=% edge=% asg=% inst=% inode=% task=% cc=%',
                E'\n', v_defs, v_vers, v_nodes, v_edges, v_asg, v_inst, v_inode, v_tasks, v_cc,
                c_defs, c_vers, c_nodes, c_edges, c_asg, c_inst, c_inode, c_tasks, c_cc;
        end if;
    end if;

    -- ---------- 闸③：残留实例不许 PENDING（在飞审批不删） ----------
    execute format('select count(*) from biz_flow_instance where %s and status = ''PENDING''', c_residue)
        into v_pending;
    if v_pending > 0 then
        raise exception 'E2ERES/10 闸③ 不过：有 % 条测试残留实例处于 PENDING（在飞审批），本卡不删在飞单据 —— 先确认它们是谁', v_pending;
    end if;

    -- ---------- 闸④：残留 def 不被非残留实例引用 ----------
    select count(*) into v_badref
      from biz_flow_instance i join biz_flow_def d on d.id = i.def_id
     where (left(d.biz_type, 4) = 'E2E_' or d.biz_type = 'D2_SMOKE')
       and (left(i.biz_type, 4) <> 'E2E_' and i.biz_type <> 'D2_SMOKE');
    if v_badref > 0 then
        raise exception 'E2ERES/10 闸④ 不过：% 条真实业务实例挂在本卡要删的测试流程定义上（说明定义不是测试垃圾，停下来人工核对）', v_badref;
    end if;

    -- ---------- 闸⑤：非残留实例不许 prev_instance_id 指向残留实例 ----------
    select count(*) into v_prevref
      from biz_flow_instance i
     where (left(i.biz_type, 4) <> 'E2E_' and i.biz_type <> 'D2_SMOKE')
       and i.prev_instance_id in (select id from biz_flow_instance
                                   where left(biz_type,4)='E2E_' or biz_type='D2_SMOKE');
    if v_prevref > 0 then
        raise exception 'E2ERES/10 闸⑤ 不过：% 条真实业务实例的上一轮指向测试实例（删了会断链）', v_prevref;
    end if;

    -- ---------- 闸⑥：biz_task 外键指向残留实例 ----------
    select count(*) into v_btask
      from biz_task
     where flow_instance_id in (select id from biz_flow_instance
                                 where left(biz_type,4)='E2E_' or biz_type='D2_SMOKE');
    if v_btask > 0 then
        raise exception 'E2ERES/10 闸⑥ 不过：% 条 biz_task 的 flow_instance_id 指向测试实例（外键是 SET NULL，会动业务表，先人工确认）', v_btask;
    end if;

    -- ---------- 闸⑦：存活流程定义必须 11 条 / 全 PUBLISHED / 不命中残留口径 ----------
    select count(*) into v_keep from biz_flow_def where deleted_at is null;
    if v_keep <> c_keep then
        raise exception 'E2ERES/10 闸⑦ 不过：存活流程定义 % 条，基线是 % 条 —— 存活集变了，停下来人工核对', v_keep, c_keep;
    end if;
    select count(*) into v_keep_bad from biz_flow_def
     where deleted_at is null and (left(biz_type,4) = 'E2E_' or biz_type = 'D2_SMOKE');
    if v_keep_bad > 0 then
        raise exception 'E2ERES/10 闸⑦ 不过：有 % 条**存活**流程命中了测试残留口径（新的泄漏）', v_keep_bad;
    end if;
    select count(*) into v_keep_unpub from biz_flow_def
     where deleted_at is null and status <> 'PUBLISHED';
    if v_keep_unpub > 0 then
        raise exception 'E2ERES/10 闸⑦ 不过：有 % 条存活流程不是 PUBLISHED（与基线不符）', v_keep_unpub;
    end if;

    -- ---------- 基线落库（只增不改） ----------
    select md5(string_agg(x, E'\n' order by x)) into v_fp
      from (select id::text||'|'||flow_code||'|'||flow_name||'|'||biz_type||'|'||status||'|'
                   ||coalesce(current_version_id::text,'-')||'|'||current_version_no||'|'
                   ||coalesce(category,'-')||'|'||is_default||'|'||allow_auto_approve||'|'
                   ||coalesce(owner_dept_id::text,'-')||'|'||created_at||'|'||updated_at as x
              from biz_flow_def where deleted_at is null) t;

    select count(*) into v_slog_total from biz_status_log;
    select count(*) into v_slog_res  from biz_status_log
     where left(entity_type, 4) = 'E2E_' or entity_type = 'D2_SMOKE'
        or left(entity_no, 4) = 'E2E_' or left(coalesce(biz_ref_no, ''), 4) = 'E2E_';

    insert into datafix_e2e_residue_guard (batch, item, val) values
        (c_batch, 'keepers_count',      v_keep::text),
        (c_batch, 'keepers_fingerprint',v_fp),
        (c_batch, 'keeper_versions',    (select count(*) from biz_flow_def_version
                                          where def_id in (select id from biz_flow_def where deleted_at is null))::text),
        (c_batch, 'keeper_nodes',       (select count(*) from biz_flow_node
                                          where version_id in (select id from biz_flow_def_version
                                                                where def_id in (select id from biz_flow_def where deleted_at is null)))::text),
        (c_batch, 'keeper_edges',       (select count(*) from biz_flow_edge
                                          where version_id in (select id from biz_flow_def_version
                                                                where def_id in (select id from biz_flow_def where deleted_at is null)))::text),
        (c_batch, 'biz_status_log_total', v_slog_total::text),
        (c_batch, 'biz_status_log_residue', v_slog_res::text),
        (c_batch, 'residue_biz_flow_def',              v_defs::text),
        (c_batch, 'residue_biz_flow_def_version',      v_vers::text),
        (c_batch, 'residue_biz_flow_node',             v_nodes::text),
        (c_batch, 'residue_biz_flow_edge',             v_edges::text),
        (c_batch, 'residue_biz_flow_node_assignee',    v_asg::text),
        (c_batch, 'residue_biz_flow_instance',         v_inst::text),
        (c_batch, 'residue_biz_flow_instance_node',    v_inode::text),
        (c_batch, 'residue_biz_flow_task',             v_tasks::text),
        (c_batch, 'residue_biz_flow_cc',               v_cc::text),
        (c_batch, 'table_biz_flow_def_before',          (select count(*) from biz_flow_def)::text),
        (c_batch, 'table_biz_flow_def_version_before',  (select count(*) from biz_flow_def_version)::text),
        (c_batch, 'table_biz_flow_node_before',         (select count(*) from biz_flow_node)::text),
        (c_batch, 'table_biz_flow_edge_before',         (select count(*) from biz_flow_edge)::text),
        (c_batch, 'table_biz_flow_node_assignee_before',(select count(*) from biz_flow_node_assignee)::text),
        (c_batch, 'table_biz_flow_instance_before',     (select count(*) from biz_flow_instance)::text),
        (c_batch, 'table_biz_flow_instance_node_before',(select count(*) from biz_flow_instance_node)::text),
        (c_batch, 'table_biz_flow_task_before',         (select count(*) from biz_flow_task)::text),
        (c_batch, 'table_biz_flow_cc_before',           (select count(*) from biz_flow_cc)::text),
        (c_batch, 'table_biz_status_log_before',        v_slog_total::text),
        (c_batch, 'residue_state', case when v_clean then 'clean' else 'present' end)
    on conflict (batch, item) do nothing;

    raise notice 'E2ERES/10 前置校验全部通过（keepers=% fp=% / 残留 def=% inst=%）',
        v_keep, v_fp, v_defs, v_inst;
end $$;

-- 打印基线（人工可读）
select item, val from datafix_e2e_residue_guard where batch = 'E2ERES-20261104' order by item;
