-- V0129 down：还原「流程定义」菜单为不可见（回到 V0121 的状态）
UPDATE sys_menu
   SET is_visible = FALSE,
       updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000000780';
