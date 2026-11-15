-- ============================================================
-- V0196：ERP 内控 —— 制单 ≠ 审批 / 过账 / 付款（**可配开关**）
--
-- 背景（卡 t_31f0d857 / ERP-B2）：评审实测「收货单制单人 = 过账人」8 条，
-- 内控上应当分离，但小企业常常只有一个人能干活，硬分离等于单据谁也点不动。
-- 因此把三项职责拆成三个独立开关，由管理员在「支撑功能 / 参数配置」里按需打开。
--
-- # 这次迁移只做两件事（不建表、不加列、不改任何业务表）
--
--   ① `sys_config` 四条参数（**默认值与改动前的真实行为逐字一致**）：
--        erp.separation_of_duties.enabled   false  总闸：一键把未单独配置的项默认成「开」
--        erp.separation_of_duties.post      false  制单 ↔ 过账（收货单过账 / 调拨发货收货）
--        erp.separation_of_duties.approve   true   制单 ↔ 审批（采购 / 过账 / 作废 / 红冲审批）
--        erp.separation_of_duties.pay       true   制单 ↔ 付款（P17 付款审批闸）
--      两个 `true` 不是「新开的内控」，而是**记录既有行为**：审批与付款本就有 403 内控闸，
--      开关化之后它们必须仍然默认拦，否则升级即静默放宽内控。
--   ② `config_category` 字典项 `erp_internal_control` → 「ERP 内控」，
--      让配置页（按 category 分组）自动多出一组「ERP 内控」，不需要改前端代码。
--
-- # 为什么 config_type 写 `BOOLEAN`
--
-- `backend/src/support/config/model.rs::config_type` 是唯一口径（`STRING` / `NUMBER` /
-- `BOOLEAN` / `JSON`，前端据此渲染开关控件）。种子里老的 `BOOL` 是历史遗留，
-- 新参数一律按白名单写 —— 写错类型前端就退化成文本框。
-- ============================================================

-- ---------- ① 四条内控参数 ----------
INSERT INTO sys_config (
    id, config_key, config_value, config_type, description,
    category, config_level, is_modifiable, is_encrypted, is_restart, status,
    sort_order, ref_value, select_value
) VALUES
    (
        'e2b00000-0000-4000-8000-000000000001',
        'erp.separation_of_duties.enabled',
        'false',
        'BOOLEAN',
        '【总闸】一键开启职责分离：未单独配置的项（post / approve / pay）'
        || '默认按「开」处理；只升不降，要关某一项请显式关闭那一项',
        'erp_internal_control', 1, TRUE, FALSE, FALSE, 1,
        10, 'true|false', 'true|开启,false|关闭'
    ),
    (
        'e2b00000-0000-4000-8000-000000000002',
        'erp.separation_of_duties.post',
        'false',
        'BOOLEAN',
        '【制单≠过账】开启后制单人不能自己过账（收货过账 / 调拨发货收货）；'
        || '小团队可不开，内控要求上建议开启',
        'erp_internal_control', 1, TRUE, FALSE, FALSE, 1,
        20, 'true|false', 'true|开启,false|关闭'
    ),
    (
        'e2b00000-0000-4000-8000-000000000003',
        'erp.separation_of_duties.approve',
        'true',
        'BOOLEAN',
        '【制单≠审批】开启后制单人不能审批自己那一票'
        || '（采购 / 收货过账与作废 / 凭证红冲 / 应付作废）；默认开启 = 系统原有内控闸',
        'erp_internal_control', 1, TRUE, FALSE, FALSE, 1,
        30, 'true|false', 'true|开启,false|关闭'
    ),
    (
        'e2b00000-0000-4000-8000-000000000004',
        'erp.separation_of_duties.pay',
        'true',
        'BOOLEAN',
        '【制单≠付款】开启后制单人不能审批自己提交的付款'
        || '（P17 付款闸）；默认开启 = 系统原有内控闸',
        'erp_internal_control', 1, TRUE, FALSE, FALSE, 1,
        40, 'true|false', 'true|开启,false|关闭'
    )
ON CONFLICT (config_key) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- ② 参数分类字典项（配置页按 category 分组 / 页签译名） ----------
--
-- ⚠️ 不能只写「SELECT id FROM sys_dict_type WHERE dict_code='config_category'」：
-- 字典类型本身是 **seed 迁移** `migrations/seed/S__dict_data.sql` 种的，而 seed 在
-- **全部 V* 迁移之后**才跑。空库首启时这条 SELECT 命中 0 行 → 字典项插不进去 →
-- 下面的自检直接把启动打挂（实测：`INSERT 0 0` + `V0196 自检失败：config_category
-- 缺 erp_internal_control 字典项`）。
-- 因此这里**先确保字典类型存在**（固定 UUID + 与 seed 同一个 `ON CONFLICT` 目标），
-- 再插字典项；seed 之后照常 `DO UPDATE` 覆盖名称/描述，两处不会打架。
INSERT INTO sys_dict_type (id, dict_code, dict_name, description, status, sort_order)
VALUES (
    'e2b00000-0000-4000-8000-000000000100',
    'config_category', '参数分类', '系统参数所属业务分类（配置管理页签）', 1, 50
)
ON CONFLICT (dict_code) WHERE deleted_at IS NULL DO NOTHING;

INSERT INTO sys_dict_item (dict_type_id, item_value, item_label, sort_order, is_default, status)
SELECT t.id, 'erp_internal_control', 'ERP 内控', 30, FALSE, 1
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
BEGIN
    -- 四条参数齐全、启用、可修改、类型是 BOOLEAN
    SELECT count(*) INTO v_cfg
      FROM sys_config
     WHERE config_key IN (
               'erp.separation_of_duties.enabled',
               'erp.separation_of_duties.post',
               'erp.separation_of_duties.approve',
               'erp.separation_of_duties.pay'
           )
       AND status = 1
       AND deleted_at IS NULL;
    IF v_cfg <> 4 THEN
        RAISE EXCEPTION 'V0196 自检失败：ERP 内控参数应 4 条启用，实际命中 % 条', v_cfg;
    END IF;

    SELECT count(*) INTO v_bad
      FROM sys_config
     WHERE config_key IN (
               'erp.separation_of_duties.enabled',
               'erp.separation_of_duties.post',
               'erp.separation_of_duties.approve',
               'erp.separation_of_duties.pay'
           )
       AND deleted_at IS NULL
       AND (config_type <> 'BOOLEAN' OR is_modifiable IS NOT TRUE OR category <> 'erp_internal_control');
    IF v_bad <> 0 THEN
        RAISE EXCEPTION 'V0196 自检失败：% 条内控参数的类型 / 可修改标记 / 分类不合口径', v_bad;
    END IF;

    -- 默认值必须逐字钉死（post=false 不影响存量；approve/pay=true 保住既有内控闸）
    SELECT count(*) INTO v_default
      FROM sys_config
     WHERE (config_key = 'erp.separation_of_duties.enabled'  AND config_value = 'false')
        OR (config_key = 'erp.separation_of_duties.post'     AND config_value = 'false')
        OR (config_key = 'erp.separation_of_duties.approve'  AND config_value = 'true')
        OR (config_key = 'erp.separation_of_duties.pay'      AND config_value = 'true');
    IF v_default <> 4 THEN
        RAISE EXCEPTION 'V0196 自检失败：内控参数默认值被改动（应 false/false/true/true，实际命中 % 条）', v_default;
    END IF;

    -- 字典项存在（前端页签才有中文名）
    IF NOT EXISTS (
        SELECT 1 FROM sys_dict_item i
          JOIN sys_dict_type t ON t.id = i.dict_type_id
         WHERE t.dict_code = 'config_category'
           AND i.item_value = 'erp_internal_control'
           AND i.deleted_at IS NULL
    ) THEN
        RAISE EXCEPTION 'V0196 自检失败：config_category 缺 erp_internal_control 字典项';
    END IF;
END $$;
