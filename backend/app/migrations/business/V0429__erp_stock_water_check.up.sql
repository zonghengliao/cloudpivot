-- ============================================================================
-- V0429__erp_stock_water_check（M2-14 · 仓库：安全库存水位检查（低于 → 生成待办）· 2026-11-16）
--
-- 卡：t_df0d7d48（M2-14【M2·仓库】安全库存：水位检查（低于 → 生成待办））
-- 上游：V0428（M2-13 建 `biz_material_stock_policy` + 生效水位的三级回退口径）
-- 代码：`backend/app/src/business/erp/stock_policy/water_check.rs`（检查本体）
--       `backend/app/src/business/erp/stock_policy/handler.rs`（POST …/stock-policy/check）
--
-- ── 本迁移做四件事（一条索引、一个码、两条配置、一段自检）────────────────────
--
--   ① **未完成水位待办唯一** —— `biz_task(doc_type, doc_id)` 的**部分唯一索引**：
--        WHERE doc_type = 'INVENTORY' AND deleted_at IS NULL AND status IN ('TODO','DOING')
--      同一格（`doc_id` = `biz_inventory.id`）同时只能有一条「未完成」待办。这是 M2-14 幂等的
--      **库层**那一层：多实例并发扫描时，先到的插入成功、后到的拿 UniqueViolation
--      （代码把它记成 `skipped_open`，不是失败）。没有它，「每小时扫一次」在双实例部署下
--      一小时就能给同一格建出两条待办。
--   ② 种 1 个权限码 `business:erp:stock-policy:check`（menu_type = 3 按钮，挂「即时库存」页，
--      sort 100）。它是**写**码：一次检查会建 `biz_task`、也会关 `biz_task`。
--   ③ 定时任务可见化 + 参数：
--        · `sys_job` 一行 `erp_stock_water_check`（status = 1，每小时第 7 分）；
--        · `sys_config` 一行 `erp.stock_water.realert_days`（重提醒间隔，缺省 3 天）。
--   ④ 自检：索引形状、菜单形状、同父 sort 唯一、授权成对、任务行与参数在。
--
-- ── 为什么是「部分」唯一索引，而不是普通唯一索引 ────────────────────────────
--
--   普通 `UNIQUE(doc_type, doc_id)` 会让「上周那条已完成的待办」永久挡住「这周的新待办」
--   —— 待办是**事件**（可以反复发生），不是**实体**（一格一条）。部分谓词里的
--   `status IN ('TODO','DOING')` 把「未完成」这个瞬时状态作为唯一性作用域：
--   完成/取消后同格可以再建，历史行全部保留供审计（这正是待办与设置表的区别）。
--
--   `doc_type = 'INVENTORY'` 也必须在谓词里：`biz_task` 还有 `PURCHASE_ORDER` /
--   `STOCK_IN` / `CONTRACT` / `PAYABLE` / `VOUCHER` 五种挂接类型（P2-9 / DC-19），
--   没有这一条就会把它们也串进同一个唯一域，直接挡住「同一张采购单挂两条跟进任务」。
--
-- ── 为什么 index 建在别人的表上（`biz_task` 属任务模块） ────────────────────
--
--   唯一性的语义是「水位待办在同一格上不许重复」，而水位待办**就是** `biz_task` 的一行
--   （`doc_type='INVENTORY'`）—— 没有第二张表能承载这条约束。它不修改任务表的任何列 /
--   任何既有行（只对 `doc_type='INVENTORY'` 的行生效，此前该类行为 0 行），
--   任务模块自己的索引与查询一行不动；回滚只 DROP 这一个索引。
--
-- ── 幂等 / 回滚 ─────────────────────────────────────────────────────────────
--
--   · 索引：`CREATE UNIQUE INDEX IF NOT EXISTS`；建之前先探一次「已有重复行」（有则点名报错，
--     不静默失败）；
--   · 菜单：固定 UUID + `ON CONFLICT (id) DO NOTHING`（重放不覆盖运维改过的 title / sort）；
--   · 授权：`NOT EXISTS` 去重、按码判重（重放不增长）；父节点用 `auth_code` 反查；
--   · `sys_job` / `sys_config`：`WHERE NOT EXISTS` 按业务键判重（重放不新增，也不覆盖运维改过
--     的 cron / 参数值 —— 本迁移只管「有这一行」）；
--   · down：只删本迁移种的四样东西（索引 / 菜单 + 授权 / 任务行 / 参数行），
--     **不动** V0428 的表与三个码、不动 `biz_task` 里已有的水位待办以外的任何行。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：底座表在、父页面唯一、码没被占用、没有会挡住唯一索引的存量重复
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n_base    integer;
    n_parent  integer;
    n_code    integer;
    n_dup     integer;
BEGIN
    SELECT count(*) INTO n_base
      FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname = 'public'
       AND c.relname IN ('sys_menu', 'sys_role_menu', 'biz_task', 'biz_inventory',
                         'biz_material', 'biz_material_stock_policy', 'sys_job', 'sys_config');
    IF n_base <> 8 THEN
        RAISE EXCEPTION 'V0429: 前置表不全（sys_menu / sys_role_menu / biz_task / biz_inventory / biz_material / biz_material_stock_policy / sys_job / sys_config 实得 % 张），请先应用 V0137、V0100、V0428 与系统底座', n_base;
    END IF;

    -- 父页面「即时库存」（menu_type=2，auth_code=business:erp:inventory:list）：必须唯一
    SELECT count(*) INTO n_parent
      FROM sys_menu
     WHERE auth_code = 'business:erp:inventory:list'
       AND menu_type = 2
       AND deleted_at IS NULL;
    IF n_parent <> 1 THEN
        RAISE EXCEPTION 'V0429: 找不到唯一的「即时库存」页面行（auth_code=business:erp:inventory:list，实得 % 行）—— 检查按钮要挂在它下面', n_parent;
    END IF;

    -- 同一个码只能有一个承载节点（多于一个会让角色授权页出现两处同码勾选）
    SELECT count(*) INTO n_code
      FROM sys_menu
     WHERE auth_code = 'business:erp:stock-policy:check'
       AND deleted_at IS NULL;
    IF n_code > 1 THEN
        RAISE EXCEPTION 'V0429: business:erp:stock-policy:check 已有 % 个承载节点（应为 0，重放时 ≤ 1）', n_code;
    END IF;

    -- 已有「同一格两条未完成水位待办」的存量行吗？有则先点名（否则 CREATE UNIQUE INDEX 会
    -- 只抛一个 23505，运维看不出是哪一格、也不知道怎么修）
    SELECT count(*) INTO n_dup
      FROM (
            SELECT doc_id
              FROM biz_task
             WHERE doc_type = 'INVENTORY'
               AND doc_id IS NOT NULL
               AND deleted_at IS NULL
               AND status IN ('TODO', 'DOING')
             GROUP BY doc_id
            HAVING count(*) > 1
           ) dup;
    IF n_dup > 0 THEN
        RAISE EXCEPTION 'V0429: 已有 % 格存在多条未完成水位待办（doc_type=''INVENTORY''），唯一索引建不上。请先关掉多余的（保留每格最新一条），再重跑本迁移', n_dup;
    END IF;

    RAISE NOTICE 'V0429: 前置检查通过（底座表在、即时库存页唯一、码当前 % 个承载节点、存量重复 0 格）', n_code;
END $$;

-- ---------------------------------------------------------------------------
-- ② 未完成水位待办唯一（部分唯一索引）
--
--   谓词三段缺一不可，理由见头注释：
--     · doc_type = 'INVENTORY'  → 只约束水位待办，不串别的挂接类型；
--     · deleted_at IS NULL      → 软删行不占位（删了可以重建）；
--     · status IN ('TODO','DOING') → 只约束「未完成」，完成/取消后同格可再建。
--
--   注意 `CANCELLED` 不在作用域内：自动收口（库存回升）与被人工取消的待办都会让出位置，
--   「回升 → 又跌破」可以立刻重新提醒（重提醒间隔只抑制人工取消的，见 water_check 文档）。
-- ---------------------------------------------------------------------------
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_task_open_inventory
    ON biz_task (doc_type, doc_id)
 WHERE doc_type = 'INVENTORY'
   AND deleted_at IS NULL
   AND status IN ('TODO', 'DOING');

COMMENT ON INDEX uk_biz_task_open_inventory IS
    'M2-14 水位待办幂等的库层一层：同一格（doc_id = biz_inventory.id）同时只允许一条「未完成」待办。部分谓词只覆盖 doc_type=INVENTORY 且未软删且 status 为 TODO/DOING，完成/取消后可再建（历史行保留）';

-- ---------------------------------------------------------------------------
-- ③ 权限点（menu_type = 3，不进侧栏；title 是授权树里的显示名）
--
--   sort：同父（即时库存页）按钮已有 10 库存巡检 / 70 水位设置列表 / 80 设置水位 / 90 删除水位
--         （V0421 + V0428），本卡取 100（约定 12：同父唯一）。
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT v.id::uuid, v.code, 3, v.code, NULL, p.id, NULL, v.sort_order, 1, NULL, v.title, FALSE, 'business'
  FROM (VALUES
        ('20000000-0000-0000-0000-000000004290', 'business:erp:stock-policy:check', 100, '执行水位检查')
       ) AS v(id, code, sort_order, title)
  JOIN sys_menu p
    ON p.auth_code = 'business:erp:inventory:list'
   AND p.menu_type = 2
   AND p.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- ④ 授权：仓库线两类角色拿**成对的四个码**（V0428 的三个 + 本卡的 check）+ 超管
--
--   锚点与 V0428 完全一致（数据驱动、不手抄角色名单）：
--     A「看得到即时库存」= `business:erp:inventory:list`
--     B「能维护仓库主数据」= `business:erp:warehouse:update`
--   并集 → 四个码一起授。为什么 check 必须跟着 list/save/delete 成对授：
--   「能设置水位但点不了检查」= 设置完不知道有没有生效；「能检查但改不了水位」= 看得见噪音
--   却没法消音。两者都要求人去找工程师开权限，属于半残授权。
-- ---------------------------------------------------------------------------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT DISTINCT anchor.role_id, m.id
  FROM sys_menu m
  JOIN (
        SELECT rm.role_id
          FROM sys_role_menu rm
          JOIN sys_menu src ON src.id = rm.menu_id AND src.deleted_at IS NULL
         WHERE src.auth_code IN ('business:erp:inventory:list', 'business:erp:warehouse:update')
        UNION
        SELECT r.id FROM sys_role r WHERE r.role_code = 'SUPER_ADMIN' AND r.deleted_at IS NULL
       ) anchor ON TRUE
  JOIN sys_role r ON r.id = anchor.role_id AND r.deleted_at IS NULL
 WHERE m.id = '20000000-0000-0000-0000-000000004290'::uuid
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu dup WHERE dup.role_id = anchor.role_id AND dup.menu_id = m.id
   );

-- ---------------------------------------------------------------------------
-- ⑤ 定时任务行 + 参数行
--
--   `status = 1`（启用）而不是 V0210 当年的 0：与 V0245 之后的四个提醒任务同口径 ——
--   由 `sys_job` 驱动才会每轮落一条 `sys_job_log`（`scanned/low/raised/closed` 摘要）并回写
--   `last_run_at`，「定时任务」页能一眼看出「昨晚跑没跑、建了几条」。内置注册
--   （`water_check::register_builtin`）保留为兜底：任务行被删 / 被停用时照跑。
--   同名注册表是 HashMap 覆盖语义（`task::tests::dynamic_registration_overwrites_builtin_of_same_name`
--   守着），不会一天跑两遍。
--
--   cron `0 7 * * * *` = 每小时第 7 分（6 段，第 1 段是秒）。小时段是 `*`，
--   所以「UTC 还是北京时间」对这个任务不成立；第 7 分避开了整点与 07:10~07:40 的提醒尖峰。
-- ---------------------------------------------------------------------------
INSERT INTO sys_job (job_name, job_group, cron_expr, invoke_target, params, status, remark)
SELECT 'erp_stock_water_check', 'ERP', '0 7 * * * *', 'erp_stock_water_check', NULL, 1,
       '安全库存水位检查（启用：每小时第 7 分）：结存 < 生效最低水位 → 给仓管主管生成待办；库存回升 → 自动关闭该待办；每轮落 sys_job_log'
 WHERE NOT EXISTS (
        SELECT 1 FROM sys_job j WHERE j.job_name = 'erp_stock_water_check' AND j.deleted_at IS NULL
   );

INSERT INTO sys_config (
    id, config_key, config_value, config_type, description,
    category, config_level, is_modifiable, is_encrypted, is_restart, status,
    sort_order, ref_value
)
SELECT '2f121000-0000-4000-8000-000000004290'::uuid,
       'erp.stock_water.realert_days', '3', 'NUMBER',
       '水位待办重提醒间隔（天）：待办被人取消/完成后，同一格在间隔内不再重新生成（1 = 每天一次）。库存回升被系统自动关闭的待办不参与抑制 —— 回升后又跌破是新事件，会立刻提醒',
       'erp', 2, TRUE, FALSE, FALSE, 1, 40, '1~90'
 WHERE NOT EXISTS (
        SELECT 1 FROM sys_config c WHERE c.config_key = 'erp.stock_water.realert_days'
   );

-- ---------------------------------------------------------------------------
-- ⑥ 自检（任一不符即 RAISE EXCEPTION 回滚整个迁移）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n           integer;
    n_dup_sort  integer;
    n_roles     integer;
    n_half      integer;
    n_missing   integer;
    n_granted   integer;
    v            text;
BEGIN
    -- ===== 6.1 索引在、且是「部分唯一索引」，谓词三段齐全 =====
    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = current_schema()
       AND tablename = 'biz_task'
       AND indexname = 'uk_biz_task_open_inventory'
       AND indexdef LIKE '%UNIQUE%'
       AND indexdef LIKE '%doc_type%'
       AND indexdef LIKE '%INVENTORY%'
       AND indexdef LIKE '%deleted_at IS NULL%'
       AND indexdef LIKE '%TODO%'
       AND indexdef LIKE '%DOING%';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0429 自检失败：uk_biz_task_open_inventory 不是预期的部分唯一索引（doc_type=INVENTORY / deleted_at IS NULL / status IN (TODO,DOING)）';
    END IF;

    -- ===== 6.2 权限点形状正确（menu_type=3 / path NULL / 不可见 / 挂即时库存页 / name=code）=====
    SELECT count(*) INTO n
      FROM sys_menu c
      JOIN sys_menu p ON p.id = c.parent_id
     WHERE c.id = '20000000-0000-0000-0000-000000004290'::uuid
       AND c.menu_type = 3
       AND c.path IS NULL
       AND c.is_visible = FALSE
       AND c.status = 1
       AND c.deleted_at IS NULL
       AND c.name = c.auth_code
       AND c.auth_code = 'business:erp:stock-policy:check'
       AND p.menu_type = 2
       AND p.auth_code = 'business:erp:inventory:list'
       AND p.deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0429 自检失败：水位检查按钮节点形状不对（要求 menu_type=3 / path NULL / 不可见 / 挂「即时库存」页 / name=auth_code）';
    END IF;

    -- ===== 6.3 title 长度 ≤ 12（约定 12 的菜单命名规范：title 是显示名不是说明）=====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004290'::uuid
       AND length(title) <= 12;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0429 自检失败：菜单 title 超过 12 字（约定 12：title 是侧栏/授权树显示名，说明放 description）';
    END IF;

    -- ===== 6.4 同父 sort_order 不重复（约定 12）=====
    SELECT count(*) INTO n_dup_sort
      FROM (
            SELECT parent_id, sort_order
              FROM sys_menu
             WHERE parent_id = (SELECT parent_id FROM sys_menu
                                 WHERE id = '20000000-0000-0000-0000-000000004290'::uuid)
               AND deleted_at IS NULL
             GROUP BY parent_id, sort_order
            HAVING count(*) > 1
           ) dup;
    IF n_dup_sort <> 0 THEN
        RAISE EXCEPTION 'V0429 自检失败：即时库存页下有 % 组重复 sort_order（约定 12）', n_dup_sort;
    END IF;

    -- ===== 6.5 授权完整性 =====
    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0429：库里还没有任何角色（迁移先于种子），跳过授权核对；种子/bootstrap 之后重放本迁移即可补齐（幂等）';
    ELSE
        -- 6.5.1 半残授权：拿到四个水位码里任意一个但不是四个（能设置不能检查 / 能检查不能设置）
        SELECT count(*), string_agg(DISTINCT r.role_code, ', ') INTO n_half, v
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM sys_role_menu rm
                         JOIN sys_menu m ON m.id = rm.menu_id
                        WHERE rm.role_id = r.id
                          AND m.deleted_at IS NULL
                          AND m.auth_code IN ('business:erp:stock-policy:list',
                                              'business:erp:stock-policy:save',
                                              'business:erp:stock-policy:delete',
                                              'business:erp:stock-policy:check'))
           AND (SELECT count(DISTINCT m.auth_code) FROM sys_role_menu rm
                 JOIN sys_menu m ON m.id = rm.menu_id
                WHERE rm.role_id = r.id
                  AND m.deleted_at IS NULL
                  AND m.auth_code IN ('business:erp:stock-policy:list',
                                      'business:erp:stock-policy:save',
                                      'business:erp:stock-policy:delete',
                                      'business:erp:stock-policy:check')) <> 4;
        IF n_half <> 0 THEN
            RAISE EXCEPTION 'V0429 自检失败：% 个角色只拿到部分水位码（应为四个成对）→ %', n_half, COALESCE(v, '—');
        END IF;

        -- 6.5.2 锚点覆盖：持「即时库存」或「编辑仓库」的角色都必须拿到 check
        SELECT count(*), string_agg(r.role_code, ', ') INTO n_missing, v
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM sys_role_menu rm
                         JOIN sys_menu m ON m.id = rm.menu_id
                        WHERE rm.role_id = r.id
                          AND m.auth_code IN ('business:erp:inventory:list', 'business:erp:warehouse:update')
                          AND m.deleted_at IS NULL)
           AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm
                            WHERE rm.role_id = r.id
                              AND rm.menu_id = '20000000-0000-0000-0000-000000004290'::uuid);
        IF n_missing <> 0 THEN
            RAISE EXCEPTION 'V0429 自检失败：% 个仓库线角色没拿到水位检查码 → %', n_missing, COALESCE(v, '—');
        END IF;

        IF NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm
              JOIN sys_role r ON r.id = rm.role_id
             WHERE r.role_code = 'SUPER_ADMIN'
               AND rm.menu_id = '20000000-0000-0000-0000-000000004290'::uuid
        ) THEN
            RAISE NOTICE 'V0429：超管尚无显式授权行（空库首启时超管由种子晚于本迁移创建；重放本迁移即可补齐）';
        END IF;
    END IF;

    -- ===== 6.6 定时任务行与参数行在（重放不新增、也不覆盖运维改过的值）=====
    SELECT count(*) INTO n
      FROM sys_job
     WHERE deleted_at IS NULL
       AND job_name = 'erp_stock_water_check'
       AND cron_expr = '0 7 * * * *'
       AND invoke_target = 'erp_stock_water_check';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0429 自检失败：sys_job 里应有 1 行 erp_stock_water_check（cron 0 7 * * * * / invoke_target 同名），实得 %', n;
    END IF;

    SELECT count(*) INTO n
      FROM sys_config
     WHERE config_key = 'erp.stock_water.realert_days';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0429 自检失败：sys_config 里应有 1 行 erp.stock_water.realert_days，实得 %', n;
    END IF;

    SELECT count(DISTINCT role_id) INTO n_granted
      FROM sys_role_menu
     WHERE menu_id = '20000000-0000-0000-0000-000000004290'::uuid;
    RAISE NOTICE 'V0429: 自检通过（部分唯一索引 / 1 个权限点挂即时库存页 / 任务行 status=1 / 参数 realert_days=3 / 授权 % 个角色）',
        n_granted;
END $$;
