-- V0390：成本核算 5 个菜单收进「成本核算」子目录（ARCH-12 菜单收口）
--
-- 背景：COST-B1-7 把 5 个成本菜单平铺在「财务管理」下（sort 60~68），
--       使财务管理二级菜单达 19 项，用户在长列表里找不到。
--
-- 本迁移：
--   ① 在「财务管理」下新建目录「成本核算」（menu_type=1，path=/business/erp/cost）
--   ② 把 5 个成本页面挂到该目录下（parent_id 改写）
--   ③ 目录本身 sort=55（在报表 45~49 之后、其他之后）
--
-- 幂等：
--   · 目录用 `WHERE NOT EXISTS` 守卫（按 path 判重）
--   · UPDATE 按 path 精确匹配，重跑后 parent_id 已正确（0 行变更）

DO $$
DECLARE
    v_parent_id  uuid;
    v_dir_id     uuid;
    v_moved      int;
BEGIN
    -- ① 找「财务管理」一级菜单
    SELECT id INTO v_parent_id
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND parent_id IS NULL
       AND title = '财务管理'
     LIMIT 1;

    IF v_parent_id IS NULL THEN
        RAISE EXCEPTION 'V0390：找不到一级菜单「财务管理」，无法挂成本目录';
    END IF;

    -- ② 建「成本核算」目录（幂等：按 path 判重）
    SELECT id INTO v_dir_id
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND path = '/business/erp/cost'
       AND menu_type = 1
     LIMIT 1;

    IF v_dir_id IS NULL THEN
        INSERT INTO sys_menu (
            id, name, parent_id, title, path, menu_type, icon, sort_order,
            is_visible, status, auth_code, component, created_at, updated_at
        ) VALUES (
            gen_random_uuid(), '成本核算', v_parent_id, '成本核算',
            '/business/erp/cost', 1, 'money', 55,
            true, 1, '', '', now(), now()
        )
        RETURNING id INTO v_dir_id;
        RAISE NOTICE 'V0390：已新建目录「成本核算」(id=%)', v_dir_id;
    ELSE
        RAISE NOTICE 'V0390：目录「成本核算」已存在 (id=%)，跳过新建', v_dir_id;
    END IF;

    -- ③ 把 5 个成本页面挂到目录下
    UPDATE sys_menu
       SET parent_id  = v_dir_id,
           updated_at = now()
     WHERE deleted_at IS NULL
       AND path IN (
           '/business/erp/cost-object',
           '/business/erp/cost-labor',
           '/business/erp/cost-overhead',
           '/business/erp/cost-alloc-rule',
           '/business/erp/cost-period'
       )
       AND (parent_id IS DISTINCT FROM v_dir_id);

    GET DIAGNOSTICS v_moved = ROW_COUNT;
    RAISE NOTICE 'V0390：% 个成本页面已挂到目录下', v_moved;

    -- ④ 自检：5 个页面必须都在目录下
    IF (SELECT count(*) FROM sys_menu
         WHERE deleted_at IS NULL
           AND parent_id = v_dir_id
           AND path LIKE '/business/erp/cost-%') <> 5 THEN
        RAISE EXCEPTION 'V0390 自检失败：目录下成本页面数 != 5';
    END IF;

    -- ⑤ 自检：财务管理下不应再有平铺的成本页面
    IF EXISTS (SELECT 1 FROM sys_menu
                WHERE deleted_at IS NULL
                  AND parent_id = v_parent_id
                  AND path LIKE '/business/erp/cost-%') THEN
        RAISE EXCEPTION 'V0390 自检失败：财务管理下仍有平铺的成本页面';
    END IF;

    RAISE NOTICE 'V0390 完成：成本 5 页已收进「财务管理 > 成本核算」';
END $$;
