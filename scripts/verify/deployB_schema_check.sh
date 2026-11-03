#!/usr/bin/env bash
# =============================================================================
# DEPLOY-B 迁移后结构自检（逐项对上卡面第 3/4 步的验证点）
#
# 用法（144 上）：
#   set -a; . /etc/cloudpivot/cloudpivot.env; set +a
#   bash scripts/verify/deployB_schema_check.sh <db>          # 默认 cloudpivot
#
# 判据（全部应满足）：
#   schema_migrations 里 201..206 六条 imported+success
#   ck_biz_material_costing = 单值 MOVING_AVG；违反 0 行；同表另 3 条 CHECK 完好
#   V0202 两个权限码（approval-flow:health / skip-approval）
#   biz_stock_out 21 列 / biz_stock_out_item 13 列；6 个 stock-out 权限码；STOCK_OUT 取号规则
#   biz_flow_state_edge + UNIQUE(entity,from_status,to_status)
#   biz_flow_linkage_rule + ck_flow_linkage_action_code（11 个白名单动作）+ 禁 SQL 键 CHECK
#   V0206 两个权限码 + FLOW_CONFIG_APPROVER 角色 + flow_edge_config 种子流程（PUBLISHED, 3 节点）
#   四张新表合计 = 4
# =============================================================================
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="${NIK_DATABASE_PASSWORD:?}"
DB="${1:-cloudpivot}"
P() { psql -h "${NIK_DATABASE_HOST:-127.0.0.1}" -U "${NIK_DATABASE_USER:-cloudpivot}" -d "$DB" -Atq -c "$1"; }
H() { echo; echo "----- $1"; }

H "schema_migrations ≥201"
P "select version||' | '||name||' | '||applied_via||' | '||success from schema_migrations where kind='versioned' and version::int>=201 order by version::int"

H "V0201 ck_biz_material_costing 定义"
P "select pg_get_constraintdef(oid) from pg_constraint where conrelid='biz_material'::regclass and conname='ck_biz_material_costing'"
H "V0201 违反行数（应为 0）"
P "select count(*) from biz_material where (costing_method='MOVING_AVG') is not true"
H "V0201 同表其余 CHECK 仍在（应为 3）"
P "select count(*) from pg_constraint where conrelid='biz_material'::regclass and contype='c' and conname in ('ck_biz_material_type','ck_biz_material_safety','ck_biz_material_tax')"

H "V0202 权限码 1200/1201"
P "select string_agg(auth_code, ', ' order by auth_code) from sys_menu where id in ('20000000-0000-0000-0000-000000001200','20000000-0000-0000-0000-000000001201')"

H "V0203 两张表 + 列数"
P "select table_name, count(*) from information_schema.columns where table_schema='public' and table_name in ('biz_stock_out','biz_stock_out_item') group by 1 order by 1"
H "V0203 权限码 6 个 + 取号规则"
P "select count(*) from sys_menu where auth_code like 'business:erp:stock-out:%'"
P "select seq_type||'|'||prefix||'|'||current_no from biz_seq where seq_type='STOCK_OUT'"

H "V0204 表 + 唯一约束"
P "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_flow_state_edge'"
P "select pg_get_constraintdef(oid) from pg_constraint where conrelid='biz_flow_state_edge'::regclass and contype='u'"

H "V0205 表 + action_code CHECK"
P "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_flow_linkage_rule'"
P "select conname||' :: '||pg_get_constraintdef(oid) from pg_constraint where conrelid='biz_flow_linkage_rule'::regclass and contype='c'"

H "V0206 权限码 1210/1211 + 角色 + 种子流程"
P "select string_agg(auth_code, ', ' order by auth_code) from sys_menu where id in ('20000000-0000-0000-0000-000000001210','20000000-0000-0000-0000-000000001211')"
P "select role_code||'|'||role_name from sys_role where role_code='FLOW_CONFIG_APPROVER'"
P "select flow_code||'|'||status||'|'||biz_type||'|'||current_version_no from biz_flow_def where flow_code='flow_edge_config'"
P "select count(*) from biz_flow_node where version_id='20000000-0000-4000-8000-0000000e0207'"

H "四张新表合计（期望 4）"
P "select count(*) from information_schema.tables where table_schema='public' and table_name in ('biz_stock_out','biz_stock_out_item','biz_flow_state_edge','biz_flow_linkage_rule')"
