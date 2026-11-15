-- ============================================================
-- V0342：ERP 主数据编码 —— 「允许手工指定编码」开关（CODE-08）
--
-- 背景：CODE-01~07 让 6 个主数据的编码可以「留空自动取号（SUP-0001 / WH-01 …）
-- 或手工填写企业自有编码（GYS-001 / CK-001）」。但有些企业要求编码**强制统一**，
-- 不允许任何人手填。本迁移只补一条参数行 + 一个参数分类字典项，判定逻辑在
-- `business::erp::code_policy`（唯一读取点）。
--
-- # 参数
--
--   erp.master_data.allow_manual_code   true   允许手工指定主数据编码
--
-- 默认 **true** = 改动前的真实行为（本来就允许手填）。改 false 后：建单接口收到
-- 手工编码一律 `400 + 10001`「系统已禁止手工指定编码」，前端编码栏只读、
-- 「生成编码」按钮隐藏。
--
-- # config_type 写 `BOOLEAN`
--
-- `support::config::model::config_type` 是唯一口径（`STRING` / `NUMBER` / `BOOLEAN`
-- / `JSON`），前端据此渲染开关控件；写错类型配置页会退化成文本框。
--
-- # 分类 `erp_master_data`
--
-- 配置页按 `category` 分组、页签译名取字典 `config_category`。字典类型本身是
-- **seed 迁移** `migrations/seed/S__dict_data.sql` 种的，而 seed 在全部 V* 之后才跑，
-- 空库首启时直接 `SELECT` 会命中 0 行（V0196 踩过）。因此这里先确保字典类型存在，
-- 再插字典项。
-- ============================================================

-- ---------- ① 参数行 ----------
INSERT INTO sys_config (
    id, config_key, config_value, config_type, description,
    category, config_level, is_modifiable, is_encrypted, is_restart, status,
    sort_order, ref_value, select_value
) VALUES (
    'e2b00000-0000-4000-8000-000000000020',
    'erp.master_data.allow_manual_code',
    'true',
    'BOOLEAN',
    '【允许手工指定编码】开启后（默认）主数据建单可手填编码，企业自有编码体系可直接对接；'
    || '关闭后编码栏只读、「生成编码」按钮隐藏，编码完全由系统按号段规则生成'
    || '（供应商 SUP-0001 / 客户 CUS-0001 / 物料 MAT-0001 / 仓库 WH-01 / 资产分类 AC-01 / 费用类型 ET-01）',
    'erp_master_data', 2, TRUE, FALSE, FALSE, 1,
    10, 'true|false', 'true|开启,false|关闭'
)
ON CONFLICT (config_key) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- ② 参数分类字典项（配置页页签译名） ----------
INSERT INTO sys_dict_type (id, dict_code, dict_name, description, status, sort_order)
VALUES (
    'e2b00000-0000-4000-8000-000000000100',
    'config_category', '参数分类', '系统参数所属业务分类（配置管理页签）', 1, 50
)
ON CONFLICT (dict_code) WHERE deleted_at IS NULL DO NOTHING;

INSERT INTO sys_dict_item (dict_type_id, item_value, item_label, sort_order, is_default, status)
SELECT t.id, 'erp_master_data', 'ERP 主数据', 40, FALSE, 1
  FROM sys_dict_type t
 WHERE t.dict_code = 'config_category'
   AND t.deleted_at IS NULL
ON CONFLICT (dict_type_id, item_value) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- ③ 自检（空库首启与有数据重放都必须过） ----------
DO $$
DECLARE
    v_cfg int;
    v_bad int;
    v_default int;
    v_dict int;
BEGIN
    SELECT count(*) INTO v_cfg
      FROM sys_config
     WHERE config_key = 'erp.master_data.allow_manual_code'
       AND status = 1
       AND deleted_at IS NULL;
    IF v_cfg <> 1 THEN
        RAISE EXCEPTION 'V0342 自检失败：主数据编码开关应 1 条启用，实际命中 % 条', v_cfg;
    END IF;

    SELECT count(*) INTO v_bad
      FROM sys_config
     WHERE config_key = 'erp.master_data.allow_manual_code'
       AND deleted_at IS NULL
       AND (config_type <> 'BOOLEAN'
            OR is_modifiable IS NOT TRUE
            OR is_encrypted IS TRUE
            OR category <> 'erp_master_data');
    IF v_bad <> 0 THEN
        RAISE EXCEPTION 'V0342 自检失败：主数据编码开关的类型 / 可修改 / 加密 / 分类不合口径';
    END IF;

    -- 默认值逐字 `true`：升级后**不能**改变存量环境「可以手填」的行为
    SELECT count(*) INTO v_default
      FROM sys_config
     WHERE config_key = 'erp.master_data.allow_manual_code'
       AND deleted_at IS NULL
       AND config_value = 'true';
    IF v_default <> 1 THEN
        RAISE EXCEPTION 'V0342 自检失败：开关默认值必须是 true（实际命中 % 条）', v_default;
    END IF;

    SELECT count(*) INTO v_dict
      FROM sys_dict_item i
      JOIN sys_dict_type t ON t.id = i.dict_type_id
     WHERE t.dict_code = 'config_category'
       AND t.deleted_at IS NULL
       AND i.item_value = 'erp_master_data'
       AND i.deleted_at IS NULL;
    IF v_dict <> 1 THEN
        RAISE EXCEPTION 'V0342 自检失败：config_category 缺 erp_master_data 字典项（实际 % 条）', v_dict;
    END IF;

    RAISE NOTICE 'V0342：主数据编码开关 erp.master_data.allow_manual_code 已就位（默认 true）';
END $$;
