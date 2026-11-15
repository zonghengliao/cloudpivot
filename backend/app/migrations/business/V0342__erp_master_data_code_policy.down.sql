-- ============================================================
-- V0342（回滚）：撤销「允许手工指定编码」开关与参数分类字典项
--
-- 只删本迁移自己种的行：参数按 config_key、字典项按 (dict_type_id, item_value)。
-- 字典类型 `config_category` 是 V0196 / seed 的公共底座，**不删**。
-- ============================================================

DELETE FROM sys_config
 WHERE config_key = 'erp.master_data.allow_manual_code';

DELETE FROM sys_dict_item
 WHERE item_value = 'erp_master_data'
   AND dict_type_id IN (
       SELECT id FROM sys_dict_type
        WHERE dict_code = 'config_category' AND deleted_at IS NULL
   );

DO $$
DECLARE
    v_cfg int;
    v_dict int;
BEGIN
    SELECT count(*) INTO v_cfg FROM sys_config
     WHERE config_key = 'erp.master_data.allow_manual_code';
    IF v_cfg <> 0 THEN
        RAISE EXCEPTION 'V0342 down 自检失败：开关行残留 % 条', v_cfg;
    END IF;

    SELECT count(*) INTO v_dict
      FROM sys_dict_item i
      JOIN sys_dict_type t ON t.id = i.dict_type_id
     WHERE t.dict_code = 'config_category'
       AND i.item_value = 'erp_master_data';
    IF v_dict <> 0 THEN
        RAISE EXCEPTION 'V0342 down 自检失败：字典项残留 % 条', v_dict;
    END IF;

    RAISE NOTICE 'V0342 已回滚：主数据编码开关与 erp_master_data 字典项已删除';
END $$;
