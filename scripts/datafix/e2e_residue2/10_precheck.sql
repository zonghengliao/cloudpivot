-- ============================================================================
-- E2ERES2 / 10：前置校验（硬闸）+ 目标集钉表 + 快照表 / 基线表
-- ----------------------------------------------------------------------------
-- 背景（卡 t_142317a2，承接 t_61dca492 / datafix/e2e_residue）：
--   t_61dca492 只清了「能证明是测试脚本写的 biz_type」（E2E_* / D2_SMOKE）。
--   清完之后库里还剩两类**判据抓不到**的痕迹，本卡逐条判性质后处理：
--
--   SET A-1  36 条**已软删**的流程定义：biz_type 是业务值（OTHER / EXPENSE /
--            TASK_ACCEPT），但 flow_code 带 e2e_/diag_/diag2_/vrf_/d5_/acc_/tn_ 前缀。
--   SET A-2  16 条**存活**实例：def_id 指向上面这些已软删定义（即「孤儿实例」），
--            biz_type 也是 OTHER。用户能在「我发起的 / 审批记录」里看见。
--
--   ⚠ 判据不是「flow_code 前缀可疑就算垃圾」。本包能删的唯一理由是**逐条判过性质**：
--     36 条全部 created_by ∈ {laoliu_test（专用测试账号）, admin}，flow_name 全部是
--     「E2E单签测试 / E2E驳回测试 / E2E会签测试 / 空图 / 诊断 / 诊断2 / 验证* /
--      D5 报销审批演示 / D5 校验发布演示 / D5 验证流程」，创建时间集中在
--     2026-10-30 08:46–09:30（e2e 跑批）与 2026-10-29 21:38–22:09（D5 卡演示），
--     且 11 处业务单据引用列（biz_purchase_order.approval_instance_id 等）= 全 0。
--     判决表见 docs/evidence/20261104-e2eres2/README.md，卡面也贴了。
--
--   2026-11-04 生产实测（本脚本把这些数字做成硬闸，对不上就整脚本回滚）：
--     biz_flow_def              36 行（全部已软删）
--     biz_flow_def_version      39 行
--     biz_flow_node             98 行 / biz_flow_edge 55 行 / node_assignee 12 行
--     biz_flow_instance         16 行（全部活行、全部非 PENDING：APPROVED 13 / CANCELLED 3）
--     biz_flow_instance_node    37 行 / biz_flow_task 8 行 / biz_flow_cc 0 行
--     保留（一个字节都不许动）：11 条存活流程定义，全部 PUBLISHED，指纹 7fcb32a3…
--     biz_status_log            113 行；引用这 16 条实例的行 = 0（**一行不删**）
--
-- 闸门（任一不过 → RAISE EXCEPTION，整脚本回滚，一行都不动）：
--   ① 9 张 flow 表 + sys_user 都在（连错库立刻停）
--   ② 现算目标集 == 已钉目标集（防判据漂移）；命中量 == 基线，或全 0（幂等放行）
--   ③ 目标实例里不许有 PENDING；且在飞审批不许挂在目标定义上
--   ④ 目标定义下的实例必须**全部**在目标集内（不许有真业务实例挂在要删的定义上）
--   ⑤ 非目标实例不许 prev_instance_id 指向目标实例（删了会断链）
--   ⑥ 11 处业务单据引用列 + biz_task 外键不许指向目标实例
--   ⑦ 存活流程定义必须 11 条 / 全 PUBLISHED / 指纹与基线一致
--   ⑧ 审计表 biz_status_log 行数与指纹固定（本卡一行不删）
--   ⑨ 非目标待办的 transferred_from、非目标实例节点的 node_id 不许指向目标行
--
-- 只写三张 datafix_* 辅助表 / 两个只读视图：
--   datafix_e2eres2_target    目标集钉表（kind = def / inst / keeper）
--   datafix_e2eres2_snapshot  整行快照（down 精确写回）
--   datafix_e2eres2_guard     基线（各表总量 / 命中量 / 指纹 / 审计表行数）
--   datafix_e2eres2_v_target_def / _inst   判据的单一来源（只读视图）
-- ============================================================================

\set ON_ERROR_STOP on
\pset pager off

-- ---------------------------------------------------------------- 判据单一来源
\ir 05_views.sql

-- ---------------------------------------------------------------- 三张辅助表
create table if not exists datafix_e2eres2_target (
    batch       text        not null,
    kind        text        not null check (kind in ('def', 'inst', 'keeper')),
    id          uuid        not null,
    note        text,
    pinned_at   timestamptz not null default now(),
    primary key (batch, kind, id)
);
comment on table datafix_e2eres2_target is
    'E2ERES2 目标集钉表（batch=E2ERES2-20261104）。precheck 首次钉住 id 集；up 只删这里列的 id，不重算判据。'
    ' kind: def=要删的定义 / inst=要删的实例 / keeper=必须原样保留的真流程定义。';

create table if not exists datafix_e2eres2_snapshot (
    batch       text        not null,
    tbl         text        not null,
    pk          text        not null,
    row_data    jsonb       not null,
    captured_at timestamptz not null default now(),
    primary key (batch, tbl, pk)
);
comment on table datafix_e2eres2_snapshot is
    'E2ERES2 数据修复整行快照（batch=E2ERES2-20261104）。down 脚本按此整行写回；确认无需回滚后可整表 DROP。';

create table if not exists datafix_e2eres2_guard (
    batch       text        not null,
    item        text        not null,
    val         text        not null,
    captured_at timestamptz not null default now(),
    primary key (batch, item)
);
comment on table datafix_e2eres2_guard is
    'E2ERES2 基线（batch=E2ERES2-20261104）：各表总量与命中量 / 真流程指纹 / 审计表行数与指纹。只增不改。';

do $$
declare
    c_batch   constant text := 'E2ERES2-20261104';
    -- 2026-11-04 生产实测基线（与卡 t_142317a2 的盘点数字一致）
    c_defs    constant int := 36;
    c_vers    constant int := 39;
    c_nodes   constant int := 98;
    c_edges   constant int := 55;
    c_asg     constant int := 12;
    c_inst    constant int := 16;
    c_inode   constant int := 37;
    c_tasks   constant int := 8;
    c_cc      constant int := 0;
    c_keep    constant int := 11;
    v_defs int; v_vers int; v_nodes int; v_edges int; v_asg int;
    v_inst int; v_inode int; v_tasks int; v_cc int;
    v_pending int; v_pending_global int; v_pending_on_target int;
    v_badref int; v_prevref int; v_docref int; v_btask int;
    v_xfer int; v_inode_node int;
    v_keep int; v_keep_bad int; v_keep_unpub int;
    v_slog_total int; v_slog_fp text; v_slog_touch int;
    v_pin_ts timestamptz := now();
    v_fp text;
    v_clean boolean;
    v_pin_def int; v_pin_inst int;
    v_drift int;
begin
    -- ---------- 闸①：表都在 ----------
    if to_regclass('public.biz_flow_def') is null
       or to_regclass('public.biz_flow_def_version') is null
       or to_regclass('public.biz_flow_node') is null
       or to_regclass('public.biz_flow_edge') is null
       or to_regclass('public.biz_flow_node_assignee') is null
       or to_regclass('public.biz_flow_instance') is null
       or to_regclass('public.biz_flow_instance_node') is null
       or to_regclass('public.biz_flow_task') is null
       or to_regclass('public.biz_flow_cc') is null
       or to_regclass('public.biz_status_log') is null
       or to_regclass('public.sys_user') is null then
        raise exception 'E2ERES2/10 闸① 不过：当前库缺 biz_flow_* / biz_status_log / sys_user —— 连错库了，停下';
    end if;

    -- ---------- 命中量（现算） ----------
    select count(*) into v_defs from datafix_e2eres2_v_target_def;
    select count(*) into v_inst from datafix_e2eres2_v_target_inst;
    select count(*) into v_vers from biz_flow_def_version
     where def_id in (select id from datafix_e2eres2_v_target_def);
    select count(*) into v_nodes from biz_flow_node
     where version_id in (select id from biz_flow_def_version
                           where def_id in (select id from datafix_e2eres2_v_target_def));
    select count(*) into v_edges from biz_flow_edge
     where version_id in (select id from biz_flow_def_version
                           where def_id in (select id from datafix_e2eres2_v_target_def));
    select count(*) into v_asg from biz_flow_node_assignee
     where node_id in (select id from biz_flow_node
                        where version_id in (select id from biz_flow_def_version
                                              where def_id in (select id from datafix_e2eres2_v_target_def)));
    select count(*) into v_inode from biz_flow_instance_node
     where instance_id in (select id from datafix_e2eres2_v_target_inst);
    select count(*) into v_tasks from biz_flow_task
     where instance_id in (select id from datafix_e2eres2_v_target_inst);
    select count(*) into v_cc from biz_flow_cc
     where instance_id in (select id from datafix_e2eres2_v_target_inst);

    raise notice 'E2ERES2/10 命中量：def=% ver=% node=% edge=% asg=% / inst=% inode=% task=% cc=%',
        v_defs, v_vers, v_nodes, v_edges, v_asg, v_inst, v_inode, v_tasks, v_cc;

    -- ---------- 闸②：目标集钉表 + 基线 ----------
    v_clean := (v_defs = 0 and v_inst = 0 and v_vers = 0 and v_nodes = 0
                and v_edges = 0 and v_asg = 0 and v_inode = 0 and v_tasks = 0 and v_cc = 0);

    select count(*) into v_pin_def  from datafix_e2eres2_target where batch = c_batch and kind = 'def';
    select count(*) into v_pin_inst from datafix_e2eres2_target where batch = c_batch and kind = 'inst';

    if v_pin_def = 0 and v_pin_inst = 0 then
        -- 首次：钉住（此时必须等于基线，否则说明库已被别人动过）
        if v_clean then
            raise notice 'E2ERES2/10：目标集已是 0 行（清理过或本来就干净），幂等放行（不钉表）';
        else
            if (v_defs, v_vers, v_nodes, v_edges, v_asg, v_inst, v_inode, v_tasks, v_cc)
               is distinct from (c_defs, c_vers, c_nodes, c_edges, c_asg, c_inst, c_inode, c_tasks, c_cc) then
                raise exception
                    'E2ERES2/10 闸② 不过：命中行数与 2026-11-04 基线不符，停。%'
                    ' 实测 def=% ver=% node=% edge=% asg=% inst=% inode=% task=% cc=%'
                    ' / 基线 def=% ver=% node=% edge=% asg=% inst=% inode=% task=% cc=%',
                    E'\n', v_defs, v_vers, v_nodes, v_edges, v_asg, v_inst, v_inode, v_tasks, v_cc,
                    c_defs, c_vers, c_nodes, c_edges, c_asg, c_inst, c_inode, c_tasks, c_cc;
            end if;
            insert into datafix_e2eres2_target (batch, kind, id, note)
            select c_batch, 'def', id, flow_code || ' | ' || flow_name || ' | ' || biz_type || ' | ' || status
              from datafix_e2eres2_v_target_def
            on conflict do nothing;
            insert into datafix_e2eres2_target (batch, kind, id, note)
            select c_batch, 'inst', id, flow_code || ' | ' || status
              from datafix_e2eres2_v_target_inst
            on conflict do nothing;
            insert into datafix_e2eres2_target (batch, kind, id, note)
            select c_batch, 'keeper', id, flow_code || ' | ' || flow_name
              from biz_flow_def where deleted_at is null
            on conflict do nothing;
            raise notice 'E2ERES2/10：目标集已钉住 def=% inst=% keeper=%',
                (select count(*) from datafix_e2eres2_target where batch=c_batch and kind='def'),
                (select count(*) from datafix_e2eres2_target where batch=c_batch and kind='inst'),
                (select count(*) from datafix_e2eres2_target where batch=c_batch and kind='keeper');
        end if;
    else
        -- 非首次：判据不许漂移（钉住之后又冒出来的 e2e_* 定义不会被本卡删，只报警）
        select count(*) into v_drift
          from (select id from datafix_e2eres2_v_target_def
                except select id from datafix_e2eres2_target where batch=c_batch and kind='def') s;
        if v_drift > 0 then
            raise exception
                'E2ERES2/10 闸② 不过：钉住之后又出现 % 条符合前缀判据的软删定义（判据漂移）。%'
                ' 本卡只删已钉住的 id，新出现的必须人工判性质后再另开数据修复。',
                v_drift, E'\n';
        end if;
        if not v_clean then
            select count(*) into v_drift
              from (select id from datafix_e2eres2_v_target_inst
                    except select id from datafix_e2eres2_target where batch=c_batch and kind='inst') s;
            if v_drift > 0 then
                raise exception 'E2ERES2/10 闸② 不过：钉住之后又出现 % 条孤儿实例，停', v_drift;
            end if;
        end if;
        raise notice 'E2ERES2/10：目标集沿用已钉住的集合（def=% inst=%），现算 def=% inst=%',
            v_pin_def, v_pin_inst, v_defs, v_inst;
    end if;

    -- ---------- 闸③：在飞审批 ----------
    select count(*) into v_pending from datafix_e2eres2_v_target_inst where status = 'PENDING';
    if v_pending > 0 then
        raise exception 'E2ERES2/10 闸③ 不过：有 % 条目标实例处于 PENDING（在飞审批），本卡不删在飞单据 —— 先确认它们是谁', v_pending;
    end if;
    -- 钉住集合里的实例也不许在飞（防 precheck 与 up 之间有人发起）
    select count(*) into v_pending
      from biz_flow_instance
     where status = 'PENDING'
       and id in (select id from datafix_e2eres2_target where batch = c_batch and kind = 'inst');
    if v_pending > 0 then
        raise exception 'E2ERES2/10 闸③ 不过：钉住的目标实例里有 % 条 PENDING，停', v_pending;
    end if;
    -- 全库在飞审批：只记录不拦（本卡的删除集合与它们无交集时才允许继续）
    select count(*) into v_pending_global from biz_flow_instance
     where deleted_at is null and status = 'PENDING';
    select count(*) into v_pending_on_target from biz_flow_instance i
     where i.deleted_at is null and i.status = 'PENDING'
       and i.def_id in (select id from datafix_e2eres2_target where batch = c_batch and kind = 'def');
    if v_pending_on_target > 0 then
        raise exception 'E2ERES2/10 闸③ 不过：有 % 条在飞审批挂在目标定义上（删定义会把在飞单据打成孤儿）', v_pending_on_target;
    end if;
    raise notice 'E2ERES2/10 闸③：全库在飞实例 % 条（与目标集无交集），目标实例 PENDING 0 条', v_pending_global;

    -- ---------- 闸④：目标定义下的实例必须全部在目标集内 ----------
    select count(*) into v_badref
      from biz_flow_instance i
     where i.def_id in (select id from datafix_e2eres2_target where batch = c_batch and kind = 'def')
       and i.id not in (select id from datafix_e2eres2_target where batch = c_batch and kind = 'inst');
    if v_badref > 0 then
        raise exception 'E2ERES2/10 闸④ 不过：% 条实例挂在目标定义上但不在目标集内（可能不是测试数据），停下来人工核对', v_badref;
    end if;

    -- ---------- 闸⑤：prev_instance_id 串链 ----------
    select count(*) into v_prevref
      from biz_flow_instance i
     where i.prev_instance_id in (select id from datafix_e2eres2_target where batch = c_batch and kind = 'inst')
       and i.id not in (select id from datafix_e2eres2_target where batch = c_batch and kind = 'inst');
    if v_prevref > 0 then
        raise exception 'E2ERES2/10 闸⑤ 不过：% 条非目标实例的上一轮指向目标实例（删了会断链）', v_prevref;
    end if;

    -- ---------- 闸⑥：业务单据引用（11 处多态引用列 + biz_task 外键） ----------
    select count(*) into v_docref from (
        select id from biz_contract          where approval_instance_id         in (select id from datafix_e2eres2_target where batch=c_batch and kind='inst')
        union all
        select id from biz_payable           where cancel_instance_id           in (select id from datafix_e2eres2_target where batch=c_batch and kind='inst')
        union all
        select id from biz_payable           where payment_approval_instance_id in (select id from datafix_e2eres2_target where batch=c_batch and kind='inst')
        union all
        select id from biz_payment           where approval_instance_id         in (select id from datafix_e2eres2_target where batch=c_batch and kind='inst')
        union all
        select id from biz_purchase_order    where approval_instance_id         in (select id from datafix_e2eres2_target where batch=c_batch and kind='inst')
        union all
        select id from biz_stock_in          where approval_instance_id         in (select id from datafix_e2eres2_target where batch=c_batch and kind='inst')
        union all
        select id from biz_stock_take        where approval_instance_id         in (select id from datafix_e2eres2_target where batch=c_batch and kind='inst')
        union all
        select id from biz_stock_transfer    where approval_instance_id         in (select id from datafix_e2eres2_target where batch=c_batch and kind='inst')
        union all
        select id from biz_task              where approval_instance_id         in (select id from datafix_e2eres2_target where batch=c_batch and kind='inst')
        union all
        select id from biz_voucher           where pending_reverse_instance_id  in (select id from datafix_e2eres2_target where batch=c_batch and kind='inst')
    ) s;
    select count(*) into v_btask from biz_task
     where flow_instance_id in (select id from datafix_e2eres2_target where batch=c_batch and kind='inst');
    if v_docref > 0 or v_btask > 0 then
        raise exception 'E2ERES2/10 闸⑥ 不过：有业务单据引用目标实例（单据列 % 条 / biz_task.flow_instance_id % 条）。'
                        ' 说明这些实例不是纯测试数据，停下来人工判性质', v_docref, v_btask;
    end if;

    -- ---------- 闸⑦：存活流程定义（keeper） ----------
    select count(*) into v_keep from biz_flow_def where deleted_at is null;
    if v_keep <> c_keep then
        raise exception 'E2ERES2/10 闸⑦ 不过：存活流程定义 % 条，基线是 % 条 —— 存活集变了，停下来人工核对', v_keep, c_keep;
    end if;
    select count(*) into v_keep_bad from biz_flow_def
     where deleted_at is null and (left(biz_type,4) = 'E2E_' or biz_type = 'D2_SMOKE');
    if v_keep_bad > 0 then
        raise exception 'E2ERES2/10 闸⑦ 不过：有 % 条**存活**流程命中测试残留口径（新的泄漏）', v_keep_bad;
    end if;
    select count(*) into v_keep_unpub from biz_flow_def
     where deleted_at is null and status <> 'PUBLISHED';
    if v_keep_unpub > 0 then
        raise exception 'E2ERES2/10 闸⑦ 不过：有 % 条存活流程不是 PUBLISHED（与基线不符）', v_keep_unpub;
    end if;

    -- keeper 指纹（只按 keeper id 集合算，新增流程不会污染基线）
    insert into datafix_e2eres2_target (batch, kind, id, note)
    select c_batch, 'keeper', id, flow_code || ' | ' || flow_name
      from biz_flow_def where deleted_at is null
    on conflict do nothing;

    select md5(string_agg(x, E'\n' order by x)) into v_fp
      from (select id::text||'|'||flow_code||'|'||flow_name||'|'||biz_type||'|'||status||'|'
                   ||coalesce(current_version_id::text,'-')||'|'||current_version_no||'|'
                   ||coalesce(category,'-')||'|'||is_default||'|'||allow_auto_approve||'|'
                   ||coalesce(owner_dept_id::text,'-')||'|'||created_at||'|'||updated_at as x
              from biz_flow_def
             where id in (select id from datafix_e2eres2_target where batch=c_batch and kind='keeper')) t;
    if v_fp is null then
        raise exception 'E2ERES2/10 闸⑦ 不过：keeper 指纹算不出来（keeper 一条都没有？）';
    end if;

    -- ---------- 闸⑧：审计证据表 ----------
    select count(*) into v_slog_total from biz_status_log;
    -- 指纹只覆盖「pin 之前就存在的行」⇒ 之后业务正常写审计表不会让 verify 假红
    select md5(string_agg(x, E'\n' order by x)) into v_slog_fp
      from (select id::text||'|'||entity_type||'|'||coalesce(entity_no,'-')||'|'
                   ||coalesce(from_status,'-')||'|'||to_status||'|'||created_at as x
              from biz_status_log where created_at <= v_pin_ts) t;
    -- 审计表里指向目标实例的行（按 entity_id / entity_no）
    select count(*) into v_slog_touch from biz_status_log l
     where l.entity_id in (select id from datafix_e2eres2_target where batch=c_batch and kind='inst')
        or l.entity_no in (select instance_no from biz_flow_instance
                            where id in (select id from datafix_e2eres2_target where batch=c_batch and kind='inst'));
    raise notice 'E2ERES2/10 闸⑧：biz_status_log % 行（本卡一行不删）；其中指向目标实例的 % 行（保留）',
        v_slog_total, v_slog_touch;

    -- ---------- 闸⑨：连带影响（SET NULL / SET NULL 类） ----------
    select count(*) into v_xfer from biz_flow_task t
     where t.transferred_from in (select id from biz_flow_task
                                   where instance_id in (select id from datafix_e2eres2_target
                                                          where batch=c_batch and kind='inst'))
       and t.instance_id not in (select id from datafix_e2eres2_target where batch=c_batch and kind='inst');
    if v_xfer > 0 then
        raise exception 'E2ERES2/10 闸⑨ 不过：% 条非目标待办的 transferred_from 指向目标待办（会 SET NULL）', v_xfer;
    end if;
    select count(*) into v_inode_node from biz_flow_instance_node n
     where n.node_id in (select id from biz_flow_node
                          where version_id in (select id from biz_flow_def_version
                                                where def_id in (select id from datafix_e2eres2_target
                                                                  where batch=c_batch and kind='def')))
       and n.instance_id not in (select id from datafix_e2eres2_target where batch=c_batch and kind='inst');
    if v_inode_node > 0 then
        raise exception 'E2ERES2/10 闸⑨ 不过：% 条非目标实例节点挂在目标节点上（会 SET NULL）', v_inode_node;
    end if;

    -- ---------- 基线落库（只增不改） ----------
    insert into datafix_e2eres2_guard (batch, item, val) values
        (c_batch, 'keepers_count',       (select count(*) from datafix_e2eres2_target where batch=c_batch and kind='keeper')::text),
        (c_batch, 'keepers_fingerprint', v_fp),
        (c_batch, 'keeper_versions',     (select count(*) from biz_flow_def_version
                                           where def_id in (select id from datafix_e2eres2_target where batch=c_batch and kind='keeper'))::text),
        (c_batch, 'keeper_nodes',        (select count(*) from biz_flow_node
                                           where version_id in (select id from biz_flow_def_version
                                                                 where def_id in (select id from datafix_e2eres2_target where batch=c_batch and kind='keeper')))::text),
        (c_batch, 'keeper_edges',        (select count(*) from biz_flow_edge
                                           where version_id in (select id from biz_flow_def_version
                                                                 where def_id in (select id from datafix_e2eres2_target where batch=c_batch and kind='keeper')))::text),
        (c_batch, 'biz_status_log_total', v_slog_total::text),
        (c_batch, 'biz_status_log_fingerprint', v_slog_fp),
        (c_batch, 'biz_status_log_pin_ts', v_pin_ts::text),
        (c_batch, 'biz_status_log_target_rows', v_slog_touch::text),
        (c_batch, 'pending_instances_global', v_pending_global::text),
        (c_batch, 'target_biz_flow_def',              v_defs::text),
        (c_batch, 'target_biz_flow_def_version',      v_vers::text),
        (c_batch, 'target_biz_flow_node',             v_nodes::text),
        (c_batch, 'target_biz_flow_edge',             v_edges::text),
        (c_batch, 'target_biz_flow_node_assignee',    v_asg::text),
        (c_batch, 'target_biz_flow_instance',         v_inst::text),
        (c_batch, 'target_biz_flow_instance_node',    v_inode::text),
        (c_batch, 'target_biz_flow_task',             v_tasks::text),
        (c_batch, 'target_biz_flow_cc',               v_cc::text),
        (c_batch, 'table_biz_flow_def_before',           (select count(*) from biz_flow_def)::text),
        (c_batch, 'table_biz_flow_def_version_before',   (select count(*) from biz_flow_def_version)::text),
        (c_batch, 'table_biz_flow_node_before',          (select count(*) from biz_flow_node)::text),
        (c_batch, 'table_biz_flow_edge_before',          (select count(*) from biz_flow_edge)::text),
        (c_batch, 'table_biz_flow_node_assignee_before', (select count(*) from biz_flow_node_assignee)::text),
        (c_batch, 'table_biz_flow_instance_before',      (select count(*) from biz_flow_instance)::text),
        (c_batch, 'table_biz_flow_instance_node_before', (select count(*) from biz_flow_instance_node)::text),
        (c_batch, 'table_biz_flow_task_before',          (select count(*) from biz_flow_task)::text),
        (c_batch, 'table_biz_flow_cc_before',            (select count(*) from biz_flow_cc)::text),
        (c_batch, 'table_biz_status_log_before',         v_slog_total::text),
        (c_batch, 'target_state', case when v_clean then 'clean' else 'present' end)
    on conflict (batch, item) do nothing;

    raise notice 'E2ERES2/10 前置校验全部通过（keeper=% fp=% / 目标 def=% inst=%）',
        v_keep, v_fp, v_defs, v_inst;
end $$;

-- 打印基线（人工可读）
select item, val from datafix_e2eres2_guard where batch = 'E2ERES2-20261104' order by item;
select kind, count(*) as n from datafix_e2eres2_target
 where batch = 'E2ERES2-20261104' group by kind order by kind;
