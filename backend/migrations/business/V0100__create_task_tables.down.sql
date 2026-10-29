-- V0100 回滚：工作任务管理 7 张表（逆序 DROP：先子表后父表）
--
-- 顺序不是「好看」而是必须：biz_task 的外键被 member / comment / attachment /
-- dependency / worklog 引用，先 DROP 父表会因依赖被拒绝（即使 PG 允许 CASCADE 删，
-- 也不该依赖 CASCADE 隐式删子表 —— 回滚脚本要能逐条核对）。
DROP TABLE IF EXISTS biz_task_worklog;
DROP TABLE IF EXISTS biz_task_dependency;
DROP TABLE IF EXISTS biz_task_attachment;
DROP TABLE IF EXISTS biz_task_comment;
DROP TABLE IF EXISTS biz_task_member;
DROP TABLE IF EXISTS biz_task;
DROP TABLE IF EXISTS biz_project;
