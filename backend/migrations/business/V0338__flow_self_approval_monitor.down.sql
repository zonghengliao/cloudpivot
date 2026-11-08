-- V0338 回滚：删掉自审监控菜单行 + 授权行 + 自审金额上限参数。
--
-- 顺序：先删授权（sys_role_menu 有外键指 sys_menu），再删菜单，最后删参数。
-- 参数行按 config_key 删（不按 id：有环境的 id 是别的卡种进去的同名键会更安全地保留）。
DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000001243'::uuid;

DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000001243'::uuid;

-- 参数：只删本迁移种的那一行（id 精确匹配，避免误删客户自己加的同名键）
DELETE FROM sys_config
 WHERE id = 'e2b00000-0000-4000-8000-000000000005'::uuid
   AND config_key = 'erp.separation_of_duties.self_approval_max_amount';
