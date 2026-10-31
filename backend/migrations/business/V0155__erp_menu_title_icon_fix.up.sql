-- V0155：修 ERP 菜单 title/icon（菜单显示不全的真因）
--
-- 老板反馈（2026-10-31 08:21）：
--   「ERP主数据菜单里为什么有很多有图标没名字的？又有很多有名字没图标的？」
--
-- 【真因】前端 SideMenuItem.vue 渲染的是 {{ item.title }}，不是 name！
--   V0154 重组时只改了 name，没改 title →
--     ① 3 个新建业务目录 title 为空 → 只显示图标不显示文字
--     ② ERP/采购管理 的 title 还是旧的「ERP 主数据」「ERP 台账」
--     ③ 应付账款/记账凭证/总账 的 title 与 name 不一致
--
-- 【修法】
--   ① 统一 title = name（前端显示与菜单名一致，不再两套名字）
--   ② 补 5 个缺失 icon

BEGIN;

-- ① 统一 title = name（ERP 全部可见菜单）
UPDATE sys_menu
SET title = name, updated_at = now()
WHERE deleted_at IS NULL AND path LIKE '%erp%'
  AND (title IS NULL OR title = '' OR title <> name);

-- ② 补 5 个缺失 icon
UPDATE sys_menu SET icon = 'swap-right',  updated_at = now() WHERE path = '/business/erp/transfer'  AND (icon IS NULL OR icon = '');
UPDATE sys_menu SET icon = 'money',       updated_at = now() WHERE path = '/business/erp/payable'   AND (icon IS NULL OR icon = '');
UPDATE sys_menu SET icon = 'chart-pie',   updated_at = now() WHERE path = '/business/erp/ledger'    AND (icon IS NULL OR icon = '');
UPDATE sys_menu SET icon = 'download',    updated_at = now() WHERE path = '/business/erp/stock-in'  AND (icon IS NULL OR icon = '');
UPDATE sys_menu SET icon = 'file-paste',  updated_at = now() WHERE path = '/business/erp/voucher'   AND (icon IS NULL OR icon = '');

COMMIT;

SELECT
  count(*) FILTER (WHERE title IS NULL OR title = '')            AS 无title,
  count(*) FILTER (WHERE title IS NOT NULL AND title <> name)    AS title不等name,
  count(*) FILTER (WHERE icon IS NULL OR icon = '')              AS 无icon,
  count(*)                                                        AS 总数
FROM sys_menu WHERE deleted_at IS NULL AND is_visible = true AND path LIKE '%erp%';
