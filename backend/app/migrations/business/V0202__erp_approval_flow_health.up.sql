-- ============================================================
-- V0202：审批流「默认拒绝」配套 —— 2 个权限码 + 8 个流程的配置自检
--
-- 依据：ARCH-3 审计 §5 DC-01 / §6 R-P0-2（P0），卡 t_4130a770。
--
-- # 为什么需要这条迁移
--
--   DC-01 把 6 处「审批流未配置 → 静默降级为直接生效」改成**默认拒绝**
--   （409 + `APPROVAL_FLOW_NOT_CONFIGURED` / 66033）。这意味着
--   **没有已发布流程的单据类型会挡住业务**，所以上线前必须：
--     ① 确认 8 个流程的种子都还在、且都是 PUBLISHED（本迁移的自检块如实报告）；
--     ② 给「确实暂不审批」的单据类型留一条显式的出路 —— 权限码
--        `business:erp:skip-approval`（本迁移的 ① ）。
--
-- # 权限码口径（两个码，都只是**登记**，不默认放开）
--
--   | 权限码 | 用途 | 默认授予 |
--   |--------|------|----------|
--   | `business:erp:skip-approval` | 没有已发布流程时，显式跳过审批（必填理由 ≥10 字，写 `biz_status_log`） | **只有 SUPER_ADMIN** |
--   | `business:erp:approval-flow:health` | `GET /business/erp/approval-flow/health` 流程配置自检 | **只有 SUPER_ADMIN** |
--
--   ⚠️ `business:erp:skip-approval` **刻意不按「老码矩阵」批量授予**（与 V0192 的做法相反）：
--   它是**内控例外**，授给谁就等于「谁可以不经复核让单据生效」。默认只给超管，
--   由管理员按单据类型、按角色逐个开 —— 这与卡面的风险提示一致：
--   「如果某个单据类型暂不审批，请给对应角色开 skip-approval」。
--
-- # 菜单为什么挂在「流程定义」（menu_type = 3 按钮 + is_visible = FALSE）
--
--   两个码都没有独立的页面（自检是后端端点、跳过审批是各单据接口的入参），
--   所以都落成 `流程定义`（`…0780`）下的**按钮级**权限：
--   角色授权树里能勾选（`load_permissions` 不看 `is_visible`），侧栏不会多出 404 入口。
--
-- # 幂等
--
--   固定 UUID + `ON CONFLICT (id) DO NOTHING`（不覆盖运维改过的标题/排序）；
--   `sys_role_menu` 用 `NOT EXISTS` 去重（表上还有 `UNIQUE (role_id, menu_id)` 兜底）。
--   ID 段 1200/1201（09xx–11xx 已被 ERP 主数据、批次 D、合同域占满）。
--
-- # 空库首启（迁移先于 bootstrap）
--
--   与 V0192 / V0197 同口径：此刻 `sys_role` 为空 → 授权是空操作，
--   自检块只核对权限码（本迁移自己插的），不核对角色授权。
--   bootstrap 之后重放一次本迁移即可补齐超管授权（幂等，已验证）。
--
-- # 自检块为什么对「流程缺失」只 WARNING 不 EXCEPTION
--
--   8 个流程的种子分散在 V0157 / V0174 / V0175 / V0182 / V0191 等多条更早的迁移里。
--   本迁移**不替客户决定**「该不该有流程」（客户可能有意下线某个流程，那是配置变更），
--   只把体检结论打进启动日志（`RAISE WARNING`，journalctl 里一眼可见）：
--
--       V0202 警告：以下审批流程当前不是 PUBLISHED（这些单据类型提交时会 409 + 66033）：…
--
--   权威结论以 `GET /business/erp/approval-flow/health` 为准（同一份 8 个 biz_type 清单）。
-- ============================================================

-- ---------- ① 两个权限码（按钮级，挂在「流程定义」下） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001200'::uuid, 'business:erp:skip-approval', 3,
     'business:erp:skip-approval', NULL, '20000000-0000-0000-0000-000000000780'::uuid, NULL,
     900, 1, NULL, '跳过审批（无已发布流程时显式放行，理由进状态日志）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001201'::uuid, 'business:erp:approval-flow:health', 3,
     'business:erp:approval-flow:health', NULL, '20000000-0000-0000-0000-000000000780'::uuid, NULL,
     910, 1, NULL, '审批流配置自检（8 个 biz_type 的发布状态 + 图形校验）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 授权：只给 SUPER_ADMIN（跳过审批是内控例外，默认不放开） ----------
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, m.id
  FROM sys_role r
  CROSS JOIN (VALUES
        ('20000000-0000-0000-0000-000000001200'::uuid),
        ('20000000-0000-0000-0000-000000001201'::uuid)) AS m(id)
 WHERE r.deleted_at IS NULL
   AND r.role_code = 'SUPER_ADMIN'
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- ---------- ③ 自检：两个码必须在；8 个流程逐个报告 ----------
DO $$
DECLARE
    n_missing int;
    v_missing text;
    v_broken  text;
BEGIN
    -- ① 本迁移自己的两行：缺一不可（这是 EXCEPTION，因为是我们自己写的 INSERT）
    SELECT count(*) INTO n_missing
      FROM (VALUES ('business:erp:skip-approval'),
                   ('business:erp:approval-flow:health')) AS t(code)
     WHERE NOT EXISTS (
           SELECT 1 FROM sys_menu m
            WHERE m.auth_code = t.code AND m.deleted_at IS NULL AND m.status = 1);
    IF n_missing <> 0 THEN
        SELECT string_agg(code, ', ') INTO v_missing
          FROM (VALUES ('business:erp:skip-approval'),
                       ('business:erp:approval-flow:health')) AS t(code)
         WHERE NOT EXISTS (
               SELECT 1 FROM sys_menu m
                WHERE m.auth_code = t.code AND m.deleted_at IS NULL AND m.status = 1);
        RAISE EXCEPTION 'V0202 自检失败：缺权限码 %', v_missing;
    END IF;

    -- ② 两行的形状（父节点、按钮类型、隐藏）必须与设计一致
    SELECT count(*) INTO n_missing
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001200'::uuid,
                  '20000000-0000-0000-0000-000000001201'::uuid)
       AND (menu_type <> 3
            OR is_visible <> FALSE
            OR parent_id <> '20000000-0000-0000-0000-000000000780'::uuid);
    IF n_missing <> 0 THEN
        RAISE EXCEPTION 'V0202 自检失败：% 行权限码的形状（menu_type / is_visible / parent_id）与设计不一致',
            n_missing;
    END IF;

    -- ③ 8 个业务类型：不是 PUBLISHED 的全部列进 WARNING（不阻断启动，由运维处置）
    SELECT string_agg(t.biz_type || '(' || t.flow_code || ')', ', ')
      INTO v_broken
      FROM (VALUES ('PO_APPROVE', 'po_approve'),
                   ('STOCK_IN_POST', 'stock_in_post'),
                   ('STOCK_IN_CANCEL', 'stock_in_cancel'),
                   ('PAYABLE_PAY', 'payable_pay'),
                   ('PAYABLE_CANCEL', 'payable_cancel'),
                   ('VOUCHER_REVERSE', 'voucher_reverse'),
                   ('STOCK_TRANSFER', 'stock_transfer'),
                   ('TASK_ACCEPT', 'task_accept')) AS t(biz_type, flow_code)
     WHERE NOT EXISTS (
           SELECT 1 FROM biz_flow_def d
            WHERE d.deleted_at IS NULL
              AND d.status = 'PUBLISHED'
              AND d.current_version_id IS NOT NULL
              AND (d.biz_type = t.biz_type OR lower(d.flow_code) = t.flow_code));

    IF v_broken IS NOT NULL THEN
        RAISE WARNING 'V0202 警告：以下审批流程当前不是 PUBLISHED（这些单据类型提交时会 409 + 66033）：%。处置：在「流程定义」里发布该流程；或给对应角色授予 business:erp:skip-approval（理由 ≥10 字）。', v_broken;
    ELSE
        RAISE NOTICE 'V0202 自检通过：2 个权限码已登记，8 个审批流程全部 PUBLISHED';
    END IF;
END $$;
