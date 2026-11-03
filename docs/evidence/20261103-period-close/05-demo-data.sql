-- PERIOD-CLOSE 真机验证的演示数据（cloudpivot_test，2026-11-03 实测）
--
-- 用**专用期间 2036-03**（不是 2026-10/11）：共用测试库里那两个月份塞满了别的卡遗留的夹具
-- （实测：P2D1-0004 / W1B-PO-0001 / P1FIX-* / IN-20261102-*），截图会糊成十几行、说不清「3 张」。
-- 2036-03 与 `period.rs` 用 2031/2032 同一思路：每个用例用自己的期间，跑完不欠别人。
--
-- 前置：期间 2036-03 已开账
--   curl -X POST :8023/api/business/erp/period -d '{"period_year":2036,"period_month":3}'
--   （实测 200，返回 id 01a19f2c-9cfd-7785-a465-47ca5ac431b2）
--
-- 三张挡路单据 + 三个「该谁处理」的岗位配置：
--   ① PC-V-203603-001  草稿凭证 1234.00    → business:erp:voucher:create          → FINANCE_MANAGER / 李四
--   ② PC-IN-203603-001 草稿收货单 560.00   → business:erp:stock-in:status         → WAREHOUSE_MANAGER（多人 → 只给岗位）
--   ③ PC-PO-203603-001 已提交采购单 880.00 → business:erp:purchase-order:status  → PURCHASE_MANAGER（多人 → 只给岗位）
BEGIN;

-- ---------- ① 演示用户（只用于「该谁处理」显示；口令是假哈希，不能登录） ----------
INSERT INTO sys_user (id, username, password, real_name, status)
VALUES
    ('00000000-0000-7000-8000-00000000c001', 'pc_demo_li', 'pc-demo-not-a-real-hash', '李四', 1),
    ('00000000-0000-7000-8000-00000000c002', 'pc_demo_zhao', 'pc-demo-not-a-real-hash', '赵六', 1),
    ('00000000-0000-7000-8000-00000000c003', 'pc_demo_sun', 'pc-demo-not-a-real-hash', '孙七', 1)
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 把权限节点勾给岗位（真实部署里是「系统管理 · 菜单权限」勾选） ----------
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, m.id
  FROM sys_role r
  JOIN sys_menu m ON m.auth_code IN (
        'business:erp:voucher:create',
        'business:erp:stock-in:status',
        'business:erp:purchase-order:status',
        'business:erp:period:remind')
 WHERE r.role_code IN ('FINANCE_MANAGER', 'WAREHOUSE_MANAGER', 'PURCHASE_MANAGER')
   AND ((r.role_code = 'FINANCE_MANAGER' AND m.auth_code IN ('business:erp:voucher:create', 'business:erp:period:remind'))
     OR (r.role_code = 'WAREHOUSE_MANAGER' AND m.auth_code = 'business:erp:stock-in:status')
     OR (r.role_code = 'PURCHASE_MANAGER' AND m.auth_code = 'business:erp:purchase-order:status'))
   AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id);

INSERT INTO sys_user_role (id, user_id, role_id)
SELECT gen_random_uuid(), u.id, r.id
  FROM sys_user u JOIN sys_role r ON
       (u.username = 'pc_demo_li' AND r.role_code = 'FINANCE_MANAGER')
    OR (u.username = 'pc_demo_zhao' AND r.role_code = 'WAREHOUSE_MANAGER')
    OR (u.username = 'pc_demo_sun' AND r.role_code = 'PURCHASE_MANAGER')
 WHERE NOT EXISTS (SELECT 1 FROM sys_user_role ur WHERE ur.user_id = u.id AND ur.role_id = r.id);

-- 让 admin 在「消息中心」里能看到催办消息（fanout 在发送那一刻定收件人，事后加人收不到历史）
INSERT INTO sys_user_role (id, user_id, role_id)
SELECT gen_random_uuid(), u.id, r.id FROM sys_user u, sys_role r
 WHERE u.username = 'admin' AND r.role_code = 'WAREHOUSE_MANAGER'
ON CONFLICT (user_id, role_id) DO NOTHING;

-- ---------- ③ 三张挡路单据（期间 2036-03，状态都还没到终态） ----------
-- ③-1 草稿凭证：单头 1234/1234（满足 ck_biz_voucher_balance），分录 1234/1234（满足 A1/A2）
INSERT INTO biz_voucher (id, voucher_no, period, voucher_date, total_debit, total_credit,
                         entry_count, status, summary, voucher_type, source_type, create_by)
VALUES ('00000000-0000-7000-8000-00000000d001', 'PC-V-203603-001', '2036-03', '2036-03-15',
        1234, 1234, 2, 'DRAFT', 'PERIOD-CLOSE 演示：未过账草稿', 'MANUAL', 'MANUAL',
        '00000000-0000-7000-8000-00000000c001')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name,
                               debit_amount, credit_amount)
VALUES
    ('00000000-0000-7000-8000-00000000d001', 1, 'PERIOD-CLOSE 演示', gen_random_uuid(),
     '1001', '库存现金', 1234, 0),
    ('00000000-0000-7000-8000-00000000d001', 2, 'PERIOD-CLOSE 演示', gen_random_uuid(),
     '3001', '实收资本', 0, 1234)
ON CONFLICT DO NOTHING;

-- ③-2 DRAFT 收货单（无源入库：OTHER + is_sourceless，满足 V0199 的 ck_biz_si_supplier_scope / ck_biz_si_sourceless_pair）
INSERT INTO biz_stock_in (id, in_no, in_type, warehouse_id, in_date, status,
                          total_qty, total_amount, total_tax, total_with_tax,
                          is_sourceless, create_by)
VALUES ('00000000-0000-7000-8000-00000000d002', 'PC-IN-203603-001', 'OTHER',
        '01a19c6f-d7c1-74c9-8ed2-53f1b0c93a23', '2036-03-15', 'DRAFT',
        10, 560, 0, 560, TRUE,
        '00000000-0000-7000-8000-00000000c002')
ON CONFLICT (id) DO NOTHING;

-- ③-3 SUBMITTED 采购单
INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status,
                                total_qty, total_amount, total_tax, total_with_tax, create_by)
VALUES ('00000000-0000-7000-8000-00000000d003', 'PC-PO-203603-001', gen_random_uuid(),
        '2036-03-15', '01a19c6f-d7c1-74c9-8ed2-53f1b0c93a23', 'SUBMITTED',
        5, 880, 0, 880,
        '00000000-0000-7000-8000-00000000c003')
ON CONFLICT (id) DO NOTHING;

COMMIT;

-- ---------- ④ 「把三张处理掉」怎么做（真机验证第 3 步，实测记录） ----------
-- 采购单：走**真实接口** PATCH /api/business/erp/purchase-order/{id}/status {"status":"CLOSED"} → 200
-- 收货单：没有任何明细行，真实接口拒绝过账
--         （400「收货入库单 … 没有任何明细，不能提交过账审批」）→ 改走作废
--         PATCH /api/business/erp/stock-in/{id}/status {"status":"CANCELLED","reason":"…"} → 200
-- 草稿凭证：本仓**没有** DRAFT → POSTED 的接口路径（voucher.rs 头注释写死
--         「本仓目前没有 DRAFT 凭证的生成路径」），为了演示只能直接改库：
--         UPDATE biz_voucher SET status='POSTED', posted_at=now() WHERE voucher_no='PC-V-203603-001';
