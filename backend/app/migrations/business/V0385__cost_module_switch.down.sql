-- ============================================================================
-- V0385 down：撤销成本核算模块开关 `erp.cost.enabled`（COST-B4-4）
--
-- # 撤销范围（只有本迁移写进去的两行）
--   sys_config  erp.cost.enabled
--   sys_dict_item  config_category / erp_cost
--
-- # 回滚的业务后果（写在这里，避免有人以为「回滚无害」）
--   代码侧（`business::shared::cost_switch::is_enabled`）参数缺失时的默认值是 **false**：
--   回滚之后成本核算模块**变为整体关闭**（菜单隐藏、接口 403、领料单成本对象字段隐藏），
--   且管理员再没有地方把它打开。这是**收紧**，不会让任何数据变得可写。
--
--   已有业务数据不受影响：本关开只影响「能不能进模块」，不动 biz_cost_* 任何一行。
--   ⚠️ 若客户已经把成本核算跑起来（有 SETTLED 批次），回滚前请确认这是有意的
--   —— 关掉之后这些批次在界面上就看不到了，但仍可照常查询库与巡检。
--
-- `DELETE ... WHERE config_key = ...` ⇒ 重复执行不报错。
-- ============================================================================

DO $$
DECLARE
    v_left int;
BEGIN
    -- 回滚前看清：开关当前是不是开着的（开着的环境关掉是「有业务影响」的回滚）
    SELECT count(*) INTO v_left
      FROM sys_config
     WHERE config_key = 'erp.cost.enabled'
       AND deleted_at IS NULL
       AND config_value = 'true';
    IF v_left > 0 THEN
        RAISE NOTICE 'V0385 down：erp.cost.enabled 当前为 true —— 回滚后成本核算模块将整体关闭（菜单/接口/字段全部隐藏）';
    END IF;
END $$;

DELETE FROM sys_config WHERE config_key = 'erp.cost.enabled';

DELETE FROM sys_dict_item i
 USING sys_dict_type t
 WHERE i.dict_type_id = t.id
   AND t.dict_code = 'config_category'
   AND i.item_value = 'erp_cost';

-- 自检：确实不在了
DO $$
DECLARE
    v_cfg  int;
    v_dict int;
BEGIN
    SELECT count(*) INTO v_cfg FROM sys_config WHERE config_key = 'erp.cost.enabled';
    IF v_cfg <> 0 THEN
        RAISE EXCEPTION 'V0385 down 自检失败：仍有 % 条 erp.cost.enabled 未删除', v_cfg;
    END IF;

    SELECT count(*) INTO v_dict
      FROM sys_dict_item i
      JOIN sys_dict_type t ON t.id = i.dict_type_id
     WHERE t.dict_code = 'config_category'
       AND i.item_value = 'erp_cost';
    IF v_dict <> 0 THEN
        RAISE EXCEPTION 'V0385 down 自检失败：仍有 % 条 erp_cost 字典项未删除', v_dict;
    END IF;
END $$;
