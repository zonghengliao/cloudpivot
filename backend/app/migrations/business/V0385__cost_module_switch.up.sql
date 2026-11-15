-- ============================================================================
-- V0385：成本核算升级 · 模块总开关 `erp.cost.enabled`（ARCH-9 §7.4 灰度 · COST-B4-4）
--
-- 卡：t_e757c1e7（COST-B4-4【成本】开关 `erp.cost.enabled`）
-- 依据：docs/architecture/ARCH-9-成本核算升级设计.md
--       §7.4「分几批上线（能否灰度）」—— 新增一个 `sys_config` 开关 `erp.cost.enabled`
--       （`BOOLEAN`，默认 `false`，`config_level = 2` 业务级）；
--       §6.1 页面清单（本开关关闭时要隐藏的 6 个页面菜单，V0378 已种，本迁移**不动 sys_menu**）；
--       §8.3 已知风险（12 条，风险口径见下方「# 与风险的关系」）。
--
-- # 只种一行参数 + 一个字典项，不动任何业务表、不动菜单
--   菜单隐藏是**运行期过滤**（`business::shared::cost_switch::filter_menus`）：开关一改
--   立刻生效，不需要重启、不需要重跑迁移、也不需要在 `sys_menu.is_visible` 上做持久改写
--   （后者会在「开关关→开」时把管理员手工调过的可见性覆盖掉）。
--
-- # 默认值必须是 false
--   存量环境里成本核算从来没上线过（B1~B3 才落地）：默认 `true` 等于升级当天就把
--   6 个页面、20 个按钮、30 多个接口一次性暴露给全部租户，那是升级事故不是发版。
--   试点客户由管理员在「系统参数」页手工打开（§7.4 的 B2/B3 门槛正是「试点客户」）。
--
-- # config_level = 2（业务级）
--   与设计 §7.4 逐字一致。1 = 系统级（默认值通常由运维定、影响全站安全策略），
--   2 = 业务级（客户按自身业务决定开不开）。
--
-- # 与风险的关系（§8.3）
--   R-1 数据源质量 / R-4 完工进度拍脑袋 / R-5 移动加权口径 / R-12 BOM 缺失：都属于
--   「成本算得准不准」，开关关闭时这些风险不暴露给客户（模块整体不可见）；
--   R-6 期间唯一键 / R-9 重算删行 / R-10 与薪资双计提：属于「已经开了之后怎么兜」，
--   由各卡的事务断言与巡检兜住，与本开关无关 —— 本开关只解决「先不给谁用」。
--
-- # 幂等
--   `ON CONFLICT (config_key) WHERE deleted_at IS NULL DO NOTHING`：重放 0 行；
--   不覆盖管理员改过的值（值被改过之后再跑迁移**不会**把它掰回 false）。
-- ============================================================================

-- ---------- ① 开关本身 ----------
INSERT INTO sys_config (
    id, config_key, config_value, config_type, description,
    category, config_level, is_modifiable, is_encrypted, is_restart, status,
    sort_order, ref_value, select_value
) VALUES (
    'e2b00000-0000-4000-8000-000000000010',
    'erp.cost.enabled',
    'false',
    'BOOLEAN',
    '【成本核算模块】开启后侧边栏出现成本对象 / 工时台账 / 费用台账 / 分摊规则 / 成本核算 / 批次管理 6 个页面，'
    || '相关接口放行；关闭时这些菜单与按钮隐藏、接口一律 403（领料单的成本对象字段也隐藏）。'
    || '切换即时生效，不需要重启。默认关闭 = 成本核算未上线（存量客户的现状）',
    'erp_cost', 2, TRUE, FALSE, FALSE, 1,
    10, 'true|false', 'true|开启,false|关闭'
)
ON CONFLICT (config_key) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- ② 参数分类字典项（配置页按 category 分组 / 页签译名） ----------
--
-- ⚠️ 与 V0196 同一个坑：`config_category` 这个**字典类型**本身是 seed 迁移
-- `migrations/seed/S__dict_data.sql` 种的，而 seed 在全部 V* 迁移之后才跑。空库首启时
-- 直接 `SELECT id FROM sys_dict_type WHERE dict_code='config_category'` 会命中 0 行，
-- 字典项插不进去 → 下面自检把启动打挂。故这里**先确保字典类型存在**再插字典项。
INSERT INTO sys_dict_type (id, dict_code, dict_name, description, status, sort_order)
VALUES (
    'e2b00000-0000-4000-8000-000000000100',
    'config_category', '参数分类', '系统参数所属业务分类（配置管理页签）', 1, 50
)
ON CONFLICT (dict_code) WHERE deleted_at IS NULL DO NOTHING;

INSERT INTO sys_dict_item (dict_type_id, item_value, item_label, sort_order, is_default, status)
SELECT t.id, 'erp_cost', 'ERP 成本', 35, FALSE, 1
  FROM sys_dict_type t
 WHERE t.dict_code = 'config_category'
   AND t.deleted_at IS NULL
ON CONFLICT (dict_type_id, item_value) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- ③ 自检（空库首启与有数据重放都必须过） ----------
DO $$
DECLARE
    v_cfg     int;
    v_bad     int;
    v_default int;
    v_dict    int;
BEGIN
    -- 3.1 开关存在、启用、可修改
    SELECT count(*) INTO v_cfg
      FROM sys_config
     WHERE config_key = 'erp.cost.enabled'
       AND status = 1
       AND deleted_at IS NULL;
    IF v_cfg <> 1 THEN
        RAISE EXCEPTION 'V0385 自检失败：erp.cost.enabled 应有且仅有 1 条启用参数，实际 %', v_cfg;
    END IF;

    -- 3.2 形态：BOOLEAN / 业务级 / 可修改 / 明文 / 不要求重启 / 落在 ERP 成本分组
    SELECT count(*) INTO v_bad
      FROM sys_config
     WHERE config_key = 'erp.cost.enabled'
       AND deleted_at IS NULL
       AND (config_type <> 'BOOLEAN'
            OR config_level <> 2
            OR is_modifiable IS NOT TRUE
            OR is_encrypted IS NOT FALSE
            OR is_restart IS NOT FALSE
            OR category <> 'erp_cost');
    IF v_bad <> 0 THEN
        RAISE EXCEPTION 'V0385 自检失败：开关的类型 / 级别 / 可修改 / 分组口径不对（% 条）', v_bad;
    END IF;

    -- 3.3 默认值：只允许 true/false 两个词；首次落地必须是 false
    --     （重放时管理员可能已手工打开 → 这里只校验「值合法」，不强制 false）
    SELECT count(*) INTO v_bad
      FROM sys_config
     WHERE config_key = 'erp.cost.enabled'
       AND deleted_at IS NULL
       AND config_value NOT IN ('true', 'false');
    IF v_bad <> 0 THEN
        RAISE EXCEPTION 'V0385 自检失败：开关值只能是 true / false';
    END IF;

    -- 3.4 分类字典项存在（配置页页签才会显示「ERP 成本」而不是裸值 erp_cost）
    SELECT count(*) INTO v_dict
      FROM sys_dict_item i
      JOIN sys_dict_type t ON t.id = i.dict_type_id AND t.deleted_at IS NULL
     WHERE t.dict_code = 'config_category'
       AND i.item_value = 'erp_cost'
       AND i.deleted_at IS NULL;
    IF v_dict <> 1 THEN
        RAISE EXCEPTION 'V0385 自检失败：config_category 缺 erp_cost 字典项';
    END IF;

    RAISE NOTICE 'V0385：成本核算开关 erp.cost.enabled 已就位（BOOLEAN / 业务级 / 默认 false）';
END $$;
