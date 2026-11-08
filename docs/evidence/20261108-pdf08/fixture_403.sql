-- PDF-08 浏览器 403 用例夹具（cloudpivot_pdf07）
-- 角色：能看资产负债表（report:balance-sheet），但没有导出码（report:export）
BEGIN;

INSERT INTO sys_role (role_code, role_name, status, sort_order, remark)
VALUES ('PDF08_RPT_NOEXPORT', 'PDF08 报表无导出权限', 1, 972, 'PDF-08 浏览器 403 用例')
ON CONFLICT (role_code) DO UPDATE SET status = 1;

DELETE FROM sys_role_menu
 WHERE role_id = (SELECT id FROM sys_role WHERE role_code = 'PDF08_RPT_NOEXPORT' AND deleted_at IS NULL);

INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, now()
  FROM sys_role r
  JOIN sys_menu m ON m.auth_code = 'business:erp:report:balance-sheet' AND m.deleted_at IS NULL
 WHERE r.role_code = 'PDF08_RPT_NOEXPORT' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- 用户：密码哈希与 admin 相同（= Admin@12345678），只挂这一个角色
INSERT INTO sys_user (username, password, real_name, status, must_change_password, user_type)
SELECT 'pdf08_rpt_noperm', u.password, 'PDF08 报表无导出', 1, false, u.user_type
  FROM sys_user u
 WHERE u.username = 'admin' AND u.deleted_at IS NULL
ON CONFLICT (username) DO UPDATE SET password = EXCLUDED.password, status = 1, must_change_password = false;

DELETE FROM sys_user_role
 WHERE user_id = (SELECT id FROM sys_user WHERE username = 'pdf08_rpt_noperm' AND deleted_at IS NULL);

INSERT INTO sys_user_role (user_id, role_id, created_at)
SELECT us.id, r.id, now()
  FROM sys_user us, sys_role r
 WHERE us.username = 'pdf08_rpt_noperm' AND us.deleted_at IS NULL
   AND r.role_code = 'PDF08_RPT_NOEXPORT' AND r.deleted_at IS NULL
ON CONFLICT (user_id, role_id) DO NOTHING;

COMMIT;

SELECT r.role_code, coalesce(string_agg(m.auth_code, ','), '-') AS perms
  FROM sys_role r
  LEFT JOIN sys_role_menu p ON p.role_id = r.id
  LEFT JOIN sys_menu m ON m.id = p.menu_id
 WHERE r.role_code = 'PDF08_RPT_NOEXPORT'
 GROUP BY r.role_code;
