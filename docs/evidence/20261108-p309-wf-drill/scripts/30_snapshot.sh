#!/usr/bin/env bash
# ============================================================================
# P3-09 证据 · 关键 DB 行快照（实例停掉之后跑，库保留待复核）
#
#   bash docs/evidence/20261108-p309-wf-drill/scripts/30_snapshot.sh
#
# 只读本机测试库（127.0.0.1 / cloudpivot_p309_drill），144 生产库零读写。
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

OUT="$RAW/30_db_snapshot.txt"
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1
export PGPASSWORD=alun
q() { psql -h 127.0.0.1 -U alun -d "$DB" -Atc "$1"; }

say "================ P3-09 关键 DB 行快照（$DB）================"
say ""
say "--- 0) 迁移与夹具规模 ---"
q "select 'schema_migrations versioned=' || count(*) || ' max_version=' || max(version)
     from schema_migrations where kind='versioned'"
q "select '夹具角色 P309_* = ' || count(*) from sys_role where role_code like 'P309%' and deleted_at is null"
q "select '夹具用户 = ' || count(*) from sys_user where username in
     ('u_t1','u_t2','u_t3','u_a1','u_a2','u_a3','u_m1','u_b1','u_b2','u_l1','u_fb','u_solo','u_del','u_fin','u_init','u_pa','u_pb','u_pbn','u_pc')"

say ""
say "--- 1) 演练产生的实例（按流程分组）---"
q "select flow_code || ' | 实例数=' || count(*) || ' | 状态=' ||
          string_agg(distinct status, ',') || ' | 版本号=' || string_agg(distinct version_no::text, ',')
     from biz_flow_instance where flow_code like 'p309%' group by flow_code order by flow_code"
say ""
say "--- 2) 待办留痕：四类降级/自审各一行样例 ---"
q "select '升级 | ' || assignee_name || ' | ' || status || ' | rule=' || coalesce(assigned_by_rule,'-') ||
          ' | from_role=' || coalesce(escalated_from_role,'-') || ' | reason=' || coalesce(escalate_reason,'-')
     from biz_flow_task where escalated_from_role is not null order by created_at desc limit 2"
q "select '兜底 | ' || assignee_name || ' | ' || status || ' | rule=' || coalesce(assigned_by_rule,'-')
     from biz_flow_task where assigned_by_rule='FALLBACK' order by created_at desc limit 2"
q "select '委托 | ' || assignee_name || ' | ' || status || ' | rule=' || coalesce(assigned_by_rule,'-') ||
          ' | ' || coalesce(comment,'-')
     from biz_flow_task where assigned_by_rule='DELEGATED' order by created_at desc limit 2"
q "select '自审SKIP | ' || assignee_name || ' | ' || status || ' | ' || coalesce(comment,'-')
     from biz_flow_task where status='SKIPPED' order by created_at desc limit 3"

say ""
say "--- 3) 自审 ALLOW 审计（operation_logs）---"
q "select operation || ' | ' || coalesce(username,'') || ' | node=' || coalesce(detail->>'node_name','') ||
          ' | amount=' || coalesce(detail->>'amount','') || ' | vote=' || coalesce(detail->>'vote','')
     from operation_logs where operation like 'FLOW_SELF_APPROVAL_%' order by created_at"

say ""
say "--- 4) 驳回四目标：round 与 RESUBMIT ---"
q "select 'RESUBMIT 待办数=' || count(*) from biz_flow_task where task_kind='RESUBMIT'"
q "select 'round>1 的实例节点行数=' || count(*) from biz_flow_instance_node where round > 1"
q "select i.instance_no || ' | max_round=' || max(n.round) || ' | round_total=' || max(i.round_total)
     from biz_flow_instance i join biz_flow_instance_node n on n.instance_id=i.id
    where i.flow_code='p309_c' group by i.instance_no, i.round_total order by 1 desc limit 5"

say ""
say "--- 5) 并行网关 / 条件路由 / 版本冻结 关键行 ---"
say "  [B1 并行] 汇聚节点 j 的到达与放行："
q "select i.instance_no || ' | j=' || n.status || ' | arrived_branches=' ||
          (select count(*) from biz_flow_instance_node n2 where n2.instance_id=i.id
            and n2.node_key in ('n_a','n_b') and n2.status in ('APPROVED','SKIPPED')) || '/2'
     from biz_flow_instance i join biz_flow_instance_node n on n.instance_id=i.id and n.node_key='j'
    where i.flow_code='p309_b1_par' order by i.started_at desc limit 3"
say "  [B2 条件] 每个实例走过的边："
q "select i.title || ' | ' || n.node_key || ' -> ' || coalesce(n.taken_edge_key,'-')
     from biz_flow_instance i join biz_flow_instance_node n on n.instance_id=i.id
    where i.flow_code='p309_b2' and n.taken_edge_key is not null order by i.started_at desc, n.node_key limit 12"
say "  [B2b 缺陷] END 默认 AND 的实例（应 APPROVED，实际 PENDING）："
q "select i.instance_no || ' | inst=' || i.status || ' | END=' ||
          coalesce((select status from biz_flow_instance_node n where n.instance_id=i.id and n.node_key='e'),'-')
     from biz_flow_instance i where i.flow_code='p309_b2_end_and' order by i.started_at desc limit 3"
say "  [B3 版本冻结] 新旧实例绑的版本与节点名："
q "select i.instance_no || ' | v' || i.version_no || ' | ' || i.title || ' | node=' ||
          (select node_name from biz_flow_instance_node n where n.instance_id=i.id and n.node_key='n_a')
     from biz_flow_instance i where i.flow_code='p309_b3' order by i.started_at"

say ""
say "--- 6) 种子流程（回归面）现状：END 的 join_mode 口径 ---"
q "select d.flow_code || ' | END join_mode=' || coalesce(n->>'join_mode','<NULL>')
     from biz_flow_def d join biz_flow_def_version v on v.id=d.current_version_id,
          jsonb_array_elements(v.graph_snapshot->'nodes') n
    where n->>'node_type'='END' and d.flow_code in ('po_approve','pr_approve') order by 1"

say ""
say "--- 7) 端口复核 ---"
ss -lntp 2>/dev/null | grep -c ":18091 " | sed 's/^/18091 监听行数=/'
say "health=$(curl -s -o /dev/null -w '%{http_code}' --max-time 3 http://127.0.0.1:18091/health || true)"
