-- V0130 down：撤销工作任务目录化
DELETE FROM sys_role_menu
 WHERE menu_id IN (
     '20000000-0000-0000-0000-000000000900',
     '20000000-0000-0000-0000-000000000901'
 );

DELETE FROM sys_menu
 WHERE id IN (
     '20000000-0000-0000-0000-000000000900',
     '20000000-0000-0000-0000-000000000901'
 );

-- 工作任务改回菜单（原状）
UPDATE sys_menu
   SET menu_type = 2,
       component = 'business/task/index',
       updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000000760';

-- 甘特图还原 sort_order
UPDATE sys_menu
   SET sort_order = 60,
       updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000000770';
