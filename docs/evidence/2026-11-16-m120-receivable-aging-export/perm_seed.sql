-- M1-20 权限对照用户（可重复跑）：有 business:erp:receivable:aging，没有 business:erp:report:export。
-- 角色 SALES_SUPERVISOR 在本库的 sys_role_menu 里正好是这一对（推盘实测）：
--   aging=1 / report:export=0 —— 所以「报表 200 + 导出 403」这一对能证明导出的闸是
--   独立权限码在管，不是「能看报表就能导」。
-- 口令沿用 admin 的哈希（登录口令同 admin，仅用于本夹具库）。
DELETE FROM sys_user_role
 WHERE user_id IN (SELECT id FROM sys_user WHERE username = 'm120sales');
DELETE FROM sys_user WHERE username = 'm120sales';

INSERT INTO sys_user (username, password, real_name, status, must_change_password, dept_id)
SELECT 'm120sales', password, 'M120 销售主管', 1, false, dept_id
  FROM sys_user WHERE username = 'admin';

INSERT INTO sys_user_role (user_id, role_id)
SELECT (SELECT id FROM sys_user WHERE username = 'm120sales'),
       (SELECT id FROM sys_role WHERE role_code = 'SALES_SUPERVISOR');

SELECT u.username, r.role_code
  FROM sys_user u JOIN sys_user_role ur ON ur.user_id = u.id JOIN sys_role r ON r.id = ur.role_id
 WHERE u.username = 'm120sales';
