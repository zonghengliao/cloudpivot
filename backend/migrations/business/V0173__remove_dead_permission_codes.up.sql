-- V0160：清理死权限码（BUG-SYS-05）
--
-- 来源：测试报告 docs/test-report-system.md 第一节风险清单 BUG-SYS-05。
-- 背景：sys_menu.auth_code 是「可授予权限码」的唯一来源，管理面按它勾给角色；
-- 若某个码在源码里既没有 #[nik::permission(...)] 路由注解、也没有
-- claims.has_permission(...) / ensure_writable(..., PERM_*) 这类**行级**校验，
-- 那么把它勾给角色永远不会生效 ——「配了权限却没权限」，是最难排查的一类缺陷。
--
-- 逐个判定（扫描口径 = 剥离行注释后的源码全文检索，见 docs/fix-bug-sys-030405.md）：
--
-- ① 4 条目录菜单把**模块名**当权限码（menu_type = 1）：目录不承载权限，
--    auth_code 置空（与库里另外 6 条目录菜单写法一致）。
--      erp-finance / erp-purchase / erp-warehouse / erp-master-data
--
-- ② 5 条按钮菜单的权限码在代码里 0 校验点，走「下架」（软删表，这里按 V0156 先例
--    物理删除并清理 sys_role_menu 绑定；它们都是 is_visible = false 的按钮）：
--      business:erp:category:list      —— biz_material_category 表未建、无 /business/erp/category
--      business:erp:category:detail       路由；代码里只剩一个「预留清单」常量（已同批移除）
--      business:erp:category:create
--      business:erp:purchase-order:detail —— 采购订单没有 GET 详情路由，详情随
--                                           GET /business/erp/purchase-order/page 的行返回
--      system:change-log:clean         —— 没有手工清理入口（清理走定时任务 run_handler）
--
-- ③ **保留**但登记为「前端路由级」权限码：
--      system:preference:view —— 后端无接口用它，但 frontend/src/router/index.ts 的
--      meta.permission 用它拦前端路由（外观设置页纯 localStorage，无后端接口），
--      属有效权限码，不下架。
--
-- ④ **不动**那 14 个「无路由注解但有字符串引用」的码：它们是行级校验，真实生效
--    （business:task:assign / member / dependency / worklog / comment:delete /
--    attachment:delete、business:project:update / delete、business:flow:admin、
--    system:field-mask:plain 等）。测试报告里把它们算进「死码」是因为只扫了
--    #[nik::permission] 注解，未覆盖 claims.has_permission 的行级校验。
--
-- 幂等性：UPDATE 带 `auth_code <> ''` 条件；DELETE 带「无未删子节点」守卫，
-- 重复执行影响行数为 0，不报错。
--
-- 回滚（down）说明：会把这 5 条菜单按原值插回、4 条目录菜单的 auth_code 复原；
-- **不重建 sys_role_menu 绑定**（删除前它们指向的码本就不生效，如确需请重新在管理面授权）。

BEGIN;

-- ① 目录菜单不再挂权限码（模块名不是权限码）
UPDATE sys_menu
SET auth_code = '', updated_at = now()
WHERE id IN (
        '20000000-0000-0000-0000-000000000946',  -- 采购管理
        '20000000-0000-0000-0000-000000000e01',  -- 仓库管理
        '20000000-0000-0000-0000-000000000e02',  -- 财务管理
        '20000000-0000-0000-0000-000000000e03'   -- 基础数据
    )
  AND menu_type = 1
  AND deleted_at IS NULL
  AND auth_code <> '';

-- ② 下架 5 条死码按钮：先清授权绑定（避免 sys_role_menu 孤儿），再删菜单
DELETE FROM sys_role_menu
WHERE menu_id IN (
    '20000000-0000-0000-0000-000000000972',  -- business:erp:category:list
    '20000000-0000-0000-0000-000000000973',  -- business:erp:category:detail
    '20000000-0000-0000-0000-000000000974',  -- business:erp:category:create
    '20000000-0000-0000-0000-000000000993',  -- business:erp:purchase-order:detail
    '20000000-0000-0000-0000-000000000733'   -- system:change-log:clean
);

DELETE FROM sys_menu AS m
WHERE m.id IN (
    '20000000-0000-0000-0000-000000000972',
    '20000000-0000-0000-0000-000000000973',
    '20000000-0000-0000-0000-000000000974',
    '20000000-0000-0000-0000-000000000993',
    '20000000-0000-0000-0000-000000000733'
)
  AND NOT EXISTS (
      SELECT 1 FROM sys_menu AS c WHERE c.parent_id = m.id AND c.deleted_at IS NULL
  );

-- ③ 自检：目标码必须已从菜单消失（有子节点而跳过删除时会在这里暴露）
DO $$
DECLARE
    v_left  int;
    v_codes text;
BEGIN
    SELECT count(*), string_agg(auth_code, ', ')
    INTO v_left, v_codes
    FROM sys_menu
    WHERE deleted_at IS NULL
      AND auth_code IN (
          'business:erp:category:list',
          'business:erp:category:detail',
          'business:erp:category:create',
          'business:erp:purchase-order:detail',
          'system:change-log:clean'
      );
    IF v_left > 0 THEN
        RAISE EXCEPTION 'V0160 自检失败：仍有 % 条死权限码菜单未被下架（%）', v_left, v_codes;
    END IF;

    SELECT count(*) INTO v_left
    FROM sys_menu
    WHERE deleted_at IS NULL
      AND menu_type = 1
      AND id IN (
          '20000000-0000-0000-0000-000000000946',
          '20000000-0000-0000-0000-000000000e01',
          '20000000-0000-0000-0000-000000000e02',
          '20000000-0000-0000-0000-000000000e03'
      )
      AND auth_code <> '';
    IF v_left > 0 THEN
        RAISE EXCEPTION 'V0160 自检失败：% 条目录菜单的 auth_code 未置空', v_left;
    END IF;
END $$;

COMMIT;

SELECT menu_type, name, auth_code FROM sys_menu
WHERE deleted_at IS NULL AND parent_id IS NULL
ORDER BY sort_order;
