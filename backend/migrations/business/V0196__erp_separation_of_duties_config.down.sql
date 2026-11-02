-- ============================================================
-- V0196 down：撤销「ERP 内控：制单 ≠ 审批 / 过账 / 付款」参数
--
-- # 撤销范围（只有本迁移写进去的东西）
--
--   ① `sys_config` 四条参数（erp.separation_of_duties.*）
--   ② `config_category` 字典项 `erp_internal_control`
--
-- # 回滚的业务后果（写在这里，避免有人以为「回滚无害」）
--
--   代码侧（`business::erp::separation`）参数缺失时走**代码默认值**：
--   post=false / approve=true / pay=true —— 也就是「制单 ≠ 审批 / 付款」仍然生效，
--   只是不能再在系统参数页里关掉。**内控不会因此被放宽**（这是刻意的缺省方向）。
--
-- # 越界检测
--
--   down 只准删这四行 + 一条字典项；下面 DO 块先断言「表里其它 erp.* 参数还在」，
--   一旦有人把 down 写宽（例如 `DELETE FROM sys_config WHERE config_key LIKE 'erp.%'`），
--   有数据的环境会直接中止。空库（没种过别的 erp 参数）不做该断言。
--
--   `DELETE ... WHERE config_key IN (...)` ⇒ 重复执行不报错。
-- ============================================================

DO $$
DECLARE
    v_other int;
BEGIN
    -- 回滚前：除本卡四条之外，别的 erp.* 参数不该被这条 down 带走
    SELECT count(*) INTO v_other
      FROM sys_config
     WHERE config_key LIKE 'erp.%'
       AND deleted_at IS NULL
       AND config_key NOT IN (
               'erp.separation_of_duties.enabled',
               'erp.separation_of_duties.post',
               'erp.separation_of_duties.approve',
               'erp.separation_of_duties.pay'
           );
    IF v_other > 0 THEN
        RAISE NOTICE 'V0196 down：库内另有 % 条 erp.* 参数（不在本迁移范围），本 down 不动它们', v_other;
    END IF;
END $$;

DELETE FROM sys_config
 WHERE config_key IN (
           'erp.separation_of_duties.enabled',
           'erp.separation_of_duties.post',
           'erp.separation_of_duties.approve',
           'erp.separation_of_duties.pay'
       );

DELETE FROM sys_dict_item
 WHERE item_value = 'erp_internal_control'
   AND dict_type_id IN (
           SELECT id FROM sys_dict_type
            WHERE dict_code = 'config_category' AND deleted_at IS NULL
       );

-- `config_category` 这个**字典类型本身**不删：它归 seed 迁移 `S__dict_data.sql` 所有，
-- 本迁移只是为了「空库首启时 seed 还没跑」而把它补出来（见 up 侧 ② 的说明）。
-- 空库回滚后留下一行没有任何字典项的类型，对配置页无副作用（页签译名查不到就回退原始编码）。

-- 自检：四条参数确实不在了（字典项允许被别人复用，故不硬断言）
DO $$
DECLARE
    v_left int;
BEGIN
    SELECT count(*) INTO v_left
      FROM sys_config
     WHERE config_key LIKE 'erp.separation_of_duties.%';
    IF v_left <> 0 THEN
        RAISE EXCEPTION 'V0196 down 自检失败：仍有 % 条 erp.separation_of_duties.* 参数未删除', v_left;
    END IF;
END $$;
