-- ============================================================================
-- V0410__period_fx_revaluation（M1-05 · 多币种收口：期末调汇 · 2026-11-16）
--
-- 【本卡做什么】只种**一个权限码**（+ 它的菜单按钮行与角色授权），不动表结构：
--   ① `business:erp:period:fx-reval`（按钮行，挂「会计期间」页面 …0951 下，sort_order = 6）
--   ② 授权 SUPER_ADMIN / FINANCE_MANAGER（谁维护月度账谁调汇；空库首启时 sys_role 为空 → 空操作）
--   ③ 自检块（节点在位 / 一码一节点 / 同父 sort 唯一 / 授权到位 / 行为探针），任一不符即
--      RAISE EXCEPTION 让整个迁移回滚 —— 「跑过了」和「验过了」是两件事。
--
-- 【为什么本卡不带 CREATE TABLE：调汇结果就是那张凭证本身】
--   调汇的产出是「一张本位币凭证 + 逐科目调整分录」，凭证台账（`biz_voucher` /
--   `biz_voucher_entry`）已经把每一行都留住了：科目、方向、金额、期间、操作人。
--   再建一张 `biz_fx_reval_account` 台账表 = **双真相**：
--     · 凭证被红冲（`reverse_posted`）而台账没跟着改 ⇒ 下期算 `prior_adjust` 时把已冲掉的
--       差额再加一遍（SO-41 的应收台账 `biz_fx_revaluation` 就是因为要按「逐笔应收」累计
--       才对，本模块是**科目余额**口径，账面余额本身就含历次调汇，不需要另存）；
--     · 台账行与分录金额不一致时，没有任何库层约束能跨表断言（同 V0409 文件头「为什么不在
--       biz_voucher 上加原币合计」的理由）。
--   ⇒ 幂等靠「同期间同 `source_type`（`FX_REVAL_ACCOUNT`）只有一张未被红冲的蓝字凭证」，
--   不靠台账表；重跑口径写在 `period_fx_reval.rs` 的「幂等与重跑」段。
--
-- 【为什么新开一个权限码，而不是复用 `business:erp:period:close`】
--   结账是**封账开关**：进这一步该期间就只读了，撤回要反结账（有原因留痕）。
--   调汇是**生成一张凭证**：做错了可以红冲再跑，期间始终可写。
--   合成一个码的后果是「只想给会计调汇权」就得连封账权一起给（V0188 头注释反对的正是这种
--   绑定：能看期间列表 ≠ 能封账）。同理也不复用 `:reopen`（撤封账是更高危的动作）。
--   `:fx-reval` 命名与 `:open` / `:close` / `:reopen` / `:remind` 同一套 `{模块}:{资源}:{动作}`。
--
-- 【为什么调汇不挂在结账事务里（写在这里，免得下一个人「顺手补上」）】
--   结账第 ⑤ 步已经跑 SO-41 的**应收逐笔**重估（`FX_REVALUATION`），它的口径是
--   「取不到牌价就 4xx」但**只覆盖未核销外币应收**；科目级调汇覆盖的是**全部外币科目**
--   （含应付、银行外币户…）。两者诉求不同：
--     · 结账不允许因为「某个币种没维护期末牌价」而封不了账（月结有硬时间窗）；
--     · 科目级调汇必须对缺牌价**硬拒绝**（少调一个科目 = 静默错账，见 66082）。
--   ⇒ 调汇是独立动作：先试算（`dry_run`）、财务看一眼、再落账、最后结账。
--   结账前是否已经调汇，由流程与 `close-check` 之外的月度操作规范约束（本卡不改结账校验）。
--
-- 【幂等】
--   `INSERT … ON CONFLICT (id) DO NOTHING` + 授权 `NOT EXISTS` 去重 + 自检块可反复执行；
--   探针行自清场（重放零残留）。
--
-- 【回滚】
--   down 在「库里已有 FX_REVAL_ACCOUNT 凭证」时 RAISE EXCEPTION 拒绝执行：那些凭证是真实
--   记账依据（调过损益），删掉权限节点不会删数据，但会让「谁有权做这件事」失去追溯 ——
--   先红冲凭证再回滚，是这里要的表达。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：sys_menu / sys_role 必须在（缺表说明系统底座迁移没跑）
-- ---------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.sys_menu') IS NULL THEN
        RAISE EXCEPTION 'V0410: sys_menu 不存在，请先应用系统底座迁移';
    END IF;
    IF to_regclass('public.sys_role_menu') IS NULL THEN
        RAISE EXCEPTION 'V0410: sys_role_menu 不存在，请先应用系统底座迁移';
    END IF;
    RAISE NOTICE 'V0410: 前置表齐备，开始种期末调汇权限码';
END $$;

-- ---------------------------------------------------------------------------
-- ② 权限码 + 菜单按钮行（parent = 「会计期间」页面 …0951，与 V0188 三个期间码同父）
--   sort_order = 6：V0188 占 2/3/4（开账 / 结账 / 反结账）、V0207 占 5（催办），
--   同父必须唯一（约定 12），所以取 6。
--   id = …001730：…0010xx 段被 payable / voucher / report / period / opening / yearend 的历史
--   迁移占满（…001022 是「期初余额」页面），复用会撞主键；0017xx 段是 V0408 开的「外币」小段
--   （V0408 用 1720），本卡续 1730。
--   title = 「期末调汇」（4 字，≤12；不带说明、不带标点）。
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001730'::uuid, 'business:erp:period:fx-reval', 3,
     'business:erp:period:fx-reval', NULL,
     '20000000-0000-0000-0000-000000000951'::uuid, NULL,
     6, 1, NULL, '期末调汇', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 授权：超管 / 财务经理（空库首启时 sys_role 为空 → 空操作，bootstrap 后重放本迁移补齐）
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, '20000000-0000-0000-0000-000000001730'::uuid
  FROM sys_role r
 WHERE r.deleted_at IS NULL
   AND r.role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER')
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm
         WHERE rm.role_id = r.id
           AND rm.menu_id = '20000000-0000-0000-0000-000000001730'::uuid
   );

-- ---------------------------------------------------------------------------
-- ③ 迁移自检（节点 / 唯一性 / 同父排序 / 授权；任一不符即回滚）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n        integer;
    n_roles  integer;
    n_grant  integer;
BEGIN
    -- ===== 节点在位：码 / 类型 / 父节点 / 启用 / 未软删 =====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001730'::uuid
       AND auth_code = 'business:erp:period:fx-reval'
       AND name = 'business:erp:period:fx-reval'
       AND menu_type = 3
       AND parent_id = '20000000-0000-0000-0000-000000000951'::uuid
       AND status = 1
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0410 自检失败：期末调汇权限节点不对（码授不出去 = 除超管没人能调汇）';
    END IF;

    -- ===== 一码一节点：同一个 auth_code 不许出现在多行（重复行会让授权页出现两个同名勾选框）=====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE auth_code = 'business:erp:period:fx-reval'
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0410 自检失败：business:erp:period:fx-reval 在 sys_menu 里出现 % 次（应为 1）', n;
    END IF;

    -- ===== title 规范：≤12 字、不是功能说明（约定 12 / menu_invariants 第 6 条）=====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001730'::uuid
       AND length(title) <= 12
       AND title NOT LIKE '%，%'
       AND title NOT LIKE '%（%';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0410 自检失败：期末调汇节点的 title 不合菜单规范（≤12 字、不带标点/说明）';
    END IF;

    -- ===== 同父 sort_order 唯一（重复会让侧边栏顺序每次刷新都变，约定 12）=====
    SELECT count(*) INTO n
      FROM (
            SELECT sort_order
              FROM sys_menu
             WHERE parent_id = '20000000-0000-0000-0000-000000000951'::uuid
               AND menu_type = 3
               AND deleted_at IS NULL
             GROUP BY sort_order
            HAVING count(*) > 1
           ) dup;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0410 自检失败：「会计期间」页面下有 % 组重复的 sort_order（本卡用 5）', n;
    END IF;

    -- ===== 授权：有角色就该授到；角色不存在（空库首启）则跳过 =====
    SELECT count(*) INTO n_roles
      FROM sys_role
     WHERE deleted_at IS NULL
       AND role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER');
    SELECT count(*) INTO n_grant
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE rm.menu_id = '20000000-0000-0000-0000-000000001730'::uuid
       AND r.role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER');
    IF n_grant < n_roles THEN
        RAISE EXCEPTION 'V0410 自检失败：% 个目标角色只授到 % 行（超管/财务经理必须有调汇权）',
            n_roles, n_grant;
    END IF;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0410：sys_role 为空（空库首启，迁移先于 bootstrap），授权待重放补齐';
    END IF;

    RAISE NOTICE 'V0410 自检通过：权限码 business:erp:period:fx-reval + 菜单按钮行（父=会计期间，sort=6）+ 授权 %/% 角色',
        n_grant, n_roles;
END $$;
