-- ============================================================================
-- E2ERES / 30：删后自检（每行 PASS / FAIL / WARN；apply.sh 见到 FAIL 即退出码 1）
-- ----------------------------------------------------------------------------
-- 硬 FAIL（不许当成功交付）：
--   ① 9 张 flow 表里「测试残留口径」行数全部为 0
--   ② 没有半截残留：版本/节点/边/实例节点/待办/抄送 不许有孤儿父行
--   ③ 存活流程定义 = 基线 11 条，且**指纹逐字节一致**（真流程一个字节都没动）
--   ④ 存活流程的版本/节点/边行数与基线一致
--   ⑤ 静态配置表（def/version/node/edge/node_assignee）总量 = 删除前 - 实际删除
--   ⑥ 审计证据表 biz_status_log 行数与基线一致（本卡一行不删）
-- WARN（只提示，不影响退出码）：
--   * 运行期表（instance / instance_node / task / cc）总量比「删除前 - 删除」**多**
--     —— 那是清场后有人正常发起了审批（不是数据修复动过）
--   * 库里还有其它非本卡口径的测试痕迹（flow_code 带 e2e/diag/vrf 前缀的软删定义、
--     孤儿实例）—— 本卡范围外，已在卡面点名跟进
-- ============================================================================
\pset tuples_only on
\pset format unaligned
\pset fieldsep '|'

-- ① 测试残留口径（与 biz_type_domain.py::is_test_residue 一致）
select case when count(*) = 0 then 'PASS' else 'FAIL' end, 'residue_biz_flow_def',
       '残留 ' || count(*) || ' 行'
  from biz_flow_def where left(biz_type,4) = 'E2E_' or biz_type = 'D2_SMOKE';

select case when count(*) = 0 then 'PASS' else 'FAIL' end, 'residue_biz_flow_instance',
       '残留 ' || count(*) || ' 行'
  from biz_flow_instance where left(biz_type,4) = 'E2E_' or biz_type = 'D2_SMOKE';

-- ② 半截残留 / 孤儿
select case when count(*) = 0 then 'PASS' else 'FAIL' end, 'orphan_biz_flow_def_version',
       count(*) || ' 个版本没有父定义'
  from biz_flow_def_version v
 where not exists (select 1 from biz_flow_def d where d.id = v.def_id);

select case when count(*) = 0 then 'PASS' else 'FAIL' end, 'orphan_biz_flow_node',
       count(*) || ' 个节点没有父版本'
  from biz_flow_node n
 where not exists (select 1 from biz_flow_def_version v where v.id = n.version_id);

select case when count(*) = 0 then 'PASS' else 'FAIL' end, 'orphan_biz_flow_edge',
       count(*) || ' 条边没有父版本'
  from biz_flow_edge e
 where not exists (select 1 from biz_flow_def_version v where v.id = e.version_id);

select case when count(*) = 0 then 'PASS' else 'FAIL' end, 'orphan_biz_flow_instance',
       count(*) || ' 个实例没有父定义'
  from biz_flow_instance i
 where not exists (select 1 from biz_flow_def d where d.id = i.def_id);

select case when count(*) = 0 then 'PASS' else 'FAIL' end, 'orphan_biz_flow_instance_node',
       count(*) || ' 个实例节点没有父实例'
  from biz_flow_instance_node n
 where not exists (select 1 from biz_flow_instance i where i.id = n.instance_id);

select case when count(*) = 0 then 'PASS' else 'FAIL' end, 'orphan_biz_flow_task',
       count(*) || ' 条待办没有父实例'
  from biz_flow_task t
 where not exists (select 1 from biz_flow_instance i where i.id = t.instance_id);

select case when count(*) = 0 then 'PASS' else 'FAIL' end, 'orphan_biz_flow_cc',
       count(*) || ' 条抄送没有父实例'
  from biz_flow_cc c
 where not exists (select 1 from biz_flow_instance i where i.id = c.instance_id);

-- ③ 存活流程定义：条数 + 指纹（真流程一个字节都不许动）
select case when count(*) = (select val::int from datafix_e2e_residue_guard
                              where batch='E2ERES-20261104' and item='keepers_count')
            then 'PASS' else 'FAIL' end,
       'alive_flow_def',
       '实际 ' || count(*) || ' 条（基线 ' ||
       (select val from datafix_e2e_residue_guard where batch='E2ERES-20261104' and item='keepers_count')
       || '）'
  from biz_flow_def where deleted_at is null;

select case when fp = (select val from datafix_e2e_residue_guard
                        where batch='E2ERES-20261104' and item='keepers_fingerprint')
            then 'PASS' else 'FAIL' end,
       'keepers_fingerprint', coalesce(fp, '(空)')
  from (
    select md5(string_agg(x, E'\n' order by x)) as fp
      from (select id::text||'|'||flow_code||'|'||flow_name||'|'||biz_type||'|'||status||'|'
                   ||coalesce(current_version_id::text,'-')||'|'||current_version_no||'|'
                   ||coalesce(category,'-')||'|'||is_default||'|'||allow_auto_approve||'|'
                   ||coalesce(owner_dept_id::text,'-')||'|'||created_at||'|'||updated_at as x
              from biz_flow_def where deleted_at is null) t
  ) s;

-- ④ 存活流程的版本/节点/边行数
select case when count(*) = (select val::int from datafix_e2e_residue_guard
                              where batch='E2ERES-20261104' and item='keeper_versions')
            then 'PASS' else 'FAIL' end, 'keeper_versions',
       count(*) || ' 行' from biz_flow_def_version
 where def_id in (select id from biz_flow_def where deleted_at is null);

select case when count(*) = (select val::int from datafix_e2e_residue_guard
                              where batch='E2ERES-20261104' and item='keeper_nodes')
            then 'PASS' else 'FAIL' end, 'keeper_nodes',
       count(*) || ' 行' from biz_flow_node
 where version_id in (select id from biz_flow_def_version
                       where def_id in (select id from biz_flow_def where deleted_at is null));

select case when count(*) = (select val::int from datafix_e2e_residue_guard
                              where batch='E2ERES-20261104' and item='keeper_edges')
            then 'PASS' else 'FAIL' end, 'keeper_edges',
       count(*) || ' 行' from biz_flow_edge
 where version_id in (select id from biz_flow_def_version
                       where def_id in (select id from biz_flow_def where deleted_at is null));

-- ⑤ 静态配置表：总量 == 删除前 - 实际删除
select case when actual = expected then 'PASS' else 'FAIL' end, 'count_biz_flow_def',
       'actual=' || actual || ' expected=' || expected
  from (select (select count(*) from biz_flow_def) as actual,
               (select val::int - coalesce((select val::int from datafix_e2e_residue_guard
                                             where batch='E2ERES-20261104' and item='deleted_biz_flow_def'), 0)
                  from datafix_e2e_residue_guard where batch='E2ERES-20261104' and item='table_biz_flow_def_before') as expected) s;

select case when actual = expected then 'PASS' else 'FAIL' end, 'count_biz_flow_def_version',
       'actual=' || actual || ' expected=' || expected
  from (select (select count(*) from biz_flow_def_version) as actual,
               (select val::int - coalesce((select val::int from datafix_e2e_residue_guard
                                             where batch='E2ERES-20261104' and item='deleted_biz_flow_def_version'), 0)
                  from datafix_e2e_residue_guard where batch='E2ERES-20261104' and item='table_biz_flow_def_version_before') as expected) s;

select case when actual = expected then 'PASS' else 'FAIL' end, 'count_biz_flow_node',
       'actual=' || actual || ' expected=' || expected
  from (select (select count(*) from biz_flow_node) as actual,
               (select val::int - coalesce((select val::int from datafix_e2e_residue_guard
                                             where batch='E2ERES-20261104' and item='deleted_biz_flow_node'), 0)
                  from datafix_e2e_residue_guard where batch='E2ERES-20261104' and item='table_biz_flow_node_before') as expected) s;

select case when actual = expected then 'PASS' else 'FAIL' end, 'count_biz_flow_edge',
       'actual=' || actual || ' expected=' || expected
  from (select (select count(*) from biz_flow_edge) as actual,
               (select val::int - coalesce((select val::int from datafix_e2e_residue_guard
                                             where batch='E2ERES-20261104' and item='deleted_biz_flow_edge'), 0)
                  from datafix_e2e_residue_guard where batch='E2ERES-20261104' and item='table_biz_flow_edge_before') as expected) s;

select case when actual = expected then 'PASS' else 'FAIL' end, 'count_biz_flow_node_assignee',
       'actual=' || actual || ' expected=' || expected
  from (select (select count(*) from biz_flow_node_assignee) as actual,
               (select val::int - coalesce((select val::int from datafix_e2e_residue_guard
                                             where batch='E2ERES-20261104' and item='deleted_biz_flow_node_assignee'), 0)
                  from datafix_e2e_residue_guard where batch='E2ERES-20261104' and item='table_biz_flow_node_assignee_before') as expected) s;

-- ⑤b 运行期表：只允许「比预期多」（清场后有人正常发起审批），少了才 FAIL
select case when actual >= expected then 'PASS' else 'FAIL' end, 'count_biz_flow_instance',
       'actual=' || actual || ' expected>=' || expected
  from (select (select count(*) from biz_flow_instance) as actual,
               (select val::int - coalesce((select val::int from datafix_e2e_residue_guard
                                             where batch='E2ERES-20261104' and item='deleted_biz_flow_instance'), 0)
                  from datafix_e2e_residue_guard where batch='E2ERES-20261104' and item='table_biz_flow_instance_before') as expected) s;

select case when actual >= expected then 'PASS' else 'FAIL' end, 'count_biz_flow_instance_node',
       'actual=' || actual || ' expected>=' || expected
  from (select (select count(*) from biz_flow_instance_node) as actual,
               (select val::int - coalesce((select val::int from datafix_e2e_residue_guard
                                             where batch='E2ERES-20261104' and item='deleted_biz_flow_instance_node'), 0)
                  from datafix_e2e_residue_guard where batch='E2ERES-20261104' and item='table_biz_flow_instance_node_before') as expected) s;

select case when actual >= expected then 'PASS' else 'FAIL' end, 'count_biz_flow_task',
       'actual=' || actual || ' expected>=' || expected
  from (select (select count(*) from biz_flow_task) as actual,
               (select val::int - coalesce((select val::int from datafix_e2e_residue_guard
                                             where batch='E2ERES-20261104' and item='deleted_biz_flow_task'), 0)
                  from datafix_e2e_residue_guard where batch='E2ERES-20261104' and item='table_biz_flow_task_before') as expected) s;

-- ⑥ 审计证据表：行数与基线一致（本卡一行不删）
select case when count(*) = (select val::int from datafix_e2e_residue_guard
                              where batch='E2ERES-20261104' and item='biz_status_log_total')
            then 'PASS' else 'FAIL' end, 'biz_status_log_untouched',
       '实际 ' || count(*) || ' 行'
  from biz_status_log;

-- ⑦ WARN：本卡范围外的其它测试痕迹（只提示）
select case when count(*) = 0 then 'PASS' else 'WARN' end, 'out_of_scope_deleted_e2e_like_defs',
       count(*) || ' 条软删定义的 flow_code 带 e2e/diag/vrf/d5/acc 前缀（biz_type 是业务值，本卡不判垃圾）'
  from biz_flow_def
 where deleted_at is not null
   and (left(biz_type,4) <> 'E2E_' and biz_type <> 'D2_SMOKE')
   and (flow_code ~ '^(e2e|diag|diag2|vrf|d5|acc|tn)_');

select case when count(*) = 0 then 'PASS' else 'WARN' end, 'out_of_scope_orphan_instances',
       count(*) || ' 条存活实例的流程定义已软删（清场前 53，其中 37 条是本卡删掉的 E2E 实例）'
  from biz_flow_instance i
 where i.deleted_at is null
   and not exists (select 1 from biz_flow_def d where d.id = i.def_id and d.deleted_at is null);
