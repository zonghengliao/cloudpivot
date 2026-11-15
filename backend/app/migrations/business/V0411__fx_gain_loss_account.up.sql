-- ============================================================================
-- V0411__fx_gain_loss_account（M1-06 · 多币种：汇兑损益科目配置 · 2026-11-16）
--
-- 【本卡做什么】把「汇兑损益记哪个科目」从**硬编码常量**变成**可配参数**：
--   ① `sys_config.erp.fx.gain_loss_account`（STRING，缺省 `5603`）—— 唯一的真相来源；
--   ② `business:erp:fx-settings:update`（按钮行，挂「汇率管理」页面 …3168 下，sort_order = 60）
--      + 授权 SUPER_ADMIN / FINANCE_MANAGER（空库首启时 sys_role 为空 → 空操作，重放补齐）；
--   ③ 自检块（参数行 / 节点在位 / 一码一节点 / 同父 sort 唯一 / title 规范 / 授权到位），
--      任一不符即 RAISE EXCEPTION 让整个迁移回滚 ——「跑过了」和「验过了」是两件事。
--
-- 【为什么用 sys_config 而不是新开一张 biz_fx_settings 表】
--   本仓已有同一类先例：库存期初的对方科目就是 `sys_config.inventory_opening_equity_account`
--   （FIX-6 / V0328）——「一个科目编码的账务口径参数」不值一张表：
--     · 一行 KV 表（id / 扩展列 / 软删 / 类型 / 级别 / 可维护标记）在 `sys_config` 里已齐备，
--       自建表要么复制这 14 列，要么退化成「两列但没有审计与权限」；
--     · 系统参数页（`/support/config`）已经能查/改/审计（写操作触发运行时快照 reload），
--       新开表就要再写一遍同样的 CRUD 与快照语义；
--     · 参数键是**全局唯一**的（`uk_sys_config_key` 部分唯一索引，`deleted_at IS NULL`），
--       天然满足「只能有一个汇兑损益科目」。
--   代价是「科目编码不是外键」：改配置时由接口校验科目存在 / 末级 / 损益类 / 未标外币核算，
--   运行期取不到就 400 + 66083 点名（**不静默回落到 5603**，见 `fx_settings.rs`）。
--
-- 【为什么缺省是 5603】
--   本仓科目表是小企业准则（损益类只有 5xxx），财务费用 = 5603（V0183 资产负债表映射 +
--   V0184 利润表 FINANCE_EXPENSE 都按 5603 取数）。M1-05 期末调汇与 SO-41 应收逐笔重估
--   两处此前都硬编码 5603，本卡改读本参数、缺省仍是 5603 ⇒ **存量行为零变化**。
--   参数值只存**科目编码**（与 `inventory_opening_equity_account` 同口径），不存 id：
--   科目 id 在克隆库 / 重建环境里会变，编码是业务可读、跨库稳定的那个键。
--
-- 【为什么权限码叫 fx-settings:update 而不是复用 fx-rate:update】
--   汇率维护与「汇兑损益记哪个科目」是两件事：前者改牌价（天天改），后者改**账务口径**
--   （一年改一次，改错会让当期损益科目错位）。合成一个码 = 只想让会计录牌价就得连口径
--   一起给。与 V0410 给调汇单独开 `period:fx-reval` 的同一条理由。
--   读接口 `GET /business/erp/fx-settings` 不另开码，复用 `business:erp:fx-rate:list`
--   （能看汇率管理页的人看得见「汇兑损益记在哪个科目」——只读，无副作用）。
--
-- 【幂等】
--   参数行 `WHERE NOT EXISTS`（不动已有值：运维已经改过的配置不许被重放覆盖）；
--   菜单 `ON CONFLICT (id) DO NOTHING`；授权 `NOT EXISTS` 去重；自检块可反复执行。
--
-- 【回滚】
--   down 先删授权与菜单行，再删参数行 —— **但只在值仍等于缺省 `5603` 时才删**：
--   值已被人工改成别的科目，说明那是真实配置意图（本卡之前的硬编码行为也是 5603），
--   回滚代码不该顺手把它抹掉；此时留一行 RAISE NOTICE 让人知道参数还在。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：参数表 / 菜单 / 角色必须在（缺表说明底座迁移没跑）
-- ---------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.sys_config') IS NULL THEN
        RAISE EXCEPTION 'V0411: sys_config 不存在，请先应用系统底座迁移';
    END IF;
    IF to_regclass('public.sys_menu') IS NULL THEN
        RAISE EXCEPTION 'V0411: sys_menu 不存在，请先应用系统底座迁移';
    END IF;
    IF to_regclass('public.sys_role_menu') IS NULL THEN
        RAISE EXCEPTION 'V0411: sys_role_menu 不存在，请先应用系统底座迁移';
    END IF;
    RAISE NOTICE 'V0411: 前置表齐备，开始种汇兑损益科目参数';
END $$;

-- ---------------------------------------------------------------------------
-- ② 参数种子：erp.fx.gain_loss_account = '5603'
--    不动已有值（`WHERE NOT EXISTS`）：重放本迁移不许把运维改过的科目顶回去。
--    is_modifiable = true：这条参数就是给管理端改的（不是重启生效的部署常量）。
-- ---------------------------------------------------------------------------
INSERT INTO sys_config (id, config_key, config_value, config_type, description, category,
                        sort_order, config_level, is_modifiable, is_encrypted, is_restart, status)
SELECT gen_random_uuid(), 'erp.fx.gain_loss_account', '5603', 'STRING',
       '汇兑损益承接科目编码（期末调汇 M1-05 / 应收重估 SO-41 的差额都记这个科目；缺省 5603 财务费用）',
       'erp_fx', 10, 2, TRUE, FALSE, FALSE, 1
 WHERE NOT EXISTS (
        SELECT 1 FROM sys_config
         WHERE config_key = 'erp.fx.gain_loss_account'
           AND deleted_at IS NULL
 );

-- ---------------------------------------------------------------------------
-- ③ 权限码 + 菜单按钮行（parent = 「汇率管理」页面 …3168，与 V0316 的 5 个汇率按钮同父）
--   sort_order = 60：V0316 占 10/20/30/40/50（新建 / 编辑 / 删除 / 批量 / 预览），
--   同父必须唯一（约定 12），所以取 60。
--   id = …001740：…001010–…001022 与 …001700–…001730 已被历史迁移占用
--   （V0408 用 1720「科目外币核算」、V0410 用 1730「期末调汇」），本卡续 1740。
--   title = 「汇兑损益科目」（6 字，≤12；不带说明、不带标点）。
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001740'::uuid, 'business:erp:fx-settings:update', 3,
     'business:erp:fx-settings:update', NULL,
     '20000000-0000-0000-0000-000000003168'::uuid, NULL,
     60, 1, NULL, '汇兑损益科目', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 授权：超管 / 财务经理（与 V0316 汇率维护同两个角色；空库首启时 sys_role 为空 → 空操作）
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, '20000000-0000-0000-0000-000000001740'::uuid
  FROM sys_role r
 WHERE r.deleted_at IS NULL
   AND r.role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER')
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm
         WHERE rm.role_id = r.id
           AND rm.menu_id = '20000000-0000-0000-0000-000000001740'::uuid
   );

-- ---------------------------------------------------------------------------
-- ④ 迁移自检（参数行 / 节点 / 唯一性 / 同父排序 / 授权；任一不符即回滚）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n        integer;
    n_roles  integer;
    n_grant  integer;
    v_value  text;
BEGIN
    -- ===== 参数行在位：键 / 类型 / 可维护 / 启用 / 未软删 / 值是「科目编码形状」 =====
    --   **不断言值等于 5603**：重放本迁移时运维可能已经把科目改成别的（5601 / 5602.01…），
    --   那是真实配置意图，迁移不许把它顶回去（插入侧 `WHERE NOT EXISTS` 已保证不覆盖）。
    --   这里只断言「这一行是可用参数」：非空、≤30 字（与 `biz_account.account_code` 同宽）、
    --   只含编码字符（防止有人把科目**名称**写进值里）。
    SELECT count(*) INTO n
      FROM sys_config
     WHERE config_key = 'erp.fx.gain_loss_account'
       AND config_type = 'STRING'
       AND config_value IS NOT NULL
       AND btrim(config_value) <> ''
       AND length(btrim(config_value)) <= 30
       AND btrim(config_value) ~ '^[A-Za-z0-9._-]+$'
       AND is_modifiable = TRUE
       AND status = 1
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0411 自检失败：参数 erp.fx.gain_loss_account 应恰好 1 行可用配置（非空 / ≤30 字 / 只含编码字符），实际命中 % 行', n;
    END IF;
    -- 键在软删表上必须唯一（`uk_sys_config_key` 已兜底，这里再断言一次，免得索引将来被摘）
    SELECT count(*) INTO n
      FROM sys_config
     WHERE config_key = 'erp.fx.gain_loss_account'
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0411 自检失败：erp.fx.gain_loss_account 有 % 行未软删记录（应为 1）', n;
    END IF;

    -- ===== 菜单节点在位：码 / 类型 / 父节点 / 启用 / 未软删 =====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001740'::uuid
       AND auth_code = 'business:erp:fx-settings:update'
       AND name = 'business:erp:fx-settings:update'
       AND menu_type = 3
       AND parent_id = '20000000-0000-0000-0000-000000003168'::uuid
       AND status = 1
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0411 自检失败：汇兑损益科目权限节点不对（码授不出去 = 除超管没人能改口径）';
    END IF;

    -- ===== 一码一节点 =====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE auth_code = 'business:erp:fx-settings:update'
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0411 自检失败：business:erp:fx-settings:update 在 sys_menu 里出现 % 次（应为 1）', n;
    END IF;

    -- ===== title 规范：≤12 字、不带标点/说明（menu_invariants 第 6 条）=====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001740'::uuid
       AND length(title) <= 12
       AND title NOT LIKE '%，%'
       AND title NOT LIKE '%（%';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0411 自检失败：汇兑损益科目节点的 title 不合菜单规范（≤12 字、不带标点/说明）';
    END IF;

    -- ===== 同父 sort_order 唯一（重复会让侧边栏顺序每次刷新都变，约定 12）=====
    SELECT count(*) INTO n
      FROM (
            SELECT sort_order
              FROM sys_menu
             WHERE parent_id = '20000000-0000-0000-0000-000000003168'::uuid
               AND menu_type = 3
               AND deleted_at IS NULL
             GROUP BY sort_order
            HAVING count(*) > 1
           ) dup;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0411 自检失败：「汇率管理」页面下有 % 组重复的 sort_order（本卡用 60）', n;
    END IF;

    -- ===== 授权：有角色就该授到；角色不存在（空库首启）则跳过 =====
    SELECT count(*) INTO n_roles
      FROM sys_role
     WHERE deleted_at IS NULL
       AND role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER');
    SELECT count(*) INTO n_grant
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE rm.menu_id = '20000000-0000-0000-0000-000000001740'::uuid
       AND r.role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER');
    IF n_grant < n_roles THEN
        RAISE EXCEPTION 'V0411 自检失败：% 个目标角色只授到 % 行（超管/财务经理必须能改汇兑损益科目）',
            n_roles, n_grant;
    END IF;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0411：sys_role 为空（空库首启，迁移先于 bootstrap），授权待重放补齐';
    END IF;

    SELECT config_value INTO v_value
      FROM sys_config
     WHERE config_key = 'erp.fx.gain_loss_account' AND deleted_at IS NULL;

    RAISE NOTICE 'V0411 自检通过：参数 erp.fx.gain_loss_account=% + 权限码 business:erp:fx-settings:update（父=汇率管理，sort=60）+ 授权 %/% 角色',
        v_value, n_grant, n_roles;
END $$;
