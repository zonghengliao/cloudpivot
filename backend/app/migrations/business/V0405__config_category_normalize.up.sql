-- ============================================================================
-- V0405__config_category_normalize（参数配置分类规范化 · 2026-11-15 / laoliu）
--
-- 【为什么要有这个迁移】
--   参数配置页（支撑功能 → 参数配置）的 tab 完全由 `sys_config.category` 驱动，
--   tab 的中文名由字典 `config_category` 翻译。实测 145 上三层问题：
--
--   ① **分类为空**：`erp.cost.enabled` 的 category=''，不在任何 tab 里
--      ⇒ 用户在页面上**找不到成本核算开关**，只能靠 curl 改库（本次事故的直接原因）。
--   ② **字典缺翻译**：config_category 只有 5 条，而 sys_config 实际用了 10 种分类
--      ⇒ 页面 tab 直接显示 `erp` / `erp_budget` / `hr` / `announcement` / `changelog`
--        这些英文编码，用户原话「很没规则，简直就是乱七八糟」。
--   ③ **分类太碎**：ERP 被拆成 5 个分类（erp / erp_budget / erp_internal_control /
--      erp_master_data / erp_cost），其中 erp_master_data 和 erp_cost 各只有 1 项
--      ⇒ 1 项也占一个 tab，横向看毫无规律。
--
-- 【本迁移做的事】
--   A. 补字典 config_category 缺失的 5 条中文名
--   B. 合并 ERP 分类：5 → 4（erp + erp_master_data → erp_basic；erp_internal_control → erp_control）
--   C. 归类 2 个游离项：erp.cost.enabled → erp_cost；auth.idle_timeout_minutes → auth
--   D. 给这 2 项补 description（原本都是空，页面上只显示一个 key 看不懂）
--
-- 【为什么只改 category / description / 字典，不动 config_key 与 config_value】
--   `category` 只影响**页面分组**，不参与任何业务判断（业务读配置一律按 config_key）。
--   所以这是一个**纯展示层**迁移，零业务风险，也不需要重启服务。
--
-- 【幂等性】
--   全部用 `WHERE category = '旧值'` / `NOT EXISTS` 守卫：
--     · 145（已手工改过）→ 所有条件不命中 → 空转
--     · 144（未改）      → 全部生效
--     · 重复执行         → 第二次空转
--
-- 【自检】
--   末尾断言：① 没有空分类 ② 每个分类都有字典中文名 ③ ERP 只剩 4 类。
--   任一不满足 → RAISE EXCEPTION，让迁移失败（而不是带着脏数据上线）。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- A. 补字典 config_category 缺失项
--    已有：auth / log / erp_internal_control / erp_cost / erp_master_data
--    补：announcement / hr / changelog / erp_basic / erp_control
--        并把 erp_budget 补上（原本也没有）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    v_dict_id uuid;
    rec       record;
    v_missing record;
BEGIN
    SELECT id INTO v_dict_id
    FROM sys_dict_type
    WHERE dict_code = 'config_category' AND deleted_at IS NULL
    LIMIT 1;

    IF v_dict_id IS NULL THEN
        -- 字典类型本身不存在：说明这套环境没有参数分类字典，直接跳过（不阻断迁移）
        RAISE NOTICE 'V0405: 未找到字典 config_category，跳过字典补录';
        RETURN;
    END IF;

    FOR v_missing IN
        SELECT * FROM (VALUES
            ('announcement', '公告',        60),
            ('hr',           '人力资源',    70),
            ('changelog',    '变更日志',    80),
            ('erp_basic',    'ERP 基础',    30),
            ('erp_budget',   'ERP 预算',    35),
            ('erp_control',  'ERP 内控',    40),
            ('erp_cost',     'ERP 成本',    45)
        ) AS t(item_value, item_label, sort_order)
    LOOP
        IF NOT EXISTS (
            SELECT 1 FROM sys_dict_item
            WHERE dict_type_id = v_dict_id AND item_value = v_missing.item_value
        ) THEN
            INSERT INTO sys_dict_item (
                id, dict_type_id, item_value, item_label, sort_order,
                status, created_at, updated_at
            ) VALUES (
                gen_random_uuid(), v_dict_id, v_missing.item_value, v_missing.item_label,
                v_missing.sort_order, 1, now(), now()
            );
        ELSE
            -- 已存在则对齐标签与排序（容错：早期手工加的可能是裸编码）
            UPDATE sys_dict_item
            SET item_label = v_missing.item_label,
                sort_order = v_missing.sort_order,
                updated_at = now()
            WHERE dict_type_id = v_dict_id
              AND item_value = v_missing.item_value
              AND (item_label IS DISTINCT FROM v_missing.item_label
                   OR sort_order IS DISTINCT FROM v_missing.sort_order);
        END IF;
    END LOOP;

    -- 清掉已被合并掉的旧分类编码（erp / erp_internal_control / erp_master_data）
    DELETE FROM sys_dict_item
    WHERE dict_type_id = v_dict_id
      AND item_value IN ('erp', 'erp_internal_control', 'erp_master_data');
END $$;

-- ---------------------------------------------------------------------------
-- B. 合并 ERP 分类（5 → 4）
-- ---------------------------------------------------------------------------
-- erp_master_data（1 项）→ erp_basic
UPDATE sys_config
SET category = 'erp_basic', updated_at = now()
WHERE category = 'erp_master_data' AND deleted_at IS NULL;

-- erp（在途/提醒，4~5 项）→ erp_basic
UPDATE sys_config
SET category = 'erp_basic', updated_at = now()
WHERE category = 'erp' AND deleted_at IS NULL;

-- erp_internal_control（职责分离，8 项）→ erp_control（名字更短、和 erp_budget/erp_cost 同构）
UPDATE sys_config
SET category = 'erp_control', updated_at = now()
WHERE category = 'erp_internal_control' AND deleted_at IS NULL;

-- ---------------------------------------------------------------------------
-- C. 归类游离项 + 补说明
-- ---------------------------------------------------------------------------
-- 成本核算模块总开关（原本 category='' 且 description=''）
UPDATE sys_config
SET category    = 'erp_cost',
    description = '成本核算模块总开关：关闭后侧边栏隐藏成本菜单（COST-B4-4），关闭时成本页面按 URL 仍可进但菜单不显示',
    updated_at  = now()
WHERE config_key = 'erp.cost.enabled'
  AND deleted_at IS NULL
  AND (category IS NULL OR category = '' OR description IS NULL OR description = '');

-- 空闲自动退出登录（原本 category='' 且 description=''）
UPDATE sys_config
SET category    = 'auth',
    description = '空闲自动退出登录：无操作超过该分钟数自动登出；0=禁用（仅超管可设），上限 60；仅超管可把值设为「无限」',
    updated_at  = now()
WHERE config_key = 'auth.idle_timeout_minutes'
  AND deleted_at IS NULL
  AND (category IS NULL OR category = '' OR description IS NULL OR description = '');

-- ---------------------------------------------------------------------------
-- D. 自检：三条断言，任一不满足就让迁移失败
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    v_dict_id   uuid;
    v_empty     int;
    v_no_label  int;
    v_erp_kinds int;
    v_bad       text;
BEGIN
    -- ① 不允许还有空分类
    SELECT count(*) INTO v_empty
    FROM sys_config
    WHERE deleted_at IS NULL AND (category IS NULL OR category = '');
    IF v_empty > 0 THEN
        RAISE EXCEPTION 'V0405 自检失败：仍有 % 条配置没有分类（category 为空）', v_empty;
    END IF;

    -- ② 每个用到的分类都必须在字典里有中文名
    SELECT id INTO v_dict_id
    FROM sys_dict_type WHERE dict_code = 'config_category' AND deleted_at IS NULL LIMIT 1;

    IF v_dict_id IS NOT NULL THEN
        SELECT count(*) INTO v_no_label
        FROM (
            SELECT DISTINCT category AS cat FROM sys_config WHERE deleted_at IS NULL
        ) c
        WHERE NOT EXISTS (
            SELECT 1 FROM sys_dict_item i
            WHERE i.dict_type_id = v_dict_id AND i.item_value = c.cat
        );
        IF v_no_label > 0 THEN
            SELECT string_agg(c.cat, ', ') INTO v_bad
            FROM (
                SELECT DISTINCT category AS cat FROM sys_config WHERE deleted_at IS NULL
            ) c
            WHERE NOT EXISTS (
                SELECT 1 FROM sys_dict_item i
                WHERE i.dict_type_id = v_dict_id AND i.item_value = c.cat
            );
            RAISE EXCEPTION 'V0405 自检失败：以下分类在字典 config_category 里没有中文名 → %', v_bad;
        END IF;
    END IF;

    -- ③ ERP 只能剩 4 类：erp_basic / erp_budget / erp_control / erp_cost
    SELECT count(DISTINCT category) INTO v_erp_kinds
    FROM sys_config
    WHERE deleted_at IS NULL AND config_key LIKE 'erp.%';
    IF v_erp_kinds > 4 THEN
        RAISE EXCEPTION 'V0405 自检失败：ERP 仍有 % 个分类（期望 4 个：erp_basic/erp_budget/erp_control/erp_cost）', v_erp_kinds;
    END IF;

    RAISE NOTICE 'V0405 自检通过：无空分类、字典全覆盖、ERP 分类 = % 类', v_erp_kinds;
END $$;
