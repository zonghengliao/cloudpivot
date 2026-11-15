-- ============================================================================
-- V0353：采购订单图标修正（shopping-cart → cart）+ 物料管理归位（基础数据 → 仓库管理）
--        —— **MENU-FIX-3**（卡 t_7c9a6c3b）
--
-- # ① 为什么改图标：DB 里的图标名在 TDesign 图标库里不存在
--
--   prod（144 `cloudpivot`）实测：
--     SELECT title, path, icon FROM sys_menu WHERE path='/business/erp/purchase';
--       → 采购订单 | /business/erp/purchase | shopping-cart
--
--   而 `tdesign-icons-vue-next/esm/components/` 下**没有** `shopping-cart.js`
--   （本机实测：`cart.js` 存在，`shopping-cart.js` 不存在）。前端
--   `layouts/components/SideMenuItem.vue` 是 `<t-icon :name="item.icon" />`，
--   name 解析不到组件时 TDesign 渲染空节点 ⇒ 侧栏「采购订单」前面是空白，
--   且 `icon` 非空所以连 `folder` / `app` 兜底都不会触发（兜底只在 icon 为空串时才走）。
--   这就是 AGENTS.md「高频踩坑」里记的「图标名不存在导致空白」。
--
--   对 prod 全部 menu_type IN (1,2) 的 distinct icon 逐个比对图标库，**只有
--   `shopping-cart` 一个不存在**（其余 60 个都能在 components/ 下找到同名 .js），
--   所以本迁移只改这一处，不顺手批量"规范化"别的图标名。
--
-- # ② 为什么 WHERE 里有两个 path：prod / dev 的同一条菜单路径不同
--
--   V0319 头注释已记录（2026-11-07 全表比对）：同一条逻辑菜单（都解析到
--   `views/business/erp/purchase/index.vue`，router 里 `purchase` 是
--   `purchase-order` 的 alias）在两个库里 path 不一样：
--       prod（144 cloudpivot）       → '/business/erp/purchase'        icon='shopping-cart' ❌
--       dev / 新库（145、V0141 种子）→ '/business/erp/purchase-order'  icon='cart'          ✅
--   V0319 的决定是「不 INSERT、不改 path」（两条等价，改名要连带动路由/硬编码/文档），
--   所以本迁移必须同时覆盖两个 path：prod 上改 1 行，dev/新库上改 0 行。
--   只写一个 path 的版本在另一个库上会静默 0 行 —— 这就是"修了但没修上"。
--
-- # ③ 物料管理归位
--
--   用户原话：「基础数据里的物料管理应该是在仓库管理里」。
--   物料是仓库业务（仓库/库位/序列号/即时库存/调拨/流水/出库/盘点）的主数据，
--   放「基础数据」下与「计量单位 / 单号序列」这两个真·基础配置混在一起不合理。
--
--   目标：parent = /business/erp/warehouse-mgmt，sort_order = 5（最前面）。
--   仓库管理现有子菜单 sort = 10/20/30/40/50/60/70/80/90，5 空着且唯一
--   （menu_invariants.sh ⑦ 断言要求同父 sort 唯一，本迁移在 §3 自检里也验一遍）。
--
--   父 id 用**子查询按 path 取**，不硬编码 UUID：UUID 是 V0141 种子的固定值
--   （20000000-…0e01），但按 path 取能让本迁移在任何改名/重建的库上仍然正确，
--   也避免把"父节点 id"这种易漂移的字面量写死。
--
-- # ④ 幂等
--
--   两条 UPDATE 都带 `IS DISTINCT FROM` 目标值判据：生产/测试库若已经是目标状态，
--   各自 0 行，重复执行不产生额外写入（不动 updated_at，不刷新审计噪音）。
--   文件内**不写** BEGIN/COMMIT（迁移器自己在事务里跑）。
--
-- # ⑤ 自检
--
--   §3 的 DO 块在 up 之后必须全为真（不满足就 RAISE EXCEPTION 让迁移回滚）：
--     · 采购订单（两个 path 中存在的那些）icon = 'cart'
--     · 物料管理 parent = 仓库管理 且 sort_order = 5
--     · 仓库管理下 menu_type=2 的子菜单 sort 不重复
--     · 基础数据下不再有物料管理
--   物料行不存在时（理论上不会：V0141 种子就有）只 NOTICE 不报错 —— 别让一条
--   纯数据修正迁移把"某个精简库"的启动链卡死。
-- ============================================================================

-- ============================================================================
-- §1 采购订单图标：shopping-cart → cart（TDesign 真实存在的名字）
-- ============================================================================
UPDATE sys_menu
   SET icon = 'cart',
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path IN ('/business/erp/purchase',        -- prod（144）现存的别名路径
                '/business/erp/purchase-order')  -- dev / 新库（V0141 种子路径）
   AND icon IS DISTINCT FROM 'cart';

-- ============================================================================
-- §2 物料管理：基础数据 → 仓库管理，排在子菜单最前面（sort_order = 5）
-- ============================================================================
UPDATE sys_menu
   SET parent_id = (SELECT id FROM sys_menu
                     WHERE path = '/business/erp/warehouse-mgmt'
                       AND deleted_at IS NULL),
       sort_order = 5,
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path = '/business/erp/material'
   AND (parent_id IS DISTINCT FROM (SELECT id FROM sys_menu
                                     WHERE path = '/business/erp/warehouse-mgmt'
                                       AND deleted_at IS NULL)
        OR sort_order IS DISTINCT FROM 5);

-- ============================================================================
-- §3 自检（up 后必须全为真）
-- ============================================================================
DO $$
DECLARE
    v_wh_id        uuid;
    v_bad_icon     integer;
    v_mat_parent   text;
    v_mat_sort     integer;
    v_dup_sort     integer;
    v_left_in_base integer;
BEGIN
    -- ① 仓库管理目录必须存在（否则 §2 的父 id 会变成 NULL，物料会掉到顶级）
    SELECT id INTO v_wh_id
      FROM sys_menu
     WHERE path = '/business/erp/warehouse-mgmt'
       AND deleted_at IS NULL;
    IF v_wh_id IS NULL THEN
        RAISE EXCEPTION 'V0353 自检失败：找不到 /business/erp/warehouse-mgmt 目录，物料管理无处可挂';
    END IF;

    -- ② 采购订单图标必须是 cart（两个 path 谁存在查谁；都存在就都查）
    SELECT count(*) INTO v_bad_icon
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND path IN ('/business/erp/purchase', '/business/erp/purchase-order')
       AND icon IS DISTINCT FROM 'cart';
    IF v_bad_icon > 0 THEN
        RAISE EXCEPTION 'V0353 自检失败：采购订单 icon 仍有 % 行不是 cart', v_bad_icon;
    END IF;

    -- ③ 物料管理归属与排序（行不存在时只提示，不阻断启动链）
    SELECT p.path, m.sort_order INTO v_mat_parent, v_mat_sort
      FROM sys_menu m
      LEFT JOIN sys_menu p ON p.id = m.parent_id
     WHERE m.path = '/business/erp/material'
       AND m.deleted_at IS NULL;

    IF v_mat_parent IS NULL AND v_mat_sort IS NULL THEN
        RAISE NOTICE 'V0353 提示：本库没有 /business/erp/material 菜单，跳过归属自检';
    ELSE
        IF v_mat_parent IS DISTINCT FROM '/business/erp/warehouse-mgmt' THEN
            RAISE EXCEPTION 'V0353 自检失败：物料管理父节点为 % ，预期 /business/erp/warehouse-mgmt', v_mat_parent;
        END IF;
        IF v_mat_sort IS DISTINCT FROM 5 THEN
            RAISE EXCEPTION 'V0353 自检失败：物料管理 sort_order = % ，预期 5', v_mat_sort;
        END IF;
    END IF;

    -- ④ 仓库管理下 menu_type=2 的 sort 不许重复（menu_invariants.sh ⑦ 的同一口径）
    SELECT count(*) INTO v_dup_sort
      FROM (SELECT sort_order
              FROM sys_menu
             WHERE deleted_at IS NULL
               AND menu_type = 2
               AND parent_id = v_wh_id
             GROUP BY sort_order
            HAVING count(*) > 1) d;
    IF v_dup_sort > 0 THEN
        RAISE EXCEPTION 'V0353 自检失败：仓库管理下有 % 组重复 sort_order', v_dup_sort;
    END IF;

    -- ⑤ 基础数据下不该再有物料管理
    SELECT count(*) INTO v_left_in_base
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND path = '/business/erp/material'
       AND parent_id = (SELECT id FROM sys_menu
                         WHERE path = '/business/erp/master-data'
                           AND deleted_at IS NULL);
    IF v_left_in_base > 0 THEN
        RAISE EXCEPTION 'V0353 自检失败：物料管理仍挂在基础数据下';
    END IF;

    RAISE NOTICE 'V0353 完成：采购订单 icon=cart，物料管理已挂到仓库管理（sort_order=5）';
END $$;
