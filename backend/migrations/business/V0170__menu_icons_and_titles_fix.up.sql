-- ======================================================
-- V0160：菜单 3 处真实缺陷修复（测试C 三方一致性扫描发现，卡 t_6f023cee）
--
-- 来源：docs/test-report-system.md 第一节 🔴 风险清单
--   BUG-SYS-02（P2）14 条菜单的 icon 在 tdesign-icons-vue-next 里**不存在** → 侧栏图标空白
--   BUG-SYS-06（P2）3 条菜单把权限码当 name/title → 侧栏直接显示 business:erp:ledger:list
--
-- 判定口径（与回归用例 n09 完全一致）：
--   icon 名 = frontend/node_modules/tdesign-icons-vue-next/esm/components/<name>.js 的
--   kebab-case 文件名。缺的 12 个名字（mask / question-circle / flow / list /
--   view-board / announcement / shopping-cart / team / unordered-list / database /
--   environment / field-number / box）逐个用真实文件核对过，替换值也逐个核对过
--   （`secured.js` / `help-circle.js` / `flowchart.js` / `view-list.js` /
--   `dashboard-1.js` / `sound.js` / `cart.js` / `usergroup.js` / `order-list.js` /
--   `data-base.js` / `location.js` / `automatic-numbering.js` / `layers.js` 全部存在）。
--
-- 为什么 name/title 是权限码：V0150 / V0151 种这三条菜单时 **name 就写成了权限码**、
--   title 才是中文；V0155 的「统一 title = name」把唯一的中文展示名也覆盖掉了。
--   所以本迁移把两者一起改成中文（name 是搜索关键词、导航不渲染它，但它是显示名的第二来源）。
--
-- 幂等：固定 id / path 定位，`IS DISTINCT FROM` 判定，重跑无副作用；
--   已被运维改过的行（name 不再是该权限码）不会被覆盖。
--
-- BUG-SYS-01（2 条菜单无路由 → 404）不改数据：前端已补
--   `views/business/erp/purchase-order-item/index.vue`、`views/business/erp/seq/index.vue`
--   并在 `src/router/index.ts` 注册了这两条路由（后端接口本来就齐全）。
-- ======================================================

BEGIN;

-- ---------- ① BUG-SYS-06：3 条菜单 name/title 是权限码 → 中文 ----------
-- 1007 总账（V0150 种，原 title「总账 / 科目余额表」）
UPDATE sys_menu
   SET name = '总账', title = '总账 / 科目余额表', updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000001007'::uuid
   AND deleted_at IS NULL
   AND name = auth_code;

-- 1009 应付单（V0151 种，原 title「应付单」）
UPDATE sys_menu
   SET name = '应付单', title = '应付单', updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000001009'::uuid
   AND deleted_at IS NULL
   AND name = auth_code;

-- 1004 凭证查询（V0150 种，原 title「凭证查询」）
UPDATE sys_menu
   SET name = '凭证查询', title = '凭证查询', updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000001004'::uuid
   AND deleted_at IS NULL
   AND name = auth_code;

-- ---------- ② BUG-SYS-02：14 条菜单的 icon 换成图标库里真实存在的名字 ----------
-- 按 path 定位（这些行 id 分散在 V0030/V0141/V0146/V0148… 多个种子，path 才是稳定键）；
-- 只在「当前值仍是那个不存在的老名字」时才改，避免覆盖运维手改过的图标。
UPDATE sys_menu AS m
   SET icon = v.new_icon, updated_at = now()
  FROM (VALUES
        -- 系统管理
        ('/system/field-mask',            'mask',            'secured'),
        -- 支持 / 内容
        ('/help',                         'question-circle', 'help-circle'),
        ('/business/announcement',        'announcement',    'sound'),
        -- 流程 / 任务
        ('/flow/def',                     'flow',            'flowchart'),
        ('/task/list',                    'list',            'view-list'),
        ('/task/board',                   'view-board',      'dashboard-1'),
        -- ERP 采购
        ('/business/erp/purchase-mgmt',   'shopping-cart',   'cart'),
        ('/business/erp/purchase-order',  'shopping-cart',   'cart'),
        ('/business/erp/purchase-order-item', 'unordered-list', 'order-list'),
        ('/business/erp/supplier',        'team',            'usergroup'),
        -- ERP 仓储
        ('/business/erp/inventory',       'database',        'data-base'),
        ('/business/erp/inventory-location', 'environment',  'location'),
        -- ERP 财务 / 主数据
        ('/business/erp/seq',             'field-number',    'automatic-numbering'),
        ('/business/erp/material',        'box',             'layers')
       ) AS v(path, old_icon, new_icon)
 WHERE m.deleted_at IS NULL
   AND m.path = v.path
   AND m.icon = v.old_icon;

COMMIT;

-- 自检（迁移引擎按 simple query 执行，SELECT 结果由驱动丢弃；不写 psql 元命令）：
--   残留的「不存在图标」应为 0
SELECT count(*) FILTER (WHERE icon IN ('mask', 'question-circle', 'flow', 'list', 'view-board',
                                       'announcement', 'shopping-cart', 'team', 'unordered-list',
                                       'database', 'environment', 'field-number', 'box')) AS 残留坏图标,
       count(*) FILTER (WHERE deleted_at IS NULL AND name = auth_code AND menu_type = 2) AS 残留权限码标题,
       count(*) AS 菜单总数
  FROM sys_menu
 WHERE deleted_at IS NULL;
