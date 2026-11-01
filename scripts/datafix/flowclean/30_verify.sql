-- ============================================================================
-- FLOWCLEAN / 30：删后自检（每行 PASS / FAIL / WARN，apply.sh 见到 FAIL 就退出 1）
-- ----------------------------------------------------------------------------
-- 硬 FAIL（不许当成功交付）：
--   ① 存活流程定义正好 6 条，且 == 保留的 6 个业务流，全部 PUBLISHED + is_default
--   ② 6 个待清理流程在 9 张 flow 表里 0 行残留
--   ③ 保留流程的 def / version / node / edge 行指纹与删除前基线逐字节一致
--   ④ 孤儿待办 = 0（卡面要求）
--   ⑤ 存活实例的孤儿数不得比删除前多（基线里已存在的历史孤儿不在本卡范围）
-- WARN（只提示，不影响退出码）：
--   * 保留流程的 instance / task 行指纹变化 —— 那是运行时数据（有人审批/建实例），不是数据修复动过
--   * 快照行数（回滚底气是否齐）
-- ============================================================================
\pset tuples_only on
\pset format unaligned
\pset fieldsep '|'

-- ① 只剩 6 条
select case when count(*) = 6 then 'PASS' else 'FAIL' end,
       'alive_flow_def=6',
       '实际 ' || count(*) || ' 条存活流程定义'
  from biz_flow_def where deleted_at is null;

-- ①b 六条必须就是保留集，且都 PUBLISHED + is_default
select case when count(*) = 6 then 'PASS' else 'FAIL' end,
       'keepers_published_default',
       '匹配 ' || count(*) || ' 条（期望 6）'
  from biz_flow_def
 where deleted_at is null and status = 'PUBLISHED' and is_default
   and flow_code = any(array['task_accept','stock_transfer','stock_in_cancel',
                             'payable_pay','po_approve','stock_in_post']);

-- ①c 存活集里不许有第 7 个
select case when count(*) = 0 then 'PASS' else 'FAIL' end,
       'no_unexpected_alive_def',
       coalesce(string_agg(flow_code, ','), '无计划外流程')
  from biz_flow_def
 where deleted_at is null
   and not (flow_code = any(array['task_accept','stock_transfer','stock_in_cancel',
                                  'payable_pay','po_approve','stock_in_post']));

-- ② 6 个待清理流程的残留（9 张表合计，任何 >0 都是 FAIL）
--    注意：def 被物理删掉之后，version/node/edge/assignee 已经**追溯不到 def** 了，
--    所以这里用「up 之前存下的快照 id 集」逐表反查，任何一行没删干净都能抓到。
with snap as (
    select tbl, pk from datafix_flowclean_snapshot where batch = 'FLOWCLEAN-20261101'
), r as (
    select 'biz_flow_def' as tbl,
           (select count(*) from biz_flow_def d
             where d.flow_code = any(array['acc_tasknode_141800','acc_tn_141831','d5_281866',
                                            'd5_282996','e2e_cs_084913','p12_canvas_demo']::varchar[]))
         + (select count(*) from biz_flow_def d
             where d.id::text = any(array(select pk from snap where tbl = 'biz_flow_def'))) as n
    union all
    select 'biz_flow_def_version',
           (select count(*) from biz_flow_def_version t
             where t.id::text = any(array(select pk from snap where tbl = 'biz_flow_def_version')))
    union all
    select 'biz_flow_node',
           (select count(*) from biz_flow_node t
             where t.id::text = any(array(select pk from snap where tbl = 'biz_flow_node')))
    union all
    select 'biz_flow_node_assignee',
           (select count(*) from biz_flow_node_assignee a
             where a.node_id::text = any(array(select pk from snap where tbl = 'biz_flow_node')))
    union all
    select 'biz_flow_edge',
           (select count(*) from biz_flow_edge t
             where t.id::text = any(array(select pk from snap where tbl = 'biz_flow_edge')))
    union all
    select 'biz_flow_instance',
           (select count(*) from biz_flow_instance i
             where i.flow_code = any(array['acc_tasknode_141800','acc_tn_141831','d5_281866',
                                            'd5_282996','e2e_cs_084913','p12_canvas_demo']::varchar[]))
         + (select count(*) from biz_flow_instance i
             where i.id::text = any(array(select pk from snap where tbl = 'biz_flow_instance')))
    union all
    select 'biz_flow_instance_node',
           (select count(*) from biz_flow_instance_node x
             where x.instance_id::text = any(array(select pk from snap where tbl = 'biz_flow_instance')))
    union all
    select 'biz_flow_task',
           (select count(*) from biz_flow_task t
             where t.instance_id::text = any(array(select pk from snap where tbl = 'biz_flow_instance')))
    union all
    select 'biz_flow_cc',
           (select count(*) from biz_flow_cc cc
             where cc.instance_id::text = any(array(select pk from snap where tbl = 'biz_flow_instance')))
    union all
    select 'biz_task_flow_ref',
           (select count(*) from biz_task t
             where t.flow_instance_id::text = any(array(select pk from snap where tbl = 'biz_flow_instance')))
)
select case when sum(n) = 0 then 'PASS' else 'FAIL' end,
       'deleted_flows_zero_residue',
       '合计 ' || sum(n) || ' 行残留 [' || string_agg(tbl || '=' || n, ' ' order by tbl) || ']'
  from r;

-- ③ 保留流程结构指纹（def 逐条 / version / node / edge 汇总）—— 必须与删除前基线一致
select case when count(*) = 6 and bool_and(g.val = md5(to_jsonb(d)::text)) then 'PASS' else 'FAIL' end,
       'keepers_def_md5',
       string_agg(d.flow_code || '=' || left(md5(to_jsonb(d)::text), 8), ' ' order by d.flow_code)
  from datafix_flowclean_guard g
  join biz_flow_def d on d.flow_code = replace(g.item, 'def_md5:', '') and d.deleted_at is null
 where g.batch = 'FLOWCLEAN-20261101' and g.item like 'def_md5:%';

select case when g.val = (select md5(coalesce(string_agg(to_jsonb(v)::text, '' order by v.id), ''))
                            from biz_flow_def_version v join biz_flow_def d on d.id = v.def_id
                           where d.deleted_at is null
                             and d.flow_code = any(array['task_accept','stock_transfer','stock_in_cancel',
                                                         'payable_pay','po_approve','stock_in_post']))
            then 'PASS' else 'FAIL' end,
       'keepers_versions_md5', '基线 ' || left(g.val, 8)
  from datafix_flowclean_guard g
 where g.batch = 'FLOWCLEAN-20261101' and g.item = 'versions_md5';

select case when g.val = (select md5(coalesce(string_agg(to_jsonb(n)::text, '' order by n.id), ''))
                            from biz_flow_node n join biz_flow_def_version v on v.id = n.version_id
                            join biz_flow_def d on d.id = v.def_id
                           where d.deleted_at is null
                             and d.flow_code = any(array['task_accept','stock_transfer','stock_in_cancel',
                                                         'payable_pay','po_approve','stock_in_post']))
            then 'PASS' else 'FAIL' end,
       'keepers_nodes_md5', '基线 ' || left(g.val, 8)
  from datafix_flowclean_guard g
 where g.batch = 'FLOWCLEAN-20261101' and g.item = 'nodes_md5';

select case when g.val = (select md5(coalesce(string_agg(to_jsonb(e)::text, '' order by e.id), ''))
                            from biz_flow_edge e join biz_flow_def_version v on v.id = e.version_id
                            join biz_flow_def d on d.id = v.def_id
                           where d.deleted_at is null
                             and d.flow_code = any(array['task_accept','stock_transfer','stock_in_cancel',
                                                         'payable_pay','po_approve','stock_in_post']))
            then 'PASS' else 'FAIL' end,
       'keepers_edges_md5', '基线 ' || left(g.val, 8)
  from datafix_flowclean_guard g
 where g.batch = 'FLOWCLEAN-20261101' and g.item = 'edges_md5';

-- ③b 保留流程的运行时数据指纹（WARN：有人审批 / 建新实例就会变，不算数据修复动过）
select case when g.val = (select md5(coalesce(string_agg(to_jsonb(i)::text, '' order by i.id), ''))
                            from biz_flow_instance i
                           where i.flow_code = any(array['task_accept','stock_transfer','stock_in_cancel',
                                                         'payable_pay','po_approve','stock_in_post']))
            then 'PASS' else 'WARN' end,
       'keepers_instances_md5', '基线 ' || left(g.val, 8)
  from datafix_flowclean_guard g
 where g.batch = 'FLOWCLEAN-20261101' and g.item = 'instances_md5';

select case when g.val = (select md5(coalesce(string_agg(to_jsonb(t)::text, '' order by t.id), ''))
                            from biz_flow_task t join biz_flow_instance i on i.id = t.instance_id
                           where i.flow_code = any(array['task_accept','stock_transfer','stock_in_cancel',
                                                         'payable_pay','po_approve','stock_in_post']))
            then 'PASS' else 'WARN' end,
       'keepers_tasks_md5', '基线 ' || left(g.val, 8)
  from datafix_flowclean_guard g
 where g.batch = 'FLOWCLEAN-20261101' and g.item = 'tasks_md5';

-- ④ 孤儿待办必须 0（卡面要求）
select case when count(*) = 0 then 'PASS' else 'FAIL' end,
       'orphan_pending_tasks=0',
       '实际 ' || count(*) || ' 条'
  from biz_flow_task ft
 where ft.status = 'PENDING'
   and not exists (select 1 from biz_flow_instance i
                     join biz_flow_def d on d.flow_code = i.flow_code and d.deleted_at is null
                    where i.id = ft.instance_id);

-- ⑤ 孤儿实例：不得比删除前多（历史孤儿来自早先「只软删 def、不删实例」，不在本卡范围）
with cur as (
    select count(*) n from biz_flow_instance i
     where i.deleted_at is null
       and not exists (select 1 from biz_flow_def d
                        where d.flow_code = i.flow_code and d.deleted_at is null)
), base as (
    select coalesce((select val::int from datafix_flowclean_guard
                      where batch = 'FLOWCLEAN-20261101' and item = 'preexisting_orphan_instances'), -1) n
)
select case when cur.n <= base.n then 'PASS' else 'FAIL' end,
       'no_new_orphan_instances',
       '现在 ' || cur.n || ' 条 / 删除前基线 ' || base.n || ' 条（不变或减少才算过）'
  from cur, base;

-- ⑥ 快照齐备（WARN：只影响「能不能回滚」，不影响清理结果）
select case when count(*) >= 154 then 'PASS' else 'WARN' end,
       'rollback_snapshot_rows',
       count(*) || ' 行快照（up 删除前实测口径 154 行）'
  from datafix_flowclean_snapshot where batch = 'FLOWCLEAN-20261101';

-- ⑦ 现状明细（人工看一眼）
select 'INFO', 'alive_flow_defs',
       string_agg(flow_code || ':' || status || ':default=' || is_default, '  ' order by flow_code)
  from biz_flow_def where deleted_at is null;

-- ⑧ 残留的 5 个实例详情（删干净后应无输出）
select 'INFO', 'leftover_instance',
       i.flow_code || ' ' || i.instance_no || ' ' || i.status
  from biz_flow_instance i
 where i.flow_code = any(array['acc_tasknode_141800','acc_tn_141831','d5_281866',
                               'd5_282996','e2e_cs_084913','p12_canvas_demo']);

\pset tuples_only off
\pset format aligned
