-- ============================================================================
-- E2ERES2 / 30：删后自检（每行 PASS / FAIL / WARN；apply.sh 见到 FAIL 即退出码 1）
-- ----------------------------------------------------------------------------
-- 硬 FAIL（不许当成功交付）：
--   ① 钉住的目标行在 9 张 flow 表里全部为 0
--   ② 全库没有半截残留：版本/节点/边/实例节点/待办/抄送 不许有孤儿父行；
--      「活实例的父定义已软删」= 0（这就是本卡要消灭的那类）
--   ③ 11 条 keeper 真流程：条数、deleted_at、**指纹逐字节一致**
--   ④ keeper 的版本/节点/边行数与基线一致
--   ⑤ 静态配置表（def/version/node/edge/node_assignee）总量 = 删除前 - 实际删除
--   ⑥ 审计证据表 biz_status_log：行数没少 + pin 前那段指纹不变（本卡一行不删）
-- WARN（只提示，不影响退出码）：
--   * 运行期表（instance / instance_node / task / cc）总量比「删除前 - 删除」**多**
--     —— 那是清场后有人正常发起了审批（不是数据修复动过）
--   * 库里还有**其它**同类痕迹（前缀判据新命中的定义 / laoliu_test 建的 0 实例软删定义）
--     —— 不在卡面枚举内，本卡不动，只报警，别让它悄悄消失
-- ============================================================================
\pset tuples_only on
\pset format unaligned
\pset fieldsep '|'

-- ① 钉住的目标行全部消失
select case when count(*) = 0 then 'PASS' else 'FAIL' end, 'target_biz_flow_def_gone',
       '钉住的 ' || (select count(*) from datafix_e2eres2_target
                      where batch='E2ERES2-20261104' and kind='def')
       || ' 条定义里还剩 ' || count(*) || ' 条'
  from biz_flow_def
 where id in (select id from datafix_e2eres2_target where batch='E2ERES2-20261104' and kind='def');

select case when count(*) = 0 then 'PASS' else 'FAIL' end, 'target_biz_flow_instance_gone',
       '钉住的 ' || (select count(*) from datafix_e2eres2_target
                      where batch='E2ERES2-20261104' and kind='inst')
       || ' 条实例里还剩 ' || count(*) || ' 条'
  from biz_flow_instance
 where id in (select id from datafix_e2eres2_target where batch='E2ERES2-20261104' and kind='inst');

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

select case when count(*) = 0 then 'PASS' else 'FAIL' end, 'orphan_biz_flow_node_assignee',
       count(*) || ' 条节点审批人没有父节点'
  from biz_flow_node_assignee a
 where not exists (select 1 from biz_flow_node n where n.id = a.node_id);

select case when count(*) = 0 then 'PASS' else 'FAIL' end, 'orphan_biz_flow_instance_phys',
       count(*) || ' 个实例的父定义**物理**不存在'
  from biz_flow_instance i
 where not exists (select 1 from biz_flow_def d where d.id = i.def_id);

select case when count(*) = 0 then 'PASS' else 'FAIL' end, 'orphan_biz_flow_instance_softdel',
       count(*) || ' 条活实例的父定义已软删（本卡要消灭的就是这类）'
  from biz_flow_instance i
 where i.deleted_at is null
   and not exists (select 1 from biz_flow_def d where d.id = i.def_id and d.deleted_at is null);

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

-- ③ keeper：条数 + 指纹（真流程一个字节都不许动）
select case when count(*) = (select val::int from datafix_e2eres2_guard
                              where batch='E2ERES2-20261104' and item='keepers_count')
            then 'PASS' else 'FAIL' end,
       'keeper_alive_count',
       '实际 ' || count(*) || ' 条（基线 ' ||
       (select val from datafix_e2eres2_guard where batch='E2ERES2-20261104' and item='keepers_count')
       || '）'
  from biz_flow_def
 where deleted_at is null
   and id in (select id from datafix_e2eres2_target where batch='E2ERES2-20261104' and kind='keeper');

select case when fp = (select val from datafix_e2eres2_guard
                        where batch='E2ERES2-20261104' and item='keepers_fingerprint')
            then 'PASS' else 'FAIL' end,
       'keeper_fingerprint', coalesce(fp, '(空)')
  from (
    select md5(string_agg(x, E'\n' order by x)) as fp
      from (select id::text||'|'||flow_code||'|'||flow_name||'|'||biz_type||'|'||status||'|'
                   ||coalesce(current_version_id::text,'-')||'|'||current_version_no||'|'
                   ||coalesce(category,'-')||'|'||is_default||'|'||allow_auto_approve||'|'
                   ||coalesce(owner_dept_id::text,'-')||'|'||created_at||'|'||updated_at as x
              from biz_flow_def
             where deleted_at is null
               and id in (select id from datafix_e2eres2_target
                           where batch='E2ERES2-20261104' and kind='keeper')) t
  ) s;

-- ④ keeper 的版本/节点/边行数
select case when count(*) = (select val::int from datafix_e2eres2_guard
                              where batch='E2ERES2-20261104' and item='keeper_versions')
            then 'PASS' else 'FAIL' end, 'keeper_versions',
       count(*) || ' 行（基线 ' ||
       (select val from datafix_e2eres2_guard where batch='E2ERES2-20261104' and item='keeper_versions') || '）'
  from biz_flow_def_version
 where def_id in (select id from datafix_e2eres2_target where batch='E2ERES2-20261104' and kind='keeper');

select case when count(*) = (select val::int from datafix_e2eres2_guard
                              where batch='E2ERES2-20261104' and item='keeper_nodes')
            then 'PASS' else 'FAIL' end, 'keeper_nodes',
       count(*) || ' 行（基线 ' ||
       (select val from datafix_e2eres2_guard where batch='E2ERES2-20261104' and item='keeper_nodes') || '）'
  from biz_flow_node
 where version_id in (select id from biz_flow_def_version
                       where def_id in (select id from datafix_e2eres2_target
                                         where batch='E2ERES2-20261104' and kind='keeper'));

select case when count(*) = (select val::int from datafix_e2eres2_guard
                              where batch='E2ERES2-20261104' and item='keeper_edges')
            then 'PASS' else 'FAIL' end, 'keeper_edges',
       count(*) || ' 行（基线 ' ||
       (select val from datafix_e2eres2_guard where batch='E2ERES2-20261104' and item='keeper_edges') || '）'
  from biz_flow_edge
 where version_id in (select id from biz_flow_def_version
                       where def_id in (select id from datafix_e2eres2_target
                                         where batch='E2ERES2-20261104' and kind='keeper'));

-- ⑤ 静态配置表：总量 == 删除前 - 实际删除
select case when actual = expected then 'PASS' else 'FAIL' end, 'count_biz_flow_def',
       'actual=' || actual || ' expected=' || expected
  from (select (select count(*) from biz_flow_def) as actual,
               (select val::int - coalesce((select val::int from datafix_e2eres2_guard
                                             where batch='E2ERES2-20261104' and item='deleted_biz_flow_def'), 0)
                  from datafix_e2eres2_guard where batch='E2ERES2-20261104' and item='table_biz_flow_def_before') as expected) s;

select case when actual = expected then 'PASS' else 'FAIL' end, 'count_biz_flow_def_version',
       'actual=' || actual || ' expected=' || expected
  from (select (select count(*) from biz_flow_def_version) as actual,
               (select val::int - coalesce((select val::int from datafix_e2eres2_guard
                                             where batch='E2ERES2-20261104' and item='deleted_biz_flow_def_version'), 0)
                  from datafix_e2eres2_guard where batch='E2ERES2-20261104' and item='table_biz_flow_def_version_before') as expected) s;

select case when actual = expected then 'PASS' else 'FAIL' end, 'count_biz_flow_node',
       'actual=' || actual || ' expected=' || expected
  from (select (select count(*) from biz_flow_node) as actual,
               (select val::int - coalesce((select val::int from datafix_e2eres2_guard
                                             where batch='E2ERES2-20261104' and item='deleted_biz_flow_node'), 0)
                  from datafix_e2eres2_guard where batch='E2ERES2-20261104' and item='table_biz_flow_node_before') as expected) s;

select case when actual = expected then 'PASS' else 'FAIL' end, 'count_biz_flow_edge',
       'actual=' || actual || ' expected=' || expected
  from (select (select count(*) from biz_flow_edge) as actual,
               (select val::int - coalesce((select val::int from datafix_e2eres2_guard
                                             where batch='E2ERES2-20261104' and item='deleted_biz_flow_edge'), 0)
                  from datafix_e2eres2_guard where batch='E2ERES2-20261104' and item='table_biz_flow_edge_before') as expected) s;

select case when actual = expected then 'PASS' else 'FAIL' end, 'count_biz_flow_node_assignee',
       'actual=' || actual || ' expected=' || expected
  from (select (select count(*) from biz_flow_node_assignee) as actual,
               (select val::int - coalesce((select val::int from datafix_e2eres2_guard
                                             where batch='E2ERES2-20261104' and item='deleted_biz_flow_node_assignee'), 0)
                  from datafix_e2eres2_guard where batch='E2ERES2-20261104' and item='table_biz_flow_node_assignee_before') as expected) s;

-- ⑤b 运行期表：只允许「比预期多」（清场后有人正常发起审批），少了才 FAIL
select case when actual >= expected then 'PASS' else 'FAIL' end, 'count_biz_flow_instance',
       'actual=' || actual || ' expected>=' || expected
  from (select (select count(*) from biz_flow_instance) as actual,
               (select val::int - coalesce((select val::int from datafix_e2eres2_guard
                                             where batch='E2ERES2-20261104' and item='deleted_biz_flow_instance'), 0)
                  from datafix_e2eres2_guard where batch='E2ERES2-20261104' and item='table_biz_flow_instance_before') as expected) s;

select case when actual >= expected then 'PASS' else 'FAIL' end, 'count_biz_flow_instance_node',
       'actual=' || actual || ' expected>=' || expected
  from (select (select count(*) from biz_flow_instance_node) as actual,
               (select val::int - coalesce((select val::int from datafix_e2eres2_guard
                                             where batch='E2ERES2-20261104' and item='deleted_biz_flow_instance_node'), 0)
                  from datafix_e2eres2_guard where batch='E2ERES2-20261104' and item='table_biz_flow_instance_node_before') as expected) s;

select case when actual >= expected then 'PASS' else 'FAIL' end, 'count_biz_flow_task',
       'actual=' || actual || ' expected>=' || expected
  from (select (select count(*) from biz_flow_task) as actual,
               (select val::int - coalesce((select val::int from datafix_e2eres2_guard
                                             where batch='E2ERES2-20261104' and item='deleted_biz_flow_task'), 0)
                  from datafix_e2eres2_guard where batch='E2ERES2-20261104' and item='table_biz_flow_task_before') as expected) s;

select case when actual >= expected then 'PASS' else 'FAIL' end, 'count_biz_flow_cc',
       'actual=' || actual || ' expected>=' || expected
  from (select (select count(*) from biz_flow_cc) as actual,
               (select val::int - coalesce((select val::int from datafix_e2eres2_guard
                                             where batch='E2ERES2-20261104' and item='deleted_biz_flow_cc'), 0)
                  from datafix_e2eres2_guard where batch='E2ERES2-20261104' and item='table_biz_flow_cc_before') as expected) s;

-- ⑥ 审计证据表：行数没少 + pin 前那段指纹不变
select case when count(*) >= (select val::int from datafix_e2eres2_guard
                               where batch='E2ERES2-20261104' and item='biz_status_log_total')
            then 'PASS' else 'FAIL' end, 'biz_status_log_untouched',
       '实际 ' || count(*) || ' 行（基线 ' ||
       (select val from datafix_e2eres2_guard where batch='E2ERES2-20261104' and item='biz_status_log_total') || '）'
  from biz_status_log;

select case when fp = (select val from datafix_e2eres2_guard
                        where batch='E2ERES2-20261104' and item='biz_status_log_fingerprint')
            then 'PASS' else 'FAIL' end, 'biz_status_log_fingerprint', coalesce(fp, '(空)')
  from (
    select md5(string_agg(x, E'\n' order by x)) as fp
      from (select id::text||'|'||entity_type||'|'||coalesce(entity_no,'-')||'|'
                   ||coalesce(from_status,'-')||'|'||to_status||'|'||created_at as x
              from biz_status_log
             where created_at <= (select val::timestamptz from datafix_e2eres2_guard
                                   where batch='E2ERES2-20261104' and item='biz_status_log_pin_ts')) t
  ) s;

-- ⑦ WARN：其它同类痕迹（本卡范围外，只提示，别让它悄悄消失）
select case when count(*) = 0 then 'PASS' else 'WARN' end, 'out_of_scope_prefix_defs',
       count(*) || ' 条软删定义仍命中前缀判据（新冒出来的，或本卡没钉住的）'
  from biz_flow_def
 where deleted_at is not null
   and not (left(biz_type,4) = 'E2E_' or biz_type = 'D2_SMOKE')
   and flow_code ~ '^(e2e|diag|diag2|vrf|d5|acc|tn)_';

select case when count(*) = 0 then 'PASS' else 'WARN' end, 'out_of_scope_test_account_defs',
       count(*) || ' 条软删定义是测试账号 laoliu_test 建的、0 实例、但 flow_code 不在卡面前缀枚举内（同性质，未授权不动）'
  from biz_flow_def d
 where d.deleted_at is not null
   and d.create_by = (select id from sys_user where username = 'laoliu_test')
   and not exists (select 1 from biz_flow_instance i where i.def_id = d.id)
   and d.flow_code !~ '^(e2e|diag|diag2|vrf|d5|acc|tn)_'
   and not (left(d.biz_type,4) = 'E2E_' or d.biz_type = 'D2_SMOKE');
