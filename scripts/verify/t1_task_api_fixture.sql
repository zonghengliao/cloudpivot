-- T1 接口走查夹具（在验证库 cloudpivot_t1v 上执行）
--
-- 造一套跨部门组织：研发部（含研发一组）与市场部，
-- 4 个普通账号（权限码从 sys_menu 授权，密码哈希直接抄 admin —— 便于 curl 真登录）。
-- 幂等：先按固定 UUID 删一遍，再插。

-- ---------- 0) 清理（反外键序） ----------
DELETE FROM biz_task WHERE creator_id IN (
    '7d000000-0000-0000-0000-000000000001'::uuid,
    '7d000000-0000-0000-0000-000000000002'::uuid,
    '7d000000-0000-0000-0000-000000000003'::uuid,
    '7d000000-0000-0000-0000-000000000004'::uuid);
DELETE FROM sys_user_role WHERE user_id IN (
    '7d000000-0000-0000-0000-000000000001'::uuid,
    '7d000000-0000-0000-0000-000000000002'::uuid,
    '7d000000-0000-0000-0000-000000000003'::uuid,
    '7d000000-0000-0000-0000-000000000004'::uuid);
DELETE FROM sys_user WHERE id IN (
    '7d000000-0000-0000-0000-000000000001'::uuid,
    '7d000000-0000-0000-0000-000000000002'::uuid,
    '7d000000-0000-0000-0000-000000000003'::uuid,
    '7d000000-0000-0000-0000-000000000004'::uuid);
DELETE FROM sys_role_menu WHERE role_id IN (
    '7b000000-0000-0000-0000-000000000001'::uuid,
    '7b000000-0000-0000-0000-000000000002'::uuid);
DELETE FROM sys_role WHERE id IN (
    '7b000000-0000-0000-0000-000000000001'::uuid,
    '7b000000-0000-0000-0000-000000000002'::uuid);
DELETE FROM sys_dept WHERE id IN (
    '7a000000-0000-0000-0000-000000000001'::uuid,
    '7a000000-0000-0000-0000-000000000002'::uuid,
    '7a000000-0000-0000-0000-000000000003'::uuid);

-- ---------- 1) 部门 ----------
INSERT INTO sys_dept (id, dept_code, dept_name, parent_id, status, sort_order) VALUES
    ('7a000000-0000-0000-0000-000000000001'::uuid, 'T1_R', 'T1研发部', NULL, 1, 10),
    ('7a000000-0000-0000-0000-000000000002'::uuid, 'T1_R_SUB', 'T1研发一组',
     '7a000000-0000-0000-0000-000000000001'::uuid, 1, 20),
    ('7a000000-0000-0000-0000-000000000003'::uuid, 'T1_M', 'T1市场部', NULL, 1, 30);

-- ---------- 2) 角色（数据范围 4 仅本人 / 3 本部门及以下） ----------
INSERT INTO sys_role (id, role_code, role_name, status, data_scope) VALUES
    ('7b000000-0000-0000-0000-000000000001'::uuid, 'T1_SELF', 'T1仅本人', 1, 4),
    ('7b000000-0000-0000-0000-000000000002'::uuid, 'T1_DEPT_CHILD', 'T1本部门及以下', 1, 3);

-- 新角色不在种子迁移的授权范围内（种子只在启动时给「当时已有的角色」授权），
-- 因此这里显式授权 —— 等价于运维在角色授权页勾上「工作任务」5 个节点。
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
  FROM sys_role r
 CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000000760'::uuid),
                    ('20000000-0000-0000-0000-000000000761'::uuid),
                    ('20000000-0000-0000-0000-000000000762'::uuid),
                    ('20000000-0000-0000-0000-000000000763'::uuid),
                    ('20000000-0000-0000-0000-000000000764'::uuid)) AS m(id)
 WHERE r.id IN ('7b000000-0000-0000-0000-000000000001'::uuid,
                '7b000000-0000-0000-0000-000000000002'::uuid)
   AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id);

-- ---------- 3) 用户（密码哈希抄 admin，便于 curl 真登录） ----------
-- 验证库里的 admin 是 bootstrap 新建的，带 must_change_password=TRUE（CODE-06 首登改密），
-- 会让除改密/登出以外的接口全部 403。走查要拿 admin 令牌调任务接口，所以在这里关掉 ——
-- **只对验证库**（cloudpivot_t1v）这么做，prod/dev 的 admin 不受影响。
UPDATE sys_user SET must_change_password = FALSE WHERE username = 'admin' AND deleted_at IS NULL;

INSERT INTO sys_user (id, username, password, real_name, dept_id, status, must_change_password)
SELECT u.id, u.username, a.password, u.real_name, u.dept_id, 1, FALSE
  FROM (VALUES
        ('7d000000-0000-0000-0000-000000000001'::uuid, 't1_emp',   'T1员工（研发一组）',
         '7a000000-0000-0000-0000-000000000002'::uuid),
        ('7d000000-0000-0000-0000-000000000002'::uuid, 't1_mgr',   'T1主管（研发部）',
         '7a000000-0000-0000-0000-000000000001'::uuid),
        ('7d000000-0000-0000-0000-000000000003'::uuid, 't1_cross', 'T1协作人（市场部）',
         '7a000000-0000-0000-0000-000000000003'::uuid),
        ('7d000000-0000-0000-0000-000000000004'::uuid, 't1_other', 'T1市场同事（市场部）',
         '7a000000-0000-0000-0000-000000000003'::uuid)
       ) AS u(id, username, real_name, dept_id)
 CROSS JOIN (SELECT password FROM sys_user WHERE username = 'admin' AND deleted_at IS NULL) AS a;

INSERT INTO sys_user_role (user_id, role_id) VALUES
    ('7d000000-0000-0000-0000-000000000001'::uuid, '7b000000-0000-0000-0000-000000000001'::uuid),
    ('7d000000-0000-0000-0000-000000000002'::uuid, '7b000000-0000-0000-0000-000000000002'::uuid),
    ('7d000000-0000-0000-0000-000000000003'::uuid, '7b000000-0000-0000-0000-000000000001'::uuid),
    ('7d000000-0000-0000-0000-000000000004'::uuid, '7b000000-0000-0000-0000-000000000001'::uuid);

-- ---------- 4) 自检 ----------
DO $$
DECLARE n int;
BEGIN
    SELECT count(*) INTO n FROM sys_user
     WHERE id::text LIKE '7d000000-0000-0000-0000-00000000000%' AND deleted_at IS NULL;
    IF n <> 4 THEN RAISE EXCEPTION '夹具用户数 % ≠ 4', n; END IF;
    SELECT count(*) INTO n FROM sys_role_menu
     WHERE role_id IN ('7b000000-0000-0000-0000-000000000001'::uuid,
                       '7b000000-0000-0000-0000-000000000002'::uuid);
    IF n <> 10 THEN RAISE EXCEPTION '夹具角色授权行 % ≠ 10', n; END IF;
END
$$;

SELECT u.username, u.id, d.dept_name, r.role_name, r.data_scope
  FROM sys_user u
  LEFT JOIN sys_dept d ON d.id = u.dept_id
  LEFT JOIN sys_user_role ur ON ur.user_id = u.id
  LEFT JOIN sys_role r ON r.id = ur.role_id
 WHERE u.id::text LIKE '7d000000-%'
 ORDER BY u.username;
