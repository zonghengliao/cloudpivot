-- ======================================================
-- V0192：P-FLOW-3B 补漏 —— 4 个新权限码按老码的角色矩阵补齐
--
-- ── 为什么需要这条迁移 ──────────────────────────────────
-- V0191 只把 4 个新按钮码授给了 SUPER_ADMIN：
--     business:erp:payable:submit-cancel    应付单提交作废审批
--     business:erp:payable:withdraw-cancel  应付单撤回作废审批
--     business:erp:voucher:submit-reverse   凭证提交红冲审批
--     business:erp:voucher:withdraw-reverse 凭证撤回红冲审批
-- 而 V0158 的老码（同一件事的旧入口）授给的是：
--     business:erp:payable:cancel  / business:erp:voucher:reverse
--     → SUPER_ADMIN, ADMIN, TEST_UADMIN, FINANCE, ACCOUNTANT
--
-- 后果（实测，前端卡 t_c10da409 真机走查）：
--   FINANCE / ACCOUNTANT 能**发起**（前端为避开 403 仍打老 URL `/cancel` `/reverse`，
--   同一函数、同样走审批），但**撤不回自己发起的审批** —— 撤回只有新 URL，
--   点下去 `403`（缺 business:erp:payable:withdraw-cancel / voucher:withdraw-reverse）。
--   前端只能按权限隐藏撤回入口并在详情里说明缺哪个码，属于可感知的功能缺口。
--
-- 本迁移把 4 个新码按**老码的同一组角色**补齐，判据与 V0158 逐字一致，
-- 另外用「数据驱动」覆盖客户自定义角色：**凡是持有老码的角色，一律补上新码**，
-- 这样「能发起 ⇒ 能撤回」在任意角色配置下都成立（不是只对 5 个内置角色成立）。
--
-- ── 幂等 ──────────────────────────────────────────────
-- · 只插 `sys_role_menu`，用 `NOT EXISTS (role_id, menu_id)` 判重（表上还有
--   `UNIQUE (role_id, menu_id)` 兜底），重放多少次都是「新增 0 行」；
-- · 不动 sys_menu / sys_role，不覆盖任何既有授权。
--
-- ── 空库首启（迁移先于 bootstrap）────────────────────
-- 与 V0158 / V0191 同口径：此刻 `sys_role` 为空 → 授权是空操作，
-- 自检块打 NOTICE 跳过核对（不能 RAISE，否则空库永远起不来）。
-- bootstrap 之后重放一次本迁移即可补齐（幂等，已验证）。
--
-- ── 回滚 ──────────────────────────────────────────────
-- `V0192__pflow3b_grant_new_codes_by_old_matrix.down.sql`：
-- 删掉这 4 个码**除 SUPER_ADMIN 之外**的授权，回到 V0191 交付时的形态。
-- ⚠️ 若运维在 V0191 之后手工把新码授给过别的角色，down 会一并收回（已在 down 里注明）。
-- ======================================================

-- ---------- 1) 5 个内置角色：按 V0158 老码矩阵补齐 ----------
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, m.id
  FROM sys_role r
  CROSS JOIN (VALUES
        ('20000000-0000-0000-0000-000000001028'::uuid),  -- payable:submit-cancel
        ('20000000-0000-0000-0000-000000001029'::uuid),  -- payable:withdraw-cancel
        ('20000000-0000-0000-0000-000000001030'::uuid),  -- voucher:submit-reverse
        ('20000000-0000-0000-0000-000000001031'::uuid)) -- voucher:withdraw-reverse
       AS m(id)
 WHERE r.deleted_at IS NULL
   AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'TEST_UADMIN', 'FINANCE', 'ACCOUNTANT')
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- ---------- 2) 数据驱动：持有老码的自定义角色也补齐 ----------
-- 判据 = 「有 business:erp:payable:cancel」⟹ 必须有 submit-cancel + withdraw-cancel；
--         「有 business:erp:voucher:reverse」⟹ 必须有 submit-reverse + withdraw-reverse。
-- 5 个内置角色上面已覆盖（这里 NOT EXISTS 自然跳过），本段只补客户自建角色/改组后的角色。
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, m.id
  FROM sys_role r
  CROSS JOIN (VALUES
        ('20000000-0000-0000-0000-000000001028'::uuid),
        ('20000000-0000-0000-0000-000000001029'::uuid),
        ('20000000-0000-0000-0000-000000001030'::uuid),
        ('20000000-0000-0000-0000-000000001031'::uuid))
       AS m(id)
 WHERE r.deleted_at IS NULL
   -- 老码：应付作废 或 凭证红冲（两者任一即视为「本来就能发起」，于是必须能撤回）
   AND EXISTS (
        SELECT 1 FROM sys_role_menu rm
          JOIN sys_menu mu ON mu.id = rm.menu_id
         WHERE rm.role_id = r.id
           AND mu.deleted_at IS NULL
           AND mu.auth_code IN ('business:erp:payable:cancel', 'business:erp:voucher:reverse')
   )
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- ---------- 3) 自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    v_menus    int;
    v_roles    int;
    v_present  int;
    v_grants   int;
    v_rows     int;
    v_missing  int;
    v_role     record;
BEGIN
    -- 3.1 4 个菜单/权限节点必须都在（V0191 种，本迁移只授权）
    SELECT count(*) INTO v_menus
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001028'::uuid,
                  '20000000-0000-0000-0000-000000001029'::uuid,
                  '20000000-0000-0000-0000-000000001030'::uuid,
                  '20000000-0000-0000-0000-000000001031'::uuid)
       AND deleted_at IS NULL;
    IF v_menus <> 4 THEN
        RAISE EXCEPTION 'V0192 自检失败：预期 4 个新权限节点，实际 %', v_menus;
    END IF;

    -- 3.2 老码矩阵的 5 个角色，逐个必须恰好 4 个新码
    SELECT count(*) INTO v_roles FROM sys_role WHERE deleted_at IS NULL;
    IF v_roles = 0 THEN
        RAISE NOTICE 'V0192：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权核对；bootstrap 后重放本迁移即可补齐';
        RETURN;
    END IF;

    FOR v_role IN
        SELECT id, role_code FROM sys_role
         WHERE deleted_at IS NULL
           AND role_code IN ('SUPER_ADMIN', 'ADMIN', 'TEST_UADMIN', 'FINANCE', 'ACCOUNTANT')
    LOOP
        SELECT count(*) INTO v_rows
          FROM sys_role_menu rm
         WHERE rm.role_id = v_role.id
           AND rm.menu_id IN ('20000000-0000-0000-0000-000000001028'::uuid,
                              '20000000-0000-0000-0000-000000001029'::uuid,
                              '20000000-0000-0000-0000-000000001030'::uuid,
                              '20000000-0000-0000-0000-000000001031'::uuid);
        IF v_rows <> 4 THEN
            RAISE EXCEPTION 'V0192 自检失败：角色 % 只有 %/4 个新权限码', v_role.role_code, v_rows;
        END IF;
    END LOOP;

    -- 3.3 汇总：4 个码 × 在场角色数（标准库 = 4 × 5 = 20 行，缺一即失败）
    SELECT count(*) INTO v_present
      FROM sys_role
     WHERE deleted_at IS NULL
       AND role_code IN ('SUPER_ADMIN', 'ADMIN', 'TEST_UADMIN', 'FINANCE', 'ACCOUNTANT');
    SELECT count(*) INTO v_grants
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'TEST_UADMIN', 'FINANCE', 'ACCOUNTANT')
       AND rm.menu_id IN ('20000000-0000-0000-0000-000000001028'::uuid,
                          '20000000-0000-0000-0000-000000001029'::uuid,
                          '20000000-0000-0000-0000-000000001030'::uuid,
                          '20000000-0000-0000-0000-000000001031'::uuid);
    IF v_grants <> 4 * v_present THEN
        RAISE EXCEPTION 'V0192 自检失败：新码授权应有 % 行（4 码 × % 个老码角色），实际 %',
            4 * v_present, v_present, v_grants;
    END IF;
    IF v_present = 5 AND v_grants <> 20 THEN
        RAISE EXCEPTION 'V0192 自检失败：标准矩阵必须是 20 行（4 码 × 5 角色），实际 %', v_grants;
    END IF;

    -- 3.4 反向不变量：任何持有老码的角色都不得缺对应新码（覆盖客户自定义角色）
    SELECT count(*) INTO v_missing
      FROM sys_role r
     WHERE r.deleted_at IS NULL
       AND EXISTS (
            SELECT 1 FROM sys_role_menu rm
              JOIN sys_menu mu ON mu.id = rm.menu_id
             WHERE rm.role_id = r.id AND mu.deleted_at IS NULL
               AND mu.auth_code = 'business:erp:payable:cancel')
       AND NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm
              JOIN sys_menu mu ON mu.id = rm.menu_id
             WHERE rm.role_id = r.id AND mu.deleted_at IS NULL
               AND mu.auth_code IN ('business:erp:payable:submit-cancel',
                                    'business:erp:payable:withdraw-cancel')
             GROUP BY rm.role_id
            HAVING count(DISTINCT mu.auth_code) = 2);
    IF v_missing > 0 THEN
        RAISE EXCEPTION 'V0192 自检失败：% 个持有 business:erp:payable:cancel 的角色缺新码', v_missing;
    END IF;

    SELECT count(*) INTO v_missing
      FROM sys_role r
     WHERE r.deleted_at IS NULL
       AND EXISTS (
            SELECT 1 FROM sys_role_menu rm
              JOIN sys_menu mu ON mu.id = rm.menu_id
             WHERE rm.role_id = r.id AND mu.deleted_at IS NULL
               AND mu.auth_code = 'business:erp:voucher:reverse')
       AND NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm
              JOIN sys_menu mu ON mu.id = rm.menu_id
             WHERE rm.role_id = r.id AND mu.deleted_at IS NULL
               AND mu.auth_code IN ('business:erp:voucher:submit-reverse',
                                    'business:erp:voucher:withdraw-reverse')
             GROUP BY rm.role_id
            HAVING count(DISTINCT mu.auth_code) = 2);
    IF v_missing > 0 THEN
        RAISE EXCEPTION 'V0192 自检失败：% 个持有 business:erp:voucher:reverse 的角色缺新码', v_missing;
    END IF;

    RAISE NOTICE 'V0192 自检通过：4 个新权限码 × % 个老码角色 = % 行（已按老码矩阵补齐 应付作废/凭证红冲 的撤回能力）',
        v_present, v_grants;
END $$;
