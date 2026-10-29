-- T7b 看板验收夹具（生产库 cloudpivot）—— 幂等：先删再建
-- 造的账号只授「任务」的 read + status（无验收权），用来验 REVIEW→DONE 的 403。
-- 任务数据经 API 造（见 t7b_board_setup.py），这里只建账号与角色。

DELETE FROM sys_user_role WHERE user_id = '7f000000-0000-0000-0000-000000000b02'::uuid;
DELETE FROM sys_user WHERE id = '7f000000-0000-0000-0000-000000000b02'::uuid;
DELETE FROM sys_role_menu WHERE role_id = '7f000000-0000-0000-0000-000000000b01'::uuid;
DELETE FROM sys_role WHERE id = '7f000000-0000-0000-0000-000000000b01'::uuid;

INSERT INTO sys_role (id, role_code, role_name, description, status, data_scope)
VALUES ('7f000000-0000-0000-0000-000000000b01'::uuid, 'T7B_BOARDER', 'T7b看板验收（仅任务读+改状态）',
        '临时角色 · 验收后删除', 1, 4);

INSERT INTO sys_role_menu (role_id, menu_id)
SELECT '7f000000-0000-0000-0000-000000000b01'::uuid, m.id
  FROM (VALUES ('20000000-0000-0000-0000-000000000760'::uuid),
               ('20000000-0000-0000-0000-000000000761'::uuid),
               ('20000000-0000-0000-0000-000000000765'::uuid)) AS m(id);

INSERT INTO sys_user (id, username, password, real_name, status, must_change_password)
SELECT '7f000000-0000-0000-0000-000000000b02'::uuid, 'qa.boarder', a.password, '看板验收员', 1, FALSE
  FROM (SELECT password FROM sys_user WHERE username = 'admin' AND deleted_at IS NULL) AS a;

INSERT INTO sys_user_role (user_id, role_id)
VALUES ('7f000000-0000-0000-0000-000000000b02'::uuid, '7f000000-0000-0000-0000-000000000b01'::uuid);

SELECT u.username, u.real_name, r.role_code, r.data_scope
  FROM sys_user u JOIN sys_user_role ur ON ur.user_id = u.id JOIN sys_role r ON r.id = ur.role_id
 WHERE u.username = 'qa.boarder';
