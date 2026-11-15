-- V0390 down：把 5 个成本页面挂回「财务管理」直接下级，并删除「成本核算」目录
--
-- 注意：down 与 up 严格互逆。
--   ① 先把 5 个页面 parent_id 改回财务管理
--   ② 再删目录（此时目录下已无子项）
--   ③ 若目录下还有别的子项，报错而不是强删（防误删）

DO $$
DECLARE
    v_parent_id uuid;
    v_dir_id    uuid;
    v_moved     int;
    v_left      int;
BEGIN
    SELECT id INTO v_parent_id
      FROM sys_menu
     WHERE deleted_at IS NULL AND parent_id IS NULL AND title = '财务管理'
     LIMIT 1;

    SELECT id INTO v_dir_id
      FROM sys_menu
     WHERE deleted_at IS NULL AND path = '/business/erp/cost' AND menu_type = 1
     LIMIT 1;

    IF v_dir_id IS NULL THEN
        RAISE NOTICE 'V0390 down：目录不存在，无需回滚';
        RETURN;
    END IF;

    IF v_parent_id IS NULL THEN
        RAISE EXCEPTION 'V0390 down：找不到「财务管理」一级菜单';
    END IF;

    -- ① 页面挂回财务管理
    UPDATE sys_menu
       SET parent_id = v_parent_id, updated_at = now()
     WHERE deleted_at IS NULL
       AND parent_id = v_dir_id
       AND path LIKE '/business/erp/cost-%';

    GET DIAGNOSTICS v_moved = ROW_COUNT;
    RAISE NOTICE 'V0390 down：% 个成本页面已挂回「财务管理」', v_moved;

    -- ② 目录下还有子项就报错（防误删）
    SELECT count(*) INTO v_left
      FROM sys_menu
     WHERE deleted_at IS NULL AND parent_id = v_dir_id;

    IF v_left > 0 THEN
        RAISE EXCEPTION 'V0390 down：目录下仍有 % 个子项，拒绝删除', v_left;
    END IF;

    -- ③ 删目录
    DELETE FROM sys_menu WHERE id = v_dir_id;
    RAISE NOTICE 'V0390 down 完成：目录「成本核算」已删除';
END $$;
