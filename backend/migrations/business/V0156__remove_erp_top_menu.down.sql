-- 回滚：恢复 ERP 顶级目录，4 个业务目录挂回去
BEGIN;
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, component, parent_id, sort_order, icon, title, is_visible)
VALUES ('20000000-0000-0000-0000-000000000940','ERP',1,'erp','/business/erp',NULL,NULL,60,'app','ERP',true)
ON CONFLICT (id) DO UPDATE SET parent_id = NULL, sort_order = 60, updated_at = now();

UPDATE sys_menu SET parent_id='20000000-0000-0000-0000-000000000940', sort_order=10, updated_at=now()
WHERE id='20000000-0000-0000-0000-000000000946';
UPDATE sys_menu SET parent_id='20000000-0000-0000-0000-000000000940', sort_order=20, updated_at=now()
WHERE id='20000000-0000-0000-0000-000000000e01';
UPDATE sys_menu SET parent_id='20000000-0000-0000-0000-000000000940', sort_order=30, updated_at=now()
WHERE id='20000000-0000-0000-0000-000000000e02';
UPDATE sys_menu SET parent_id='20000000-0000-0000-0000-000000000940', sort_order=40, updated_at=now()
WHERE id='20000000-0000-0000-0000-000000000e03';
COMMIT;
