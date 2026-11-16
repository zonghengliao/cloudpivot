-- V0418 回滚：应收账龄分析页下线
--
-- 顺序有讲究：`sys_role_menu.menu_id` 外键指向 `sys_menu`，先删授权再删菜单行。
--
-- 回滚范围（只动本迁移种下的东西）：
--   ① 页面行 …1830 的全部授权
--   ② 页面行 …1830 本身
--   ③ 本卡为目标角色补授的「父目录 …0e02」与「取数码 …2882」授权
--      —— 目标角色 = SUPER_ADMIN / FINANCE_MANAGER / FINANCE_DIRECTOR / GENERAL_MANAGER。
--      其中 FINANCE_MANAGER / GENERAL_MANAGER 在 V0255/V0288 时代**本来就**有 …2882，
--      所以这里按角色白名单删，且只删「本卡才可能新增」的那几行：
--        · …2882 只删 FINANCE_DIRECTOR（V0418 之前这家没有，见 up 的 ④ 注释）；
--        · …0e02 只删 FINANCE_DIRECTOR / SUPER_ADMIN（其余两家 V0154 时代就有）。
--      宁可少删（留下一条无害的多余授权）也不要误删历史授权。
--
-- 本迁移没有任何业务数据落地（报表不落表、不落快照），所以没有数据护栏：
-- 删掉权限节点不会丢账。SUPER_ADMIN 仍有 `*:*:*` 通配，接口与页面都还能被超管访问。
--
-- 幂等：全是 DELETE … WHERE，重复执行无副作用。

-- ---------- ① 页面行授权 ----------
DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000001830'::uuid;

-- ---------- ② 页面行 ----------
DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000001830'::uuid
   AND auth_code = 'business:erp:report:receivable-aging';

-- ---------- ③ 本卡补授的父目录 / 取数码 ----------
-- 取数码：只删本卡补授的角色
DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000002882'::uuid
   AND role_id IN (
        SELECT r.id FROM sys_role r
         WHERE r.role_code IN ('FINANCE_DIRECTOR')
           AND r.deleted_at IS NULL
   );

-- 父目录：只删本卡可能新授的角色
DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000000e02'::uuid
   AND role_id IN (
        SELECT r.id FROM sys_role r
         WHERE r.role_code IN ('FINANCE_DIRECTOR', 'SUPER_ADMIN')
           AND r.deleted_at IS NULL
   );
