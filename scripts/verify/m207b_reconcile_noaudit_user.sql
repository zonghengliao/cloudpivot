-- M2-07b 真机验收：造一个**只有页面 + 取数码、没有对账码**的角色与账号。
--
-- 用途：证明第三个 Tab「在途对账」是**按 `business:erp:inventory-transit:audit` 判显隐**的
-- （没有该码 → 看不到 Tab，但页面照常能开、明细/汇总照常能查 —— 不是整页 403）。
--
-- 手法（照技能里的实测套路）：
--   * 菜单授权只给三行：页面行 …4230（`...:page`）、取数按钮 …4210（`...:list`）、
--     祖先目录 …0e01（仓库管理；缺父目录 build_menu_tree 会静默丢子树）；
--   * 口令直接拷 admin 的哈希（演练库里 admin 已改成已知口令，见 api_check 脚本）——
--     免得为造一个只读账号去反向推 argon2。
--
-- 幂等：角色/用户按 role_code / username 判重；授权 DELETE 后重插。
-- 清场：DELETE FROM sys_user_role WHERE user_id = (SELECT id FROM sys_user WHERE username='m207b_noaudit');
--       DELETE FROM sys_role_menu WHERE role_id = (SELECT id FROM sys_role WHERE role_code='M207B_NO_AUDIT');
--       DELETE FROM sys_user WHERE username='m207b_noaudit';
--       DELETE FROM sys_role WHERE role_code='M207B_NO_AUDIT';

BEGIN;

INSERT INTO sys_role (id, role_code, role_name, description, status, sort_order, data_scope)
VALUES ('207b0000-0000-4000-8000-000000000201'::uuid, 'M207B_NO_AUDIT', '在途只读（无对账码）',
        'M2-07b 验收：只有页面码 + 取数码，没有 :audit —— 对账 Tab 必须看不到', 1, 980, 1)
ON CONFLICT (id) DO UPDATE SET role_name = EXCLUDED.role_name, status = 1, deleted_at = NULL;

DELETE FROM sys_role_menu WHERE role_id = '207b0000-0000-4000-8000-000000000201'::uuid;
INSERT INTO sys_role_menu (id, role_id, menu_id)
VALUES
  ('207b0000-0000-4000-8000-000000000211'::uuid, '207b0000-0000-4000-8000-000000000201'::uuid,
   '20000000-0000-0000-0000-000000000e01'::uuid),  -- 仓库管理（祖先目录）
  ('207b0000-0000-4000-8000-000000000212'::uuid, '207b0000-0000-4000-8000-000000000201'::uuid,
   '20000000-0000-0000-0000-000000004230'::uuid),  -- 在途库存页面行（:page）
  ('207b0000-0000-4000-8000-000000000213'::uuid, '207b0000-0000-4000-8000-000000000201'::uuid,
   '20000000-0000-0000-0000-000000004210'::uuid)   -- 在途查询按钮（:list）
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_user (id, username, password, real_name, dept_id, status, must_change_password)
SELECT '207b0000-0000-4000-8000-000000000202'::uuid, 'm207b_noaudit', u.password, '在途只读（M207B）', u.dept_id, 1, false
  FROM sys_user u
 WHERE u.username = 'admin'
ON CONFLICT (id) DO UPDATE
  SET password = EXCLUDED.password,
      status = 1,
      must_change_password = false,
      deleted_at = NULL;

INSERT INTO sys_user_role (id, user_id, role_id)
VALUES ('207b0000-0000-4000-8000-000000000203'::uuid,
        '207b0000-0000-4000-8000-000000000202'::uuid,
        '207b0000-0000-4000-8000-000000000201'::uuid)
ON CONFLICT (id) DO NOTHING;

COMMIT;
