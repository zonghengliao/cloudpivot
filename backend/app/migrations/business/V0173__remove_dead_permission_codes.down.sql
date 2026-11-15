-- V0160 down：恢复 BUG-SYS-05 下架的菜单与目录 auth_code
--
-- 注意：**不重建 sys_role_menu 绑定**（被删的码本就不生效，如确需请重新在管理面授权）。
-- 5 条菜单按 up 之前的原值插回（is_visible = false 的按钮，parent 为物料 / 采购订单 / 变更日志）。

BEGIN;

-- ① 4 条目录菜单恢复「模块名当权限码」的原状（仅用于回滚取证，不建议长期停留在此状态）
UPDATE sys_menu SET auth_code = 'erp-purchase', updated_at = now()
WHERE id = '20000000-0000-0000-0000-000000000946' AND menu_type = 1;
UPDATE sys_menu SET auth_code = 'erp-warehouse', updated_at = now()
WHERE id = '20000000-0000-0000-0000-000000000e01' AND menu_type = 1;
UPDATE sys_menu SET auth_code = 'erp-finance', updated_at = now()
WHERE id = '20000000-0000-0000-0000-000000000e02' AND menu_type = 1;
UPDATE sys_menu SET auth_code = 'erp-master-data', updated_at = now()
WHERE id = '20000000-0000-0000-0000-000000000e03' AND menu_type = 1;

-- ② 5 条按钮菜单插回（原值见 up 头注释与 docs/fix-bug-sys-05-dead-codes.md 的取证输出）
INSERT INTO sys_menu (
    id, name, menu_type, auth_code, path, component, parent_id, sort_order,
    icon, title, is_visible, status
) VALUES
    ('20000000-0000-0000-0000-000000000972', 'business:erp:category:list', 3,
     'business:erp:category:list', NULL, NULL,
     '20000000-0000-0000-0000-000000000943', 50, NULL, '物料分类列表（预留）', false, 1),
    ('20000000-0000-0000-0000-000000000973', 'business:erp:category:detail', 3,
     'business:erp:category:detail', NULL, NULL,
     '20000000-0000-0000-0000-000000000943', 60, NULL, '物料分类详情（预留）', false, 1),
    ('20000000-0000-0000-0000-000000000974', 'business:erp:category:create', 3,
     'business:erp:category:create', NULL, NULL,
     '20000000-0000-0000-0000-000000000943', 70, NULL, '新建物料分类（预留）', false, 1),
    ('20000000-0000-0000-0000-000000000993', 'business:erp:purchase-order:detail', 3,
     'business:erp:purchase-order:detail', NULL, NULL,
     '20000000-0000-0000-0000-000000000953', 1, NULL, '采购订单详情（含明细与三金额）', false, 1),
    ('20000000-0000-0000-0000-000000000733', 'system:change-log:clean', 3,
     'system:change-log:clean', NULL, NULL,
     '20000000-0000-0000-0000-000000000730', 30, NULL, '手工清理过期分区（需 confirm）', false, 1)
ON CONFLICT (id) DO UPDATE
SET auth_code = EXCLUDED.auth_code,
    title = EXCLUDED.title,
    parent_id = EXCLUDED.parent_id,
    sort_order = EXCLUDED.sort_order,
    is_visible = EXCLUDED.is_visible,
    deleted_at = NULL,
    updated_at = now();

-- ③ 自检：5 条菜单都在
DO $$
DECLARE
    v_n int;
BEGIN
    SELECT count(*) INTO v_n
    FROM sys_menu
    WHERE id IN (
        '20000000-0000-0000-0000-000000000972',
        '20000000-0000-0000-0000-000000000973',
        '20000000-0000-0000-0000-000000000974',
        '20000000-0000-0000-0000-000000000993',
        '20000000-0000-0000-0000-000000000733'
    )
      AND deleted_at IS NULL;
    IF v_n <> 5 THEN
        RAISE EXCEPTION 'V0160 down 自检失败：应恢复 5 条菜单，实际 %', v_n;
    END IF;
END $$;

COMMIT;
