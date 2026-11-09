#!/usr/bin/env bash
# E2E-09 演练数据自清：**只删本卡 E2E09 前缀造的行**（不 DROP / TRUNCATE / 不动他人数据）
# 顺序按外键依赖：关联表 → 业务行 → 主表
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/e2e09_common.sh"

say "--- 清理前本卡存活行 ---"
SQ "select 'user='||count(*) from sys_user where username like 'e2e09%' and deleted_at is null
 union all select 'role='||count(*) from sys_role where role_code like 'E2E09%' and deleted_at is null
 union all select 'dept='||count(*) from sys_dept where dept_code like 'E2E09-%' and deleted_at is null
 union all select 'menu='||count(*) from sys_menu where deleted_at is null and (path like '/e2e09%' or name like 'e2e09_%')
 union all select 'mask='||count(*) from sys_field_mask where deleted_at is null and reason like 'E2E09%'
 union all select 'mutex='||count(*) from sys_role_mutex where reason like 'E2E09%'
 union all select 'flowdef='||count(*) from biz_flow_def where flow_code like 'e2e09_%' and deleted_at is null
 union all select 'flowinst='||count(*) from biz_flow_instance where title like 'E2E09%'"

PSQLQ >/dev/null <<'SQL'
BEGIN;
-- ⚠ 顺序由外键决定：**流程/任务的子表 → 实例 → 定义**，全部在删用户之前
-- （biz_flow_task.assignee_id → sys_user；biz_task.flow_instance_id → biz_flow_instance）
-- 任务（数据权限演练载体）
DELETE FROM biz_task_member WHERE task_id IN (SELECT id FROM biz_task WHERE title LIKE 'E2E09%');
DELETE FROM biz_task_comment WHERE task_id IN (SELECT id FROM biz_task WHERE title LIKE 'E2E09%');
DELETE FROM biz_task WHERE title LIKE 'E2E09%';
-- 流程实例侧
DELETE FROM biz_flow_task WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE title LIKE 'E2E09%');
DELETE FROM biz_flow_cc WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE title LIKE 'E2E09%');
DELETE FROM biz_flow_instance_node WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE title LIKE 'E2E09%');
DELETE FROM biz_flow_instance WHERE title LIKE 'E2E09%';
-- 流程定义侧（边 → 节点审批人 → 节点 → 版本 → 定义）
DELETE FROM biz_flow_edge WHERE version_id IN (
  SELECT v.id FROM biz_flow_def_version v JOIN biz_flow_def d ON d.id = v.def_id WHERE d.flow_code LIKE 'e2e09_%');
DELETE FROM biz_flow_node_assignee WHERE node_id IN (
  SELECT n.id FROM biz_flow_node n JOIN biz_flow_def_version v ON v.id = n.version_id
    JOIN biz_flow_def d ON d.id = v.def_id WHERE d.flow_code LIKE 'e2e09_%');
DELETE FROM biz_flow_node WHERE version_id IN (
  SELECT v.id FROM biz_flow_def_version v JOIN biz_flow_def d ON d.id = v.def_id WHERE d.flow_code LIKE 'e2e09_%');
DELETE FROM biz_flow_def_version WHERE def_id IN (SELECT id FROM biz_flow_def WHERE flow_code LIKE 'e2e09_%');
DELETE FROM biz_flow_def WHERE flow_code LIKE 'e2e09_%';
-- 系统侧：关联表 → 脱敏/互斥 → 用户/角色/部门/菜单
DELETE FROM sys_role_dept WHERE role_id IN (SELECT id FROM sys_role WHERE role_code LIKE 'E2E09%');
DELETE FROM sys_role_menu WHERE role_id IN (SELECT id FROM sys_role WHERE role_code LIKE 'E2E09%');
DELETE FROM sys_user_role WHERE user_id IN (SELECT id FROM sys_user WHERE username LIKE 'e2e09%')
                             OR role_id IN (SELECT id FROM sys_role WHERE role_code LIKE 'E2E09%');
DELETE FROM sys_field_mask WHERE reason LIKE 'E2E09%';
DELETE FROM sys_role_mutex WHERE reason LIKE 'E2E09%';
DELETE FROM sys_user WHERE username LIKE 'e2e09%';
DELETE FROM sys_role WHERE role_code LIKE 'E2E09%';
DELETE FROM sys_dept WHERE dept_code LIKE 'E2E09-%';
DELETE FROM sys_menu WHERE path LIKE '/e2e09%' OR name LIKE 'e2e09_%';
COMMIT;
SQL

say "--- 清理后本卡存活行（应全 0）---"
SQ "select 'user='||count(*) from sys_user where username like 'e2e09%'
 union all select 'role='||count(*) from sys_role where role_code like 'E2E09%'
 union all select 'dept='||count(*) from sys_dept where dept_code like 'E2E09-%'
 union all select 'menu='||count(*) from sys_menu where path like '/e2e09%' or name like 'e2e09_%'
 union all select 'mask='||count(*) from sys_field_mask where reason like 'E2E09%'
 union all select 'mutex='||count(*) from sys_role_mutex where reason like 'E2E09%'
 union all select 'flowdef='||count(*) from biz_flow_def where flow_code like 'e2e09_%'
 union all select 'flowinst='||count(*) from biz_flow_instance where title like 'E2E09%'"
