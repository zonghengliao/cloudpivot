-- ======================================================
-- V0295：收款退款单接口的 5 个权限码（SO-14 遗留卡 · 路线 A 的菜单承载）
--
-- ## 为什么单独一条迁移（不并进 V0294）
--
-- 建表（DDL / 约束 / 索引）与「授权面」（sys_menu 权限点）是两类评审：
-- 合并会让 down 脚本既要 DROP TABLE 又要撤授权，回滚粒度被绑死。
-- 与 V0288（应收）/ V0291（应收页面）/ V0293（收款）同口径。
--
-- ## 门禁依据
--
-- `scripts/lint/perm_menu_gate.py`（check.sh 闸⑥）扫描 `#[nik::permission(permission = "x")]`，
-- 每个码都必须在 `sys_menu.auth_code` 有承载行，否则该码**授不出去** —— 除超管外
-- 任何人都调不动接口，而且不报错、不打日志。
--
-- 本卡的 5 个接口 ↔ 5 个码（与 receipt_refund.rs 的 PERM_* 常量、注解逐字一致）：
--
--     business:erp:receipt-refund:list      分页（GET   /receipt-refund/page）
--     business:erp:receipt-refund:detail    详情（GET   /receipt-refund/{id}）
--     business:erp:receipt-refund:create    登记退款（POST  /receipt-refund）
--     business:erp:receipt-refund:allocate  退款核销（POST  /receipt-refund/{id}/allocate）
--     business:erp:receipt-refund:status    状态迁移（PATCH /receipt-refund/{id}/status）
--
-- ## 结构：一个页面节点 + 4 个按钮
--
--   收款退款单（menu_type=2，auth_code = …:list，id …2950）
--     ├─ …:detail    （menu_type=3，…2951）
--     ├─ …:create    （menu_type=3，…2952）
--     ├─ …:allocate  （menu_type=3，…2953）
--     └─ …:status    （menu_type=3，…2954）
--
-- `path` / `component` 故意留 NULL、`is_visible = FALSE`（与 V0261 / V0284 / V0288 / V0293 同一套口子）：
-- `system::menu::tests::n08` 是硬门禁（menu_type=2 的 path 必须能在前端路由里解析），
-- 前端页面由后续前端卡落地后，用一条 UPDATE 补 path/component/is_visible（照 V0291 的手法）。
--
-- ## 不静默扩权
--
-- 本迁移**不写** `sys_role_menu`：退款授给哪些角色（销售 / 财务 / 出纳）是管理决策。
-- 现状：只有 SUPER_ADMIN 能调（短路）。因为一行授权都没有，
-- `orphan_button_gate`（闸⑦）也不会把 4 个按钮判成孤儿。
--
-- ## 幂等与回滚
--
-- 固定 uuid + `ON CONFLICT (id) DO NOTHING`；末尾 DO 块自检 5 个码与父子关系，任一不符 → 迁移失败。
-- 回滚：V0295__sales_receipt_refund_permissions.down.sql 先删 sys_role_menu 授权行，再删 5 行菜单。
-- ======================================================

-- ---------- ① 页面节点（path/component 留 NULL，等前端路由卡落地后补） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002950'::uuid, '收款退款单', 2,
     'business:erp:receipt-refund:list', NULL, NULL,
     NULL, 85, 1, 'rollback',
     '收款退款单：RR-yyyyMMdd-000001 / DRAFT-CONFIRMED-POSTED-CANCELLED；退款过账 借 1122 应收 贷 1002 银行',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 四个动作按钮（挂在页面节点下） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002951'::uuid, 'business:erp:receipt-refund:detail', 3,
     'business:erp:receipt-refund:detail', NULL,
     '20000000-0000-0000-0000-000000002950'::uuid, NULL,
     10, 1, NULL, '退款单详情：客户名 / 凭证号 / 逐张应收的冲减金额与恢复后的状态',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002952'::uuid, 'business:erp:receipt-refund:create', 3,
     'business:erp:receipt-refund:create', NULL,
     '20000000-0000-0000-0000-000000002950'::uuid, NULL,
     20, 1, NULL, '登记退款：取号 RR；amount > 0；原因必填 >= 4 字；已结账期间 400',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002953'::uuid, 'business:erp:receipt-refund:allocate', 3,
     'business:erp:receipt-refund:allocate', NULL,
     '20000000-0000-0000-0000-000000002950'::uuid, NULL,
     30, 1, NULL, '退款核销：冲减应收已收（received_amount -= 金额），不得超出该应收已收',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002954'::uuid, 'business:erp:receipt-refund:status', 3,
     'business:erp:receipt-refund:status', NULL,
     '20000000-0000-0000-0000-000000002950'::uuid, NULL,
     40, 1, NULL, '状态迁移：确认 / 过账生凭证 / 草稿作废；过账要求核销满且期间未结账',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ③ 自检：5 个码齐全 + 父子关系正确 ----------
DO $$
DECLARE
    codes TEXT[] := ARRAY[
        'business:erp:receipt-refund:list',
        'business:erp:receipt-refund:detail',
        'business:erp:receipt-refund:create',
        'business:erp:receipt-refund:allocate',
        'business:erp:receipt-refund:status'
    ];
    c       TEXT;
    n       INT;
    parent  UUID := '20000000-0000-0000-0000-000000002950'::uuid;
BEGIN
    FOREACH c IN ARRAY codes
    LOOP
        SELECT count(*) INTO n FROM sys_menu WHERE auth_code = c AND status = 1;
        IF n <> 1 THEN
            RAISE EXCEPTION 'V0295 自检失败：权限码 % 在 sys_menu 里应有且只有 1 行（实际 %）', c, n;
        END IF;
    END LOOP;

    -- 4 个按钮必须挂在页面节点下（not-null parent 且等于 parent）
    SELECT count(*) INTO n FROM sys_menu
     WHERE auth_code IN ('business:erp:receipt-refund:detail',
                         'business:erp:receipt-refund:create',
                         'business:erp:receipt-refund:allocate',
                         'business:erp:receipt-refund:status')
       AND parent_id = parent;
    IF n <> 4 THEN
        RAISE EXCEPTION 'V0295 自检失败：4 个按钮里只有 % 个挂在页面节点下', n;
    END IF;

    -- 页面节点必须是 menu_type=2 且无父节点（一级菜单）
    SELECT count(*) INTO n FROM sys_menu
     WHERE id = parent AND menu_type = 2 AND parent_id IS NULL AND status = 1;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0295 自检失败：页面节点不是「menu_type=2 / 一级菜单 / 启用」';
    END IF;

    RAISE NOTICE 'V0295 自检通过：收款退款单 5 个权限码（1 页面 + 4 按钮）已登记';
END $$;
