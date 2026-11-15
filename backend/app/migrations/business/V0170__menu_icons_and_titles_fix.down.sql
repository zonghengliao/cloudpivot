-- ======================================================
-- V0160 down：把 BUG-SYS-02 / BUG-SYS-06 的修回滚成修复前状态
--
-- 回滚目标**刻意**是「坏状态」（图标名不存在 / name = 权限码）：down 的语义就是
-- 让本迁移的净效果归零，而不是留一个更好看的半成品。回滚后再跑一次 up 应完全复原。
-- ======================================================

BEGIN;

-- ---------- ① 图标回滚 ----------
UPDATE sys_menu AS m
   SET icon = v.old_icon, updated_at = now()
  FROM (VALUES
        ('/system/field-mask',            'secured',            'mask'),
        ('/help',                         'help-circle',        'question-circle'),
        ('/business/announcement',        'sound',              'announcement'),
        ('/flow/def',                     'flowchart',          'flow'),
        ('/task/list',                    'view-list',          'list'),
        ('/task/board',                   'dashboard-1',         'view-board'),
        ('/business/erp/purchase-mgmt',   'cart',               'shopping-cart'),
        ('/business/erp/purchase-order',  'cart',               'shopping-cart'),
        ('/business/erp/purchase-order-item', 'order-list',     'unordered-list'),
        ('/business/erp/supplier',        'usergroup',          'team'),
        ('/business/erp/inventory',       'data-base',          'database'),
        ('/business/erp/inventory-location', 'location',        'environment'),
        ('/business/erp/seq',             'automatic-numbering', 'field-number'),
        ('/business/erp/material',        'layers',             'box')
       ) AS v(path, new_icon, old_icon)
 WHERE m.deleted_at IS NULL
   AND m.path = v.path
   AND m.icon = v.new_icon;

-- ---------- ② name / title 回滚成权限码（V0155 之后的实际状态） ----------
UPDATE sys_menu
   SET name = auth_code, title = auth_code, updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000001007'::uuid
   AND deleted_at IS NULL
   AND name = '总账';

UPDATE sys_menu
   SET name = auth_code, title = auth_code, updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000001009'::uuid
   AND deleted_at IS NULL
   AND name = '应付单';

UPDATE sys_menu
   SET name = auth_code, title = auth_code, updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000001004'::uuid
   AND deleted_at IS NULL
   AND name = '凭证查询';

COMMIT;
