-- ============================================================================
-- FLOWCLEAN / 20（up）：清掉 6 个测试/演示审批流
-- ----------------------------------------------------------------------------
-- 范围（**只按 flow_code 精确匹配**，禁止 LIKE / 按名字模糊匹配）：
--   acc_tasknode_141800 / acc_tn_141831 / d5_281866 / d5_282996 / e2e_cs_084913 / p12_canvas_demo
--
-- 为什么是「物理删」而不是软删：
--   老板要「不要有残留」；这 6 条是测试/演示数据，业务上没有任何挂靠
--   （实测：0 条收货/调拨/采购/应付引用了它们的实例；只有 1 条测试任务 biz_task
--    「验收任务-ACC-141831」用 flow_instance_id 指着 acc_tn_141831 的实例，外键是 ON DELETE SET NULL）。
--   物理删之后 def/version/node/edge/instance 一行都不剩；可逆性由 datafix_flowclean_snapshot
--   的整行快照 + 20_*.down.sql 保证（不是靠软删标志）。
--
-- 顺序（先子后父，与 FK 方向一致）：
--   biz_task(外键引用) → biz_flow_task → biz_flow_cc → biz_flow_instance_node
--   → biz_flow_instance → biz_flow_edge → biz_flow_node_assignee → biz_flow_node
--   → biz_flow_def_version → biz_flow_def
--
-- 硬校验（任一条不符 → 整个事务回滚，一行都不动）：
--   * 待清理 def 必须 6 条（0 条 = 已清过，幂等退出）
--   * 每张子表的命中行数必须等于「2026-11-01 生产实测口径」的快照数
--     （def 6 / version 12 / node 55 / node_assignee 6 / edge 47 / instance 5 /
--       instance_node 15 / task 7 / cc 0 / 外部引用 biz_task 1）
--     —— 数量变了说明有人动过数据，停下来人工核对，不许硬上
--   * 外部业务表（收货/调拨/采购/应付）不得引用这 5 个实例的 approval_instance_id
--   * 这 5 个实例不得被别的实例 prev_instance_id 引用
--
-- 幂等：第二次跑待清理 def = 0 → NOTICE 跳过，0 行受影响；子表若还有半截残留则报错。
-- 回滚：bash apply.sh --rollback（先跑 20 的 down，再核对 verify）。
-- ============================================================================

do $$
declare
    c_batch   constant text   := 'FLOWCLEAN-20261101';
    c_del     constant text[] := array['acc_tasknode_141800','acc_tn_141831','d5_281866',
                                       'd5_282996','e2e_cs_084913','p12_canvas_demo'];
    -- 2026-11-01 生产实测口径（备份 MANIFEST 与 target/*.csv 行数一致）
    c_defs    constant int := 6;
    c_vers    constant int := 12;
    c_nodes   constant int := 55;
    c_asg     constant int := 6;
    c_edges   constant int := 47;
    c_inst    constant int := 5;
    c_inode   constant int := 15;
    c_tasks   constant int := 7;
    c_cc      constant int := 0;
    c_task_refs constant int := 1;   -- biz_task.flow_instance_id 指向待删实例的行数
    v_def_ids   uuid[];
    v_ver_ids   uuid[];
    v_node_ids  uuid[];
    v_inst_ids  uuid[];
    v_n         int;
    v_snap      int := 0;
    v_tmp       int;
    v_ext       int;
begin
    if to_regclass('public.datafix_flowclean_snapshot') is null then
        raise exception 'FLOWCLEAN/20: 缺少 datafix_flowclean_snapshot —— 先跑 10_precheck.sql';
    end if;

    -- ---------- 0) 幂等闸：待清理 def 数 ----------
    select count(*) into v_n from biz_flow_def where deleted_at is null and flow_code = any(c_del);
    if v_n = 0 then
        -- 半截残留（def 没了但实例还在）必须报错，不能当成功
        select count(*) into v_tmp from biz_flow_instance where flow_code = any(c_del);
        if v_tmp > 0 then
            raise exception 'FLOWCLEAN/20: def 已清但仍有 % 个实例残留 —— 半截状态，停下人工核对', v_tmp;
        end if;
        raise notice 'FLOWCLEAN/20: 6 个流程已清理完毕 —— 幂等跳过（本步 0 行受影响）';
        return;
    end if;
    if v_n <> c_defs then
        raise exception 'FLOWCLEAN/20: 待清理 def 实际 % 条（预期 %）—— 停下人工核对', v_n, c_defs;
    end if;

    -- ---------- 1) 收集 id 集（只来自 6 个 def）----------
    select array_agg(id) into v_def_ids
      from biz_flow_def where deleted_at is null and flow_code = any(c_del);

    select array_agg(id) into v_ver_ids
      from biz_flow_def_version where def_id = any(v_def_ids);

    select array_agg(id) into v_node_ids
      from biz_flow_node where version_id = any(v_ver_ids);

    select array_agg(id) into v_inst_ids
      from biz_flow_instance where def_id = any(v_def_ids) or flow_code = any(c_del);

    -- ---------- 2) 行数硬校验（口径必须与备份一致）----------
    select count(*) into v_tmp from biz_flow_def_version where def_id = any(v_def_ids);
    if v_tmp <> c_vers then
        raise exception 'FLOWCLEAN/20: 版本 % 行（预期 %）—— 数据变了，停下人工核对', v_tmp, c_vers;
    end if;

    select count(*) into v_tmp from biz_flow_node where version_id = any(v_ver_ids);
    if v_tmp <> c_nodes then
        raise exception 'FLOWCLEAN/20: 节点 % 行（预期 %）—— 数据变了，停下人工核对', v_tmp, c_nodes;
    end if;

    select count(*) into v_tmp from biz_flow_node_assignee where node_id = any(v_node_ids);
    if v_tmp <> c_asg then
        raise exception 'FLOWCLEAN/20: 节点审批人 % 行（预期 %）—— 数据变了，停下人工核对', v_tmp, c_asg;
    end if;

    select count(*) into v_tmp from biz_flow_edge where version_id = any(v_ver_ids);
    if v_tmp <> c_edges then
        raise exception 'FLOWCLEAN/20: 边 % 行（预期 %）—— 数据变了，停下人工核对', v_tmp, c_edges;
    end if;

    select count(*) into v_tmp from biz_flow_instance where flow_code = any(c_del);
    if v_tmp <> c_inst then
        raise exception 'FLOWCLEAN/20: 实例 % 行（预期 %，含已软删）—— 数据变了，停下人工核对', v_tmp, c_inst;
    end if;

    select count(*) into v_tmp from biz_flow_instance_node where instance_id = any(v_inst_ids);
    if v_tmp <> c_inode then
        raise exception 'FLOWCLEAN/20: 实例节点 % 行（预期 %）—— 数据变了，停下人工核对', v_tmp, c_inode;
    end if;

    select count(*) into v_tmp from biz_flow_task where instance_id = any(v_inst_ids);
    if v_tmp <> c_tasks then
        raise exception 'FLOWCLEAN/20: 待办 % 行（预期 %）—— 数据变了，停下人工核对', v_tmp, c_tasks;
    end if;

    select count(*) into v_tmp from biz_flow_cc where instance_id = any(v_inst_ids);
    if v_tmp <> c_cc then
        raise exception 'FLOWCLEAN/20: 抄送 % 行（预期 %）—— 数据变了，停下人工核对', v_tmp, c_cc;
    end if;

    -- ---------- 3) 外部引用检查（误伤闸）----------
    select count(*) into v_ext from biz_stock_in
     where approval_instance_id = any(v_inst_ids);
    if v_ext > 0 then
        raise exception 'FLOWCLEAN/20: 收货单 % 张引用了待删实例的审批 —— 应走业务作废流程，已中止', v_ext;
    end if;

    select count(*) into v_ext from biz_stock_transfer
     where approval_instance_id = any(v_inst_ids);
    if v_ext > 0 then
        raise exception 'FLOWCLEAN/20: 调拨单 % 张引用了待删实例的审批 —— 已中止', v_ext;
    end if;

    select count(*) into v_ext from biz_purchase_order
     where approval_instance_id = any(v_inst_ids);
    if v_ext > 0 then
        raise exception 'FLOWCLEAN/20: 采购订单 % 张引用了待删实例的审批 —— 已中止', v_ext;
    end if;

    select count(*) into v_ext from biz_payment
     where approval_instance_id = any(v_inst_ids);
    if v_ext > 0 then
        raise exception 'FLOWCLEAN/20: 付款单 % 张引用了待删实例的审批 —— 已中止', v_ext;
    end if;

    select count(*) into v_ext from biz_task
     where approval_instance_id = any(v_inst_ids);
    if v_ext > 0 then
        raise exception 'FLOWCLEAN/20: 任务 % 条引用了待删实例的审批 —— 已中止', v_ext;
    end if;

    -- 别的实例不许把 prev_instance_id 指过来（否则删实例会撞 FK）
    select count(*) into v_ext from biz_flow_instance
     where prev_instance_id = any(v_inst_ids) and id <> all(v_inst_ids);
    if v_ext > 0 then
        raise exception 'FLOWCLEAN/20: % 个外部实例的 prev_instance_id 指向待删实例 —— 已中止', v_ext;
    end if;

    -- 别的待办不许把 transferred_from 指过来（FK 是 SET NULL，会偷改别人的行）
    select count(*) into v_ext from biz_flow_task
     where transferred_from in (select id from biz_flow_task where instance_id = any(v_inst_ids))
       and instance_id <> all(v_inst_ids);
    if v_ext > 0 then
        raise exception 'FLOWCLEAN/20: % 条外部待办的 transferred_from 指向待删待办 —— 已中止', v_ext;
    end if;

    -- ---------- 4) 整行快照（down 精确写回用）----------
    insert into datafix_flowclean_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_def', d.id::text, to_jsonb(d) from biz_flow_def d where d.id = any(v_def_ids)
    on conflict (batch, tbl, pk) do nothing;
    get diagnostics v_tmp = row_count; v_snap := v_snap + v_tmp;

    insert into datafix_flowclean_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_def_version', v.id::text, to_jsonb(v) from biz_flow_def_version v where v.id = any(v_ver_ids)
    on conflict (batch, tbl, pk) do nothing;
    get diagnostics v_tmp = row_count; v_snap := v_snap + v_tmp;

    insert into datafix_flowclean_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_node', n.id::text, to_jsonb(n) from biz_flow_node n where n.id = any(v_node_ids)
    on conflict (batch, tbl, pk) do nothing;
    get diagnostics v_tmp = row_count; v_snap := v_snap + v_tmp;

    insert into datafix_flowclean_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_node_assignee',
           a.node_id::text || '|' || a.assignee_type || '|' || coalesce(a.ref_id::text, ''), to_jsonb(a)
      from biz_flow_node_assignee a where a.node_id = any(v_node_ids)
    on conflict (batch, tbl, pk) do nothing;
    get diagnostics v_tmp = row_count; v_snap := v_snap + v_tmp;

    insert into datafix_flowclean_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_edge', e.id::text, to_jsonb(e) from biz_flow_edge e where e.version_id = any(v_ver_ids)
    on conflict (batch, tbl, pk) do nothing;
    get diagnostics v_tmp = row_count; v_snap := v_snap + v_tmp;

    insert into datafix_flowclean_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_instance', i.id::text, to_jsonb(i) from biz_flow_instance i where i.id = any(v_inst_ids)
    on conflict (batch, tbl, pk) do nothing;
    get diagnostics v_tmp = row_count; v_snap := v_snap + v_tmp;

    insert into datafix_flowclean_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_instance_node', x.id::text, to_jsonb(x)
      from biz_flow_instance_node x where x.instance_id = any(v_inst_ids)
    on conflict (batch, tbl, pk) do nothing;
    get diagnostics v_tmp = row_count; v_snap := v_snap + v_tmp;

    insert into datafix_flowclean_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_task', t.id::text, to_jsonb(t) from biz_flow_task t where t.instance_id = any(v_inst_ids)
    on conflict (batch, tbl, pk) do nothing;
    get diagnostics v_tmp = row_count; v_snap := v_snap + v_tmp;

    insert into datafix_flowclean_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_flow_cc', cc.id::text, to_jsonb(cc) from biz_flow_cc cc where cc.instance_id = any(v_inst_ids)
    on conflict (batch, tbl, pk) do nothing;
    get diagnostics v_tmp = row_count; v_snap := v_snap + v_tmp;

    -- 被 FK ON DELETE SET NULL 牵连的外部行（biz_task.flow_instance_id）先存快照，down 才能写回
    insert into datafix_flowclean_snapshot (batch, tbl, pk, row_data)
    select c_batch, 'biz_task_flow_ref', t.id::text, to_jsonb(t)
      from biz_task t where t.flow_instance_id = any(v_inst_ids)
    on conflict (batch, tbl, pk) do nothing;
    get diagnostics v_tmp = row_count; v_snap := v_snap + v_tmp;

    -- ---------- 5) 删（先子后父）----------
    -- ① 外部引用：解引用（FK 本身是 SET NULL，但 biz_task 有两条 CHECK 拦着，
    --    必须把「flow 三元组」一起清空 + source 从 FLOW 改成 SELF，否则约束直接炸）：
    --      ck_biz_task_flow_triplet : (flow_instance_id, flow_node_key, flow_round) 必须同空同非空
    --      ck_biz_task_source_flow  : (source='FLOW') 必须等价于 (flow_instance_id IS NOT NULL)
    --    受影响行已在上一步存好整行快照，down 会把 4 个列原样写回。
    update biz_task
       set flow_instance_id = null,
           flow_node_key    = null,
           flow_round       = null,
           source           = case when source = 'FLOW' then 'SELF' else source end
     where flow_instance_id = any(v_inst_ids);
    get diagnostics v_tmp = row_count;
    if v_tmp <> c_task_refs then
        raise exception 'FLOWCLEAN/20: 解引用的 biz_task % 行（预期 %）—— 已回滚', v_tmp, c_task_refs;
    end if;
    raise notice 'FLOWCLEAN/20: 解除 % 条 biz_task 的 flow 三元组引用（flow_instance_id/flow_node_key/flow_round → NULL, source FLOW→SELF；快照已存，down 可写回）', v_tmp;

    -- ② 待办
    delete from biz_flow_task where instance_id = any(v_inst_ids);
    get diagnostics v_tmp = row_count;
    if v_tmp <> c_tasks then
        raise exception 'FLOWCLEAN/20: 删除待办 % 行（预期 %）—— 已回滚', v_tmp, c_tasks;
    end if;

    -- ③ 抄送
    delete from biz_flow_cc where instance_id = any(v_inst_ids);
    get diagnostics v_tmp = row_count;
    if v_tmp <> c_cc then
        raise exception 'FLOWCLEAN/20: 删除抄送 % 行（预期 %）—— 已回滚', v_tmp, c_cc;
    end if;

    -- ④ 实例节点
    delete from biz_flow_instance_node where instance_id = any(v_inst_ids);
    get diagnostics v_tmp = row_count;
    if v_tmp <> c_inode then
        raise exception 'FLOWCLEAN/20: 删除实例节点 % 行（预期 %）—— 已回滚', v_tmp, c_inode;
    end if;

    -- ⑤ 实例
    delete from biz_flow_instance where id = any(v_inst_ids);
    get diagnostics v_tmp = row_count;
    if v_tmp <> c_inst then
        raise exception 'FLOWCLEAN/20: 删除实例 % 行（预期 %）—— 已回滚', v_tmp, c_inst;
    end if;

    -- ⑥ 边
    delete from biz_flow_edge where version_id = any(v_ver_ids);
    get diagnostics v_tmp = row_count;
    if v_tmp <> c_edges then
        raise exception 'FLOWCLEAN/20: 删除边 % 行（预期 %）—— 已回滚', v_tmp, c_edges;
    end if;

    -- ⑦ 节点审批人
    delete from biz_flow_node_assignee where node_id = any(v_node_ids);
    get diagnostics v_tmp = row_count;
    if v_tmp <> c_asg then
        raise exception 'FLOWCLEAN/20: 删除节点审批人 % 行（预期 %）—— 已回滚', v_tmp, c_asg;
    end if;

    -- ⑧ 节点定义
    delete from biz_flow_node where version_id = any(v_ver_ids);
    get diagnostics v_tmp = row_count;
    if v_tmp <> c_nodes then
        raise exception 'FLOWCLEAN/20: 删除节点 % 行（预期 %）—— 已回滚', v_tmp, c_nodes;
    end if;

    -- ⑨ 版本
    delete from biz_flow_def_version where def_id = any(v_def_ids);
    get diagnostics v_tmp = row_count;
    if v_tmp <> c_vers then
        raise exception 'FLOWCLEAN/20: 删除版本 % 行（预期 %）—— 已回滚', v_tmp, c_vers;
    end if;

    -- ⑩ 定义
    delete from biz_flow_def where id = any(v_def_ids);
    get diagnostics v_tmp = row_count;
    if v_tmp <> c_defs then
        raise exception 'FLOWCLEAN/20: 删除定义 % 行（预期 %）—— 已回滚', v_tmp, c_defs;
    end if;

    raise notice 'FLOWCLEAN/20: 已清理 6 个流程（def % / version % / node % / assignee % / edge % / instance % / instance_node % / task % / cc %），快照新增 % 行',
        c_defs, c_vers, c_nodes, c_asg, c_edges, c_inst, c_inode, c_tasks, c_cc, v_snap;
end
$$;
