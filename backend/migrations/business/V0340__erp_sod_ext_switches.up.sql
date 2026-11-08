-- ============================================================
-- V0340：ERP 内控 —— SoD 扩展三开关（P3-07）
--
-- 背景：V0196 只种了「制单 ≠ 审批 / 过账 / 付款」三个开关，ARCH-8 §5 的职责对清单里
-- 还有三对没有落地（盘点确认、采购验收、审批后执行）。代码侧
-- （`business::erp::separation`）已加 `Duty::{ApproveExecute, Stocktake, PurchaseAccept}`
-- 与对应接入点；本迁移只补**参数行**，让管理员能在「支撑功能 / 参数配置 / ERP 内控」
-- 里看到并开关它们。
--
-- # 三条参数（默认值 = 改动前的真实行为：全 `false`）
--
--   erp.separation_of_duties.approve_execute    false  审批人不能自己执行（付款执行 / 盘点过账）
--   erp.separation_of_duties.stocktake          false  制单人不能确认自己建的盘点单
--   erp.separation_of_duties.purchase_accept    false  采购员不能验收自己采的货
--
-- # 为什么三条都默认 false（而不是像 approve/pay 那样默认 true）
--
-- V0196 里 approve/pay 写 true 是**记录既有行为**（那两道 403 闸本来就在）。
-- 本卡的三个开关是**新能力**：存量环境里「制单人自己确认盘点单」「采购员自己收货」
-- 「审批完自己点执行」都是今天合法且常见的操作。默认 true 会让升级后这些操作
-- 突然 409 —— 那是升级事故，不是内控加固。**默认关，管理员按需开。**
--
-- # 总闸语义（沿用 V0196）
--
-- `erp.separation_of_duties.enabled = true` 时，未单独配置的项按下表默认成「开」
-- （`separation::resolve` 的第 ② 步，只升不降）。因此只开总闸也能让这三对新职责生效。
--
-- # config_type 写 `BOOLEAN`
--
-- `support::config::model::config_type` 是唯一口径，前端据此渲染开关控件；
-- 写错类型配置页会退化成文本框。category 沿用 `erp_internal_control`（V0196 已种字典项）。
-- ============================================================

-- ---------- ① 三条扩展开关 ----------
INSERT INTO sys_config (
    id, config_key, config_value, config_type, description,
    category, config_level, is_modifiable, is_encrypted, is_restart, status,
    sort_order, ref_value, select_value
) VALUES
    (
        'e2b00000-0000-4000-8000-000000000006',
        'erp.separation_of_duties.approve_execute',
        'false',
        'BOOLEAN',
        '【审批≠执行】开启后审批人不能自己执行已批过的单据'
        || '（付款执行 / 盘点过账）；默认关闭 = 审批通过后由本人或系统继续执行（改动前行为）',
        'erp_internal_control', 1, TRUE, FALSE, FALSE, 1,
        50, 'true|false', 'true|开启,false|关闭'
    ),
    (
        'e2b00000-0000-4000-8000-000000000007',
        'erp.separation_of_duties.stocktake',
        'false',
        'BOOLEAN',
        '【制单≠盘点确认】开启后制单人不能确认自己建的盘点单'
        || '（草稿提交为待过账那一步）；默认关闭 = 制单人可自行确认（改动前行为）',
        'erp_internal_control', 1, TRUE, FALSE, FALSE, 1,
        60, 'true|false', 'true|开启,false|关闭'
    ),
    (
        'e2b00000-0000-4000-8000-000000000008',
        'erp.separation_of_duties.purchase_accept',
        'false',
        'BOOLEAN',
        '【采购≠验收】开启后采购员（采购单制单人）不能验收自己采的货'
        || '（建收货单确认收货）；默认关闭 = 采购员可自己收货（改动前行为）',
        'erp_internal_control', 1, TRUE, FALSE, FALSE, 1,
        70, 'true|false', 'true|开启,false|关闭'
    )
ON CONFLICT (config_key) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- ② 自检（空库首启与有数据重放都必须过） ----------
DO $$
DECLARE
    v_cfg int;
    v_bad int;
    v_default int;
    v_all int;
BEGIN
    -- 三条扩展参数齐全、启用、可修改、类型 BOOLEAN、落在 ERP 内控分组
    SELECT count(*) INTO v_cfg
      FROM sys_config
     WHERE config_key IN (
               'erp.separation_of_duties.approve_execute',
               'erp.separation_of_duties.stocktake',
               'erp.separation_of_duties.purchase_accept'
           )
       AND status = 1
       AND deleted_at IS NULL;
    IF v_cfg <> 3 THEN
        RAISE EXCEPTION 'V0340 自检失败：SoD 扩展参数应 3 条启用，实际命中 % 条', v_cfg;
    END IF;

    SELECT count(*) INTO v_bad
      FROM sys_config
     WHERE config_key IN (
               'erp.separation_of_duties.approve_execute',
               'erp.separation_of_duties.stocktake',
               'erp.separation_of_duties.purchase_accept'
           )
       AND deleted_at IS NULL
       AND (config_type <> 'BOOLEAN'
            OR is_modifiable IS NOT TRUE
            OR category <> 'erp_internal_control');
    IF v_bad <> 0 THEN
        RAISE EXCEPTION 'V0340 自检失败：% 条扩展参数的类型 / 可修改标记 / 分类不合口径', v_bad;
    END IF;

    -- 默认值必须逐字是 false（三条都是纯增量，任何一条变 true 都是升级事故）
    SELECT count(*) INTO v_default
      FROM sys_config
     WHERE config_key IN (
               'erp.separation_of_duties.approve_execute',
               'erp.separation_of_duties.stocktake',
               'erp.separation_of_duties.purchase_accept'
           )
       AND deleted_at IS NULL
       AND config_value = 'false';
    IF v_default <> 3 THEN
        RAISE EXCEPTION 'V0340 自检失败：扩展开关默认值必须是 false（实际命中 % 条）', v_default;
    END IF;

    -- 与 V0196 的四个老键合起来必须是 7 条（少一条说明参数命名空间被谁改过）
    SELECT count(*) INTO v_all
      FROM sys_config
     WHERE config_key LIKE 'erp.separation_of_duties.%'
       AND deleted_at IS NULL;
    IF v_all < 7 THEN
        RAISE EXCEPTION 'V0340 自检失败：erp.separation_of_duties.* 应至少 7 条，实际 % 条', v_all;
    END IF;

    RAISE NOTICE 'V0340：SoD 扩展三开关已就位（approve_execute / stocktake / purchase_accept，默认全 false）';
END $$;
