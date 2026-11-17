-- V0447 回滚：删除采购计划审批流程种子 + 6 行菜单 + 角色授权。
--
-- 顺序：先删实例/待办里**本流程**的行（否则 `biz_flow_task` / `biz_flow_instance` 的外键
-- 会挡住 node/edge 的删除），再删图 → 版本 → 定义；最后删授权与菜单（先按钮后页面）。
--
-- 注意：
--   ① **不动 `biz_purchase_plan` 表的任何数据**（V0446 建的表与本迁移无关），
--      也不把已经 APPROVED 的计划打回草稿 —— 批准是既成事实，回滚配置不负责编造历史；
--   ② 若库里有本流程在跑的实例，本脚本会先删掉它们（这是回滚流程定义的必要代价），
--      单据会停在 SUBMITTED 且没有待办 —— 生产回滚前应先让在跑的审批走完。

DELETE FROM biz_flow_task
 WHERE instance_id IN (
     SELECT id FROM biz_flow_instance WHERE biz_type = 'PP_APPROVE'
 );

DELETE FROM biz_flow_instance WHERE biz_type = 'PP_APPROVE';

DELETE FROM biz_flow_node_assignee
 WHERE node_id IN (
     SELECT id FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000d0482'::uuid
 );

DELETE FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000d0482'::uuid;
DELETE FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000d0482'::uuid;
DELETE FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0482'::uuid;
DELETE FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d0481'::uuid;

DELETE FROM sys_role_menu
 WHERE menu_id IN (SELECT unnest(ARRAY[
        '20000000-0000-0000-0000-000000004471'::uuid,
        '20000000-0000-0000-0000-000000004472'::uuid,
        '20000000-0000-0000-0000-000000004473'::uuid,
        '20000000-0000-0000-0000-000000004474'::uuid,
        '20000000-0000-0000-0000-000000004475'::uuid,
        '20000000-0000-0000-0000-000000004476'::uuid]));

DELETE FROM sys_menu
 WHERE id IN (SELECT unnest(ARRAY[
        '20000000-0000-0000-0000-000000004472'::uuid,
        '20000000-0000-0000-0000-000000004473'::uuid,
        '20000000-0000-0000-0000-000000004474'::uuid,
        '20000000-0000-0000-0000-000000004475'::uuid,
        '20000000-0000-0000-0000-000000004476'::uuid]));

DELETE FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000004471'::uuid;
